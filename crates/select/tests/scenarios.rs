// Хелперы тестов не покрыты allow-expect-in-tests: там ошибка и есть падение теста.
#![allow(clippy::expect_used)]

use std::time::Duration;

use raycat_select::{
    Candidate, Decision, Health, NodeInfo, PinTarget, Reason, Selector, Settings, Snapshot, Status,
    Warning,
};

const TAGS: [&str; 4] = ["a1", "a2", "a3", "b1"];
const DOWN: Option<u64> = None;

/// Результаты проверок узлов `a1`, `a2`, `a3`, `b1`: задержка или `DOWN`.
type Probe = [Option<u64>; 4];

#[allow(clippy::unnecessary_wraps)]
fn up(ms: u64) -> Option<u64> {
    Some(ms)
}

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn candidate(
    tag: &str,
    subscription_index: usize,
    subscription: &str,
    name: &str,
    rank: u32,
) -> Candidate {
    Candidate {
        id: format!("{subscription}/{name}"),
        tag: tag.to_owned(),
        subscription_index,
        subscription: subscription.to_owned(),
        name: name.to_owned(),
        rank,
    }
}

/// Подписка `main`: NL-1 и NL-2 одного приоритета, NL-3 без маски; подписка `backup`: DE-1.
fn nodes() -> Vec<Candidate> {
    vec![
        candidate("a1", 0, "main", "NL-1", 0),
        candidate("a2", 0, "main", "NL-2", 0),
        candidate("a3", 0, "main", "NL-3", Candidate::UNRANKED),
        candidate("b1", 1, "backup", "DE-1", 0),
    ]
}

fn settings() -> Settings {
    Settings {
        failures: 3,
        switch_gain: Duration::from_millis(50),
        return_delay: secs(60),
        pin: None,
    }
}

#[allow(clippy::unnecessary_wraps)]
fn pin(subscription: &str, node: &str) -> Option<PinTarget> {
    Some(PinTarget {
        subscription: subscription.to_owned(),
        node: node.to_owned(),
    })
}

fn selector() -> Selector {
    Selector::new(settings(), nodes())
}

fn health(at: u64, probe: Probe) -> Vec<Health> {
    TAGS.iter()
        .zip(probe)
        .map(|(tag, result)| Health {
            tag: (*tag).to_owned(),
            alive: result.is_some(),
            latency_ms: result.unwrap_or(0),
            checked_at: secs(at),
            error: result.is_none().then(|| "timeout".to_owned()),
        })
        .collect()
}

fn step(selector: &mut Selector, at: u64, probe: Probe) -> Decision {
    selector.step(secs(at), &health(at, probe))
}

/// Строка таблицы: момент, результаты проверок, ожидаемый выбор, менялся ли он.
type Row = (u64, Probe, &'static str, bool);

fn drive(selector: &mut Selector, rows: &[Row]) -> Decision {
    let mut last = None;
    for (index, &(at, probe, expected, changed)) in rows.iter().enumerate() {
        let decision = step(selector, at, probe);
        assert_eq!(
            decision.selected.as_deref(),
            Some(expected),
            "строка {index} (t={at}): {decision:?}"
        );
        assert_eq!(
            decision.changed, changed,
            "строка {index} (t={at}): {decision:?}"
        );
        last = Some(decision);
    }
    last.expect("в таблице нет строк")
}

fn healthy() -> Probe {
    [up(100), up(120), up(150), up(80)]
}

fn node<'a>(snapshot: &'a Snapshot, tag: &str) -> &'a NodeInfo {
    snapshot
        .nodes
        .iter()
        .find(|node| node.tag == tag)
        .expect("узла нет в снимке")
}

