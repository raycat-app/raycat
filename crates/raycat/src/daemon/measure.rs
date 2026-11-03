//! Тест скорости по запросу. Демон закрепляет узел только в памяти, ждёт, пока xray его
//! применит, качает через служебный вход и в конце возвращает прежнее закрепление, даже если
//! замеры не удались. После перезапуска демона тест не остаётся: на диск он не пишется.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use raycat_http::{Client, Url, redact};
use raycat_proto::{SpeedtestRequest, SpeedtestResult, SpeedtestRun};
use raycat_select::PinTarget;
use raycat_xray::SpeedtestInbound;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{Daemon, stored_pin};
use crate::api::{Command, Refusal, Shared};
use crate::log::{info, warn};
use crate::selection;
use crate::speedtest;

/// Сколько ждать, пока xray применит закреплённый узел.
const APPLY_TIMEOUT: Duration = Duration::from_secs(15);
const APPLY_POLL: Duration = Duration::from_millis(100);

type Reply = oneshot::Sender<Result<SpeedtestResult, Refusal>>;

/// Предел одного замера: дольше — замер останавливается, а пришедшее идёт в результат.
const MEASURE_LIMIT: Duration = Duration::from_secs(15);

/// Узел, через который идёт тест: `id` для ответа и тег в xray.
struct NodeTarget {
    id: String,
    tag: String,
}

/// Всё, что нужно тесту, чтобы идти в отдельной задаче без демона.
struct Job {
    shared: Arc<Shared>,
    inbound: SpeedtestInbound,
    request: SpeedtestRequest,
    /// Адреса замера по порядку: основной и запасные.
    addresses: Vec<speedtest::Address>,
    target: NodeTarget,
    /// Закрепление, которое было до теста; вернётся в конце, при любом исходе.
    previous: Option<PinTarget>,
    reply: Reply,
    apply_timeout: Duration,
    measure_limit: Duration,
    /// `DELETE /v1/speedtest` будит эту задачу, и замер прерывается.
    cancel: Arc<Notify>,
}

impl Daemon {
    pub(super) fn start_speedtest(&mut self, request: SpeedtestRequest, reply: Reply) {
        let node = match self.speedtest_node(request.node.as_deref()) {
            Ok(node) => node,
            Err(refusal) => {
                self.shared.end_speedtest();
                let _ = reply.send(Err(refusal));
                return;
            }
        };
        let previous = stored_pin(&self.store, &self.config);
        let pin = PinTarget {
            subscription: node.subscription.clone(),
            node: node.name.clone(),
        };
        self.selector.set_pin(Some(pin));
        self.next_select = Instant::now();
        self.speedtest_active = true;
        let addresses = speedtest::addresses(request.url.as_deref());
        let job = Job {
            shared: Arc::clone(&self.shared),
            inbound: self.speedtest.clone(),
            request,
            addresses,
            target: NodeTarget {
                id: node.id,
                tag: node.tag,
            },
            previous,
            reply,
            apply_timeout: APPLY_TIMEOUT,
            measure_limit: MEASURE_LIMIT,
            cancel: self.shared.speedtest_cancel_handle().unwrap_or_default(),
        };
        tokio::spawn(run(job));
    }

    /// Возвращает закрепление, которое было до теста, и подтверждает это задаче теста.
    pub(super) fn finish_speedtest(&mut self, pin: Option<PinTarget>, ack: oneshot::Sender<()>) {
        self.speedtest_active = false;
        self.selector.set_pin(pin);
        self.next_select = Instant::now();
        let _ = ack.send(());
    }

    fn speedtest_node(&self, query: Option<&str>) -> Result<raycat_select::NodeInfo, Refusal> {
        let snapshot = self.selector.snapshot(selection::now());
        let found = match query {
            Some(id) => snapshot.nodes.into_iter().find(|node| node.id == id),
            None => snapshot.nodes.into_iter().find(|node| node.selected),
        };
        found.ok_or_else(|| match query {
            Some(id) => Refusal::NotFound(format!("узла «{id}» нет среди узлов подписок")),
            None => Refusal::Unavailable(
                "узел не выбран: подписки ещё не дали рабочих узлов".to_owned(),
            ),
        })
    }
}

