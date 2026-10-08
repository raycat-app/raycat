//! Связь с демоном: поток событий, обновление снимка состояния и действия
//! пользователя. Задачи не трогают терминал и общаются с интерфейсом сообщениями.

use std::sync::Arc;
use std::time::Duration;

use raycat_proto::Event;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};

use super::app::{Effect, Msg, Snapshot};
use crate::client::{Client, EventStream};

const EVENT_QUEUE: usize = 64;
const STREAM_CLOSED: &str = "демон закрыл поток событий: возможно, он остановлен";

/// Паузы связи: опрос как страховка потока событий, сборка событий в один запрос
/// и повторное подключение.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    pub(super) poll: Duration,
    pub(super) debounce: Duration,
    pub(super) retry_first: Duration,
    pub(super) retry_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(3),
            debounce: Duration::from_millis(250),
            retry_first: Duration::from_secs(1),
            retry_max: Duration::from_secs(5),
        }
    }
}

impl Timing {
    fn retry_delay(self, attempt: u32) -> Duration {
        self.retry_first
            .saturating_mul(2_u32.saturating_pow(attempt))
            .min(self.retry_max)
    }
}

/// Останавливает задачу при выходе из области видимости.
pub(super) struct Abort(pub(super) JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Outcome {
    /// Интерфейс закрылся: связь больше не нужна.
    UiGone,
    Lost {
        reason: String,
        connected: bool,
    },
}

fn lost(reason: String) -> Outcome {
    Outcome::Lost {
        reason,
        connected: true,
    }
}

/// Запускает задачу связи с демоном; она живёт, пока жив возвращённый `Abort`.
pub(super) fn spawn(
    client: Arc<Client>,
    inbox: mpsc::Sender<Msg>,
    refresh: Arc<Notify>,
    timing: Timing,
) -> Abort {
    Abort(tokio::spawn(async move {
        drive(&client, &inbox, &refresh, timing).await;
    }))
}

/// Подключается, отдаёт события и свежие снимки, а при обрыве сообщает причину и
/// пробует снова с растущей паузой.
async fn drive(client: &Client, inbox: &mpsc::Sender<Msg>, refresh: &Notify, timing: Timing) {
    let mut attempt = 0_u32;
    loop {
        let (reason, connected) = match session(client, inbox, refresh, timing).await {
            Outcome::UiGone => return,
            Outcome::Lost { reason, connected } => (reason, connected),
        };
        if connected {
            attempt = 0;
        }
        let delay = timing.retry_delay(attempt);
        attempt = attempt.saturating_add(1);
        let down = Msg::Down {
            reason,
            retry_in: delay,
        };
        if inbox.send(down).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
    }
}

async fn session(
    client: &Client,
    inbox: &mpsc::Sender<Msg>,
    refresh: &Notify,
    timing: Timing,
) -> Outcome {
    let stream = match client.events().await {
        Ok(stream) => stream,
        Err(error) => {
            return Outcome::Lost {
                reason: error.to_string(),
                connected: false,
            };
        }
    };
    if inbox.send(Msg::Connected).await.is_err() {
        return Outcome::UiGone;
    }
    watch(client, stream, inbox, refresh, timing).await
}

/// Читает поток в отдельной задаче: чтение посреди блока нельзя прерывать, а
/// ожидание в `watch` прерывается таймерами.
fn spawn_reader(stream: EventStream, queue: mpsc::Sender<Result<Event, String>>) -> Abort {
    Abort(tokio::spawn(async move {
        let mut stream = stream;
        loop {
            let item = match stream.next().await {
                Ok(Some(event)) => Ok(event),
                Ok(None) => Err(STREAM_CLOSED.to_owned()),
                Err(error) => Err(error.to_string()),
            };
            let last = item.is_err();
            if queue.send(item).await.is_err() || last {
                return;
            }
        }
    }))
}

async fn watch(
    client: &Client,
    stream: EventStream,
    inbox: &mpsc::Sender<Msg>,
    refresh: &Notify,
    timing: Timing,
) -> Outcome {
    let (queue, mut events) = mpsc::channel(EVENT_QUEUE);
    let _reader = spawn_reader(stream, queue);
    let mut pending = Some(Instant::now());
    let mut poll = interval_at(Instant::now() + timing.poll, timing.poll);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let deadline = pending;
        let due = async move {
            match deadline {
                Some(at) => sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            item = events.recv() => match item {
                Some(Ok(event)) => {
                    let wants_refresh = !matches!(event, Event::Hello { .. });
                    if inbox.send(Msg::Event(event)).await.is_err() {
                        return Outcome::UiGone;
                    }
                    if wants_refresh {
                        pending = pending.or_else(|| Some(Instant::now() + timing.debounce));
                    }
                }
                Some(Err(reason)) => return lost(reason),
                None => return lost(STREAM_CLOSED.to_owned()),
            },
            () = due => {
                pending = None;
                if let Some(outcome) = send_snapshot(client, inbox).await {
                    return outcome;
                }
            }
            _ = poll.tick() => {
                pending = None;
                if let Some(outcome) = send_snapshot(client, inbox).await {
                    return outcome;
                }
            }
            () = refresh.notified() => {
                pending = pending.or_else(|| Some(Instant::now() + timing.debounce));
            }
        }
    }
}

/// `Some` — связь надо завершить: интерфейс закрылся или демон не ответил.
async fn send_snapshot(client: &Client, inbox: &mpsc::Sender<Msg>) -> Option<Outcome> {
    let (status, nodes) = tokio::join!(client.status(), client.nodes());
    match (status, nodes) {
        (Ok(status), Ok(nodes)) => {
            let at = std::time::Instant::now();
            let msg = Msg::Snapshot(Box::new(Snapshot { status, nodes, at }));
            inbox.send(msg).await.is_err().then_some(Outcome::UiGone)
        }
        (Err(error), _) | (_, Err(error)) => Some(lost(error.to_string())),
    }
}

/// Выполняет действие пользователя; ошибка превращается в текст для строки состояния.
pub(super) async fn perform(client: &Client, effect: Effect) -> Msg {
    match effect {
        Effect::Pin(id) => Msg::Pinned(client.pin(&id).await.map_err(|error| error.to_string())),
        Effect::Unpin => Msg::Pinned(client.unpin().await.map_err(|error| error.to_string())),
        Effect::Update(subscription) => Msg::Updated(
            client
                .update(subscription.as_deref())
                .await
                .map_err(|error| error.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::broadcast;

    use super::*;
    use crate::testing::{TempDir, http_response};

    const STATUS: &str = r#"{"version":"0.1.0","mode":"gateway","uptime_secs":5,"kill_switch":true,
        "xray":{"running":true,"pid":7,"restarts":0},"node":null,"subscriptions":[]}"#;
    const NODES: &str = r#"{"selected":"main/NL-1","nodes":[{"id":"main/NL-1","subscription":"main",
        "name":"NL-1","tag":"t","status":"alive","latency_ms":31,"failures":0,"alive_for_secs":5,
        "last_error":null,"selected":true,"pinned":false,"uplink_bytes":null,"downlink_bytes":null}]}"#;

    struct Fake {
        socket: PathBuf,
        events: broadcast::Sender<String>,
        status_calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<String>>>,
        task: JoinHandle<()>,
    }

    impl Fake {
        fn emit(&self, event: &Event) {
            let _ = self.events.send(serde_json::to_string(event).unwrap());
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn read_request(stream: &mut UnixStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0_u8; 1];
        while !raw.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.unwrap() == 0 {
                break;
            }
            raw.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&raw).into_owned();
        let length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        format!("{head}{}", String::from_utf8_lossy(&body))
    }

    async fn respond(stream: &mut UnixStream, status: &str, body: &str) {
        let text = http_response(status, &[("Content-Type", "application/json")], body);
        let _ = stream.write_all(text.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    async fn chunk(stream: &mut UnixStream, data: &str) {
        let text = format!("{:x}\r\n{data}\r\n", data.len());
        let _ = stream.write_all(text.as_bytes()).await;
    }

    async fn stream_events(mut stream: UnixStream, events: &broadcast::Sender<String>, keep: bool) {
        let mut receiver = events.subscribe();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
        let _ = stream.write_all(head.as_bytes()).await;
        chunk(
            &mut stream,
            "event: hello\ndata: {\"type\":\"hello\",\"version\":\"0.1.0\"}\n\n",
        )
        .await;
        if !keep {
            let _ = stream.write_all(b"0\r\n\r\n").await;
            let _ = stream.shutdown().await;
            return;
        }
        while let Ok(json) = receiver.recv().await {
            chunk(&mut stream, &format!("data: {json}\n\n")).await;
        }
    }

    async fn serve(
        mut stream: UnixStream,
        events: broadcast::Sender<String>,
        status_calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<String>>>,
        keep: bool,
    ) {
        let request = read_request(&mut stream).await;
        requests.lock().unwrap().push(request.clone());
        let mut words = request.split_whitespace();
        let method = words.next().unwrap_or_default();
        let path = words.next().unwrap_or_default();
        match (method, path) {
            ("GET", "/v1/status") => {
                status_calls.fetch_add(1, Ordering::SeqCst);
                respond(&mut stream, "200 OK", STATUS).await;
            }
            ("GET", "/v1/nodes") => respond(&mut stream, "200 OK", NODES).await,
            ("GET", "/v1/events") => stream_events(stream, &events, keep).await,
            ("POST", "/v1/pin") => respond(&mut stream, "200 OK", r#"{"node":"main/NL-1"}"#).await,
            ("DELETE", "/v1/pin") => respond(&mut stream, "200 OK", r#"{"node":null}"#).await,
            ("POST", "/v1/update") => {
                respond(
                    &mut stream,
                    "503 Service Unavailable",
                    r#"{"error":"демон останавливается"}"#,
                )
                .await;
            }
            _ => respond(&mut stream, "404 Not Found", r#"{"error":"нет пути"}"#).await,
        }
    }

    fn start(socket: PathBuf, keep: bool) -> Fake {
        let listener = UnixListener::bind(&socket).unwrap();
        let (events, _) = broadcast::channel(16);
        let status_calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let (events, status_calls, requests) = (
                events.clone(),
                Arc::clone(&status_calls),
                Arc::clone(&requests),
            );
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(
                        stream,
                        events.clone(),
                        Arc::clone(&status_calls),
                        Arc::clone(&requests),
                        keep,
                    ));
                }
            })
        };
        Fake {
            socket,
            events,
            status_calls,
            requests,
            task,
        }
    }

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_secs(60),
            debounce: Duration::from_millis(20),
            retry_first: Duration::from_millis(20),
            retry_max: Duration::from_millis(40),
        }
    }

    fn spawn_link_with(
        socket: PathBuf,
        refresh: Arc<Notify>,
        timing: Timing,
    ) -> (Abort, mpsc::Receiver<Msg>) {
        let (inbox, messages) = mpsc::channel(64);
        let client = Arc::new(Client::new(socket));
        (spawn(client, inbox, refresh, timing), messages)
    }

    fn spawn_link(socket: PathBuf, refresh: Arc<Notify>) -> (Abort, mpsc::Receiver<Msg>) {
        spawn_link_with(socket, refresh, fast())
    }

    async fn next_msg(messages: &mut mpsc::Receiver<Msg>) -> Msg {
        tokio::time::timeout(Duration::from_secs(5), messages.recv())
            .await
            .expect("сообщение не пришло вовремя")
            .expect("канал сообщений закрыт")
    }

    #[test]
    fn retry_pauses_grow_up_to_the_limit() {
        let timing = Timing::default();
        let pauses: Vec<u64> = (0..6)
            .map(|attempt| timing.retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(pauses, [1, 2, 4, 5, 5, 5]);
        assert_eq!(timing.retry_delay(u32::MAX), timing.retry_max);
    }

    #[tokio::test]
    async fn connecting_brings_hello_and_a_snapshot() {
        let temp = TempDir::new("link-up");
        let fake = start(temp.path().join("raycat.sock"), true);
        let (_link, mut messages) = spawn_link(fake.socket.clone(), Arc::new(Notify::new()));
        assert!(matches!(next_msg(&mut messages).await, Msg::Connected));
        let (mut hello, mut snapshot) = (false, false);
        while !(hello && snapshot) {
            match next_msg(&mut messages).await {
                Msg::Event(Event::Hello { version }) => {
                    assert_eq!(version, "0.1.0");
                    hello = true;
                }
                Msg::Snapshot(data) => {
                    assert_eq!(data.status.version, "0.1.0");
                    assert_eq!(data.nodes.nodes.len(), 1);
                    snapshot = true;
                }
                _ => panic!("неожиданное сообщение"),
            }
        }
    }

    async fn wait_snapshot(messages: &mut mpsc::Receiver<Msg>) {
        loop {
            if matches!(next_msg(messages).await, Msg::Snapshot(_)) {
                return;
            }
        }
    }

    #[tokio::test]
    async fn an_event_and_a_nudge_each_trigger_one_more_snapshot() {
        let temp = TempDir::new("link-refresh");
        let fake = start(temp.path().join("raycat.sock"), true);
        let refresh = Arc::new(Notify::new());
        let (_link, mut messages) = spawn_link(fake.socket.clone(), Arc::clone(&refresh));
        wait_snapshot(&mut messages).await;
        assert_eq!(fake.status_calls.load(Ordering::SeqCst), 1);

        fake.emit(&Event::NodeChanged {
            from: None,
            to: Some("main/NL-1".to_owned()),
            reason: "лучший".to_owned(),
        });
        let mut saw_event = false;
        loop {
            match next_msg(&mut messages).await {
                Msg::Event(Event::NodeChanged { to, .. }) => {
                    assert_eq!(to.as_deref(), Some("main/NL-1"));
                    saw_event = true;
                }
                Msg::Snapshot(_) if saw_event => break,
                _ => {}
            }
        }
        assert_eq!(fake.status_calls.load(Ordering::SeqCst), 2);

        refresh.notify_one();
        wait_snapshot(&mut messages).await;
        assert_eq!(fake.status_calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_burst_of_events_costs_one_snapshot() {
        let temp = TempDir::new("link-burst");
        let fake = start(temp.path().join("raycat.sock"), true);
        let slow = Timing {
            debounce: Duration::from_millis(300),
            ..fast()
        };
        let (_link, mut messages) =
            spawn_link_with(fake.socket.clone(), Arc::new(Notify::new()), slow);
        wait_snapshot(&mut messages).await;
        for index in 0..5 {
            fake.emit(&Event::Warning {
                level: "warn".to_owned(),
                message: format!("{index}"),
            });
        }
        let mut events = 0;
        while events < 5 {
            if matches!(next_msg(&mut messages).await, Msg::Event(_)) {
                events += 1;
            }
        }
        wait_snapshot(&mut messages).await;
        assert_eq!(fake.status_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_closed_stream_is_reported_as_down() {
        let temp = TempDir::new("link-closed");
        let fake = start(temp.path().join("raycat.sock"), false);
        let (_link, mut messages) = spawn_link(fake.socket.clone(), Arc::new(Notify::new()));
        loop {
            if let Msg::Down { reason, retry_in } = next_msg(&mut messages).await {
                assert!(reason.contains("закрыл поток"), "{reason}");
                assert_eq!(retry_in, fast().retry_first);
                break;
            }
        }
    }

    #[tokio::test]
    async fn without_a_daemon_it_retries_and_connects_when_it_appears() {
        let temp = TempDir::new("link-late");
        let socket = temp.path().join("raycat.sock");
        let (_link, mut messages) = spawn_link(socket.clone(), Arc::new(Notify::new()));
        let Msg::Down { reason, retry_in } = next_msg(&mut messages).await else {
            panic!("ожидалось сообщение о недоступности");
        };
        assert!(reason.contains("не запущен"), "{reason}");
        assert_eq!(retry_in, fast().retry_first);
        let Msg::Down { retry_in, .. } = next_msg(&mut messages).await else {
            panic!("ожидалось повторное сообщение о недоступности");
        };
        assert_eq!(retry_in, fast().retry_first * 2);

        let _fake = start(socket, true);
        loop {
            if matches!(next_msg(&mut messages).await, Msg::Connected) {
                break;
            }
        }
    }

    #[tokio::test]
    async fn actions_go_through_the_api_and_errors_become_text() {
        let temp = TempDir::new("link-actions");
        let fake = start(temp.path().join("raycat.sock"), true);
        let client = Client::new(fake.socket.clone());

        let Msg::Pinned(Ok(pinned)) = perform(&client, Effect::Pin("main/NL-1".to_owned())).await
        else {
            panic!("ожидался ответ о закреплении");
        };
        assert_eq!(pinned.node.as_deref(), Some("main/NL-1"));
        let Msg::Pinned(Ok(unpinned)) = perform(&client, Effect::Unpin).await else {
            panic!("ожидался ответ о снятии закрепления");
        };
        assert_eq!(unpinned.node, None);
        let Msg::Updated(Err(error)) =
            perform(&client, Effect::Update(Some("main".to_owned()))).await
        else {
            panic!("ожидалась ошибка обновления");
        };
        assert_eq!(error, "демон останавливается");

        let requests = fake.requests();
        assert!(requests[0].starts_with("POST /v1/pin "));
        assert!(requests[0].ends_with(r#"{"node":"main/NL-1"}"#));
        assert!(requests[1].starts_with("DELETE /v1/pin "));
        assert!(requests[2].ends_with(r#"{"subscription":"main"}"#));
    }
}
