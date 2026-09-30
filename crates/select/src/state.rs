use std::time::Duration;

use crate::{Health, Status};

/// Накопленные сведения о здоровье одного узла.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeState {
    last_checked: Option<Duration>,
    pub(crate) failures: u32,
    ever_alive: bool,
    pub(crate) latency_ms: Option<u64>,
    pub(crate) alive_since: Option<Duration>,
    pub(crate) last_error: Option<String>,
    /// На этом шаге пришла новая проверка.
    pub(crate) fresh: bool,
    /// Подряд идущих проверок, на которых узел заметно быстрее текущего.
    pub(crate) gain_streak: u32,
}

impl NodeState {
    pub(crate) fn observe(&mut self, health: &Health) {
        if self.last_checked.is_some_and(|last| health.checked_at <= last) {
            return;
        }
        self.last_checked = Some(health.checked_at);
        self.fresh = true;
        if health.alive {
            self.failures = 0;
            self.ever_alive = true;
            self.latency_ms = Some(health.latency_ms);
            self.last_error = None;
            if self.alive_since.is_none() {
                self.alive_since = Some(health.checked_at);
            }
        } else {
            self.failures = self.failures.saturating_add(1);
            self.alive_since = None;
            self.last_error.clone_from(&health.error);
        }
    }

    pub(crate) fn status(&self, threshold: u32) -> Status {
        if self.failures >= threshold {
            Status::Dead
        } else if self.ever_alive {
            Status::Alive
        } else {
            Status::Unknown
        }
    }

    pub(crate) fn last_check_ok(&self) -> bool {
        self.ever_alive && self.failures == 0
    }
}