/// Первый в списке узел без маски, лучший по рангу — третий; подписка `backup` последняя.
fn ranked_nodes() -> Vec<Candidate> {
    vec![
        candidate("a1", 0, "main", "NL-1", Candidate::UNRANKED),
        candidate("a2", 0, "main", "NL-2", 1),
        candidate("a3", 0, "main", "NL-3", 0),
        candidate("b1", 1, "backup", "DE-1", 0),
    ]
}

#[test]
fn start_without_health_picks_the_best_ranked_node() {
    let mut selector = Selector::new(settings(), ranked_nodes());
    let first = selector.step(secs(0), &[]);
    assert_eq!(first.selected.as_deref(), Some("a3"));
    assert_eq!(first.previous, None);
    assert!(first.changed);
    assert_eq!(
        first.reason,
        Reason::Initial {
            node: "NL-3".into()
        }
    );

    let second = selector.step(secs(10), &[]);
    assert_eq!(second.selected.as_deref(), Some("a3"));
    assert!(!second.changed);
}

#[test]
fn start_skips_dead_nodes_and_keeps_subscription_order() {
    let one_failure = Settings {
        failures: 1,
        ..settings()
    };

    let mut selector = Selector::new(one_failure.clone(), ranked_nodes());
    let partial = raw_health(0, &[("a3", DOWN)]);
    let decision = selector.step(secs(0), &partial);
    assert_eq!(decision.selected.as_deref(), Some("a2"));
    assert_eq!(
        decision.reason,
        Reason::Initial {
            node: "NL-2".into()
        }
    );

    let mut selector = Selector::new(one_failure, ranked_nodes());
    let main_down = raw_health(0, &[("a1", DOWN), ("a2", DOWN), ("a3", DOWN)]);
    let decision = selector.step(secs(0), &main_down);
    assert_eq!(decision.selected.as_deref(), Some("b1"));
}

#[test]
fn all_dead_at_first_data_selects_the_best_ranked_node() {
    let mut selector = Selector::new(
        Settings {
            failures: 1,
            ..settings()
        },
        ranked_nodes(),
    );
    let decision = step(&mut selector, 0, [DOWN; 4]);
    assert_eq!(decision.selected.as_deref(), Some("a3"));
    assert!(decision.changed);
    assert_eq!(
        decision.reason,
        Reason::NoAliveNodes {
            node: "NL-3".into()
        }
    );
}

#[test]
fn start_without_health_keeps_list_order_among_equal_ranks() {
    let mut selector = selector();
    let first = selector.step(secs(0), &[]);
    assert_eq!(first.selected.as_deref(), Some("a1"));
    assert_eq!(first.previous, None);
    assert!(first.changed);
    assert_eq!(
        first.reason,
        Reason::Initial {
            node: "NL-1".into()
        }
    );

    let second = selector.step(secs(10), &[]);
    assert_eq!(second.selected.as_deref(), Some("a1"));
    assert!(!second.changed);
    assert_eq!(
        second.reason,
        Reason::Kept {
            node: "NL-1".into()
        }
    );
}

#[test]
fn first_health_picks_best_alive_node() {
    let mut selector = selector();
    let decision = step(&mut selector, 0, [up(500), up(40), up(10), up(5)]);
    assert_eq!(decision.selected.as_deref(), Some("a2"));
    assert_eq!(
        decision.reason,
        Reason::Chosen {
            node: "NL-2".into()
        }
    );
}

#[test]
fn equal_latency_keeps_list_order() {
    let mut selector = selector();
    let decision = step(&mut selector, 0, [up(100), up(100), up(100), up(100)]);
    assert_eq!(decision.selected.as_deref(), Some("a1"));
}

#[test]
fn unknown_current_node_is_kept_until_data_arrives() {
    let mut selector = selector();
    assert_eq!(selector.step(secs(0), &[]).selected.as_deref(), Some("a1"));
    let partial: Vec<Health> = health(10, healthy())
        .into_iter()
        .filter(|item| item.tag != "a1")
        .collect();
    let decision = selector.step(secs(10), &partial);
    assert_eq!(decision.selected.as_deref(), Some("a1"));
    assert!(!decision.changed);

    let rows: [Row; 3] = [
        (20, [DOWN, up(120), up(150), up(80)], "a1", false),
        (30, [DOWN, up(120), up(150), up(80)], "a1", false),
        (40, [DOWN, up(120), up(150), up(80)], "a2", true),
    ];
    drive(&mut selector, &rows);
}

