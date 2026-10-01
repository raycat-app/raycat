use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::reason::{Reason, Warning};
use crate::state::NodeState;
use crate::{Candidate, Decision, Health, NodeInfo, PinTarget, Settings, Snapshot, Status};

/// Сколько новых проверок подряд узел того же приоритета должен быть заметно быстрее текущего.
const GAIN_CONFIRMATIONS: u32 = 2;

#[derive(Debug, Clone)]
struct Node {
    candidate: Candidate,
    state: NodeState,
}

struct Outcome {
    target: Option<usize>,
    reason: Reason,
}

/// Движок выбора узла: хранит историю проверок и текущий выбор.
#[derive(Debug, Clone)]
pub struct Selector {
    settings: Settings,
    nodes: Vec<Node>,
    current: Option<usize>,
}

impl Selector {
    /// Кандидаты идут в порядке приоритета подписок; узлы с повторным `id` или
    /// тегом отбрасываются.
    pub fn new(settings: Settings, candidates: Vec<Candidate>) -> Self {
        Self {
            settings,
            nodes: into_nodes(candidates, &mut HashMap::new()),
            current: None,
        }
    }

    /// Тег текущего узла.
    pub fn current(&self) -> Option<&str> {
        self.current.map(|index| self.tag(index))
    }

    pub fn current_id(&self) -> Option<&str> {
        self.current.map(|index| self.id(index))
    }

    pub fn set_pin(&mut self, pin: Option<PinTarget>) {
        self.settings.pin = pin;
    }

    /// Подменяет список узлов после пересборки конфига. История проверок и
    /// текущий выбор остаются за узлами с теми же `id`, даже если их теги сдвинулись.
    pub fn set_candidates(&mut self, candidates: Vec<Candidate>) {
        let current = self.current_id().map(str::to_owned);
        let mut history: HashMap<String, NodeState> = self
            .nodes
            .drain(..)
            .map(|node| (node.candidate.id, node.state))
            .collect();
        self.nodes = into_nodes(candidates, &mut history);
        self.current =
            current.and_then(|id| self.nodes.iter().position(|node| node.candidate.id == id));
        self.reset_streaks();
    }

    /// Один шаг: учитывает новые проверки и решает, какой узел закрепить.
    pub fn step(&mut self, now: Duration, health: &[Health]) -> Decision {
        self.observe(health);
        let previous = self.current;
        let mut warnings = Vec::new();
        let outcome = match self.pin_lookup() {
            Some(Ok(index)) => self.pinned(index),
            Some(Err(warning)) => {
                warnings.push(warning);
                self.choose(now)
            }
            None => self.choose(now),
        };
        self.current = outcome.target;
        Decision {
            selected: outcome.target.map(|index| self.tag(index).to_owned()),
            selected_id: outcome.target.map(|index| self.id(index).to_owned()),
            previous: previous.map(|index| self.tag(index).to_owned()),
            previous_id: previous.map(|index| self.id(index).to_owned()),
            changed: previous != outcome.target,
            reason: outcome.reason,
            warnings,
        }
    }

