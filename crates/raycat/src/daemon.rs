//! Демон: подписки → конфиг xray → процесс xray. Одна задача tokio разбирает
//! сигналы, результаты обновлений и завершение xray; сетевые запросы идут в
//! `spawn_blocking`.

mod control;

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use raycat_config::{Config, Mode, Subscription};
use raycat_netfilter::Rules;
use raycat_proto::{Event, Mode as ApiMode, Status, UpdateResult, Updates, XrayState, XrayStatus};
use raycat_select::Selector;
use raycat_subscription::{Usage, redact_in};
use raycat_xray::Node;
use raycat_xray_api::XrayApi;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::Instant;

use self::control::FIRST_SELECT_DELAY;
use crate::api::{self, Refusal, Shared};
use crate::gateway;
use crate::log::{self, error, info, warn};
use crate::paths;
use crate::plan;
use crate::schedule::{HEALTHY_UPTIME, RESTART_FIRST, next_restart_delay};
use crate::selection;
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

/// Блокирует поток до сигнала остановки. `socket` — путь сокета API.
pub(crate) fn run(config: Config, store: Store, socket: PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("не удалось запустить рантайм tokio")?;
    let result = runtime.block_on(serve(config, store, socket));
    // Запрос к панели в spawn_blocking нельзя прервать: не ждём его дольше секунды.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

/// Удаляет файл сокета при любом выходе из `serve`.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Состояние для `GET /v1/status` до первого обновления демоном.
fn initial_status(config: &Config) -> Status {
    let (mode, kill_switch) = match config.mode {
        Mode::Proxy { .. } => (ApiMode::Proxy, None),
        Mode::Gateway { kill_switch, .. } => (ApiMode::Gateway, Some(kill_switch)),
    };
    Status {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mode,
        uptime_secs: 0,
        kill_switch,
        xray: XrayStatus {
            running: false,
            pid: None,
            restarts: 0,
        },
        node: None,
        subscriptions: Vec::new(),
    }
}

async fn serve(config: Config, store: Store, socket: PathBuf) -> Result<()> {
    match config.mode {
        Mode::Proxy { listen } if !listen.ip().is_loopback() => warn!(
            "прокси слушает {listen} без пароля: им сможет пользоваться любой, кто до него дотянется"
        ),
        Mode::Proxy { .. } => {}
        Mode::Gateway { .. } => gateway::preflight()?,
    }
    let machine_id = store.machine_id()?;
    let (shared, mut commands) = Shared::new(initial_status(&config));
    let mut daemon = Daemon::new(
        config,
        store,
        &machine_id,
        free_port()?,
        Arc::clone(&shared),
    )?;
    let listener = api::bind(&socket)?;
    let _socket_file = SocketFile(socket.clone());
    let server = tokio::spawn(api::serve(
        listener,
        api::router(Arc::clone(&shared)),
        paths::euid(),
    ));
    log::set_tap({
        let shared = Arc::clone(&shared);
        move |level, text| {
            shared.emit(Event::Warning {
                level: level.label().to_ascii_lowercase(),
                message: text.to_owned(),
            });
        }
    });
    // Правила ставятся до запуска xray, а при сбое подписки они остаются: это kill switch.
    if let Some(rules) = &daemon.gateway {
        gateway::install(rules)?;
    }
    info!(
        "raycat {} запущен: {}, подписок: {}, состояние: {}, API: {}",
        env!("CARGO_PKG_VERSION"),
        plan::describe_mode(&daemon.config),
        daemon.subs.len(),
        daemon.store.root().display(),
        socket.display()
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
            Some(command) = commands.recv() => daemon.command(command),
            exit = daemon.process.exited() => daemon.crashed(&exit),
            () = wait_until(wake) => {}
        }
    }
    server.abort();
    daemon.process.stop().await;
    // Только при штатной остановке: после аварии правила остаются и держат kill switch.
    if let Some(rules) = &daemon.gateway {
        gateway::remove(rules);
    }
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
    /// Сведения провайдера для `GET /v1/status`.
    title: Option<String>,
    usage: Option<Usage>,
    updated_at: Option<u64>,
    next_update: Option<u64>,
    last_error: Option<String>,
}