#[test]
fn dead_current_node_is_replaced_by_best_alive() {
    let mut selector = selector();
    let down = [DOWN, up(120), up(150), up(80)];
    let switched = drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, down, "a1", false),
            (20, down, "a1", false),
            (30, down, "a2", true),
        ],
    );
    assert_eq!(
        switched.reason.to_string(),
        "переключился с «NL-1» на «NL-2»: 3 проверки подряд без ответа"
    );
    assert_eq!(switched.previous.as_deref(), Some("a1"));

    let kept = drive(&mut selector, &[(40, down, "a2", false)]);
    assert_eq!(
        kept.reason,
        Reason::Kept {
            node: "NL-2".into()
        }
    );
}

#[test]
fn one_success_resets_the_failure_counter() {
    let mut selector = selector();
    let down = [DOWN, up(120), up(150), up(80)];
    drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, down, "a1", false),
            (20, down, "a1", false),
            (30, healthy(), "a1", false),
            (40, down, "a1", false),
            (50, down, "a1", false),
            (60, down, "a2", true),
        ],
    );
}

#[test]
fn latency_flapping_does_not_switch() {
    let mut selector = selector();
    let quick = [up(100), up(30), up(150), up(80)];
    let slow = [up(100), up(100), up(150), up(80)];
    drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, quick, "a1", false),
            (20, slow, "a1", false),
            (30, quick, "a1", false),
            (40, slow, "a1", false),
            (50, quick, "a1", false),
            (60, slow, "a1", false),
        ],
    );
}

#[test]
fn steady_gain_on_two_checks_switches() {
    let mut selector = selector();
    let quick = [up(100), up(30), up(150), up(80)];
    let switched = drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, quick, "a1", false),
            (20, quick, "a2", true),
        ],
    );
    drive(&mut selector, &[(30, quick, "a2", false)]);
    assert_eq!(
        switched.reason,
        Reason::Faster {
            from: "NL-1".into(),
            to: "NL-2".into(),
            from_ms: 100,
            to_ms: 30,
        }
    );
    assert_eq!(
        switched.reason.to_string(),
        "переключился с «NL-1» на «NL-2»: быстрее на 70 мс (30 мс против 100 мс)"
    );
}

#[test]
fn gain_below_threshold_never_switches() {
    let mut selector = selector();
    let close = [up(100), up(60), up(150), up(80)];
    let mut rows: Vec<Row> = vec![(0, healthy(), "a1", true)];
    rows.extend((10..=100).step_by(10).map(|at| (at, close, "a1", false)));
    drive(&mut selector, &rows);
}

#[test]
fn gain_exactly_at_threshold_counts() {
    let mut selector = selector();
    let exact = [up(100), up(50), up(150), up(80)];
    drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, exact, "a1", false),
            (20, exact, "a2", true),
        ],
    );
}

#[test]
fn repeated_stale_checks_do_not_count() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);
    let quick = health(10, [up(100), up(30), up(150), up(80)]);
    assert_eq!(
        selector.step(secs(10), &quick).selected.as_deref(),
        Some("a1")
    );
    for now in [20, 30, 40] {
        let decision = selector.step(secs(now), &quick);
        assert_eq!(decision.selected.as_deref(), Some("a1"));
        assert!(!decision.changed);
    }
    let decision = step(&mut selector, 50, [up(100), up(30), up(150), up(80)]);
    assert_eq!(decision.selected.as_deref(), Some("a2"));
}

