//! Команды API, выбор узла и состояние для API: часть `Daemon`, которой нужны
//! поля родительского модуля.

use std::time::Duration;

use raycat_config::Mode;
use raycat_proto::{
    CurrentNode, Event, Mode as ApiMode, Node as ApiNode, NodeStatus, Nodes, Pinned, Status,
    SubscriptionStatus, UpdateResult, Updates, XrayStatus,
};
use raycat_select::{Decision, Status as SelectStatus};
use raycat_subscription::Usage;
use raycat_xray_api::XrayApi;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::{Daemon, PendingUpdate, Sub};
use crate::api::{Command, Refusal};
use crate::log::{debug, info, warn};
use crate::{plan, selection};

const API_TIMEOUT: Duration = Duration::from_secs(3);
/// Первые секунды после запуска API xray ещё может не отвечать: это не предупреждение.
const API_GRACE: Duration = Duration::from_secs(10);
/// Первый выбор узла после запуска xray: не ждём `check_interval`, чтобы трафик не шёл
/// через узел по умолчанию.
pub(super) const FIRST_SELECT_DELAY: Duration = Duration::from_secs(1);

impl Daemon {
    pub(super) fn command(&mut self, command: Command) {
        match command {
            Command::Pin { node, reply } => {
                let _ = reply.send(self.set_pin(node));
            }
            Command::Update {
                subscription,
                reply,
            } => self.start_update(subscription, reply),
        }
    }

    /// Закрепляет узел «подписка/имя» или снимает закрепление. Выбор сохраняется в
    /// каталоге состояния и переживает перезапуск.
    pub(super) fn set_pin(&mut self, node: Option<String>) -> Result<Pinned, Refusal> {
        let target = match &node {
            Some(text) => {
                let target = selection::parse_pin(text).ok_or_else(|| {
                    Refusal::Invalid("узел задаётся как «подписка/имя узла»".to_owned())
                })?;
                let known = self
                    .selector
                    .snapshot(selection::now())
                    .nodes
                    .iter()
                    .any(|node| {
                        node.subscription == target.subscription && node.name == target.node
                    });
                if !known {
                    return Err(Refusal::NotFound(format!(
                        "узла «{}» нет среди узлов подписок",
                        selection::pin_id(&target)
                    )));
                }
                Some(target)
            }
            None => None,
        };
        let id = target.as_ref().map(selection::pin_id);
        self.store.save_pin(id.as_deref()).map_err(|error| {
            Refusal::Unavailable(format!("не удалось сохранить закрепление: {error:#}"))
        })?;
        self.selector.set_pin(target);
        self.next_select = Instant::now();
        match &id {
            Some(id) => info!("узел закреплён вручную: «{id}»"),
            None => info!("закрепление снято, выбор узла автоматический"),
        }
        self.shared.emit(Event::Pin { node: id.clone() });
        Ok(Pinned { node: id })
    }

    pub(super) fn start_update(
        &mut self,
        subscription: Option<String>,
        reply: oneshot::Sender<Result<Updates, Refusal>>,
    ) {
        let indexes: Vec<usize> = match &subscription {
            Some(name) => {
                let Some(index) = self.subs.iter().position(|sub| sub.source.name() == name) else {
                    let _ = reply.send(Err(Refusal::NotFound(format!(
                        "подписки «{name}» нет в настройках"
                    ))));
                    return;
                };
                vec![index]
            }
            None => (0..self.subs.len()).collect(),
        };
        let now = Instant::now();
        for index in &indexes {
            if let Some(sub) = self.subs.get_mut(*index)
                && !sub.in_flight
            {
                sub.due = Some(now);
            }
        }
        info!("обновление подписок по запросу API: {}", indexes.len());
        self.pending_updates.push(PendingUpdate {
            waiting: indexes,
            results: Vec::new(),
            reply,
        });
    }

    /// Отдаёт результат обновления подписки ждущим запросам `POST /v1/update`.
    pub(super) fn complete_updates(&mut self, index: usize, result: UpdateResult) {
        let mut finished = Vec::new();
        for (position, pending) in self.pending_updates.iter_mut().enumerate() {
            if let Some(slot) = pending.waiting.iter().position(|waiting| *waiting == index) {
                pending.waiting.remove(slot);
                pending.results.push(result.clone());
                if pending.waiting.is_empty() {
                    finished.push(position);
                }
            }
        }
        for position in finished.into_iter().rev() {
            let pending = self.pending_updates.remove(position);
            let _ = pending.reply.send(Ok(Updates {
                results: pending.results,
            }));
        }
    }

    /// Один шаг выбора: здоровье узлов из observatory, решение движка, закрепление
    /// выбранного узла в балансировщике xray.
    pub(super) async fn select_step(&mut self) {
        let Some(api) = self.connect_api().await else {
            return;
        };
        let statuses = match api.outbound_status().await {
            Ok(statuses) => statuses,
            Err(error) => {
                self.api_problem(&error.to_string());
                self.drop_api();
                return;
            }
        };
        let health = selection::health(&statuses);
        let decision = self.selector.step(selection::now(), &health);
        self.apply_decision(&api, decision).await;
    }