/// Запрос `POST /v1/update`, ждущий результата обновления подписок.
struct PendingUpdate {
    waiting: Vec<usize>,
    results: Vec<UpdateResult>,
    reply: oneshot::Sender<Result<Updates, Refusal>>,
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
    /// Правила перехвата в режиме шлюза и время следующей проверки, что они на месте.
    gateway: Option<Rules>,
    next_guard: Instant,
    shared: Arc<Shared>,
    selector: Selector,
    /// Тег, закреплённый сейчас в балансировщике xray; `None` после его запуска.
    pinned_in_xray: Option<String>,
    xray_api: Option<XrayApi>,
    /// xray запущен с тем же конфигом, который собран сейчас: теги узлов совпадают.
    xray_current: bool,
    xray_started: Option<Instant>,
    xray_starts: u32,
    next_select: Instant,
    last_reason: Option<String>,
    pending_updates: Vec<PendingUpdate>,
}

impl Daemon {
    fn new(
        config: Config,
        store: Store,
        machine_id: &str,
        api_port: u16,
        shared: Arc<Shared>,
    ) -> Result<Self> {
        let now = Instant::now();
        let gateway = plan::gateway_rules(&config)?;
        let pin = match store.load_pin() {
            Some(stored) => stored.as_deref().and_then(selection::parse_pin),
            None => selection::config_pin(&config),
        };
        let selector = Selector::new(selection::settings(&config, pin), Vec::new());
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
                    title: None,
                    usage: None,
                    updated_at: None,
                    next_update: None,
                    last_error: None,
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
            gateway,
            next_guard: now + gateway::GUARD_INTERVAL,
            shared,
            selector,
            pinned_in_xray: None,
            xray_api: None,
            xray_current: false,
            xray_started: None,
            xray_starts: 0,
            next_select: now,
            last_reason: None,
            pending_updates: Vec::new(),
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
                sub.title = cached.info.title;
                sub.usage = cached.info.usage;
                sub.updated_at = Some(cached.fetched_at);
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
        self.selector
            .set_candidates(selection::candidates(&self.config, &plan.tags));
        // Теги узлов в запущенном xray теперь не те: выбор ждёт его перезапуска.
        self.xray_current = false;
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
        if let Some(rules) = &self.gateway
            && self.next_guard <= now
        {
            gateway::guard(rules);
            self.next_guard = now + gateway::GUARD_INTERVAL;
        }
        self.restart_process().await;
        if self.selecting() && self.next_select <= now {
            self.next_select = now + self.config.selection.check_interval;
            self.select_step().await;
        }
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
        self.publish();
    }

    /// Выбор узла идёт, пока работает xray, запущенный с текущим конфигом.
    fn selecting(&self) -> bool {
        self.xray_current && self.process.pid().is_some()
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
        self.drop_api();
        if let Err(error) = self.process.start() {
            error!("{error:#}");
            self.schedule_restart();
            return;
        }
        self.xray_current = true;
        self.xray_started = Some(Instant::now());
        self.xray_starts = self.xray_starts.saturating_add(1);
        self.next_select = Instant::now() + FIRST_SELECT_DELAY;
        self.shared.emit(Event::Xray {
            state: XrayState::Started,
            message: format!("pid {}", self.process.pid().unwrap_or_default()),
        });
    }

    fn schedule_restart(&mut self) {
        self.restart = Restart::At(Instant::now() + self.backoff);
        self.backoff = next_restart_delay(self.backoff);
    }

    fn crashed(&mut self, exit: &Exit) {
        if exit.uptime >= HEALTHY_UPTIME {
            self.backoff = RESTART_FIRST;
        }
        self.drop_api();
        self.shared.emit(Event::Xray {
            state: XrayState::Exited,
            message: exit.status.clone(),
        });
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
        let guard = self.gateway.as_ref().map(|_| self.next_guard);
        let select = self.selecting().then_some(self.next_select);
        self.subs
            .iter()
            .filter(|sub| !sub.in_flight)
            .filter_map(|sub| sub.due)
            .chain(restart)
            .chain(self.gather_until)
            .chain(guard)
            .chain(select)
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
        sub.next_update = Some(now_unix().saturating_add(refresh.next_in.as_secs()));
        let result = match &refresh.outcome {
            Outcome::Applied(analysis) => UpdateResult {
                subscription: name.clone(),
                ok: true,
                message: format!("узлов: {}", analysis.nodes.len()),
                nodes: Some(analysis.nodes.len()),
            },
            Outcome::Rejected(message) | Outcome::Failed(message) => UpdateResult {
                subscription: name.clone(),
                ok: false,
                message: message.clone(),
                nodes: None,
            },
        };
        self.shared.emit(if result.ok {
            Event::SubscriptionUpdated {
                subscription: name.clone(),
                nodes: result.nodes.unwrap_or_default(),
            }
        } else {
            Event::SubscriptionFailed {
                subscription: name.clone(),
                error: result.message.clone(),
            }
        });
        let Some(sub) = self.subs.get_mut(index) else {
            return;
        };
        match refresh.outcome {
            Outcome::Applied(analysis) => {
                let analysis = *analysis;
                sub.failures = 0;
                sub.last_error = None;
                sub.updated_at = Some(now_unix());
                sub.title.clone_from(&analysis.info.title);
                sub.usage.clone_from(&analysis.info.usage);
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
                sub.last_error = Some(reason.clone());
                warn!(
                    "подписка «{name}»: ответ не применён ({reason}), остаются прежние узлы; повтор через {}",
                    format_duration(refresh.next_in)
                );
            }
            Outcome::Failed(message) => {
                sub.failures = sub.failures.saturating_add(1);
                sub.last_error = Some(message.clone());
                warn!(
                    "подписка «{name}»: не удалось обновить ({message}); повтор через {}",
                    format_duration(refresh.next_in)
                );
            }
        }
        self.complete_updates(index, result);
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
        daemon_with(names, "", temp)
    }