/// Весь тест: узел, замеры, возврат закрепления и ответ. Закрепление возвращается и после
/// ошибки, поэтому ответ уходит только после того, как демон его вернул.
async fn run(job: Job) {
    let Job {
        shared,
        inbound,
        request,
        addresses,
        target,
        previous,
        reply,
        apply_timeout,
        measure_limit,
        cancel,
    } = job;
    let outcome = tokio::select! {
        outcome = measure(&shared, &inbound, &request, &target, &addresses, apply_timeout, measure_limit) => outcome,
        () = cancel.notified() => Err(anyhow::anyhow!("тест отменён")),
    };
    restore(&shared, previous).await;
    shared.end_speedtest();
    let answer = match outcome {
        Ok(result) => {
            info!(
                "тест скорости «{}», адрес {}: {}",
                result.node,
                result.url,
                summary(&result.runs)
            );
            Ok(result)
        }
        Err(error) => {
            warn!("тест скорости через «{}» не удался: {error:#}", target.id);
            Err(Refusal::Unavailable(format!("{error:#}")))
        }
    };
    let _ = reply.send(answer);
}

async fn measure(
    shared: &Shared,
    inbound: &SpeedtestInbound,
    request: &SpeedtestRequest,
    target: &NodeTarget,
    addresses: &[speedtest::Address],
    apply_timeout: Duration,
    limit: Duration,
) -> Result<SpeedtestResult> {
    wait_applied(shared, &target.tag, apply_timeout).await?;
    let proxy = Url::parse(&format!("http://127.0.0.1:{}", inbound.port))
        .context("адрес служебного входа xray")?;
    let base = Client {
        proxy: Some(proxy),
        proxy_authorization: Some(speedtest::proxy_authorization(&inbound.credentials)),
        ..Client::default()
    };
    let mut pool = addresses;
    let mut runs = Vec::new();
    let mut used = None;
    for (index, streams) in speedtest::runs(request.streams).into_iter().enumerate() {
        let share = speedtest::share(request.size, streams);
        let (run, position) =
            measure_run(shared, &base, pool, index + 1, streams, share, limit).await?;
        // Следующий замер сначала пробует тот адрес, с которого пошёл этот.
        pool = &pool[position..];
        used = pool.first().cloned();
        runs.push(run);
    }
    let address = used.context("нет адреса для замера")?;
    let url = redact(&Url::parse(&address.request_url(request.size))?);
    let hint = speedtest::hint(&runs);
    Ok(SpeedtestResult {
        node: target.id.clone(),
        url,
        runs,
        hint,
    })
}

/// Итог одной попытки: замер или сбой адреса, после которого можно взять запасной.
enum Attempt {
    Done(SpeedtestRun),
    Retry(String),
}

/// Один замер: все потоки параллельно. Если адрес подвёл, замер повторяется с запасным, но
/// не дольше `limit`. Возвращает замер и место адреса, с которого он пошёл, в `addresses`.
async fn measure_run(
    shared: &Shared,
    base: &Client,
    addresses: &[speedtest::Address],
    index: usize,
    streams: u8,
    share: u64,
    limit: Duration,
) -> Result<(SpeedtestRun, usize)> {
    shared.speedtest_step(index, 0);
    let deadline = Instant::now() + limit;
    let mut reason = "нет адресов для замера".to_owned();
    for (position, address) in addresses.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let client = Arc::new(Client {
            total_timeout: remaining,
            ..base.clone()
        });
        match attempt(shared, &client, address, index, streams, share).await? {
            Attempt::Done(run) => {
                // Остановленный пределом времени замер годен, пока к нему что-то пришло.
                if Instant::now() >= deadline && run.bytes < speedtest::MIN_BYTES {
                    bail!("замер дольше {} с, пришло меньше 64 КиБ", limit.as_secs());
                }
                return Ok((run, position));
            }
            Attempt::Retry(text) => reason = text,
        }
    }
    bail!("{reason}")
}

