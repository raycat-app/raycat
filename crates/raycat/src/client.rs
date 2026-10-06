//! Клиент API демона: HTTP/1.1 по unix-сокету, ответы разбираются в типы `raycat-proto`.
//! Им пользуются команды CLI и будущий TUI.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use raycat_proto::{ErrorBody, Event, Nodes, PinRequest, Pinned, Status, UpdateRequest, Updates};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

use crate::util::sanitize;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Демон ждёт обновление подписки до 150 с.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(180);
/// Демон шлёт keep-alive раз в 15 с: тишина дольше минуты означает зависший демон.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_LINE: u64 = 16 * 1024;
const MAX_HEADERS: usize = 100;
const READ_SIZE: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) enum ClientError {
    NotRunning(PathBuf),
    Refused(PathBuf),
    Denied(PathBuf),
    Closed,
    TimedOut,
    /// Демон ответил ошибкой; текст его, на русском.
    Api(String),
    Protocol(String),
    Io(io::Error),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning(path) => write!(
                f,
                "демон не запущен: сокета {} нет (если демон работает от другого пользователя или с другим RAYCAT_SOCKET, задайте тот же RAYCAT_SOCKET)",
                path.display()
            ),
            Self::Refused(path) => write!(
                f,
                "демон не отвечает: сокет {} есть, но на нём никто не слушает (демон остановился аварийно?)",
                path.display()
            ),
            Self::Denied(path) => write!(
                f,
                "нет прав на сокет {}: API доступен только пользователю демона и root (попробуйте sudo)",
                path.display()
            ),
            Self::Closed => f.write_str(
                "демон закрыл соединение, не ответив: API доступен только пользователю демона и root, либо демон остановился",
            ),
            Self::TimedOut => f.write_str("демон не ответил вовремя"),
            Self::Api(message) => f.write_str(message),
            Self::Protocol(message) => write!(f, "неожиданный ответ демона: {message}"),
            Self::Io(error) => write!(f, "ошибка связи с демоном: {error}"),
        }
    }
}

impl std::error::Error for ClientError {}

fn map_io(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => ClientError::Closed,
        io::ErrorKind::UnexpectedEof => ClientError::Protocol("ответ оборван".to_owned()),
        _ => ClientError::Io(error),
    }
}

fn protocol(message: &str) -> ClientError {
    ClientError::Protocol(message.to_owned())
}

pub(crate) struct Client {
    socket: PathBuf,
    timeout: Duration,
}

impl Client {
    pub(crate) fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            timeout: REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn with_timeout(socket: PathBuf, timeout: Duration) -> Self {
        Self { socket, timeout }
    }

    pub(crate) async fn status(&self) -> Result<Status, ClientError> {
        self.call("GET", "/v1/status", None, self.timeout).await
    }

    pub(crate) async fn nodes(&self) -> Result<Nodes, ClientError> {
        self.call("GET", "/v1/nodes", None, self.timeout).await
    }

    pub(crate) async fn pin(&self, node: &str) -> Result<Pinned, ClientError> {
        let body = json_body(&PinRequest {
            node: node.to_owned(),
        })?;
        self.call("POST", "/v1/pin", Some(&body), self.timeout)
            .await
    }

    pub(crate) async fn unpin(&self) -> Result<Pinned, ClientError> {
        self.call("DELETE", "/v1/pin", None, self.timeout).await
    }

    /// Обновляет подписку или все сразу и ждёт результата.
    pub(crate) async fn update(&self, subscription: Option<&str>) -> Result<Updates, ClientError> {
        let body = json_body(&UpdateRequest {
            subscription: subscription.map(str::to_owned),
        })?;
        self.call(
            "POST",
            "/v1/update",
            Some(&body),
            self.timeout.max(UPDATE_TIMEOUT),
        )
        .await
    }

    /// Открывает поток событий; первым приходит `Event::Hello`.
    pub(crate) async fn events(&self) -> Result<EventStream, ClientError> {
        let open = async {
            let mut response = self
                .send("GET", "/v1/events", None, "text/event-stream")
                .await?;
            if response.status != 200 {
                let data = response.body.collect().await?;
                return Err(api_error(response.status, &data));
            }
            Ok::<_, ClientError>(response.body)
        };
        let body = tokio::time::timeout(self.timeout, open)
            .await
            .map_err(|_| ClientError::TimedOut)??;
        Ok(EventStream {
            body,
            buffer: Vec::new(),
            data: String::new(),
        })
    }