    /// `extra` дописывается в конец настроек (например, раздел `[selection]`).
    fn daemon_with(names: &[&str], extra: &str, temp: &TempDir) -> Daemon {
        let mut text = String::new();
        for name in names {
            let _ = write!(
                text,
                "[[subscription]]\nname = \"{name}\"\nurl = \"https://{name}.example.com/sub/token1234\"\napp = \"happ\"\nplatform = \"windows\"\npriority = [\"Two\"]\n"
            );
        }
        text.push_str(extra);
        let config = Config::from_toml_str(&text, &Env::new()).unwrap();
        let store = Store::open(temp.path().to_path_buf()).unwrap();
        let (shared, _commands) = Shared::new(initial_status(&config));
        Daemon::new(
            config,
            store,
            "0d0af05ee8fd4dc29275718f2ce4dff1",
            10_085,
            shared,
        )
        .unwrap()
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

    fn choice(daemon: &mut Daemon) -> Option<String> {
        daemon.selector.step(selection::now(), &[]).selected_id
    }

    #[test]
    fn a_pin_is_validated_saved_and_applied() {
        let temp = TempDir::new("pin-set");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(BOTH));
        // Без закрепления первым идёт узел с рангом по маске priority.
        assert_eq!(choice(&mut daemon).as_deref(), Some("a/Two"));

        assert!(matches!(
            daemon.set_pin(Some("a/Nope".to_owned())),
            Err(Refusal::NotFound(_))
        ));
        assert!(matches!(
            daemon.set_pin(Some("плохо".to_owned())),
            Err(Refusal::Invalid(_))
        ));
        assert_eq!(daemon.store.load_pin(), None);

        let pinned = daemon.set_pin(Some("a/One".to_owned())).unwrap();
        assert_eq!(pinned.node.as_deref(), Some("a/One"));
        assert_eq!(daemon.store.load_pin(), Some(Some("a/One".to_owned())));
        assert_eq!(choice(&mut daemon).as_deref(), Some("a/One"));
        let status = daemon.status();
        assert!(status.node.is_none() || status.node.is_some_and(|node| node.pinned));
    }

    #[test]
    fn a_saved_pin_survives_a_restart_and_beats_the_config() {
        let temp = TempDir::new("pin-saved");
        {
            let mut first = daemon(&["a"], &temp);
            first.load_caches();
            first.finished(0, applied(BOTH));
            first.set_pin(Some("a/One".to_owned())).unwrap();
        }
        let mut second = daemon_with(&["a"], "[selection]\npin = \"a/Two\"\n", &temp);
        second.load_caches();
        second.finished(0, applied(BOTH));
        assert_eq!(choice(&mut second).as_deref(), Some("a/One"));
    }

    #[test]
    fn unpinning_overrides_the_pin_from_the_config() {
        let temp = TempDir::new("pin-config");
        let config = "[selection]\npin = \"a/One\"\n";
        let mut first = daemon_with(&["a"], config, &temp);
        first.load_caches();
        first.finished(0, applied(BOTH));
        assert_eq!(choice(&mut first).as_deref(), Some("a/One"));

        assert_eq!(first.set_pin(None).unwrap().node, None);
        assert_eq!(first.store.load_pin(), Some(None));
        // Движок не прыгает без данных о здоровье: узел меняется, когда проверки покажут выигрыш.
        assert_eq!(choice(&mut first).as_deref(), Some("a/One"));

        let mut second = daemon_with(&["a"], config, &temp);
        second.load_caches();
        second.finished(0, applied(BOTH));
        assert_eq!(choice(&mut second).as_deref(), Some("a/Two"));
    }