#[test]
fn repeated_stale_failures_do_not_kill_a_node() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);
    let failed = health(10, [DOWN, up(120), up(150), up(80)]);
    for now in (10..=100).step_by(10) {
        let decision = selector.step(secs(now), &failed);
        assert_eq!(decision.selected.as_deref(), Some("a1"));
    }
    let snapshot = selector.snapshot(secs(100));
    assert_eq!(node(&snapshot, "a1").failures, 1);
    assert_eq!(node(&snapshot, "a1").status, Status::Alive);
}

#[test]
fn returns_to_priority_subscription_only_after_delay() {
    let mut selector = selector();
    let outage = [DOWN, DOWN, DOWN, up(80)];
    let recovered = [up(100), DOWN, DOWN, up(80)];
    let mut rows: Vec<Row> = vec![
        (0, healthy(), "a1", true),
        (10, outage, "a1", false),
        (20, outage, "a1", false),
        (30, outage, "b1", true),
    ];
    rows.extend((40..=90).step_by(10).map(|at| (at, recovered, "b1", false)));
    rows.push((100, recovered, "a1", true));
    let last = drive(&mut selector, &rows);
    assert_eq!(
        last.reason,
        Reason::ReturnedToSubscription {
            subscription: "main".into(),
            from: "DE-1".into(),
            to: "NL-1".into(),
        }
    );
}

#[test]
fn failed_check_restarts_the_return_delay() {
    let mut selector = selector();
    let outage = [DOWN, DOWN, DOWN, up(80)];
    let recovered = [up(100), DOWN, DOWN, up(80)];
    let mut rows: Vec<Row> = vec![
        (0, healthy(), "a1", true),
        (10, outage, "a1", false),
        (20, outage, "a1", false),
        (30, outage, "b1", true),
        (40, recovered, "b1", false),
        (50, outage, "b1", false),
        (60, recovered, "b1", false),
    ];
    rows.extend(
        (70..=110)
            .step_by(10)
            .map(|at| (at, recovered, "b1", false)),
    );
    rows.push((120, recovered, "a1", true));
    drive(&mut selector, &rows);
}

#[test]
fn returns_to_preferred_node_inside_subscription() {
    let mut selector = selector();
    let outage = [DOWN, DOWN, up(150), up(80)];
    let recovered = [up(100), DOWN, up(150), up(80)];
    let mut rows: Vec<Row> = vec![
        (0, healthy(), "a1", true),
        (10, outage, "a1", false),
        (20, outage, "a1", false),
        (30, outage, "a3", true),
    ];
    rows.extend((40..=90).step_by(10).map(|at| (at, recovered, "a3", false)));
    rows.push((100, recovered, "a1", true));
    let last = drive(&mut selector, &rows);
    assert_eq!(
        last.reason,
        Reason::ReturnedToNode {
            from: "NL-3".into(),
            to: "NL-1".into(),
        }
    );
}

#[test]
fn faster_node_of_lower_priority_is_ignored() {
    let mut selector = selector();
    let mut rows: Vec<Row> = vec![(0, [up(500), DOWN, up(20), up(1)], "a1", true)];
    rows.extend(
        (10..=100)
            .step_by(10)
            .map(|at| (at, [up(500), DOWN, up(20), up(1)], "a1", false)),
    );
    drive(&mut selector, &rows);
}

#[test]
fn pinned_node_is_kept_even_when_dead() {
    let mut selector = Selector::new(
        Settings {
            pin: pin("main", "NL-3"),
            ..settings()
        },
        nodes(),
    );
    let dead = [up(100), up(120), DOWN, up(80)];
    let last = drive(
        &mut selector,
        &[
            (0, healthy(), "a3", true),
            (10, dead, "a3", false),
            (20, dead, "a3", false),
            (30, dead, "a3", false),
            (40, dead, "a3", false),
        ],
    );
    assert_eq!(
        last.reason,
        Reason::Pinned {
            node: "NL-3".into()
        }
    );
    assert!(last.warnings.is_empty());
    let snapshot = selector.snapshot(secs(40));
    assert_eq!(node(&snapshot, "a3").status, Status::Dead);
    assert!(node(&snapshot, "a3").pinned);
    assert!(node(&snapshot, "a3").selected);
}

