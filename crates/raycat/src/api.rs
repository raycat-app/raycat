//! API демона: HTTP+JSON по unix-сокету. Сокет 0600, соединение принимается только
//! от того же пользователя или root (`SO_PEERCRED`). Состояние читается из `Shared`,
//! которое обновляет демон; изменения (закрепление, обновление) идут ему командами.

use std::convert::Infallible;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use raycat_proto::{ErrorBody, Event, Nodes, PinRequest, Pinned, Status, UpdateRequest, Updates};
use raycat_xray_api::XrayApi;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;

/// Сколько обработчик ждёт ответа демона на команду: обновление подписки может идти долго.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(150);
const EVENT_BACKLOG: usize = 256;

/// Почему демон отказал в команде.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    NotFound(String),
    Invalid(String),
    Unavailable(String),
}

pub(crate) enum Command {
    /// `Some` — закрепить узел «подписка/имя», `None` — снять закрепление.
    Pin {
        node: Option<String>,
        reply: oneshot::Sender<Result<Pinned, Refusal>>,
    },
    Update {
        subscription: Option<String>,
        reply: oneshot::Sender<Result<Updates, Refusal>>,
    },
}

/// Общее состояние демона для обработчиков API.
pub(crate) struct Shared {
    started: Instant,
    status: Mutex<Status>,
    nodes: Mutex<Nodes>,
    xray: Mutex<Option<XrayApi>>,
    events: broadcast::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn new(status: Status) -> (Arc<Self>, mpsc::UnboundedReceiver<Command>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let shared = Arc::new(Self {
            started: Instant::now(),
            status: Mutex::new(status),
            nodes: Mutex::new(Nodes {
                selected: None,
                nodes: Vec::new(),
            }),
            xray: Mutex::new(None),
            events,
            commands,
        });
        (shared, receiver)
    }

    pub(crate) fn publish_status(&self, status: Status) {
        *locked(&self.status) = status;
    }

    pub(crate) fn publish_nodes(&self, nodes: Nodes) {
        *locked(&self.nodes) = nodes;
    }

    /// Клиент API xray для запросов трафика; `None`, пока xray не работает.
    pub(crate) fn set_xray(&self, api: Option<XrayApi>) {
        *locked(&self.xray) = api;
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Рассылает событие подписчикам потока; без подписчиков ничего не делает.
    pub(crate) fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<Refusal> for ApiError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::NotFound(message) => Self::new(StatusCode::NOT_FOUND, message),
            Refusal::Invalid(message) => Self::new(StatusCode::BAD_REQUEST, message),
            Refusal::Unavailable(message) => Self::new(StatusCode::SERVICE_UNAVAILABLE, message),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/nodes", get(nodes))
        .route("/v1/pin", post(pin).delete(unpin))
        .route("/v1/update", post(update))
        .route("/v1/events", get(events))
        .fallback(not_found)
        .with_state(shared)
}

#[allow(clippy::unused_async)]
async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "такого пути в API нет")
}

#[allow(clippy::unused_async)]
async fn status(State(shared): State<Arc<Shared>>) -> Json<Status> {
    let mut status = locked(&shared.status).clone();
    status.uptime_secs = shared.started.elapsed().as_secs();
    Json(status)
}

async fn nodes(State(shared): State<Arc<Shared>>) -> Json<Nodes> {
    let mut nodes = locked(&shared.nodes).clone();
    let api = locked(&shared.xray).clone();
    if let Some(api) = api
        && let Ok(traffic) = api.outbound_traffic().await
    {
        for node in &mut nodes.nodes {
            if let Some(found) = traffic.iter().find(|entry| entry.tag == node.tag) {
                node.uplink_bytes = Some(found.uplink);
                node.downlink_bytes = Some(found.downlink);
            }
        }
    }
    Json(nodes)
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("тело запроса не разобрано: {error}"),
        )
    })
}

