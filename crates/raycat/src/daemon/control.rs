//! Команды API, выбор узла и состояние для API: часть `Daemon`, которой нужны
//! поля родительского модуля.

use std::collections::HashMap;
use std::time::Duration;

use raycat_config::Mode;
use raycat_proto::{
    CurrentNode, Event, Mode as ApiMode, Node as ApiNode, NodeStatus, Nodes, Pinned, Status,
    SubscriptionStatus, Tunnel, UpdateResult, Updates, XrayStatus,
};
use raycat_select::{Decision, Health, Reason, Status as SelectStatus};
use raycat_subscription::Usage;
use raycat_xray_api::{ApiError, XrayApi};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::details;
use super::{Daemon, PendingUpdate, Restart, Sub};
use crate::api::{Command, Refusal};
use crate::log::{self, Level, info, warn};
use crate::util::{format_date, local_zone, now_unix};
use crate::{plan, schedule, selection};

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
                let _ = reply.send(self.set_pin(node.as_deref()));
            }
            Command::Update {
                subscription,
                reply,
            } => self.start_update(subscription.as_deref(), reply),
            Command::Speedtest { request, reply } => self.start_speedtest(request, reply),
            Command::SpeedtestRestore { pin, ack } => self.finish_speedtest(pin, ack),
            Command::Reload { reply } => {
                let _ = reply.send(self.reload());
            }
            Command::Patch { patch, reply } => {
                let _ = reply.send(self.patch(patch));
            }
            Command::Settings { reply } => {
                let _ = reply.send(Ok(self.settings()));
            }
        }
    }

    /// Закрепляет узел «подписка/имя» или снимает закрепление. Выбор сохраняется в
    /// каталоге состояния и переживает перезапуск.
    pub(super) fn set_pin(&mut self, node: Option<&str>) -> Result<Pinned, Refusal> {
        if self.speedtest_active {
            return Err(Refusal::Unavailable(
                "идёт тест скорости: закрепление можно изменить после него".to_owned(),
            ));
        }
        let target = match node {
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
        subscription: Option<&str>,
        reply: oneshot::Sender<Result<Updates, Refusal>>,
    ) {
        let names: Vec<String> = match subscription {
            Some(name) => {
                if !self.subs.iter().any(|sub| sub.source.name() == name) {
                    let _ = reply.send(Err(Refusal::NotFound(format!(
                        "подписки «{name}» нет в настройках"
                    ))));
                    return;
                }
                vec![name.to_owned()]
            }
            None => self
                .subs
                .iter()
                .map(|sub| sub.source.name().to_owned())
                .collect(),
        };
        let now = Instant::now();
        for sub in self
            .subs
            .iter_mut()
            .filter(|sub| names.iter().any(|name| name.as_str() == sub.source.name()))
        {
            if !sub.in_flight {
                sub.due = Some(now);
            }
        }
        info!("обновление подписок по запросу API: {}", names.len());
        self.pending_updates.push(PendingUpdate {
            waiting: names,
            results: Vec::new(),
            reply,
        });
    }

    /// Отдаёт результат обновления подписки ждущим запросам `POST /v1/update`. Сопоставление
    /// по имени: подписки могли переставить, пока шло обновление.
    pub(super) fn complete_updates(&mut self, name: &str, result: &UpdateResult) {
        let mut finished = Vec::new();
        for (position, pending) in self.pending_updates.iter_mut().enumerate() {
            if let Some(slot) = pending
                .waiting
                .iter()
                .position(|waiting| waiting.as_str() == name)
            {
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
                // xray ответил ошибкой — значит, жив; молчанием считаются тайм-аут и обрыв связи.
                if matches!(error, ApiError::Request { .. }) {
                    self.last_api_ok = Some(Instant::now());
                }
                self.report_api_problem(&error.to_string());
                self.drop_api();
                return;
            }
        };
        self.api_problem.clear();
        self.last_api_ok = Some(Instant::now());
        let health = selection::health(&statuses);
        self.record_checks(&health);
        let decision = self.selector.step(selection::now(), &health);
        self.apply_decision(&api, decision).await;
    }

    /// Записывает новые проверки в историю узлов. Узел ищется по тегу xray, а история
    /// хранится по имени узла: при пересборке конфига теги сдвигаются, история остаётся.
    pub(super) fn record_checks(&mut self, health: &[Health]) {
        let ids: HashMap<String, String> = self
            .selector
            .snapshot(selection::now())
            .nodes
            .into_iter()
            .map(|node| (node.tag, node.id))
            .collect();
        for check in health {
            if let Some(id) = ids.get(&check.tag) {
                self.checks
                    .record(id, check.checked_at, check.alive, check.latency_ms);
            }
        }
    }

    /// Подробности всех узлов по идентификатору «подписка/имя». У одинаковых имён в одной
    /// подписке идентификатор получает номер, как в выборе узла.
    fn node_details(&self) -> HashMap<String, details::Details> {
        let mut infos = HashMap::new();
        for sub in &self.subs {
            let mut seen: HashMap<String, u32> = HashMap::new();
            for node in &sub.nodes {
                let base = selection::node_id(sub.source.name(), &node.name);
                let count = seen.entry(base.clone()).or_insert(0);
                *count += 1;
                let id = if *count == 1 {
                    base
                } else {
                    format!("{base} ({count})")
                };
                infos.insert(id, details::details(node));
            }
        }
        infos
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
                self.report_api_problem(&error.to_string());
                None
            }
        }
    }

    fn report_api_problem(&mut self, text: &str) {
        let starting = self
            .xray_started
            .is_some_and(|started| started.elapsed() < API_GRACE);
        let level = if starting {
            Level::Debug
        } else {
            self.api_problem.level(text)
        };
        log::write(level, format_args!("{text}"));
    }

    /// Забывает соединение с API xray: он перезапущен или не отвечает.
    pub(super) fn drop_api(&mut self) {
        self.xray_api = None;
        self.pinned_in_xray = None;
        self.shared.set_xray(None);
        self.shared.set_xray_pin(None);
    }

    /// Сколько API xray молчит; `None`, пока xray не запущен.
    fn api_silence(&self) -> Option<Duration> {
        let started = self.xray_started.filter(|_| self.process.pid().is_some())?;
        let silence = schedule::api_silence(Instant::now(), started, self.last_api_ok);
        Some(silence)
    }

    /// Зависший xray: API молчит дольше порога. Процесс не падает, поэтому его перезапускает
    /// общий путь перезапуска; правила шлюза при этом остаются на месте.
    pub(super) fn check_api_hang(&mut self) {
        if !self.selecting() || self.restart != Restart::Idle {
            return;
        }
        let Some(silence) = self.api_silence() else {
            return;
        };
        if silence <= schedule::api_limit(self.config.selection.check_interval) {
            return;
        }
        warn!("xray не отвечает {} с — перезапускаю", silence.as_secs());
        self.xray_crashes = self.xray_crashes.saturating_add(1);
        self.restart = Restart::Now;
    }

    async fn apply_decision(&mut self, api: &XrayApi, decision: Decision) {
        let warnings = decision
            .warnings
            .iter()
            .map(|warning| format!("выбор узла: {warning}"))
            .collect::<Vec<_>>()
            .join("; ");
        if warnings.is_empty() {
            self.selection_problem.clear();
        } else {
            let level = self.selection_problem.level(&warnings);
            log::write(level, format_args!("{warnings}"));
        }
        if decision.changed || self.last_reason.is_none() {
            self.last_reason = Some(decision.reason.to_string());
        }
        if decision.changed {
            info!("узел: {}", decision.reason);
            // Имя закреплённого узла уже стоит в событии как «до → после».
            let reason = match &decision.reason {
                Reason::Pinned { .. } => "закреплён вручную".to_owned(),
                other => other.to_string(),
            };
            self.shared.emit(Event::NodeChanged {
                from: decision.previous_id.clone(),
                to: decision.selected_id.clone(),
                reason,
            });
        }
        match decision.selected {
            Some(tag) => {
                if self.pinned_in_xray.as_deref() != Some(tag.as_str()) {
                    match api.pin(plan::BALANCER, &tag).await {
                        Ok(()) => {
                            self.shared.set_xray_pin(Some(tag.clone()));
                            self.pinned_in_xray = Some(tag);
                            self.pin_problem.clear();
                        }
                        Err(error) => {
                            let text = format!("не удалось закрепить узел в xray: {error}");
                            log::write(self.pin_problem.level(&text), format_args!("{text}"));
                            self.drop_api();
                        }
                    }
                }
            }
            None => {
                if self.pinned_in_xray.is_some() && api.unpin(plan::BALANCER).await.is_ok() {
                    self.shared.set_xray_pin(None);
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
        let selected = snapshot
            .selected_id
            .as_deref()
            .and_then(|id| snapshot.nodes.iter().find(|node| node.id == id));
        let node = selected.map(|node| CurrentNode {
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
        let silence = self.api_silence();
        let limit = schedule::api_limit(self.config.selection.check_interval);
        let stuck = silence.is_some_and(|silence| silence > limit);
        let statuses: Vec<SelectStatus> = snapshot.nodes.iter().map(|node| node.status).collect();
        let tunnel = tunnel_state(
            pid.is_some(),
            stuck,
            selected.map(|node| node.status),
            &statuses,
        );
        let now = now_unix();
        Status {
            version: crate::VERSION.to_owned(),
            mode,
            uptime_secs: 0,
            kill_switch,
            xray: XrayStatus {
                running: pid.is_some(),
                pid,
                restarts: self.xray_crashes,
                api_silent_secs: silence.map(|silence| silence.as_secs()),
                api_stuck: stuck,
            },
            tunnel,
            node,
            subscriptions: self
                .subs
                .iter()
                .map(|sub| subscription_status(sub, now))
                .collect(),
        }
    }

    pub(super) fn nodes_view(&self) -> Nodes {
        let snapshot = self.selector.snapshot(selection::now());
        let infos = self.node_details();
        let mut nodes: Vec<ApiNode> = snapshot
            .nodes
            .into_iter()
            .map(|node| {
                let info = infos.get(&node.id);
                let secrets: &[String] = info.map_or(&[], |info| info.secrets.as_slice());
                // Адрес сервера из текста ошибки убирается до того, как ответ уйдёт наружу.
                let last_error = node
                    .last_error
                    .as_deref()
                    .map(|text| details::mask(text, secrets));
                let checks = self.checks.of(&node.id);
                ApiNode {
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
                    last_error,
                    selected: node.selected,
                    pinned: node.pinned,
                    uplink_bytes: None,
                    downlink_bytes: None,
                    protocol: info.and_then(|info| info.protocol.clone()),
                    transport: info.and_then(|info| info.transport.clone()),
                    security: info.and_then(|info| info.security.clone()),
                    checks,
                    excluded: false,
                }
            })
            .collect();
        nodes.extend(self.excluded_nodes());
        Nodes {
            selected: snapshot.selected_id,
            nodes,
        }
    }

    /// Узлы, которые отсекла маска `deny`. В выборе они не участвуют и xray их не получает;
    /// здесь только имя и подписка, чтобы их можно было показать и снять маску. Собираются
    /// из уже загруженных узлов подписок, отдельно не хранятся.
    fn excluded_nodes(&self) -> Vec<ApiNode> {
        self.subs
            .iter()
            .flat_map(|sub| {
                let config = sub.source.config();
                let subscription = config.name.clone();
                sub.nodes
                    .iter()
                    .filter(move |node| config.denies(&node.name))
                    .map(move |node| ApiNode {
                        id: format!("{subscription}/{}", node.name),
                        subscription: subscription.clone(),
                        name: node.name.clone(),
                        tag: String::new(),
                        status: NodeStatus::Unknown,
                        latency_ms: None,
                        failures: 0,
                        alive_for_secs: None,
                        last_error: None,
                        selected: false,
                        pinned: false,
                        uplink_bytes: None,
                        downlink_bytes: None,
                        protocol: None,
                        transport: None,
                        security: None,
                        checks: Vec::new(),
                        excluded: true,
                    })
            })
            .collect()
    }
}

/// Состояние туннеля по тому, что видит демон: процесс xray, его API и проверки узлов.
/// `selected` — состояние выбранного узла; `None`, если узел не выбран.
fn tunnel_state(
    running: bool,
    stuck: bool,
    selected: Option<SelectStatus>,
    statuses: &[SelectStatus],
) -> Tunnel {
    if !running {
        return Tunnel::XrayDown;
    }
    if stuck {
        return Tunnel::XrayStuck;
    }
    let alive = statuses.contains(&SelectStatus::Alive);
    let checked = statuses
        .iter()
        .any(|status| *status != SelectStatus::Unknown);
    match selected {
        Some(SelectStatus::Alive) => Tunnel::Working,
        Some(SelectStatus::Unknown) => Tunnel::Checking,
        // Выбор сменится на живой при следующей проверке.
        Some(SelectStatus::Dead) if alive => Tunnel::Checking,
        None if !statuses.is_empty() && !checked => Tunnel::Checking,
        Some(SelectStatus::Dead) | None => Tunnel::NoLiveNodes,
    }
}

/// Почему подписка не отдала узлы: последний ответ провайдера — заглушка. Без заглушки
/// причины нет (ошибку связи или разбора показывает `last_error`).
fn sub_note(sub: &Sub, now: u64) -> Option<String> {
    let device_limit = sub.stub?;
    let expired = sub
        .usage
        .as_ref()
        .map(|usage| usage.expire)
        .filter(|&expire| expire != 0 && expire <= now);
    Some(match expired {
        Some(expire) => format!("истекла {}", format_date(expire, local_zone())),
        None if device_limit => "лимит устройств".to_owned(),
        None => "провайдер не отдал узлы".to_owned(),
    })
}

fn subscription_status(sub: &Sub, now: u64) -> SubscriptionStatus {
    let config = sub.source.config();
    SubscriptionStatus {
        name: sub.source.name().to_owned(),
        url: config.masked_url(),
        title: sub.title.clone(),
        used_bytes: sub.usage.as_ref().map(Usage::used),
        total_bytes: sub.usage.as_ref().map(|usage| usage.total),
        expire: sub.usage.as_ref().map(|usage| usage.expire),
        nodes: sub.nodes.len(),
        updated_at: sub.updated_at,
        next_update: sub.next_update,
        last_error: sub.last_error.clone(),
        updating: sub.in_flight,
        app: config.app.as_str().to_owned(),
        platform: config.platform.as_str().to_owned(),
        update_interval_secs: config.update_interval.map(|interval| interval.as_secs()),
        cut: sub
            .nodes
            .iter()
            .filter(|node| !config.allows(&node.name))
            .count(),
        note: sub_note(sub, now),
        announce: sub.announce.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tunnel_state_follows_xray_and_the_selected_node() {
        use SelectStatus::{Alive, Dead, Unknown};
        let cases = [
            (
                tunnel_state(false, false, Some(Alive), &[Alive]),
                Tunnel::XrayDown,
            ),
            (
                tunnel_state(true, true, Some(Alive), &[Alive]),
                Tunnel::XrayStuck,
            ),
            (
                tunnel_state(true, false, Some(Alive), &[Alive, Dead]),
                Tunnel::Working,
            ),
            (
                tunnel_state(true, false, Some(Dead), &[Dead, Dead]),
                Tunnel::NoLiveNodes,
            ),
            (
                tunnel_state(true, false, Some(Dead), &[Dead, Alive]),
                Tunnel::Checking,
            ),
            (
                tunnel_state(true, false, Some(Unknown), &[Unknown]),
                Tunnel::Checking,
            ),
            (tunnel_state(true, false, None, &[]), Tunnel::NoLiveNodes),
            (
                tunnel_state(true, false, None, &[Unknown, Unknown]),
                Tunnel::Checking,
            ),
            (
                tunnel_state(true, false, None, &[Dead]),
                Tunnel::NoLiveNodes,
            ),
        ];
        for (actual, expected) in cases {
            assert_eq!(actual, expected);
        }
    }
}