#[test]
fn missing_pin_warns_and_falls_back_to_normal_choice() {
    let mut selector = Selector::new(
        Settings {
            pin: pin("main", "NL-9"),
            ..settings()
        },
        nodes(),
    );
    let decision = step(&mut selector, 0, healthy());
    assert_eq!(decision.selected.as_deref(), Some("a1"));
    assert_eq!(
        decision.warnings,
        vec![Warning::PinNotFound {
            subscription: "main".into(),
            node: "NL-9".into(),
        }]
    );
    assert_eq!(
        decision.warnings[0].to_string(),
        "закреплённый узел «main/NL-9» не найден, выбор идёт обычным порядком"
    );
}

#[test]
fn pin_can_be_set_and_cleared_at_runtime() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);

    selector.set_pin(pin("backup", "DE-1"));
    let pinned = step(&mut selector, 10, healthy());
    assert_eq!(pinned.selected.as_deref(), Some("b1"));
    assert!(pinned.changed);
    assert_eq!(pinned.reason.to_string(), "закреплён вручную: «DE-1»");

    selector.set_pin(None);
    let released = step(&mut selector, 100, healthy());
    assert_eq!(released.selected.as_deref(), Some("a1"));
    assert!(matches!(
        released.reason,
        Reason::ReturnedToSubscription { .. }
    ));
}

#[test]
fn all_dead_keeps_the_current_node() {
    let mut selector = selector();
    let down = [DOWN; 4];
    let last = drive(
        &mut selector,
        &[
            (0, down, "a1", true),
            (10, down, "a1", false),
            (20, down, "a1", false),
            (30, down, "a1", false),
        ],
    );
    assert_eq!(
        last.reason,
        Reason::NoAliveNodes {
            node: "NL-1".into()
        }
    );
    assert_eq!(
        last.reason.to_string(),
        "нет живых узлов, текущим остаётся «NL-1»"
    );

    let recovered = step(&mut selector, 40, [DOWN, up(120), DOWN, DOWN]);
    assert_eq!(recovered.selected.as_deref(), Some("a2"));
    assert!(recovered.changed);
}

#[test]
fn all_dead_at_first_data_selects_first_node() {
    let mut selector = Selector::new(
        Settings {
            failures: 1,
            ..settings()
        },
        nodes(),
    );
    let decision = step(&mut selector, 0, [DOWN; 4]);
    assert_eq!(decision.selected.as_deref(), Some("a1"));
    assert!(decision.changed);
    assert_eq!(
        decision.reason,
        Reason::NoAliveNodes {
            node: "NL-1".into()
        }
    );
}

#[test]
fn subscription_priority_beats_latency() {
    let mut selector = selector();
    let mut rows: Vec<Row> = vec![(0, [up(500), DOWN, DOWN, up(10)], "a1", true)];
    rows.extend(
        (10..=50)
            .step_by(10)
            .map(|at| (at, [up(500), DOWN, DOWN, up(10)], "a1", false)),
    );
    drive(&mut selector, &rows);
}

#[test]
fn node_priority_masks_beat_latency_and_keep_their_order() {
    let mut selector = Selector::new(settings(), ranked_nodes());
    let all = [up(10), up(20), up(300), up(5)];
    let no_a3 = [up(10), up(20), DOWN, up(5)];
    let no_a2 = [up(10), DOWN, DOWN, up(5)];
    let only_b1 = [DOWN, DOWN, DOWN, up(5)];
    drive(
        &mut selector,
        &[
            (0, all, "a3", true),
            (10, no_a3, "a3", false),
            (20, no_a3, "a3", false),
            (30, no_a3, "a2", true),
            (40, no_a2, "a2", false),
            (50, no_a2, "a2", false),
            (60, no_a2, "a1", true),
            (70, only_b1, "a1", false),
            (80, only_b1, "a1", false),
            (90, only_b1, "b1", true),
        ],
    );
}