/// Отправляет команду демону и ждёт ответа.
async fn ask<T: Serialize>(
    shared: &Shared,
    make: impl FnOnce(oneshot::Sender<Result<T, Refusal>>) -> Command,
) -> Result<Json<T>, ApiError> {
    let (reply, answer) = oneshot::channel();
    shared
        .commands
        .send(make(reply))
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "демон останавливается"))?;
    match tokio::time::timeout(COMMAND_TIMEOUT, answer).await {
        Ok(Ok(Ok(value))) => Ok(Json(value)),
        Ok(Ok(Err(refusal))) => Err(refusal.into()),
        Ok(Err(_)) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "демон не ответил на команду",
        )),
        Err(_) => Err(ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "демон не ответил на команду вовремя",
        )),
    }
}

async fn pin(State(shared): State<Arc<Shared>>, body: Bytes) -> Result<Json<Pinned>, ApiError> {
    let request: PinRequest = parse(&body)?;
    ask(&shared, |reply| Command::Pin {
        node: Some(request.node),
        reply,
    })
    .await
}

async fn unpin(State(shared): State<Arc<Shared>>) -> Result<Json<Pinned>, ApiError> {
    ask(&shared, |reply| Command::Pin { node: None, reply }).await
}

async fn update(State(shared): State<Arc<Shared>>, body: Bytes) -> Result<Json<Updates>, ApiError> {
    let request: UpdateRequest = if body.iter().all(u8::is_ascii_whitespace) {
        UpdateRequest::default()
    } else {
        parse(&body)?
    };
    ask(&shared, |reply| Command::Update {
        subscription: request.subscription,
        reply,
    })
    .await
}

fn sse(event: &Event) -> SseEvent {
    SseEvent::default()
        .event(event.name())
        .json_data(event)
        .unwrap_or_else(|_| SseEvent::default().event("error"))
}

