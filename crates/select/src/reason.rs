use std::fmt;

use serde::{Deserialize, Serialize};

/// Почему выбран именно этот узел; `Display` даёт фразу для лога и API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    NoCandidates,
    /// Первые данные о здоровье ещё не пришли.
    Initial {
        node: String,
    },
    Chosen {
        node: String,
    },
    Pinned {
        node: String,
    },
    CurrentDead {
        from: String,
        to: String,
        failures: u32,
    },
    ReturnedToSubscription {
        subscription: String,
        from: String,
        to: String,
    },
    ReturnedToNode {
        from: String,
        to: String,
    },
    Faster {
        from: String,
        to: String,
        from_ms: u64,
        to_ms: u64,
    },
    NoAliveNodes {
        node: String,
    },
    Kept {
        node: String,
    },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCandidates => f.write_str("нет узлов для выбора"),
            Self::Initial { node } => write!(
                f,
                "выбран лучший по приоритету узел «{node}»: данных о здоровье ещё нет"
            ),
            Self::Chosen { node } => write!(f, "выбран лучший живой узел «{node}»"),
            Self::Pinned { node } => write!(f, "закреплён вручную: «{node}»"),
            Self::CurrentDead { from, to, failures } => {
                let word = plural(*failures, "проверка", "проверки", "проверок");
                write!(
                    f,
                    "переключился с «{from}» на «{to}»: {failures} {word} подряд без ответа"
                )
            }
            Self::ReturnedToSubscription {
                subscription,
                from,
                to,
            } => write!(
                f,
                "вернулся на приоритетную подписку «{subscription}»: «{to}» вместо «{from}»"
            ),
            Self::ReturnedToNode { from, to } => {
                write!(f, "вернулся на приоритетный узел «{to}» вместо «{from}»")
            }
            Self::Faster {
                from,
                to,
                from_ms,
                to_ms,
            } => {
                let gain = from_ms.saturating_sub(*to_ms);
                write!(
                    f,
                    "переключился с «{from}» на «{to}»: быстрее на {gain} мс ({to_ms} мс против {from_ms} мс)"
                )
            }
            Self::NoAliveNodes { node } => {
                write!(f, "нет живых узлов, текущим остаётся «{node}»")
            }
            Self::Kept { node } => write!(f, "остаётся «{node}»"),
        }
    }
}

/// Замечание к решению: выбор при этом сделан, но что-то в настройках не так.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    PinNotFound { subscription: String, node: String },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinNotFound { subscription, node } => write!(
                f,
                "закреплённый узел «{subscription}/{node}» не найден, выбор идёт обычным порядком"
            ),
        }
    }
}

fn plural<'a>(count: u32, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let tens = count % 100;
    let units = count % 10;
    if (11..=14).contains(&tens) {
        many
    } else if units == 1 {
        one
    } else if (2..=4).contains(&units) {
        few
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead(failures: u32) -> String {
        Reason::CurrentDead {
            from: "NL-1".into(),
            to: "DE-2".into(),
            failures,
        }
        .to_string()
    }

    #[test]
    fn dead_reason_declines_the_noun() {
        assert_eq!(
            dead(3),
            "переключился с «NL-1» на «DE-2»: 3 проверки подряд без ответа"
        );
        assert!(dead(1).ends_with("1 проверка подряд без ответа"));
        assert!(dead(5).ends_with("5 проверок подряд без ответа"));
        assert!(dead(11).ends_with("11 проверок подряд без ответа"));
        assert!(dead(21).ends_with("21 проверка подряд без ответа"));
        assert!(dead(24).ends_with("24 проверки подряд без ответа"));
        assert!(dead(112).ends_with("112 проверок подряд без ответа"));
    }

    #[test]
    fn reason_texts() {
        let text = |reason: Reason| reason.to_string();
        assert_eq!(
            text(Reason::ReturnedToSubscription {
                subscription: "основная".into(),
                from: "DE-2".into(),
                to: "NL-1".into(),
            }),
            "вернулся на приоритетную подписку «основная»: «NL-1» вместо «DE-2»"
        );
        assert_eq!(
            text(Reason::Pinned {
                node: "NL-1".into()
            }),
            "закреплён вручную: «NL-1»"
        );
        assert_eq!(
            text(Reason::Faster {
                from: "NL-1".into(),
                to: "NL-2".into(),
                from_ms: 180,
                to_ms: 60,
            }),
            "переключился с «NL-1» на «NL-2»: быстрее на 120 мс (60 мс против 180 мс)"
        );
        assert_eq!(
            text(Reason::NoAliveNodes {
                node: "NL-1".into()
            }),
            "нет живых узлов, текущим остаётся «NL-1»"
        );
    }

    #[test]
    fn warning_text() {
        let warning = Warning::PinNotFound {
            subscription: "основная".into(),
            node: "NL-9".into(),
        };
        assert_eq!(
            warning.to_string(),
            "закреплённый узел «основная/NL-9» не найден, выбор идёт обычным порядком"
        );
    }
}