#[test]
fn no_candidates_gives_no_selection() {
    let mut selector = Selector::new(settings(), Vec::new());
    let decision = selector.step(secs(0), &[]);
    assert_eq!(decision.selected, None);
    assert!(!decision.changed);
    assert_eq!(decision.reason, Reason::NoCandidates);
}

#[test]
fn duplicate_tags_and_ids_are_dropped() {
    let mut list = nodes();
    list.push(candidate("a1", 2, "other", "NL-1", 0));
    list.push(candidate("x9", 2, "main", "NL-1", 0));
    let selector = Selector::new(settings(), list);
    assert_eq!(selector.snapshot(secs(0)).nodes.len(), 4);
}

fn raw_health(at: u64, probes: &[(&str, Option<u64>)]) -> Vec<Health> {
    probes
        .iter()
        .map(|&(tag, result)| Health {
            tag: tag.to_owned(),
            alive: result.is_some(),
            latency_ms: result.unwrap_or(0),
            checked_at: secs(at),
            error: result.is_none().then(|| "timeout".to_owned()),
        })
        .collect()
}

#[test]
fn tag_shift_after_rebuild_keeps_history_and_selection() {
    let mut selector = selector();
    drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, [DOWN, up(120), up(150), up(80)], "a1", false),
        ],
    );

    // В первой подписке появился узел: у всех следующих новые теги.
    selector.set_candidates(vec![
        candidate("n0", 0, "main", "NL-0", 0),
        candidate("n1", 0, "main", "NL-1", 0),
        candidate("n2", 0, "main", "NL-2", 0),
        candidate("n3", 0, "main", "NL-3", Candidate::UNRANKED),
        candidate("n4", 1, "backup", "DE-1", 0),
    ]);
    assert_eq!(selector.current(), Some("n1"));
    assert_eq!(selector.current_id(), Some("main/NL-1"));
    let snapshot = selector.snapshot(secs(20));
    assert_eq!(snapshot.selected.as_deref(), Some("n1"));
    assert_eq!(snapshot.selected_id.as_deref(), Some("main/NL-1"));
    assert_eq!(node(&snapshot, "n1").failures, 1);
    assert_eq!(node(&snapshot, "n1").id, "main/NL-1");
    assert_eq!(node(&snapshot, "n2").alive_for_secs, Some(20));
    assert_eq!(node(&snapshot, "n0").status, Status::Unknown);

    let probes = [
        ("n0", up(90)),
        ("n1", DOWN),
        ("n2", up(120)),
        ("n3", up(150)),
        ("n4", up(80)),
    ];
    let second = selector.step(secs(20), &raw_health(20, &probes));
    assert_eq!(second.selected.as_deref(), Some("n1"));
    assert!(!second.changed);

    let third = selector.step(secs(30), &raw_health(30, &probes));
    assert_eq!(third.selected.as_deref(), Some("n0"));
    assert_eq!(third.selected_id.as_deref(), Some("main/NL-0"));
    assert_eq!(third.previous.as_deref(), Some("n1"));
    assert_eq!(third.previous_id.as_deref(), Some("main/NL-1"));
    assert_eq!(
        third.reason,
        Reason::CurrentDead {
            from: "NL-1".into(),
            to: "NL-0".into(),
            failures: 3,
        }
    );
}