/// Все потоки на одном адресе. Сбой любого потока останавливает остальные.
async fn attempt(
    shared: &Shared,
    client: &Arc<Client>,
    address: &speedtest::Address,
    index: usize,
    streams: u8,
    share: u64,
) -> Result<Attempt> {
    let total = share * u64::from(streams);
    let done = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let mut set = JoinSet::new();
    for _ in 0..streams {
        let url = Url::parse(&address.request_url(share))?;
        let client = Arc::clone(client);
        let done = Arc::clone(&done);
        let stop = Arc::clone(&stop);
        let _ = set.spawn_blocking(move || speedtest::fetch(&client, &url, share, &done, &stop));
    }
    let mut samples = Vec::new();
    let mut failure = None;
    let mut tick = tokio::time::interval(speedtest::PROGRESS_STEP);
    loop {
        tokio::select! {
            joined = set.join_next() => match joined {
                Some(joined) => match joined.context("поток замера прервался")? {
                    Ok(sample) => samples.push(sample),
                    Err(speedtest::Failure::Retry(text)) => {
                        stop.store(true, Ordering::Relaxed);
                        failure.get_or_insert(text);
                    }
                    Err(speedtest::Failure::Fatal(text)) => bail!("{text}"),
                },
                None => break,
            },
            _ = tick.tick() => {
                shared.speedtest_step(index, speedtest::percent(done.load(Ordering::Relaxed), total));
            }
        }
    }
    if let Some(text) = failure {
        return Ok(Attempt::Retry(text));
    }
    if address.builtin && samples.iter().any(|sample| sample.short) {
        return Ok(Attempt::Retry("файл короче замера".to_owned()));
    }
    Ok(Attempt::Done(speedtest::summarize(streams, &samples)))
}