#[allow(clippy::unused_async)]
async fn events(
    State(shared): State<Arc<Shared>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let receiver = shared.subscribe();
    let hello = Event::Hello {
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let live = BroadcastStream::new(receiver)
        .filter_map(Result::ok)
        .map(|event| Ok::<_, Infallible>(sse(&event)));
    let stream = tokio_stream::once(Ok::<_, Infallible>(sse(&hello))).chain(live);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Открывает сокет. Старый файл удаляется, только если это сокет, на котором никто
/// не слушает; права 0600.
pub(crate) fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    remove_stale(path)?;
    let listener = UnixListener::bind(path)
        .with_context(|| format!("не удалось открыть сокет API {}", path.display()))?;
    fs::set_permissions(path, Permissions::from_mode(0o600))
        .with_context(|| format!("не удалось ограничить права сокета {}", path.display()))?;
    Ok(listener)
}

fn remove_stale(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("не удалось проверить {}", path.display()));
        }
    };
    if !meta.file_type().is_socket() {
        bail!(
            "{} уже существует и не является сокетом: удалите его или задайте другой RAYCAT_SOCKET",
            path.display()
        );
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!(
            "на {} уже слушает другой демон raycat: остановите его или задайте другой RAYCAT_SOCKET",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)
            .with_context(|| format!("не удалось удалить старый сокет {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("не удалось проверить старый сокет {}", path.display())),
    }
}

/// Пускать ли соединение: тот же пользователь, что у демона, или root.
fn uid_allowed(peer: u32, daemon: u32) -> bool {
    peer == daemon || peer == 0
}

struct Guarded {
    listener: UnixListener,
    daemon_uid: u32,
}

impl axum::serve::Listener for Guarded {
    type Io = UnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, addr)) => {
                    let allowed = stream
                        .peer_cred()
                        .is_ok_and(|cred| uid_allowed(cred.uid(), self.daemon_uid));
                    if allowed {
                        return (stream, addr);
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// Обслуживает API, пока задачу не остановят. `daemon_uid` — пользователь демона.
pub(crate) async fn serve(
    listener: UnixListener,
    router: Router,
    daemon_uid: u32,
) -> io::Result<()> {
    axum::serve(
        Guarded {
            listener,
            daemon_uid,
        },
        router.into_make_service(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use raycat_proto::{
        CurrentNode, Mode, Node, NodeStatus, SubscriptionStatus, UpdateResult, XrayState,
        XrayStatus,
    };
    use std::os::unix::net::UnixListener as StdListener;
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

    use super::*;
    use crate::testing::TempDir;

    fn sample_status() -> Status {
        Status {
            version: "0.1.0".to_owned(),
            mode: Mode::Proxy,
            uptime_secs: 0,
            kill_switch: None,
            xray: XrayStatus {
                running: true,
                pid: Some(42),
                restarts: 0,
            },
            node: Some(CurrentNode {
                id: "main/NL-1".to_owned(),
                subscription: "main".to_owned(),
                name: "NL-1".to_owned(),
                latency_ms: Some(31),
                pinned: false,
                reason: Some("выбран лучший живой узел «NL-1»".to_owned()),
            }),
            subscriptions: vec![SubscriptionStatus {
                name: "main".to_owned(),
                url: "https://sub.example.com/…1234".to_owned(),
                title: Some("Тест".to_owned()),
                used_bytes: Some(3),
                total_bytes: Some(100),
                expire: Some(0),
                nodes: 1,
                updated_at: Some(1_000),
                next_update: Some(2_000),
                last_error: None,
                updating: false,
            }],
        }
    }

    fn sample_nodes() -> Nodes {
        Nodes {
            selected: Some("main/NL-1".to_owned()),
            nodes: vec![Node {
                id: "main/NL-1".to_owned(),
                subscription: "main".to_owned(),
                name: "NL-1".to_owned(),
                tag: "node-001-main".to_owned(),
                status: NodeStatus::Alive,
                latency_ms: Some(31),
                failures: 0,
                alive_for_secs: Some(60),
                last_error: None,
                selected: true,
                pinned: false,
                uplink_bytes: None,
                downlink_bytes: None,
            }],
        }
    }

    /// Фейковое состояние демона: команды выполняет небольшая задача.
    struct Fixture {
        _temp: TempDir,
        socket: std::path::PathBuf,
        shared: Arc<Shared>,
    }

    fn fixture() -> Fixture {
        let temp = TempDir::new("api");
        let socket = temp.path().join("raycat.sock");
        let (shared, mut commands) = Shared::new(sample_status());
        shared.publish_nodes(sample_nodes());
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    Command::Pin { node, reply } => {
                        let answer = if node.as_deref().is_none_or(|name| name == "main/NL-1") {
                            Ok(Pinned { node })
                        } else {
                            Err(Refusal::NotFound(format!(
                                "узла «{}» нет среди узлов подписок",
                                node.unwrap_or_default()
                            )))
                        };
                        let _ = reply.send(answer);
                    }
                    Command::Update {
                        subscription,
                        reply,
                    } => {
                        let answer = match subscription.as_deref() {
                            Some("nope") => Err(Refusal::NotFound(
                                "подписки «nope» нет в настройках".to_owned(),
                            )),
                            _ => Ok(Updates {
                                results: vec![UpdateResult {
                                    subscription: "main".to_owned(),
                                    ok: true,
                                    message: "узлов: 1".to_owned(),
                                    nodes: Some(1),
                                }],
                            }),
                        };
                        let _ = reply.send(answer);
                    }
                }
            }
        });
        let listener = bind(&socket).unwrap();
        tokio::spawn(serve(
            listener,
            router(Arc::clone(&shared)),
            crate::paths::euid(),
        ));
        Fixture {
            _temp: temp,
            socket,
            shared,
        }
    }

    async fn request(socket: &Path, method: &str, target: &str, body: &str) -> (u16, String) {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: raycat\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8(raw).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_owned())
    }

    async fn json(
        socket: &Path,
        method: &str,
        target: &str,
        body: &str,
    ) -> (u16, serde_json::Value) {
        let (status, text) = request(socket, method, target, body).await;
        (status, serde_json::from_str(&text).unwrap())
    }

    #[tokio::test]
    async fn status_reports_the_published_state_and_uptime() {
        let fixture = fixture();
        let (code, body) = json(&fixture.socket, "GET", "/v1/status", "").await;
        assert_eq!(code, 200);
        assert_eq!(body["version"], "0.1.0");
        assert_eq!(body["mode"], "proxy");
        assert_eq!(body["xray"]["pid"], 42);
        assert_eq!(body["node"]["id"], "main/NL-1");
        assert_eq!(
            body["subscriptions"][0]["url"],
            "https://sub.example.com/…1234"
        );
        assert!(body["uptime_secs"].as_u64().is_some());

        let mut changed = sample_status();
        changed.xray.restarts = 3;
        fixture.shared.publish_status(changed);
        let (_, body) = json(&fixture.socket, "GET", "/v1/status", "").await;
        assert_eq!(body["xray"]["restarts"], 3);
    }

    #[tokio::test]
    async fn nodes_come_from_the_snapshot_without_xray_traffic() {
        let fixture = fixture();
        let (code, body) = json(&fixture.socket, "GET", "/v1/nodes", "").await;
        assert_eq!(code, 200);
        assert_eq!(body["selected"], "main/NL-1");
        assert_eq!(body["nodes"][0]["tag"], "node-001-main");
        assert_eq!(body["nodes"][0]["status"], "alive");
        assert!(body["nodes"][0]["uplink_bytes"].is_null());
    }

    #[tokio::test]
    async fn pin_and_unpin_go_through_the_daemon() {
        let fixture = fixture();
        let (code, body) = json(
            &fixture.socket,
            "POST",
            "/v1/pin",
            r#"{"node":"main/NL-1"}"#,
        )
        .await;
        assert_eq!(
            (code, &body["node"]),
            (200, &serde_json::json!("main/NL-1"))
        );

        let (code, body) = json(&fixture.socket, "DELETE", "/v1/pin", "").await;
        assert_eq!(code, 200);
        assert!(body["node"].is_null());
    }

    #[tokio::test]
    async fn pin_errors_are_json_in_russian() {
        let fixture = fixture();
        let (code, body) = json(
            &fixture.socket,
            "POST",
            "/v1/pin",
            r#"{"node":"main/NL-9"}"#,
        )
        .await;
        assert_eq!(code, 404);
        assert!(body["error"].as_str().unwrap().contains("нет среди узлов"));

        let (code, body) = json(&fixture.socket, "POST", "/v1/pin", "не json").await;
        assert_eq!(code, 400);
        assert!(body["error"].as_str().unwrap().contains("не разобрано"));

        let (code, body) = json(&fixture.socket, "POST", "/v1/pin", "{}").await;
        assert_eq!(code, 400);
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn update_takes_an_optional_subscription() {
        let fixture = fixture();
        for body in ["", "{}", r#"{"subscription":"main"}"#] {
            let (code, answer) = json(&fixture.socket, "POST", "/v1/update", body).await;
            assert_eq!(code, 200, "{body:?}");
            assert_eq!(answer["results"][0]["ok"], true);
            assert_eq!(answer["results"][0]["nodes"], 1);
        }
        let (code, answer) = json(
            &fixture.socket,
            "POST",
            "/v1/update",
            r#"{"subscription":"nope"}"#,
        )
        .await;
        assert_eq!(code, 404);
        assert!(answer["error"].as_str().unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn unknown_paths_get_a_json_error() {
        let fixture = fixture();
        let (code, body) = json(&fixture.socket, "GET", "/v1/nothing", "").await;
        assert_eq!(code, 404);
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn a_stopped_daemon_answers_with_503() {
        let temp = TempDir::new("api-stopped");
        let socket = temp.path().join("raycat.sock");
        let (shared, commands) = Shared::new(sample_status());
        drop(commands);
        tokio::spawn(serve(
            bind(&socket).unwrap(),
            router(shared),
            crate::paths::euid(),
        ));
        let (code, body) = json(&socket, "DELETE", "/v1/pin", "").await;
        assert_eq!(code, 503);
        assert!(body["error"].as_str().unwrap().contains("останавливается"));
    }

    async fn next_event(lines: &mut tokio::io::Lines<BufReader<UnixStream>>) -> (String, String) {
        let mut name = String::new();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .expect("событие не пришло")
                .unwrap()
                .expect("поток закрыт");
            if let Some(value) = line.strip_prefix("event:") {
                name = value.trim().to_owned();
            } else if let Some(data) = line.strip_prefix("data:") {
                return (name, data.trim().to_owned());
            }
        }
    }

    #[tokio::test]
    async fn events_stream_starts_with_hello_and_carries_new_events() {
        let fixture = fixture();
        let mut stream = UnixStream::connect(&fixture.socket).await.unwrap();
        stream
            .write_all(
                b"GET /v1/events HTTP/1.1\r\nHost: raycat\r\nAccept: text/event-stream\r\n\r\n",
            )
            .await
            .unwrap();
        let mut lines = BufReader::new(stream).lines();
        let (name, data) = next_event(&mut lines).await;
        assert_eq!(name, "hello");
        assert!(data.contains("\"type\":\"hello\""));

        fixture.shared.emit(Event::NodeChanged {
            from: Some("main/NL-1".to_owned()),
            to: Some("main/DE-2".to_owned()),
            reason: "быстрее".to_owned(),
        });
        fixture.shared.emit(Event::Xray {
            state: XrayState::Exited,
            message: "упал".to_owned(),
        });
        let (name, data) = next_event(&mut lines).await;
        assert_eq!(name, "node_changed");
        let event: Event = serde_json::from_str(&data).unwrap();
        assert!(
            matches!(event, Event::NodeChanged { ref to, .. } if to.as_deref() == Some("main/DE-2"))
        );
        let (name, _) = next_event(&mut lines).await;
        assert_eq!(name, "xray");
    }

    #[tokio::test]
    async fn the_socket_is_private() {
        let fixture = fixture();
        let mode = fs::metadata(&fixture.socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn only_the_daemon_user_and_root_are_let_in() {
        assert!(uid_allowed(1000, 1000));
        assert!(uid_allowed(0, 1000));
        assert!(!uid_allowed(1001, 1000));
        assert!(uid_allowed(0, 0));
    }

    #[tokio::test]
    async fn a_dead_socket_file_is_replaced() {
        let temp = TempDir::new("api-stale");
        let path = temp.path().join("raycat.sock");
        drop(StdListener::bind(&path).unwrap());
        assert!(path.exists());
        let listener = bind(&path).unwrap();
        drop(listener);
    }

    #[tokio::test]
    async fn a_live_socket_is_never_taken_over() {
        let temp = TempDir::new("api-live");
        let path = temp.path().join("raycat.sock");
        let _alive = StdListener::bind(&path).unwrap();
        let error = bind(&path).unwrap_err();
        assert!(error.to_string().contains("уже слушает"), "{error}");
        assert!(UnixStream::connect(&path).await.is_ok());
    }

    #[tokio::test]
    async fn a_regular_file_is_never_deleted() {
        let temp = TempDir::new("api-file");
        let path = temp.path().join("raycat.sock");
        fs::write(&path, "данные").unwrap();
        let error = bind(&path).unwrap_err();
        assert!(error.to_string().contains("не является сокетом"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "данные");
    }

    #[tokio::test]
    async fn missing_parent_directories_are_created_private() {
        let temp = TempDir::new("api-parent");
        let path = temp.path().join("run/raycat/raycat.sock");
        let _listener = bind(&path).unwrap();
        let mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