#[test]
fn health_with_a_stale_tag_is_ignored_after_rebuild() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);
    selector.set_candidates(vec![
        candidate("n0", 0, "main", "NL-0", 0),
        candidate("n1", 0, "main", "NL-1", 0),
    ]);
    let decision = selector.step(secs(10), &raw_health(10, &[("a1", DOWN), ("n1", up(100))]));
    assert_eq!(decision.selected.as_deref(), Some("n1"));
    let snapshot = selector.snapshot(secs(10));
    assert_eq!(node(&snapshot, "n0").status, Status::Unknown);
    assert_eq!(node(&snapshot, "n1").failures, 0);
}

#[test]
fn replacing_candidates_keeps_history_and_current_node() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);

    selector.set_candidates(nodes());
    assert_eq!(selector.current(), Some("a1"));
    let snapshot = selector.snapshot(secs(30));
    assert_eq!(node(&snapshot, "a2").alive_for_secs, Some(30));

    let without_current: Vec<Candidate> = nodes().into_iter().skip(1).collect();
    selector.set_candidates(without_current);
    assert_eq!(selector.current(), None);
    let decision = step(&mut selector, 40, healthy());
    assert_eq!(decision.selected.as_deref(), Some("a2"));
    assert!(matches!(decision.reason, Reason::Chosen { .. }));
}

#[test]
fn snapshot_describes_every_node() {
    let mut selector = selector();
    let before = selector.snapshot(secs(0));
    assert_eq!(before.selected, None);
    assert!(
        before
            .nodes
            .iter()
            .all(|info| info.status == Status::Unknown)
    );

    drive(
        &mut selector,
        &[
            (0, healthy(), "a1", true),
            (10, [DOWN, up(120), up(150), up(80)], "a1", false),
        ],
    );
    let snapshot = selector.snapshot(secs(20));
    assert_eq!(snapshot.selected.as_deref(), Some("a1"));
    let first = node(&snapshot, "a1");
    assert_eq!(first.status, Status::Alive);
    assert_eq!(first.latency_ms, Some(100));
    assert_eq!(first.failures, 1);
    assert_eq!(first.alive_for_secs, None);
    assert_eq!(first.last_error.as_deref(), Some("timeout"));
    assert!(first.selected);
    let second = node(&snapshot, "a2");
    assert_eq!(second.alive_for_secs, Some(20));
    assert_eq!(second.last_error, None);
    assert!(!second.selected);
    assert!(!second.pinned);

    step(&mut selector, 20, [DOWN, up(120), up(150), up(80)]);
    step(&mut selector, 30, [DOWN, up(120), up(150), up(80)]);
    let dead = selector.snapshot(secs(30));
    let first = node(&dead, "a1");
    assert_eq!(first.status, Status::Dead);
    assert_eq!(first.latency_ms, None);
    assert_eq!(first.failures, 3);
    assert_eq!(dead.selected.as_deref(), Some("a2"));
}

#[test]
fn decision_and_snapshot_survive_a_serde_round_trip() {
    let mut selector = selector();
    drive(&mut selector, &[(0, healthy(), "a1", true)]);
    let decision = step(&mut selector, 10, [DOWN, up(120), up(150), up(80)]);
    let json = serde_json::to_string(&decision).expect("сериализация");
    let back: Decision = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back, decision);

    let snapshot = selector.snapshot(secs(10));
    let json = serde_json::to_string(&snapshot).expect("сериализация");
    let back: Snapshot = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back, snapshot);

    let settings = Settings {
        pin: pin("main", "NL-1"),
        ..settings()
    };
    let json = serde_json::to_string(&settings).expect("сериализация");
    let back: Settings = serde_json::from_str(&json).expect("разбор");
    assert_eq!(back, settings);
}

#[test]
fn reason_serializes_with_a_kind_tag() {
    let reason = Reason::CurrentDead {
        from: "NL-1".into(),
        to: "DE-2".into(),
        failures: 3,
    };
    let value = serde_json::to_value(&reason).expect("сериализация");
    assert_eq!(value["kind"], "current_dead");
    assert_eq!(value["failures"], 3);
}
