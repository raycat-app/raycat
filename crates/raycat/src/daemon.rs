//! Демон: подписки → конфиг xray → процесс xray. Одна задача tokio разбирает
//! сигналы, результаты обновлений и завершение xray; сетевые запросы идут в
//! `spawn_blocking`.

use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use raycat_config::{Config, Subscription};
use raycat_subscription::redact_in;
use raycat_xray::Node;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;

use crate::log::{error, info, warn};
use crate::plan;
use crate::schedule::{HEALTHY_UPTIME, RESTART_FIRST, next_restart_delay};
use crate::store::Store;
use crate::updater::{self, Outcome, Refresh, Source};
use crate::util::{format_duration, format_time, now_unix};
use crate::xray::{Exit, Process};

const XRAY_CONFIG: &str = "xray.json";
/// Сколько при старте без кэша ждать первой попытки всех подписок.
const STARTUP_WAIT: Duration = Duration::from_secs(20);
/// Изменения узлов в пределах этого срока дают один перезапуск xray.
const RESTART_DEBOUNCE: Duration = Duration::from_secs(3);

type Finished = (usize, Refresh);

/// Блокирует поток до сигнала остановки.
pub(crate) fn run(config: Config, store: Store) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("не удалось запустить рантайм tokio")?;
    let result = runtime.block_on(serve(config, store));
    // Запрос к панели в spawn_blocking нельзя прервать: не ждём его дольше секунды.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn serve(config: Config, store: Store) -> Result<()> {
    let listen = plan::proxy_listen(&config)?;
    if !listen.ip().is_loopback() {
        warn!(
            "прокси слушает {listen} без пароля: им сможет пользоваться любой, кто до него дотянется"
        );
    }
    let machine_id = store.machine_id()?;
    let mut daemon = Daemon::new(config, store, &machine_id, free_port()?)?;
    info!(
        "raycat {} запущен: режим прокси, адрес {listen}, подписок: {}, состояние: {}",
        env!("CARGO_PKG_VERSION"),
        daemon.subs.len(),
        daemon.store.root().display()
    );
    daemon.load_caches();

    let mut terminate = signal(SignalKind::terminate()).context("не удалось поймать SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("не удалось поймать SIGINT")?;
    let mut hangup = signal(SignalKind::hangup()).context("не удалось поймать SIGHUP")?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Finished>();
    loop {
        daemon.tick(&tx).await;
        let wake = daemon.next_wake();
        tokio::select! {
            _ = terminate.recv() => {
                info!("получен SIGTERM, останавливаюсь");
                break;
            }
            _ = interrupt.recv() => {
                info!("получен SIGINT, останавливаюсь");
                break;
            }
            _ = hangup.recv() => {
                info!("получен SIGHUP, обновляю подписки");
                daemon.refresh_all();
            }
            Some((index, refresh)) = rx.recv() => daemon.finished(index, refresh),
            exit = daemon.process.exited() => daemon.crashed(&exit),
            () = wait_until(wake) => {}
        }
    }
    daemon.process.stop().await;
    Ok(())
}

async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

/// Порт для API xray: свободный порт на 127.0.0.1, выбранный при старте.
pub(crate) fn free_port() -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("не удалось занять свободный порт для API xray")?;
    Ok(listener.local_addr()?.port())
}

