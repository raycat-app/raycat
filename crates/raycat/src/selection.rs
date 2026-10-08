//! Выбор узла в демоне: кандидаты из таблицы тегов, здоровье из observatory,
//! закрепление. Логика выбора живёт в `raycat-select`, здесь только сопоставление.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use raycat_config::Config;
use raycat_select::{Candidate, Health, PinTarget, Settings};
use raycat_xray::TagTable;
use raycat_xray_api::OutboundHealth;

/// Текущее время как отсчёт от начала эпохи Unix: по нему observatory датирует проверки.
pub(crate) fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Ключ узла: «подписка/имя узла». Имя подписки не содержит «/», поэтому ключ
/// однозначно разбирается по первой косой черте.
pub(crate) fn node_id(subscription: &str, name: &str) -> String {
    format!("{subscription}/{name}")
}

/// Кандидаты в порядке таблицы тегов (то есть подписок и узлов в конфиге). Ранг —
/// индекс первой маски `priority` подписки, которой подходит имя узла. У узлов с
/// одинаковым именем в подписке ключ получает номер: «подписка/имя (2)».
pub(crate) fn candidates(config: &Config, tags: &TagTable) -> Vec<Candidate> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    tags.entries()
        .iter()
        .filter_map(|entry| {
            let index = config
                .subscriptions
                .iter()
                .position(|subscription| subscription.name == entry.subscription)?;
            let rank = config.subscriptions.get(index).and_then(|subscription| {
                subscription
                    .priority
                    .iter()
                    .position(|pattern| pattern.matches(&entry.name))
            });
            let base = node_id(&entry.subscription, &entry.name);
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            let id = if *count == 1 {
                base
            } else {
                format!("{base} ({count})")
            };
            Some(Candidate {
                id,
                tag: entry.tag.clone(),
                subscription_index: index,
                subscription: entry.subscription.clone(),
                name: entry.name.clone(),
                rank: rank
                    .and_then(|rank| u32::try_from(rank).ok())
                    .unwrap_or(Candidate::UNRANKED),
            })
        })
        .collect()
}

pub(crate) fn settings(config: &Config, pin: Option<PinTarget>) -> Settings {
    Settings {
        failures: config.selection.failures,
        switch_gain: config.selection.switch_gain,
        return_delay: config.selection.return_delay,
        pin,
    }
}

/// Закрепление из настроек.
pub(crate) fn config_pin(config: &Config) -> Option<PinTarget> {
    config.selection.pin.as_ref().map(|pin| PinTarget {
        subscription: pin.subscription.clone(),
        node: pin.node.clone(),
    })
}

/// «подписка/имя узла» → закрепление; `None`, если формат неверен.
pub(crate) fn parse_pin(text: &str) -> Option<PinTarget> {
    let (subscription, node) = text.split_once('/')?;
    (!subscription.is_empty() && !node.trim().is_empty()).then(|| PinTarget {
        subscription: subscription.to_owned(),
        node: node.to_owned(),
    })
}

pub(crate) fn pin_id(pin: &PinTarget) -> String {
    node_id(&pin.subscription, &pin.node)
}