    async fn connect(&self) -> Result<UnixStream, ClientError> {
        UnixStream::connect(&self.socket)
            .await
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => ClientError::NotRunning(self.socket.clone()),
                io::ErrorKind::ConnectionRefused => ClientError::Refused(self.socket.clone()),
                io::ErrorKind::PermissionDenied => ClientError::Denied(self.socket.clone()),
                _ => ClientError::Io(error),
            })
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        accept: &str,
    ) -> Result<Response, ClientError> {
        let mut stream = self.connect().await?;
        let payload = if method == "GET" {
            "\r\n".to_owned()
        } else {
            let body = body.unwrap_or_default();
            format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: raycat\r\nUser-Agent: raycat/{}\r\nAccept: {accept}\r\nConnection: close\r\n{payload}",
            env!("CARGO_PKG_VERSION")
        );
        stream.write_all(request.as_bytes()).await.map_err(map_io)?;
        read_head(BufReader::new(stream)).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        limit: Duration,
    ) -> Result<T, ClientError> {
        let exchange = async {
            let mut response = self.send(method, path, body, "application/json").await?;
            let data = response.body.collect().await?;
            Ok::<_, ClientError>((response.status, data))
        };
        let (status, data) = tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| ClientError::TimedOut)??;
        if (200..300).contains(&status) {
            serde_json::from_slice(&data)
                .map_err(|error| ClientError::Protocol(format!("ответ не разобран: {error}")))
        } else {
            Err(api_error(status, &data))
        }
    }
}

fn json_body<T: Serialize>(value: &T) -> Result<String, ClientError> {
    serde_json::to_string(value)
        .map_err(|error| ClientError::Protocol(format!("запрос не собран: {error}")))
}

fn api_error(status: u16, data: &[u8]) -> ClientError {
    let message = serde_json::from_slice::<ErrorBody>(data).map_or_else(
        |_| format!("демон ответил кодом {status}"),
        |body| body.error,
    );
    ClientError::Api(sanitize(&message))
}

struct Response {
    status: u16,
    body: Body,
}

enum Framing {
    Chunked,
    Length(usize),
    UntilClose,
}

struct Body {
    reader: BufReader<UnixStream>,
    framing: Framing,
}

/// Строка без `\r\n`; `None` — соединение закрыто до первого байта.
async fn read_line(reader: &mut BufReader<UnixStream>) -> Result<Option<String>, ClientError> {
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(MAX_LINE)
        .read_until(b'\n', &mut line)
        .await
        .map_err(map_io)?;
    if read == 0 {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') {
        return Err(protocol("слишком длинная или оборванная строка"));
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

fn parse_status(line: &str) -> Result<u16, ClientError> {
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(protocol("это не ответ HTTP"));
    }
    parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| protocol("в ответе нет кода состояния"))
}

async fn read_head(mut reader: BufReader<UnixStream>) -> Result<Response, ClientError> {
    let first = read_line(&mut reader).await?.ok_or(ClientError::Closed)?;
    let status = parse_status(&first)?;
    let mut length = None;
    let mut chunked = false;
    for _ in 0..=MAX_HEADERS {
        let line = read_line(&mut reader)
            .await?
            .ok_or_else(|| protocol("заголовки оборваны"))?;
        if line.is_empty() {
            let framing = if chunked {
                Framing::Chunked
            } else {
                length.map_or(Framing::UntilClose, Framing::Length)
            };
            return Ok(Response {
                status,
                body: Body { reader, framing },
            });
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => {
                    length = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| protocol("неверный Content-Length"))?,
                    );
                }
                "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
    }
    Err(protocol("слишком много заголовков"))
}

impl Body {
    /// Следующая порция тела; `None` — тело закончилось.
    async fn next(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        match self.framing {
            Framing::Chunked => self.next_chunk().await,
            Framing::Length(0) => Ok(None),
            Framing::Length(left) => {
                let mut part = vec![0; left.min(READ_SIZE)];
                let read = self.reader.read(&mut part).await.map_err(map_io)?;
                if read == 0 {
                    return Err(protocol("ответ оборван"));
                }
                part.truncate(read);
                self.framing = Framing::Length(left - read);
                Ok(Some(part))
            }
            Framing::UntilClose => {
                let mut part = vec![0; READ_SIZE];
                let read = self.reader.read(&mut part).await.map_err(map_io)?;
                if read == 0 {
                    return Ok(None);
                }
                part.truncate(read);
                Ok(Some(part))
            }
        }
    }

    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        let line = read_line(&mut self.reader)
            .await?
            .ok_or_else(|| protocol("поток оборван"))?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size =
            usize::from_str_radix(size, 16).map_err(|_| protocol("неверный размер блока"))?;
        if size == 0 {
            while read_line(&mut self.reader)
                .await?
                .is_some_and(|trailer| !trailer.is_empty())
            {}
            return Ok(None);
        }
        if size > MAX_BODY {
            return Err(protocol("блок ответа слишком большой"));
        }
        let mut data = vec![0; size];
        self.reader.read_exact(&mut data).await.map_err(map_io)?;
        read_line(&mut self.reader).await?;
        Ok(Some(data))
    }

    async fn collect(&mut self) -> Result<Vec<u8>, ClientError> {
        let mut all = Vec::new();
        while let Some(part) = self.next().await? {
            if all.len() + part.len() > MAX_BODY {
                return Err(protocol("ответ слишком большой"));
            }
            all.extend(part);
        }
        Ok(all)
    }
}