struct Sub {
    source: Arc<Source>,
    /// Узлы последнего рабочего ответа (из кэша или свежие).
    nodes: Vec<Node>,
    due: Option<Instant>,
    in_flight: bool,
    failures: u32,
    announce: Option<String>,
    /// Первая попытка получения (успешная или нет) уже закончилась.
    first_done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restart {
    Idle,
    Now,
    At(Instant),
}

struct Daemon {
    config: Config,
    store: Store,
    api_port: u16,
    subs: Vec<Sub>,
    process: Process,
    /// Конфиг, который записан для xray, и число узлов в нём.
    applied: Option<Vec<u8>>,
    applied_nodes: usize,
    restart: Restart,
    backoff: Duration,
    /// Старт без кэша: xray ждёт первой попытки всех подписок до этого срока.
    gather_until: Option<Instant>,
}

impl Daemon {
    fn new(config: Config, store: Store, machine_id: &str, api_port: u16) -> Result<Self> {
        let now = Instant::now();
        let subs = config
            .subscriptions
            .iter()
            .map(|subscription| -> Result<Sub> {
                Ok(Sub {
                    source: Arc::new(Source::new(&config, subscription, machine_id)?),
                    nodes: Vec::new(),
                    due: Some(now),
                    in_flight: false,
                    failures: 0,
                    announce: None,
                    first_done: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let process = Process::new(
            config.xray.path.clone(),
            store.path(XRAY_CONFIG),
            config.xray.memory_limit,
        );
        Ok(Self {
            config,
            store,
            api_port,
            subs,
            process,
            applied: None,
            applied_nodes: 0,
            restart: Restart::Idle,
            backoff: RESTART_FIRST,
            gather_until: None,
        })
    }

    /// Поднимает xray из кэша, не дожидаясь панелей. Без кэша ждёт первой попытки
    /// всех подписок, чтобы запустить xray один раз, а не по разу на каждую.
    fn load_caches(&mut self) {
        let store = self.store.clone();
        for sub in &mut self.subs {
            let name = sub.source.config().name.clone();
            if let Some(cached) = sub.source.cached(&store) {
                info!(
                    "подписка «{name}»: кэш от {}, узлов: {}",
                    format_time(cached.fetched_at),
                    cached.nodes.len()
                );
                sub.nodes = cached.nodes;
            } else {
                info!("подписка «{name}»: кэша нет, получаю с сервера");
            }
        }
        if self.subs.iter().any(|sub| !sub.nodes.is_empty()) {
            self.reconcile();
        } else {
            self.gather_until = Some(Instant::now() + STARTUP_WAIT);
        }
    }

    fn finish_gathering(&mut self) {
        self.gather_until = None;
        self.reconcile();
    }

    /// Собирает конфиг из текущих узлов; если он изменился, записывает его и
    /// планирует перезапуск xray.
    fn reconcile(&mut self) {
        let inputs: Vec<(&Subscription, &[Node])> = self
            .subs
            .iter()
            .map(|sub| (sub.source.config(), sub.nodes.as_slice()))
            .collect();
        let plan = match plan::compile_config(&self.config, &inputs, self.api_port) {
            Ok(plan) => plan,
            Err(error) => {
                warn!("конфиг xray не собран: {error:#}");
                return;
            }
        };
        if self.applied.as_deref() == Some(plan.json.as_slice()) {
            return;
        }
        if let Err(error) = self.store.write_file(XRAY_CONFIG, &plan.json) {
            error!("{error:#}");
            return;
        }
        if self.applied.is_none() {
            info!("конфиг xray собран, узлов: {}", plan.nodes);
        } else {
            info!(
                "узлы изменились: было {}, стало {}",
                self.applied_nodes, plan.nodes
            );
        }
        if plan.skipped > 0 {
            warn!(
                "узлов, которые xray не поддерживает, пропущено: {}",
                plan.skipped
            );
        }
        let first = self.applied.is_none();
        self.applied = Some(plan.json);
        self.applied_nodes = plan.nodes;
        if first {
            self.restart = Restart::Now;
        } else if self.restart != Restart::Now {
            // Несколько изменений подряд сливаются в один перезапуск.
            self.restart = Restart::At(Instant::now() + RESTART_DEBOUNCE);
        }
    }

    /// Запускает то, что подошло по времени: перезапуск xray и обновления подписок.
    async fn tick(&mut self, tx: &UnboundedSender<Finished>) {
        let now = Instant::now();
        if self.gather_until.is_some_and(|deadline| deadline <= now) {
            self.finish_gathering();
        }
        self.restart_process().await;
        let due: Vec<usize> = self
            .subs
            .iter()
            .enumerate()
            .filter(|(_, sub)| !sub.in_flight && sub.due.is_some_and(|at| at <= now))
            .map(|(index, _)| index)
            .collect();
        for index in due {
            self.spawn_refresh(index, tx);
        }
    }

    async fn restart_process(&mut self) {
        let due = match self.restart {
            Restart::Idle => false,
            Restart::Now => true,
            Restart::At(at) => at <= Instant::now(),
        };
        if !due {
            return;
        }
        self.restart = Restart::Idle;
        self.process.stop().await;
        if let Err(error) = self.process.start() {
            error!("{error:#}");
            self.schedule_restart();
        }
    }

    fn schedule_restart(&mut self) {
        self.restart = Restart::At(Instant::now() + self.backoff);
        self.backoff = next_restart_delay(self.backoff);
    }

    fn crashed(&mut self, exit: &Exit) {
        if exit.uptime >= HEALTHY_UPTIME {
            self.backoff = RESTART_FIRST;
        }
        error!(
            "xray завершился ({}) через {}, перезапуск через {}",
            exit.status,
            format_duration(exit.uptime),
            format_duration(self.backoff)
        );
        if self.restart == Restart::Idle {
            self.schedule_restart();
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let restart = match self.restart {
            Restart::Idle => None,
            Restart::Now => Some(Instant::now()),
            Restart::At(at) => Some(at),
        };
        self.subs
            .iter()
            .filter(|sub| !sub.in_flight)
            .filter_map(|sub| sub.due)
            .chain(restart)
            .chain(self.gather_until)
            .min()
    }

    fn refresh_all(&mut self) {
        let now = Instant::now();
        for sub in self.subs.iter_mut().filter(|sub| !sub.in_flight) {
            sub.due = Some(now);
        }
    }

    fn spawn_refresh(&mut self, index: usize, tx: &UnboundedSender<Finished>) {
        let Some(sub) = self.subs.get_mut(index) else {
            return;
        };
        sub.in_flight = true;
        sub.due = None;
        let source = Arc::clone(&sub.source);
        let failures = sub.failures;
        let store = self.store.clone();
        let tx = tx.clone();
        drop(tokio::task::spawn_blocking(move || {
            let refresh = updater::refresh(&source, &store, failures, now_unix());
            let _ = tx.send((index, refresh));
        }));
    }

    fn finished(&mut self, index: usize, refresh: Refresh) {
        let Some(sub) = self.subs.get_mut(index) else {
            return;
        };
        sub.in_flight = false;
        sub.first_done = true;
        sub.due = Some(Instant::now() + refresh.next_in);
        let name = sub.source.config().name.clone();
        match refresh.outcome {
            Outcome::Applied(analysis) => {
                let analysis = *analysis;
                sub.failures = 0;
                let url = sub.source.config().url.expose();
                info!(
                    "подписка «{name}» обновлена: {}; следующее обновление через {}",
                    redact_in(
                        &updater::summary(&analysis.info, analysis.nodes.len()),
                        &[url]
                    ),
                    format_duration(refresh.next_in)
                );
                for warning in &analysis.warnings {
                    warn!("подписка «{name}»: {warning}");
                }
                if let Some(announce) = analysis.info.announce.as_deref() {
                    let announce = redact_in(announce, &[url]);
                    if sub.announce.as_deref() != Some(announce.as_str()) {
                        info!("подписка «{name}»: объявление провайдера: {announce}");
                        sub.announce = Some(announce);
                    }
                }
                sub.nodes = analysis.nodes;
                if self.gather_until.is_none() {
                    self.reconcile();
                }
            }
            Outcome::Rejected(reason) => {
                sub.failures = sub.failures.saturating_add(1);
                warn!(
                    "подписка «{name}»: ответ не применён ({reason}), остаются прежние узлы; повтор через {}",
                    format_duration(refresh.next_in)
                );
            }
            Outcome::Failed(message) => {
                sub.failures = sub.failures.saturating_add(1);
                warn!(
                    "подписка «{name}»: не удалось обновить ({message}); повтор через {}",
                    format_duration(refresh.next_in)
                );
            }
        }
        if self.gather_until.is_some() && self.subs.iter().all(|sub| sub.first_done) {
            self.finish_gathering();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use raycat_config::Env;
    use raycat_subscription::analyze;

    use super::*;
    use crate::testing::TempDir;

    const ONE: &str = "ss://aes-128-gcm:secret@203.0.113.5:8388#One\n";
    const TWO: &str = "ss://aes-128-gcm:secret@203.0.113.6:8388#Two\n";
    const BOTH: &str = "ss://aes-128-gcm:secret@203.0.113.5:8388#One\nss://aes-128-gcm:secret@203.0.113.6:8388#Two\n";

    fn daemon(names: &[&str], temp: &TempDir) -> Daemon {
        let mut text = String::new();
        for name in names {
            let _ = write!(
                text,
                "[[subscription]]\nname = \"{name}\"\nurl = \"https://{name}.example.com/sub/token1234\"\napp = \"happ\"\nplatform = \"windows\"\n"
            );
        }
        let config = Config::from_toml_str(&text, &Env::new()).unwrap();
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        Daemon::new(config, store, "0d0af05ee8fd4dc29275718f2ce4dff1", 10_085).unwrap()
    }

    fn applied(links: &str) -> Refresh {
        let analysis = analyze(200, &[], links.as_bytes());
        assert!(analysis.problem.is_none());
        Refresh {
            outcome: Outcome::Applied(Box::new(analysis)),
            next_in: Duration::from_secs(43_200),
        }
    }

    fn failed() -> Refresh {
        Refresh {
            outcome: Outcome::Failed("нет связи".to_owned()),
            next_in: Duration::from_secs(30),
        }
    }

    #[test]
    fn without_a_cache_xray_starts_once_after_every_subscription_answered() {
        let temp = TempDir::new("gather");
        let mut daemon = daemon(&["a", "b"], &temp);
        daemon.load_caches();
        assert!(daemon.gather_until.is_some());

        daemon.finished(0, applied(ONE));
        assert!(daemon.gather_until.is_some());
        assert!(daemon.applied.is_none());
        assert_eq!(daemon.restart, Restart::Idle);

        daemon.finished(1, applied(TWO));
        assert!(daemon.gather_until.is_none());
        assert_eq!(daemon.applied_nodes, 2);
        assert_eq!(daemon.restart, Restart::Now);
    }

    #[test]
    fn a_failed_subscription_does_not_hold_xray_back() {
        let temp = TempDir::new("gather-failed");
        let mut daemon = daemon(&["a", "b"], &temp);
        daemon.load_caches();
        daemon.finished(1, failed());
        assert!(daemon.gather_until.is_some());
        daemon.finished(0, applied(ONE));
        assert!(daemon.gather_until.is_none());
        assert_eq!(daemon.applied_nodes, 1);
        assert_eq!(daemon.restart, Restart::Now);
    }

    #[test]
    fn when_every_subscription_failed_nothing_starts_yet() {
        let temp = TempDir::new("gather-none");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, failed());
        assert!(daemon.gather_until.is_none());
        assert!(daemon.applied.is_none());
        assert_eq!(daemon.restart, Restart::Idle);

        daemon.finished(0, applied(ONE));
        assert_eq!(daemon.restart, Restart::Now);
    }

    #[test]
    fn later_changes_are_merged_into_one_delayed_restart() {
        let temp = TempDir::new("debounce");
        let mut daemon = daemon(&["a", "b"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(ONE));
        daemon.finished(1, applied(TWO));
        daemon.restart = Restart::Idle;

        daemon.finished(0, applied(BOTH));
        let Restart::At(first) = daemon.restart else {
            panic!("ожидался отложенный перезапуск");
        };
        assert!(first > Instant::now());

        daemon.finished(1, applied(BOTH));
        assert!(matches!(daemon.restart, Restart::At(_)));
    }

    #[test]
    fn an_unchanged_answer_does_not_restart_xray() {
        let temp = TempDir::new("unchanged");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(ONE));
        daemon.restart = Restart::Idle;
        daemon.finished(0, applied(ONE));
        assert_eq!(daemon.restart, Restart::Idle);
    }

    #[test]
    fn a_pending_immediate_restart_is_not_postponed() {
        let temp = TempDir::new("pending-now");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(ONE));
        assert_eq!(daemon.restart, Restart::Now);
        daemon.finished(0, applied(BOTH));
        assert_eq!(daemon.restart, Restart::Now);
    }
}