    #[test]
    fn the_config_pin_applies_without_a_saved_one() {
        let temp = TempDir::new("pin-from-config");
        let mut daemon = daemon_with(&["a"], "[selection]\npin = \"a/One\"\n", &temp);
        daemon.load_caches();
        daemon.finished(0, applied(BOTH));
        assert_eq!(choice(&mut daemon).as_deref(), Some("a/One"));
    }

    #[test]
    fn an_update_request_waits_for_every_subscription() {
        let temp = TempDir::new("update-all");
        let mut daemon = daemon(&["a", "b"], &temp);
        daemon.load_caches();
        let (reply, mut answer) = oneshot::channel();
        daemon.start_update(None, reply);
        assert!(answer.try_recv().is_err());

        daemon.finished(0, applied(ONE));
        assert!(answer.try_recv().is_err());
        daemon.finished(1, failed());
        let updates = answer.try_recv().unwrap().unwrap();
        assert_eq!(updates.results.len(), 2);
        assert!(updates.results[0].ok && updates.results[0].nodes == Some(1));
        assert_eq!(updates.results[0].subscription, "a");
        assert!(!updates.results[1].ok);
        assert_eq!(updates.results[1].message, "нет связи");
        assert!(daemon.pending_updates.is_empty());
    }

    #[test]
    fn an_update_request_can_name_one_subscription() {
        let temp = TempDir::new("update-one");
        let mut daemon = daemon(&["a", "b"], &temp);
        daemon.load_caches();
        let (reply, mut answer) = oneshot::channel();
        daemon.start_update(Some("b".to_owned()), reply);
        daemon.finished(1, applied(TWO));
        let updates = answer.try_recv().unwrap().unwrap();
        assert_eq!(updates.results.len(), 1);
        assert_eq!(updates.results[0].subscription, "b");
    }

    #[test]
    fn updating_an_unknown_subscription_is_refused() {
        let temp = TempDir::new("update-unknown");
        let mut daemon = daemon(&["a"], &temp);
        let (reply, mut answer) = oneshot::channel();
        daemon.start_update(Some("x".to_owned()), reply);
        let refusal = answer.try_recv().unwrap().unwrap_err();
        assert!(matches!(refusal, Refusal::NotFound(ref text) if text.contains("«x»")));
        assert!(daemon.pending_updates.is_empty());
    }

    #[test]
    fn status_shows_masked_links_errors_and_the_current_node() {
        let temp = TempDir::new("status");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(ONE));
        assert!(daemon.status().node.is_none());
        choice(&mut daemon);

        let status = daemon.status();
        assert_eq!(status.mode, ApiMode::Proxy);
        assert_eq!(status.kill_switch, None);
        assert!(!status.xray.running);
        let node = status.node.unwrap();
        assert_eq!((node.id.as_str(), node.pinned), ("a/One", false));
        assert!(node.reason.is_none());
        let sub = &status.subscriptions[0];
        assert_eq!(sub.url, "https://a.example.com/…1234");
        assert_eq!(sub.nodes, 1);
        assert!(sub.last_error.is_none() && sub.updated_at.is_some() && !sub.updating);
        assert!(sub.next_update.is_some());

        daemon.finished(0, failed());
        let sub = &daemon.status().subscriptions[0];
        assert_eq!(sub.last_error.as_deref(), Some("нет связи"));
        assert_eq!(sub.nodes, 1);
    }

    #[test]
    fn nodes_view_lists_the_candidates() {
        let temp = TempDir::new("nodes-view");
        let mut daemon = daemon(&["a"], &temp);
        daemon.load_caches();
        daemon.finished(0, applied(BOTH));
        choice(&mut daemon);
        let view = daemon.nodes_view();
        assert_eq!(view.selected.as_deref(), Some("a/Two"));
        let names: Vec<&str> = view.nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, ["One", "Two"]);
        assert!(view.nodes[1].selected);
        assert!(view.nodes.iter().all(|node| node.uplink_bytes.is_none()));
    }

    #[test]
    fn subscription_events_reach_the_stream() {
        let temp = TempDir::new("events");
        let mut daemon = daemon(&["a"], &temp);
        let mut events = daemon.shared.subscribe();
        daemon.load_caches();
        daemon.finished(0, applied(ONE));
        daemon.finished(0, failed());
        assert_eq!(
            events.try_recv().unwrap(),
            Event::SubscriptionUpdated {
                subscription: "a".to_owned(),
                nodes: 1
            }
        );
        assert_eq!(
            events.try_recv().unwrap(),
            Event::SubscriptionFailed {
                subscription: "a".to_owned(),
                error: "нет связи".to_owned()
            }
        );
    }
}
