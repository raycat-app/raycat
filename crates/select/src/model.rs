use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::reason::{Reason, Warning};

/// Узел, из которого выбирает движок.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Тег выхода узла в конфиге xray.
    pub tag: String,
    /// Позиция подписки в порядке приоритета: меньше — важнее.
    pub subscription_index: usize,
    pub subscription: String,
    pub name: String,
    /// Приоритет узла внутри подписки: индекс первой совпавшей маски `priority`,
    /// меньше — важнее. Узлам без совпадения ставят [`Candidate::UNRANKED`].
    pub rank: u32,
}

impl Candidate {
    pub const UNRANKED: u32 = u32::MAX;
}

/// Узел, закреплённый вручную.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinTarget {
    pub subscription: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Сколько проверок подряд без ответа делают узел мёртвым.
    pub failures: u32,
    /// Насколько быстрее текущего должен быть узел того же приоритета, чтобы на него перейти.
    pub switch_gain: Duration,
    /// Сколько приоритетный узел должен непрерывно жить, чтобы вернуться на него.
    pub return_delay: Duration,
    pub pin: Option<PinTarget>,
}

/// Результат одной проверки узла из observatory xray.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub tag: String,
    pub alive: bool,
    pub latency_ms: u64,
    /// Время последней проверки. Запись с тем же или меньшим временем, чем уже
    /// учтённое, новой проверкой не считается. Узлы, которых ещё не проверяли,
    /// в список не попадают.
    pub checked_at: Duration,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Alive,
    Dead,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// Выбранный тег; `None`, только если кандидатов нет.
    pub selected: Option<String>,
    pub previous: Option<String>,
    pub changed: bool,
    pub reason: Reason,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub tag: String,
    pub subscription: String,
    pub name: String,
    pub status: Status,
    /// Задержка последней удачной проверки; только у живых узлов.
    pub latency_ms: Option<u64>,
    /// Неудачных проверок подряд.
    pub failures: u32,
    /// Сколько узел непрерывно жив, в секундах.
    pub alive_for_secs: Option<u64>,
    pub last_error: Option<String>,
    pub selected: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub selected: Option<String>,
    pub nodes: Vec<NodeInfo>,
}
