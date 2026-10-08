//! Тест скорости по запросу. Демон закрепляет узел только в памяти, ждёт, пока xray его
//! применит, качает через служебный вход и в конце возвращает прежнее закрепление, даже если
//! замеры не удались. После перезапуска демона тест не остаётся: на диск он не пишется.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use raycat_http::{Client, Url, redact};
use raycat_proto::{SpeedtestRequest, SpeedtestResult, SpeedtestRun};
use raycat_select::PinTarget;
use tokio::sync::oneshot;
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

/// Узел, через который идёт тест: `id` для ответа и тег в xray.
struct NodeTarget {
    id: String,
    tag: String,
}

/// Всё, что нужно тесту, чтобы идти в отдельной задаче без демона.
struct Job {
    shared: Arc<Shared>,
    port: u16,
    request: SpeedtestRequest,
    target: NodeTarget,
    /// Закрепление, которое было до теста; вернётся в конце, при любом исходе.
    previous: Option<PinTarget>,
    reply: Reply,
    apply_timeout: Duration,
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
        let job = Job {
            shared: Arc::clone(&self.shared),
            port: self.speedtest_port,
            request,
            target: NodeTarget {
                id: node.id,
                tag: node.tag,
            },
            previous,
            reply,
            apply_timeout: APPLY_TIMEOUT,
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
        port,
        request,
        target,
        previous,
        reply,
        apply_timeout,
    } = job;
    let outcome = measure(&shared, port, &request, &target, apply_timeout).await;
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
    port: u16,
    request: &SpeedtestRequest,
    target: &NodeTarget,
    apply_timeout: Duration,
) -> Result<SpeedtestResult> {
    wait_applied(shared, &target.tag, apply_timeout).await?;
    let proxy =
        Url::parse(&format!("http://127.0.0.1:{port}")).context("адрес служебного входа xray")?;
    let client = Arc::new(Client {
        proxy: Some(proxy),
        total_timeout: speedtest::STREAM_TIMEOUT,
        ..Client::default()
    });
    let custom = request.url.as_deref();
    let mut runs = Vec::new();
    for (index, streams) in speedtest::runs(request.streams).into_iter().enumerate() {
        let share = speedtest::share(request.size, streams);
        runs.push(measure_run(shared, &client, custom, index + 1, streams, share).await?);
    }
    let url = redact(&speedtest::stream_url(custom, request.size)?);
    let hint = speedtest::hint(&runs);
    Ok(SpeedtestResult {
        node: target.id.clone(),
        url,
        runs,
        hint,
    })
}

/// Один замер: все потоки параллельно. Прогресс — доля скачанного от объёма замера.
async fn measure_run(
    shared: &Shared,
    client: &Arc<Client>,
    custom: Option<&str>,
    index: usize,
    streams: u8,
    share: u64,
) -> Result<SpeedtestRun> {
    shared.speedtest_step(index, 0);
    let total = share * u64::from(streams);
    let done = Arc::new(AtomicU64::new(0));
    let mut set = JoinSet::new();
    for _ in 0..streams {
        let url = speedtest::stream_url(custom, share)?;
        let client = Arc::clone(client);
        let done = Arc::clone(&done);
        let _ = set.spawn_blocking(move || speedtest::fetch(&client, &url, share, &done));
    }
    let mut samples = Vec::new();
    let mut tick = tokio::time::interval(speedtest::PROGRESS_STEP);
    loop {
        tokio::select! {
            joined = set.join_next() => match joined {
                Some(joined) => samples.push(joined.context("поток замера прервался")??),
                None => break,
            },
            _ = tick.tick() => {
                shared.speedtest_step(index, speedtest::percent(done.load(Ordering::Relaxed), total));
            }
        }
    }
    Ok(speedtest::summarize(streams, &samples))
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
    if shared.command(Command::SpeedtestRestore {
        pin: previous,
        ack,
    }) {
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
            },
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

    fn read_head(sock: &mut TcpStream) {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        while !raw.ends_with(b"\r\n\r\n") {
            if sock.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            raw.push(byte[0]);
        }
    }

    /// Прокси с туннелем CONNECT: внутри туннеля отдаёт `body` на любой запрос.
    fn fake_proxy(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            while let Ok((mut sock, _)) = listener.accept() {
                read_head(&mut sock);
                let _ = sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                read_head(&mut sock);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(body.as_bytes());
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
            port: 9,
            request: request(Some(1), None),
            target: target(),
            previous: previous.clone(),
            reply,
            apply_timeout: Duration::from_millis(50),
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
            port: fake_proxy("0123456789"),
            request: request(Some(1), Some("http://203.0.113.7:8080/file")),
            target: target(),
            previous: None,
            reply,
            apply_timeout: Duration::from_secs(5),
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

    #[test]
    fn the_word_for_streams_follows_the_count() {
        assert_eq!(streams_text(1), "1 поток");
        assert_eq!(streams_text(3), "3 потока");
        assert_eq!(streams_text(8), "8 потоков");
    }
}
