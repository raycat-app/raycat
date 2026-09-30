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
        })
    }

    /// Поднимает xray из кэша, не дожидаясь панелей.
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
        }
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
        self.applied = Some(plan.json);
        self.applied_nodes = plan.nodes;
        self.restart = Restart::Now;
    }

    /// Запускает то, что подошло по времени: перезапуск xray и обновления подписок.
    async fn tick(&mut self, tx: &UnboundedSender<Finished>) {
        self.restart_process().await;
        let now = Instant::now();
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
                self.reconcile();
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
    }
}