    pub fn snapshot(&self, now: Duration) -> Snapshot {
        let pin = self.settings.pin.as_ref();
        let nodes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let status = self.status(index);
                let candidate = &node.candidate;
                let state = &node.state;
                NodeInfo {
                    id: candidate.id.clone(),
                    tag: candidate.tag.clone(),
                    subscription: candidate.subscription.clone(),
                    name: candidate.name.clone(),
                    status,
                    latency_ms: if status == Status::Alive {
                        state.latency_ms
                    } else {
                        None
                    },
                    failures: state.failures,
                    alive_for_secs: self.alive_for(index, now).map(|age| age.as_secs()),
                    last_error: state.last_error.clone(),
                    selected: self.current == Some(index),
                    pinned: pin.is_some_and(|pin| {
                        pin.subscription == candidate.subscription && pin.node == candidate.name
                    }),
                }
            })
            .collect();
        Snapshot {
            selected: self.current().map(str::to_owned),
            selected_id: self.current_id().map(str::to_owned),
            nodes,
        }
    }

    fn observe(&mut self, health: &[Health]) {
        for node in &mut self.nodes {
            node.state.fresh = false;
        }
        let positions: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.candidate.tag.as_str(), index))
            .collect();
        let updates: Vec<(usize, &Health)> = health
            .iter()
            .filter_map(|item| positions.get(item.tag.as_str()).map(|&index| (index, item)))
            .collect();
        for (index, item) in updates {
            self.nodes[index].state.observe(item);
        }
    }

    fn pin_lookup(&self) -> Option<Result<usize, Warning>> {
        let pin = self.settings.pin.as_ref()?;
        let found = self.nodes.iter().position(|node| {
            node.candidate.subscription == pin.subscription && node.candidate.name == pin.node
        });
        Some(found.ok_or_else(|| Warning::PinNotFound {
            subscription: pin.subscription.clone(),
            node: pin.node.clone(),
        }))
    }

    fn pinned(&mut self, index: usize) -> Outcome {
        self.reset_streaks();
        Outcome {
            target: Some(index),
            reason: Reason::Pinned {
                node: self.name(index),
            },
        }
    }

    fn choose(&mut self, now: Duration) -> Outcome {
        if self.nodes.is_empty() {
            return Outcome {
                target: None,
                reason: Reason::NoCandidates,
            };
        }
        match self.current {
            None => self.start(),
            Some(index) if self.status(index) == Status::Dead => self.replace_dead(index),
            Some(index) => self.review(index, now),
        }
    }

    fn start(&mut self) -> Outcome {
        self.reset_streaks();
        if let Some(best) = self.best_alive() {
            return Outcome {
                target: Some(best),
                reason: Reason::Chosen {
                    node: self.name(best),
                },
            };
        }
        match self.best_ranked(|status| status != Status::Dead) {
            Some(index) => Outcome {
                target: Some(index),
                reason: Reason::Initial {
                    node: self.name(index),
                },
            },
            None => {
                let index = self.best_ranked(|_| true).unwrap_or_default();
                Outcome {
                    target: Some(index),
                    reason: Reason::NoAliveNodes {
                        node: self.name(index),
                    },
                }
            }
        }
    }

    /// Лучший по приоритету подписки, рангу и месту в списке, без учёта задержки.
    fn best_ranked(&self, allowed: impl Fn(Status) -> bool) -> Option<usize> {
        (0..self.nodes.len())
            .filter(|&index| allowed(self.status(index)))
            .min_by_key(|&index| {
                let (subscription, rank) = self.priority(index);
                (subscription, rank, index)
            })
    }

    fn replace_dead(&mut self, dead: usize) -> Outcome {
        self.reset_streaks();
        match self.best_alive() {
            Some(best) => Outcome {
                target: Some(best),
                reason: Reason::CurrentDead {
                    from: self.name(dead),
                    to: self.name(best),
                    failures: self.nodes[dead].state.failures,
                },
            },
            None => Outcome {
                target: Some(dead),
                reason: Reason::NoAliveNodes {
                    node: self.name(dead),
                },
            },
        }
    }

    fn review(&mut self, current: usize, now: Duration) -> Outcome {
        if let Some(better) = self.returning(current, now) {
            self.reset_streaks();
            return Outcome {
                target: Some(better),
                reason: self.return_reason(current, better),
            };
        }
        self.track_gain(current);
        if let Some(faster) = self.faster(current) {
            let reason = Reason::Faster {
                from: self.name(current),
                to: self.name(faster),
                from_ms: self.nodes[current].state.latency_ms.unwrap_or_default(),
                to_ms: self.nodes[faster].state.latency_ms.unwrap_or_default(),
            };
            self.reset_streaks();
            return Outcome {
                target: Some(faster),
                reason,
            };
        }
        Outcome {
            target: Some(current),
            reason: Reason::Kept {
                node: self.name(current),
            },
        }
    }

    /// Лучший из живых узлов с более высоким приоритетом, чем у текущего,
    /// которые непрерывно живут не меньше `return_delay`.
    fn returning(&self, current: usize, now: Duration) -> Option<usize> {
        let own = self.priority(current);
        (0..self.nodes.len())
            .filter(|&index| {
                self.priority(index) < own
                    && self.status(index) == Status::Alive
                    && self
                        .alive_for(index, now)
                        .is_some_and(|age| age >= self.settings.return_delay)
            })
            .min_by_key(|&index| self.sort_key(index))
    }

    fn return_reason(&self, current: usize, better: usize) -> Reason {
        let from = self.name(current);
        let to = self.name(better);
        let target = &self.nodes[better].candidate;
        if target.subscription_index == self.nodes[current].candidate.subscription_index {
            Reason::ReturnedToNode { from, to }
        } else {
            Reason::ReturnedToSubscription {
                subscription: target.subscription.clone(),
                from,
                to,
            }
        }
    }

    /// Проверка засчитывается, только если по одному из двух узлов пришли новые
    /// данные; шаг без новых проверок серию не меняет.
    fn track_gain(&mut self, current: usize) {
        let own = self.priority(current);
        let own_state = &self.nodes[current].state;
        let gain = duration_ms(self.settings.switch_gain);
        let verdicts: Vec<(usize, Option<bool>)> = (0..self.nodes.len())
            .filter(|&index| index != current && self.priority(index) == own)
            .map(|index| {
                let state = &self.nodes[index].state;
                let faster = self.status(index) == Status::Alive
                    && state.last_check_ok()
                    && is_faster(own_state.latency_ms, state.latency_ms, gain);
                let fresh = state.fresh || own_state.fresh;
                (index, fresh.then_some(faster))
            })
            .collect();
        for (index, verdict) in verdicts {
            let state = &mut self.nodes[index].state;
            match verdict {
                Some(true) => state.gain_streak = state.gain_streak.saturating_add(1),
                Some(false) => state.gain_streak = 0,
                None => {}
            }
        }
    }

    fn faster(&self, current: usize) -> Option<usize> {
        (0..self.nodes.len())
            .filter(|&index| {
                index != current
                    && self.nodes[index].state.gain_streak >= GAIN_CONFIRMATIONS
                    && self.status(index) == Status::Alive
            })
            .min_by_key(|&index| self.sort_key(index))
    }

    fn best_alive(&self) -> Option<usize> {
        (0..self.nodes.len())
            .filter(|&index| self.status(index) == Status::Alive)
            .min_by_key(|&index| self.sort_key(index))
    }

    fn reset_streaks(&mut self) {
        for node in &mut self.nodes {
            node.state.gain_streak = 0;
        }
    }

    fn status(&self, index: usize) -> Status {
        self.nodes[index]
            .state
            .status(self.settings.failures.max(1))
    }

    fn alive_for(&self, index: usize, now: Duration) -> Option<Duration> {
        self.nodes[index]
            .state
            .alive_since
            .map(|since| now.saturating_sub(since))
    }

    fn priority(&self, index: usize) -> (usize, u32) {
        let candidate = &self.nodes[index].candidate;
        (candidate.subscription_index, candidate.rank)
    }

    /// Порядок: подписка, узел внутри подписки, задержка, место в списке.
    fn sort_key(&self, index: usize) -> (usize, u32, u64, usize) {
        let (subscription, rank) = self.priority(index);
        let latency = self.nodes[index].state.latency_ms.unwrap_or(u64::MAX);
        (subscription, rank, latency, index)
    }

    fn tag(&self, index: usize) -> &str {
        &self.nodes[index].candidate.tag
    }

    fn id(&self, index: usize) -> &str {
        &self.nodes[index].candidate.id
    }

    fn name(&self, index: usize) -> String {
        self.nodes[index].candidate.name.clone()
    }
}

fn into_nodes(candidates: Vec<Candidate>, history: &mut HashMap<String, NodeState>) -> Vec<Node> {
    let mut ids = HashSet::new();
    let mut tags = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| {
            let new_id = ids.insert(candidate.id.clone());
            let new_tag = tags.insert(candidate.tag.clone());
            new_id && new_tag
        })
        .map(|candidate| {
            let state = history.remove(&candidate.id).unwrap_or_default();
            Node { candidate, state }
        })
        .collect()
}

fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

fn is_faster(current: Option<u64>, other: Option<u64>, gain_ms: u64) -> bool {
    match (current, other) {
        (Some(current), Some(other)) => current > other && current - other >= gain_ms,
        _ => false,
    }
}