/// Данные observatory → здоровье для движка. Узлы, которых ещё не проверяли, не
/// передаются: у них нет времени проверки.
pub(crate) fn health(statuses: &[OutboundHealth]) -> Vec<Health> {
    statuses
        .iter()
        .filter_map(|status| {
            let checked_at = status.last_try?.duration_since(UNIX_EPOCH).ok()?;
            Some(Health {
                tag: status.tag.clone(),
                alive: status.alive,
                latency_ms: status.delay.map_or(0, |delay| {
                    u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)
                }),
                checked_at,
                error: status.last_error.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use raycat_config::Env;
    use raycat_subscription::analyze;

    use super::*;
    use crate::plan::compile_config;

    const LINKS: &[u8] = b"ss://aes-128-gcm:secret@203.0.113.5:8388#One\nss://aes-128-gcm:secret@203.0.113.6:8388#Two\nss://aes-128-gcm:secret@203.0.113.7:8388#Two\n";

    fn config(priority: &str) -> Config {
        let text = format!(
            "[[subscription]]\nname = \"first\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n{priority}\n[[subscription]]\nname = \"second\"\nurl = \"https://b.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n"
        );
        Config::from_toml_str(&text, &Env::new()).unwrap()
    }

    fn table(config: &Config) -> TagTable {
        let nodes = analyze(200, &[], LINKS).nodes;
        let inputs = [
            (&config.subscriptions[0], nodes.as_slice(), None),
            (&config.subscriptions[1], &nodes[..1], None),
        ];
        compile_config(
            config,
            &inputs,
            10_085,
            crate::speedtest::test_inbound(),
            None,
        )
        .unwrap()
        .tags
    }

    #[test]
    fn candidates_follow_the_table_and_carry_ranks() {
        let config = config("priority = [\"Tw*\", \"One\"]");
        let list = candidates(&config, &table(&config));
        let summary: Vec<(String, usize, u32)> = list
            .iter()
            .map(|c| (c.id.clone(), c.subscription_index, c.rank))
            .collect();
        assert_eq!(
            summary,
            [
                ("first/One".to_owned(), 0, 1),
                ("first/Two".to_owned(), 0, 0),
                ("first/Two (2)".to_owned(), 0, 0),
                ("second/One".to_owned(), 1, Candidate::UNRANKED),
            ]
        );
        assert_eq!(list[0].tag, "node-001-main");
        assert_eq!(list[3].tag, "node-004-main");
        assert_eq!(list[1].name, "Two");
    }

    #[test]
    fn nodes_without_a_matching_mask_are_unranked() {
        let config = config("");
        let list = candidates(&config, &table(&config));
        assert!(list.iter().all(|c| c.rank == Candidate::UNRANKED));
    }

    #[test]
    fn pins_parse_by_the_first_slash() {
        let pin = parse_pin("основная/NL/1").unwrap();
        assert_eq!(pin.subscription, "основная");
        assert_eq!(pin.node, "NL/1");
        assert_eq!(pin_id(&pin), "основная/NL/1");
        for bad in ["", "без-косой", "/узел", "подписка/", "подписка/  "]
        {
            assert_eq!(parse_pin(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn settings_come_from_the_selection_section() {
        let text = "[[subscription]]\nname = \"a\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n[selection]\nfailures = 5\nswitch_gain = \"200ms\"\nreturn_delay = \"10m\"\npin = \"a/Node\"\n";
        let config = Config::from_toml_str(text, &Env::new()).unwrap();
        let pin = config_pin(&config);
        let settings = settings(&config, pin.clone());
        assert_eq!(settings.failures, 5);
        assert_eq!(settings.switch_gain, Duration::from_millis(200));
        assert_eq!(settings.return_delay, Duration::from_secs(600));
        assert_eq!(pin.unwrap().node, "Node");
        assert_eq!(settings.pin.unwrap().subscription, "a");
    }

    fn observed(
        tag: &str,
        alive: bool,
        delay_ms: Option<u64>,
        seen: Option<u64>,
    ) -> OutboundHealth {
        OutboundHealth {
            tag: tag.to_owned(),
            alive,
            delay: delay_ms.map(Duration::from_millis),
            last_try: seen.map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
            last_seen: None,
            last_error: (!alive).then(|| "тайм-аут".to_owned()),
        }
    }

    #[test]
    fn unchecked_nodes_are_not_passed_to_the_engine() {
        let statuses = [
            observed("node-001-main", true, Some(42), Some(1_000)),
            observed("node-002-main", false, None, Some(1_001)),
            observed("node-003-main", false, None, None),
        ];
        let list = health(&statuses);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].latency_ms, 42);
        assert_eq!(list[0].checked_at, Duration::from_secs(1_000));
        assert!(list[0].alive && list[0].error.is_none());
        assert!(!list[1].alive);
        assert_eq!(list[1].error.as_deref(), Some("тайм-аут"));
        assert_eq!(list[1].latency_ms, 0);
    }

    #[test]
    fn a_time_before_the_epoch_is_skipped() {
        let mut status = observed("node-001-main", true, Some(1), Some(1));
        status.last_try = Some(SystemTime::UNIX_EPOCH - Duration::from_secs(5));
        assert_eq!(health(&[status]), Vec::<Health>::new());
    }
}