/// Поток событий SSE.
pub(crate) struct EventStream {
    body: Body,
    buffer: Vec<u8>,
    data: String,
}

impl EventStream {
    /// Следующее событие; `None` — демон закрыл поток.
    pub(crate) async fn next(&mut self) -> Result<Option<Event>, ClientError> {
        loop {
            while let Some(line) = self.take_line() {
                if let Some(event) = self.feed(&line) {
                    return Ok(Some(event));
                }
            }
            let part = tokio::time::timeout(IDLE_TIMEOUT, self.body.next())
                .await
                .map_err(|_| ClientError::TimedOut)??;
            let Some(part) = part else {
                return Ok(None);
            };
            self.buffer.extend(part);
            if self.buffer.len() > MAX_BODY {
                return Err(protocol("строка потока слишком длинная"));
            }
        }
    }

    fn take_line(&mut self) -> Option<String> {
        let end = self.buffer.iter().position(|byte| *byte == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Пустая строка завершает событие. События неизвестного типа (от более
    /// нового демона) пропускаются: поток от этого не должен обрываться.
    fn feed(&mut self, line: &str) -> Option<Event> {
        if line.is_empty() {
            let data = std::mem::take(&mut self.data);
            return serde_json::from_str(&data).ok();
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::net::UnixListener as StdListener;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use raycat_proto::NodeStatus;
    use tokio::net::UnixListener;

    use super::*;
    use crate::testing::{TempDir, http_response};

    struct Fake {
        _temp: TempDir,
        socket: PathBuf,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn client(&self) -> Client {
            Client::new(self.socket.clone())
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    async fn read_request(stream: &mut UnixStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
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

    /// Фейковый демон: на каждый запрос отвечает частями, которые вернул обработчик.
    fn fake(handler: impl Fn(&str) -> Vec<Vec<u8>> + Send + 'static) -> Fake {
        let temp = TempDir::new("client");
        let socket = temp.path().join("raycat.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let request = read_request(&mut stream).await;
                seen.lock().unwrap().push(request.clone());
                for part in handler(&request) {
                    if stream.write_all(&part).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let _ = stream.shutdown().await;
            }
        });
        Fake {
            _temp: temp,
            socket,
            requests,
        }
    }

    fn reply(status: &str, body: &str) -> Vec<Vec<u8>> {
        vec![http_response(status, &[("Content-Type", "application/json")], body).into_bytes()]
    }

    const STATUS: &str = r#"{"version":"0.1.0","mode":"gateway","uptime_secs":5,"kill_switch":true,
        "xray":{"running":true,"pid":7,"restarts":1},
        "node":{"id":"main/NL-1","subscription":"main","name":"NL-1","latency_ms":31,"pinned":true,"reason":"причина"},
        "subscriptions":[]}"#;

    #[tokio::test]
    async fn status_is_parsed_into_proto_types() {
        let fake = fake(|_| reply("200 OK", STATUS));
        let status = fake.client().status().await.unwrap();
        assert_eq!(status.version, "0.1.0");
        assert_eq!(status.kill_switch, Some(true));
        assert_eq!(status.node.unwrap().id, "main/NL-1");
        let request = &fake.requests()[0];
        assert!(
            request.starts_with("GET /v1/status HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("Host: raycat"));
    }

    #[tokio::test]
    async fn nodes_are_parsed() {
        let body = r#"{"selected":"a/x","nodes":[{"id":"a/x","subscription":"a","name":"x","tag":"t",
            "status":"dead","latency_ms":null,"failures":4,"alive_for_secs":null,"last_error":"тайм-аут",
            "selected":true,"pinned":false,"uplink_bytes":1,"downlink_bytes":2}]}"#;
        let fake = fake(move |_| reply("200 OK", body));
        let nodes = fake.client().nodes().await.unwrap();
        assert_eq!(nodes.selected.as_deref(), Some("a/x"));
        assert_eq!(nodes.nodes[0].status, NodeStatus::Dead);
        assert_eq!(nodes.nodes[0].failures, 4);
    }

    #[tokio::test]
    async fn chunked_bodies_are_reassembled() {
        let body = r#"{"node":"main/NL-1"}"#;
        let (head, tail) = body.split_at(7);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{head}\r\n{:x};ext=1\r\n{tail}\r\n0\r\nX-Trailer: 1\r\n\r\n",
            head.len(),
            tail.len()
        );
        let fake = fake(move |_| vec![chunked.clone().into_bytes()]);
        let pinned = fake.client().unpin().await.unwrap();
        assert_eq!(pinned.node.as_deref(), Some("main/NL-1"));
    }

    #[tokio::test]
    async fn a_body_without_length_ends_with_the_connection() {
        let fake = fake(|_| {
            vec![
                b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"node\":".to_vec(),
                b"null}".to_vec(),
            ]
        });
        let pinned = fake.client().unpin().await.unwrap();
        assert_eq!(pinned.node, None);
    }

    #[tokio::test]
    async fn pin_sends_a_json_body_and_unpin_a_delete() {
        let fake = fake(|request| {
            if request.starts_with("DELETE") {
                reply("200 OK", r#"{"node":null}"#)
            } else {
                reply("200 OK", r#"{"node":"main/NL-1"}"#)
            }
        });
        let client = fake.client();
        assert_eq!(
            client.pin("main/NL-1").await.unwrap().node.as_deref(),
            Some("main/NL-1")
        );
        assert_eq!(client.unpin().await.unwrap().node, None);
        let requests = fake.requests();
        assert!(requests[0].starts_with("POST /v1/pin HTTP/1.1\r\n"));
        assert!(requests[0].contains("Content-Type: application/json"));
        assert!(
            requests[0].ends_with(r#"{"node":"main/NL-1"}"#),
            "{}",
            requests[0]
        );
        assert!(requests[1].starts_with("DELETE /v1/pin HTTP/1.1\r\n"));
        assert!(requests[1].contains("Content-Length: 0"));
    }

    #[tokio::test]
    async fn update_names_the_subscription_when_asked() {
        let body =
            r#"{"results":[{"subscription":"main","ok":true,"message":"узлов: 12","nodes":12}]}"#;
        let fake = fake(move |_| reply("200 OK", body));
        let client = fake.client();
        let all = client.update(None).await.unwrap();
        assert_eq!(all.results[0].nodes, Some(12));
        client.update(Some("main")).await.unwrap();
        let requests = fake.requests();
        assert!(requests[0].starts_with("POST /v1/update HTTP/1.1\r\n"));
        assert!(requests[0].ends_with(r#"{"subscription":null}"#));
        assert!(requests[1].ends_with(r#"{"subscription":"main"}"#));
    }

    #[tokio::test]
    async fn api_errors_carry_the_daemon_message() {
        let fake = fake(|_| reply("404 Not Found", r#"{"error":"узла «x» нет среди узлов"}"#));
        let error = fake.client().pin("x").await.unwrap_err();
        assert!(matches!(error, ClientError::Api(_)), "{error:?}");
        assert_eq!(error.to_string(), "узла «x» нет среди узлов");
    }

    #[tokio::test]
    async fn an_error_without_json_gets_a_generic_message() {
        let fake = fake(|_| reply("502 Bad Gateway", "<html>плохой шлюз</html>"));
        let error = fake.client().status().await.unwrap_err();
        assert_eq!(error.to_string(), "демон ответил кодом 502");
    }

    #[tokio::test]
    async fn control_characters_in_errors_are_neutralized() {
        let fake = fake(|_| reply("400 Bad Request", "{\"error\":\"a\\u001b[31mb\"}"));
        let error = fake.client().status().await.unwrap_err();
        assert_eq!(error.to_string(), "a [31mb");
    }

    #[tokio::test]
    async fn a_missing_socket_means_the_daemon_is_not_running() {
        let temp = TempDir::new("client-missing");
        let socket = temp.path().join("none.sock");
        let error = Client::new(socket.clone()).status().await.unwrap_err();
        assert!(matches!(error, ClientError::NotRunning(ref path) if *path == socket));
        let text = error.to_string();
        assert!(
            text.contains("не запущен") && text.contains("none.sock"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_dead_socket_file_is_reported_as_refused() {
        let temp = TempDir::new("client-dead");
        let socket = temp.path().join("raycat.sock");
        drop(StdListener::bind(&socket).unwrap());
        let error = Client::new(socket).status().await.unwrap_err();
        assert!(matches!(error, ClientError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("никто не слушает"));
    }

    #[tokio::test]
    async fn a_regular_file_is_not_a_daemon() {
        let temp = TempDir::new("client-file");
        let socket = temp.path().join("raycat.sock");
        fs::write(&socket, "данные").unwrap();
        let error = Client::new(socket).status().await.unwrap_err();
        assert!(matches!(error, ClientError::Refused(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_daemon_that_hangs_up_is_reported() {
        let fake = fake(|_| Vec::new());
        let error = fake.client().status().await.unwrap_err();
        assert!(matches!(error, ClientError::Closed), "{error:?}");
        assert!(error.to_string().contains("root"));
    }

    #[tokio::test]
    async fn a_silent_daemon_times_out() {
        let temp = TempDir::new("client-silent");
        let socket = temp.path().join("raycat.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let held = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(held);
        });
        let client = Client::with_timeout(socket, Duration::from_millis(100));
        let error = client.status().await.unwrap_err();
        assert!(matches!(error, ClientError::TimedOut), "{error:?}");
    }

    #[tokio::test]
    async fn garbage_is_a_protocol_error() {
        let fake = fake(|_| vec![b"SSH-2.0-OpenSSH\r\n\r\n".to_vec()]);
        let error = fake.client().status().await.unwrap_err();
        assert!(matches!(error, ClientError::Protocol(_)), "{error:?}");

        let fake = fake_with_body("не json");
        let error = fake.client().status().await.unwrap_err();
        assert!(error.to_string().contains("не разобран"), "{error}");
    }

    fn fake_with_body(body: &'static str) -> Fake {
        fake(move |_| reply("200 OK", body))
    }

    #[tokio::test]
    async fn an_absurd_chunk_size_is_refused() {
        let fake = fake(|_| {
            vec![b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffff\r\n".to_vec()]
        });
        let error = fake.client().status().await.unwrap_err();
        assert!(matches!(error, ClientError::Protocol(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_truncated_body_is_a_protocol_error() {
        let fake = fake(|_| vec![b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}".to_vec()]);
        let error = fake.client().status().await.unwrap_err();
        assert!(matches!(error, ClientError::Protocol(_)), "{error:?}");
    }

    fn sse(parts: &[&str]) -> Vec<Vec<u8>> {
        let mut out = vec![b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec()];
        for part in parts {
            out.push(format!("{:x}\r\n{part}\r\n", part.len()).into_bytes());
        }
        out.push(b"0\r\n\r\n".to_vec());
        out
    }

    #[tokio::test]
    async fn events_are_read_across_chunk_boundaries() {
        let fake = fake(|_| {
            sse(&[
                "event: hello\ndata: {\"type\":\"hello\",\"ver",
                "sion\":\"0.1.0\"}\n\n: keep-alive\n\n",
                "event: future\ndata: {\"type\":\"from_the_future\"}\n\n",
                "event: node_changed\r\ndata: {\"type\":\"node_changed\",\"from\":\"a/x\",\"to\":null,\"reason\":\"сбой\"}\r\n\r\n",
            ])
        });
        let mut stream = fake.client().events().await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap(),
            Some(Event::Hello { ref version }) if version == "0.1.0"
        ));
        let changed = stream.next().await.unwrap().unwrap();
        assert_eq!(
            changed,
            Event::NodeChanged {
                from: Some("a/x".to_owned()),
                to: None,
                reason: "сбой".to_owned()
            }
        );
        assert!(stream.next().await.unwrap().is_none());
        let request = &fake.requests()[0];
        assert!(request.contains("Accept: text/event-stream"));
    }

    #[tokio::test]
    async fn an_error_on_the_event_stream_is_reported() {
        let fake = fake(|_| {
            reply(
                "503 Service Unavailable",
                r#"{"error":"демон останавливается"}"#,
            )
        });
        let Err(error) = fake.client().events().await else {
            panic!("поток не должен открываться");
        };
        assert_eq!(error.to_string(), "демон останавливается");
    }

    #[test]
    fn socket_path_is_shown_in_the_errors() {
        let path = Path::new("/run/raycat/raycat.sock").to_path_buf();
        assert!(
            ClientError::Denied(path.clone())
                .to_string()
                .contains("sudo")
        );
        assert!(
            ClientError::NotRunning(path)
                .to_string()
                .contains("/run/raycat/raycat.sock")
        );
    }
}
