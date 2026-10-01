//! Типы API демона: HTTP+JSON по unix-сокету. Их используют демон, CLI и TUI.
//!
//! Время везде в секундах Unix, трафик в байтах. Ссылки подписок в ответах только
//! маскированные (`https://хост/…abcd`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Proxy,
    Gateway,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub mode: Mode,
    pub uptime_secs: u64,
    /// Только в режиме шлюза.
    pub kill_switch: Option<bool>,
    pub xray: XrayStatus,
    /// Текущий узел; `None`, пока узлов нет.
    pub node: Option<CurrentNode>,
    pub subscriptions: Vec<SubscriptionStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XrayStatus {
    pub running: bool,
    pub pid: Option<u32>,
    /// Сколько раз xray запускали заново после первого запуска.
    pub restarts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentNode {
    /// «подписка/имя узла».
    pub id: String,
    pub subscription: String,
    pub name: String,
    pub latency_ms: Option<u64>,
    pub pinned: bool,
    /// Почему выбран именно этот узел (последнее решение).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionStatus {
    pub name: String,
    /// Маскированная ссылка.
    pub url: String,
    /// Название от провайдера.
    pub title: Option<String>,
    pub used_bytes: Option<u64>,
    /// 0 — без ограничения.
    pub total_bytes: Option<u64>,
    /// Unix-время окончания; 0 — бессрочно.
    pub expire: Option<u64>,
    pub nodes: usize,
    pub updated_at: Option<u64>,
    pub next_update: Option<u64>,
    pub last_error: Option<String>,
    /// Обновление идёт прямо сейчас.
    pub updating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Alive,
    Dead,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub subscription: String,
    pub name: String,
    /// Тег выхода в конфиге xray.
    pub tag: String,
    pub status: NodeStatus,
    pub latency_ms: Option<u64>,
    /// Неудачных проверок подряд.
    pub failures: u32,
    pub alive_for_secs: Option<u64>,
    pub last_error: Option<String>,
    pub selected: bool,
    pub pinned: bool,
    /// Трафик с запуска xray; `None`, если счётчики недоступны.
    pub uplink_bytes: Option<u64>,
    pub downlink_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nodes {
    /// `id` выбранного узла.
    pub selected: Option<String>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinRequest {
    /// «подписка/имя узла».
    pub node: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// Имя подписки; без него обновляются все.
    pub subscription: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateResult {
    pub subscription: String,
    pub ok: bool,
    /// Что получилось: число узлов или причина отказа.
    pub message: String,
    pub nodes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Updates {
    pub results: Vec<UpdateResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pinned {
    /// `None` — закрепления нет, выбор автоматический.
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XrayState {
    Started,
    Exited,
}

/// Событие потока `GET /v1/events` (SSE): имя события — [`Event::name`], данные — JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Hello { version: String },
    NodeChanged {
        from: Option<String>,
        to: Option<String>,
        reason: String,
    },
    Pin { node: Option<String> },
    SubscriptionUpdated { subscription: String, nodes: usize },
    SubscriptionFailed { subscription: String, error: String },
    Xray { state: XrayState, message: String },
    Warning { level: String, message: String },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::NodeChanged { .. } => "node_changed",
            Self::Pin { .. } => "pin",
            Self::SubscriptionUpdated { .. } => "subscription_updated",
            Self::SubscriptionFailed { .. } => "subscription_failed",
            Self::Xray { .. } => "xray",
            Self::Warning { .. } => "warning",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_and_named() {
        let event = Event::NodeChanged {
            from: Some("a/NL-1".into()),
            to: Some("a/DE-2".into()),
            reason: "быстрее".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "node_changed");
        assert_eq!(json["to"], "a/DE-2");
        assert_eq!(event.name(), "node_changed");
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
    }

    #[test]
    fn every_event_round_trips() {
        let events = [
            Event::Hello { version: "1".into() },
            Event::Pin { node: None },
            Event::SubscriptionUpdated { subscription: "a".into(), nodes: 3 },
            Event::SubscriptionFailed { subscription: "a".into(), error: "x".into() },
            Event::Xray { state: XrayState::Exited, message: "упал".into() },
            Event::Warning { level: "warn".into(), message: "m".into() },
        ];
        for event in events {
            let text = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&text).unwrap(), event);
            assert!(text.contains(&format!("\"type\":\"{}\"", event.name())));
        }
    }

    #[test]
    fn status_uses_snake_case_enums() {
        let status = Status {
            version: "0.1.0".into(),
            mode: Mode::Gateway,
            uptime_secs: 5,
            kill_switch: Some(true),
            xray: XrayStatus { running: true, pid: Some(7), restarts: 0 },
            node: None,
            subscriptions: Vec::new(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["mode"], "gateway");
        assert_eq!(json["xray"]["pid"], 7);
        assert_eq!(serde_json::from_value::<Status>(json).unwrap(), status);
    }

    #[test]
    fn update_request_may_be_empty() {
        let request: UpdateRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request, UpdateRequest::default());
        let named: UpdateRequest = serde_json::from_str(r#"{"subscription":"a"}"#).unwrap();
        assert_eq!(named.subscription.as_deref(), Some("a"));
    }
}
