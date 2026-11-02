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
    /// Сколько секунд API xray молчит: с последнего ответа, а до первого ответа — со старта
    /// xray; `None`, если xray не запущен.
    #[serde(default)]
    pub api_silent_secs: Option<u64>,
    /// API молчит дольше порога демона: xray считается зависшим, демон его перезапустит.
    #[serde(default)]
    pub api_stuck: bool,
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

/// Запрос `POST /v1/speedtest`. Замеры идут через служебный вход xray на петле; на время
/// теста демон закрепляет узел и потом возвращает прежнее закрепление.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedtestRequest {
    /// `id` узла из `GET /v1/nodes`; без него тест идёт через текущий выбранный узел.
    pub node: Option<String>,
    /// Объём одного замера в байтах; делится между потоками.
    pub size: u64,
    /// Один замер с этим числом потоков; без него два замера: 1 поток и 4 потока.
    pub streams: Option<u8>,
    /// Адрес https для скачивания; без него тестовый сервер Cloudflare.
    pub url: Option<String>,
}

/// Один замер: все его потоки вместе.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedtestRun {
    pub streams: u8,
    /// Полезная нагрузка всех потоков.
    pub bytes: u64,
    /// От первого байта до последнего по всем потокам.
    pub seconds: f64,
    /// Мегабиты в секунду (10^6 бит/с).
    pub mbps: f64,
    /// Среднее по потокам время до первого байта тела, мс.
    pub ttfb_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedtestResult {
    /// `id` узла, через который шёл тест.
    pub node: String,
    /// Адрес без строки запроса (см. `raycat_http::redact`).
    pub url: String,
    pub runs: Vec<SpeedtestRun>,
    /// Подсказка, если четыре потока заметно быстрее одного.
    pub hint: Option<String>,
}

/// Ход теста для `GET /v1/speedtest`; `null`, когда теста нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedtestProgress {
    /// Номер текущего замера с 1; 0 — узел ещё применяется в xray.
    pub run: usize,
    pub runs: usize,
    /// Доля скачанного в текущем замере, 0..=100.
    pub percent: u8,
}

/// Итог перечитывания файла настроек: `POST /v1/reload` и `PATCH /v1/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reloaded {
    /// Действующие настройки изменились.
    pub changed: bool,
    /// Конфиг xray изменился: xray перезапустится.
    pub xray_restart: bool,
    /// Ключи, которые действуют только после `raycat restart`; демон держит старые значения.
    pub restart_required: Vec<String>,
}

/// Значение настройки в запросе или ответе.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Number(i64),
    Text(String),
    Texts(Vec<String>),
}

/// Одно изменение для `PATCH /v1/settings`: таблица или подписка, ключ и значение.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    /// Таблица верхнего уровня: `routing`, `selection`, `xray`, `log`, `mode`.
    pub table: Option<String>,
    /// Подписка по имени (для `allow`, `deny`, `priority`).
    pub subscription: Option<String>,
    pub key: String,
    pub value: SettingValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsPatch {
    pub changes: Vec<SettingChange>,
}

/// Текущее значение настройки для `GET /v1/settings`; секретов нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingItem {
    pub table: Option<String>,
    pub subscription: Option<String>,
    pub key: String,
    pub value: SettingValue,
    /// `файл` — ключ записан в файле настроек, `по умолчанию` — значение не задано.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    pub items: Vec<SettingItem>,
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
    Hello {
        version: String,
    },
    NodeChanged {
        from: Option<String>,
        to: Option<String>,
        reason: String,
    },
    Pin {
        node: Option<String>,
    },
    SubscriptionUpdated {
        subscription: String,
        nodes: usize,
    },
    SubscriptionFailed {
        subscription: String,
        error: String,
    },
    Xray {
        state: XrayState,
        message: String,
    },
    Warning {
        level: String,
        message: String,
    },
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
            Event::Hello {
                version: "1".into(),
            },
            Event::Pin { node: None },
            Event::SubscriptionUpdated {
                subscription: "a".into(),
                nodes: 3,
            },
            Event::SubscriptionFailed {
                subscription: "a".into(),
                error: "x".into(),
            },
            Event::Xray {
                state: XrayState::Exited,
                message: "упал".into(),
            },
            Event::Warning {
                level: "warn".into(),
                message: "m".into(),
            },
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
            xray: XrayStatus {
                running: true,
                pid: Some(7),
                restarts: 0,
                api_silent_secs: Some(2),
                api_stuck: false,
            },
            node: None,
            subscriptions: Vec::new(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["mode"], "gateway");
        assert_eq!(json["xray"]["pid"], 7);
        assert_eq!(json["xray"]["api_silent_secs"], 2);
        assert_eq!(serde_json::from_value::<Status>(json).unwrap(), status);
    }

    #[test]
    fn xray_status_from_an_older_daemon_still_parses() {
        let xray: XrayStatus =
            serde_json::from_str(r#"{"running":true,"pid":7,"restarts":1}"#).unwrap();
        assert_eq!(xray.api_silent_secs, None);
        assert!(!xray.api_stuck);
        assert_eq!(xray.restarts, 1);
    }

    #[test]
    fn update_request_may_be_empty() {
        let request: UpdateRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request, UpdateRequest::default());
        let named: UpdateRequest = serde_json::from_str(r#"{"subscription":"a"}"#).unwrap();
        assert_eq!(named.subscription.as_deref(), Some("a"));
    }

    #[test]
    fn speedtest_request_needs_only_the_size() {
        let request: SpeedtestRequest = serde_json::from_str(r#"{"size":25000000}"#).unwrap();
        assert_eq!(
            request,
            SpeedtestRequest {
                node: None,
                size: 25_000_000,
                streams: None,
                url: None,
            }
        );
        let progress = SpeedtestProgress {
            run: 1,
            runs: 2,
            percent: 34,
        };
        let text = serde_json::to_string(&Some(progress.clone())).unwrap();
        assert_eq!(
            serde_json::from_str::<Option<SpeedtestProgress>>(&text).unwrap(),
            Some(progress)
        );
        assert_eq!(
            serde_json::from_str::<Option<SpeedtestProgress>>("null").unwrap(),
            None
        );
    }
}