/// Ждёт, пока xray применит узел: иначе замер пошёл бы через прежний узел.
async fn wait_applied(shared: &Shared, tag: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while shared.xray_pin().as_deref() != Some(tag) {
        if Instant::now() >= deadline {
            bail!(
                "xray не применил узел за {} с: см. raycat status",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(APPLY_POLL).await;
    }
    Ok(())
}

/// Отправляет демону команду вернуть закрепление и ждёт подтверждения.
async fn restore(shared: &Shared, previous: Option<PinTarget>) {
    let (ack, acked) = oneshot::channel();
    if shared.command(Command::SpeedtestRestore { pin: previous, ack }) {
        let _ = acked.await;
    }
}

fn summary(runs: &[SpeedtestRun]) -> String {
    runs.iter()
        .map(|run| {
            format!(
                "{}: {:.1} Мбит/с, первый байт через {} мс",
                streams_text(run.streams),
                run.mbps,
                run.ttfb_ms
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn streams_text(streams: u8) -> String {
    let word = match streams {
        1 => "поток",
        2..=4 => "потока",
        _ => "потоков",
    };
    format!("{streams} {word}")
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use raycat_proto::{Mode, XrayStatus};

    use super::*;

    fn status() -> raycat_proto::Status {
        raycat_proto::Status {
            version: "0.1.0".to_owned(),
            mode: Mode::Proxy,
            uptime_secs: 0,
            kill_switch: None,
            xray: XrayStatus {
                running: true,
                pid: Some(1),
                restarts: 0,
                api_silent_secs: Some(0),
                api_stuck: false,
            },
            tunnel: raycat_proto::Tunnel::Working,
            node: None,
            subscriptions: Vec::new(),
        }
    }

    fn target() -> NodeTarget {
        NodeTarget {
            id: "main/NL-1".to_owned(),
            tag: "node-001-main".to_owned(),
        }
    }

    fn request(streams: Option<u8>, url: Option<&str>) -> SpeedtestRequest {
        SpeedtestRequest {
            node: None,
            size: 4,
            streams,
            url: url.map(str::to_owned),
        }
    }

    fn custom(url: &str) -> speedtest::Address {
        speedtest::Address {
            url: url.to_owned(),
            sized: false,
            builtin: false,
        }
    }

    fn fallback(url: &str) -> speedtest::Address {
        speedtest::Address {
            url: url.to_owned(),
            sized: false,
            builtin: true,
        }
    }

    /// Что отдать на запрос по пути внутри туннеля.
    #[derive(Clone, Copy)]
    enum Served {
        /// Ответ 200 с телом.
        Body(&'static str),
        /// Ответ с указанной строкой статуса и пустым телом.
        Status(&'static str),
        /// Ответ 200 обещает большое тело, отдаёт столько байт и молчит.
        Stall(usize),
    }

    /// Читает заголовки запроса и возвращает их текст.
    fn read_head(sock: &mut TcpStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        while !raw.ends_with(b"\r\n\r\n") {
            if sock.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            raw.push(byte[0]);
        }
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// Прокси с туннелем CONNECT: внутри туннеля ответ выбирается по концу пути запроса.
    fn fake_proxy(routes: &'static [(&'static str, Served)]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            while let Ok((mut sock, _)) = listener.accept() {
                read_head(&mut sock);
                let _ = sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                let head = read_head(&mut sock);
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let served = routes
                    .iter()
                    .find(|(route, _)| path.ends_with(*route))
                    .map_or(Served::Status("404 Not Found"), |(_, served)| *served);
                match served {
                    Served::Body(body) => {
                        let head =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                        let _ = sock.write_all(head.as_bytes());
                        let _ = sock.write_all(body.as_bytes());
                    }
                    Served::Status(status) => {
                        let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                        let _ = sock.write_all(head.as_bytes());
                    }
                    Served::Stall(sent) => {
                        let _ =
                            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n");
                        let _ = sock.write_all(&vec![b'x'; sent]);
                        thread::sleep(Duration::from_secs(3));
                    }
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn a_failed_test_still_gives_the_pin_back() {
        let (shared, mut commands) = Shared::new(status());
        assert!(shared.begin_speedtest(1));
        let previous = Some(PinTarget {
            subscription: "main".to_owned(),
            node: "NL-2".to_owned(),
        });
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: SpeedtestInbound {
                port: 9,
                ..speedtest::test_inbound()
            },
            request: request(Some(1), None),
            addresses: speedtest::addresses(None),
            target: target(),
            previous: previous.clone(),
            reply,
            apply_timeout: Duration::from_millis(50),
            measure_limit: MEASURE_LIMIT,
            cancel: Arc::new(Notify::new()),
        }));
        let Some(Command::SpeedtestRestore { pin, ack }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        assert_eq!(pin, previous);
        let _ = ack.send(());
        let refusal = answer.await.unwrap().unwrap_err();
        assert!(
            matches!(refusal, Refusal::Unavailable(ref text) if text.contains("xray не применил")),
            "{refusal:?}"
        );
        assert!(shared.speedtest_progress().is_none());
    }

    #[tokio::test]
    async fn a_test_measures_through_the_proxy_and_gives_the_pin_back() {
        let (shared, mut commands) = Shared::new(status());
        shared.set_xray_pin(Some("node-001-main".to_owned()));
        assert!(shared.begin_speedtest(1));
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: SpeedtestInbound {
                port: fake_proxy(&[("/file", Served::Body("0123456789"))]),
                ..speedtest::test_inbound()
            },
            request: request(Some(1), Some("http://203.0.113.7:8080/file")),
            addresses: speedtest::addresses(Some("http://203.0.113.7:8080/file")),
            target: target(),
            previous: None,
            reply,
            apply_timeout: Duration::from_secs(5),
            measure_limit: MEASURE_LIMIT,
            cancel: Arc::new(Notify::new()),
        }));
        let Some(Command::SpeedtestRestore { pin, ack }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        assert_eq!(pin, None);
        let _ = ack.send(());
        let result = answer.await.unwrap().unwrap();
        assert_eq!(result.node, "main/NL-1");
        assert_eq!(result.runs.len(), 1);
        assert_eq!(result.runs[0].bytes, 4);
        assert!(result.hint.is_none());
        assert!(shared.speedtest_progress().is_none());
    }

    #[tokio::test]
    async fn a_limited_address_moves_the_test_to_the_next_one() {
        let (shared, mut commands) = Shared::new(status());
        shared.set_xray_pin(Some("node-001-main".to_owned()));
        assert!(shared.begin_speedtest(1));
        let port = fake_proxy(&[
            ("/busy", Served::Status("429 Too Many Requests")),
            ("/free", Served::Body("0123456789")),
        ]);
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: SpeedtestInbound {
                port,
                ..speedtest::test_inbound()
            },
            request: request(Some(1), None),
            addresses: vec![
                fallback("http://203.0.113.7:8080/busy"),
                fallback("http://203.0.113.7:8080/free"),
            ],
            target: target(),
            previous: None,
            reply,
            apply_timeout: Duration::from_secs(5),
            measure_limit: MEASURE_LIMIT,
            cancel: Arc::new(Notify::new()),
        }));
        let Some(Command::SpeedtestRestore { ack, .. }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        let _ = ack.send(());
        let result = answer.await.unwrap().unwrap();
        assert_eq!(result.runs[0].bytes, 4);
        assert!(result.url.ends_with("…free"), "{}", result.url);
    }

    #[tokio::test]
    async fn a_slow_server_gives_the_bytes_it_sent_instead_of_an_error() {
        let (shared, mut commands) = Shared::new(status());
        shared.set_xray_pin(Some("node-001-main".to_owned()));
        assert!(shared.begin_speedtest(1));
        let port = fake_proxy(&[("/slow", Served::Stall(128 * 1024))]);
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: SpeedtestInbound {
                port,
                ..speedtest::test_inbound()
            },
            request: SpeedtestRequest {
                node: None,
                size: 1_000_000,
                streams: Some(1),
                url: None,
            },
            addresses: vec![custom("http://203.0.113.7:8080/slow")],
            target: target(),
            previous: None,
            reply,
            apply_timeout: Duration::from_secs(5),
            measure_limit: Duration::from_secs(1),
            cancel: Arc::new(Notify::new()),
        }));
        let Some(Command::SpeedtestRestore { ack, .. }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        let _ = ack.send(());
        let result = answer.await.unwrap().unwrap();
        let bytes = result.runs[0].bytes;
        assert!((64 * 1024..1_000_000).contains(&bytes), "{bytes}");
        assert!(result.runs[0].seconds > 0.0);
    }

    #[tokio::test]
    async fn a_slow_test_with_almost_no_data_is_an_error() {
        let (shared, mut commands) = Shared::new(status());
        shared.set_xray_pin(Some("node-001-main".to_owned()));
        assert!(shared.begin_speedtest(1));
        let port = fake_proxy(&[("/silent", Served::Stall(1000))]);
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: SpeedtestInbound {
                port,
                ..speedtest::test_inbound()
            },
            request: SpeedtestRequest {
                node: None,
                size: 1_000_000,
                streams: Some(1),
                url: None,
            },
            addresses: vec![custom("http://203.0.113.7:8080/silent")],
            target: target(),
            previous: None,
            reply,
            apply_timeout: Duration::from_secs(5),
            measure_limit: Duration::from_millis(300),
            cancel: Arc::new(Notify::new()),
        }));
        let Some(Command::SpeedtestRestore { ack, .. }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        let _ = ack.send(());
        let refusal = answer.await.unwrap().unwrap_err();
        assert!(matches!(refusal, Refusal::Unavailable(_)), "{refusal:?}");
    }

    #[tokio::test]
    async fn cancelling_a_test_gives_the_pin_back_at_once() {
        let (shared, mut commands) = Shared::new(status());
        assert!(shared.begin_speedtest(1));
        let previous = Some(PinTarget {
            subscription: "main".to_owned(),
            node: "NL-2".to_owned(),
        });
        let (reply, answer) = oneshot::channel();
        tokio::spawn(run(Job {
            shared: Arc::clone(&shared),
            inbound: speedtest::test_inbound(),
            request: request(Some(1), None),
            addresses: speedtest::addresses(None),
            target: target(),
            previous: previous.clone(),
            reply,
            apply_timeout: Duration::from_secs(60),
            measure_limit: MEASURE_LIMIT,
            cancel: shared.speedtest_cancel_handle().unwrap_or_default(),
        }));
        assert!(shared.cancel_speedtest());
        let Some(Command::SpeedtestRestore { pin, ack }) = commands.recv().await else {
            panic!("закрепление не возвращено");
        };
        assert_eq!(pin, previous);
        let _ = ack.send(());
        let refusal = answer.await.unwrap().unwrap_err();
        assert!(
            matches!(refusal, Refusal::Unavailable(ref text) if text.contains("отменён")),
            "{refusal:?}"
        );
        assert!(shared.speedtest_progress().is_none());
        assert!(!shared.cancel_speedtest());
    }

    #[test]
    fn the_word_for_streams_follows_the_count() {
        assert_eq!(streams_text(1), "1 поток");
        assert_eq!(streams_text(3), "3 потока");
        assert_eq!(streams_text(8), "8 потоков");
    }
}