    async fn connect_api(&mut self) -> Option<XrayApi> {
        if let Some(api) = &self.xray_api {
            return Some(api.clone());
        }
        match XrayApi::connect(self.api_port, API_TIMEOUT).await {
            Ok(api) => {
                self.shared.set_xray(Some(api.clone()));
                self.xray_api = Some(api.clone());
                Some(api)
            }
            Err(error) => {
                self.api_problem(&error.to_string());
                None
            }
        }
    }

    fn api_problem(&self, text: &str) {
        let starting = self
            .xray_started
            .is_some_and(|started| started.elapsed() < API_GRACE);
        if starting {
            debug!("{text}");
        } else {
            warn!("{text}");
        }
    }

    /// Забывает соединение с API xray: он перезапущен или не отвечает.
    pub(super) fn drop_api(&mut self) {
        self.xray_api = None;
        self.pinned_in_xray = None;
        self.shared.set_xray(None);
    }

    async fn apply_decision(&mut self, api: &XrayApi, decision: Decision) {
        for warning in &decision.warnings {
            warn!("выбор узла: {warning}");
        }
        if decision.changed || self.last_reason.is_none() {
            self.last_reason = Some(decision.reason.to_string());
        }
        if decision.changed {
            info!("узел: {}", decision.reason);
            self.shared.emit(Event::NodeChanged {
                from: decision.previous_id.clone(),
                to: decision.selected_id.clone(),
                reason: decision.reason.to_string(),
            });
        }
        match decision.selected {
            Some(tag) => {
                if self.pinned_in_xray.as_deref() != Some(tag.as_str()) {
                    match api.pin(plan::BALANCER, &tag).await {
                        Ok(()) => self.pinned_in_xray = Some(tag),
                        Err(error) => {
                            warn!("не удалось закрепить узел в xray: {error}");
                            self.drop_api();
                        }
                    }
                }
            }
            None => {
                if self.pinned_in_xray.is_some() && api.unpin(plan::BALANCER).await.is_ok() {
                    self.pinned_in_xray = None;
                }
            }
        }
    }

    /// Обновляет состояние, которое видит API.
    pub(super) fn publish(&self) {
        self.shared.publish_status(self.status());
        self.shared.publish_nodes(self.nodes_view());
    }

    pub(super) fn status(&self) -> Status {
        let snapshot = self.selector.snapshot(selection::now());
        let node = snapshot
            .selected_id
            .as_deref()
            .and_then(|id| snapshot.nodes.iter().find(|node| node.id == id))
            .map(|node| CurrentNode {
                id: node.id.clone(),
                subscription: node.subscription.clone(),
                name: node.name.clone(),
                latency_ms: node.latency_ms,
                pinned: node.pinned,
                reason: self.last_reason.clone(),
            });
        let (mode, kill_switch) = match self.config.mode {
            Mode::Proxy { .. } => (ApiMode::Proxy, None),
            Mode::Gateway { kill_switch, .. } => (ApiMode::Gateway, Some(kill_switch)),
        };
        let pid = self.process.pid();
        Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            mode,
            uptime_secs: 0,
            kill_switch,
            xray: XrayStatus {
                running: pid.is_some(),
                pid,
                restarts: self.xray_starts.saturating_sub(1),
            },
            node,
            subscriptions: self.subs.iter().map(subscription_status).collect(),
        }
    }

    pub(super) fn nodes_view(&self) -> Nodes {
        let snapshot = self.selector.snapshot(selection::now());
        let nodes = snapshot
            .nodes
            .into_iter()
            .map(|node| ApiNode {
                id: node.id,
                subscription: node.subscription,
                name: node.name,
                tag: node.tag,
                status: match node.status {
                    SelectStatus::Alive => NodeStatus::Alive,
                    SelectStatus::Dead => NodeStatus::Dead,
                    SelectStatus::Unknown => NodeStatus::Unknown,
                },
                latency_ms: node.latency_ms,
                failures: node.failures,
                alive_for_secs: node.alive_for_secs,
                last_error: node.last_error,
                selected: node.selected,
                pinned: node.pinned,
                uplink_bytes: None,
                downlink_bytes: None,
            })
            .collect();
        Nodes {
            selected: snapshot.selected_id,
            nodes,
        }
    }
}

fn subscription_status(sub: &Sub) -> SubscriptionStatus {
    SubscriptionStatus {
        name: sub.source.name().to_owned(),
        url: sub.source.config().masked_url(),
        title: sub.title.clone(),
        used_bytes: sub.usage.as_ref().map(Usage::used),
        total_bytes: sub.usage.as_ref().map(|usage| usage.total),
        expire: sub.usage.as_ref().map(|usage| usage.expire),
        nodes: sub.nodes.len(),
        updated_at: sub.updated_at,
        next_update: sub.next_update,
        last_error: sub.last_error.clone(),
        updating: sub.in_flight,
    }
}
