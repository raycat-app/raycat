//! Человекочитаемый вывод команд: статус, таблица узлов, обновления, события.
//! Тексты от провайдера и xray очищаются от управляющих символов: иначе
//! подписка могла бы управлять терминалом пользователя.

use std::time::Duration;

use jiff::tz::TimeZone;
use raycat_proto::{
    CurrentNode, Event, Mode, Node, NodeStatus, Nodes, Status, SubscriptionStatus, Updates,
    XrayState, XrayStatus,
};

use crate::term::{Align, Cell, Column, Term, Tone, pad, table};
use crate::util::{format_bytes, format_date, format_duration, format_moment, sanitize};

const LABEL_WIDTH: usize = 13;
const DAY: u64 = 86_400;
const WARN_BEFORE_EXPIRY: u64 = 3 * DAY;

fn field(term: Term, indent: usize, label: &str, value: &str) -> String {
    format!(
        "{}{}{value}",
        " ".repeat(indent),
        term.paint(Tone::Dim, &pad(label, LABEL_WIDTH))
    )
}

pub(crate) fn span(secs: u64) -> String {
    format_duration(Duration::from_secs(secs))
}

pub(crate) fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    if secs < 10 {
        "только что".to_owned()
    } else {
        format!("{} назад", span(secs))
    }
}

pub(crate) fn ahead(now: u64, then: u64) -> String {
    if then <= now {
        "скоро".to_owned()
    } else {
        format!("через {}", span(then - now))
    }
}

pub(crate) fn traffic_text(used: u64, total: Option<u64>) -> (String, Tone) {
    let used_text = format_bytes(used);
    match total {
        None => (used_text, Tone::Plain),
        Some(0) => (format!("{used_text} (без ограничения)"), Tone::Plain),
        Some(total) => {
            let percent = u128::from(used) * 100 / u128::from(total);
            let tone = if percent >= 100 {
                Tone::Red
            } else if percent >= 90 {
                Tone::Yellow
            } else {
                Tone::Plain
            };
            (
                format!("{used_text} из {} ({percent}%)", format_bytes(total)),
                tone,
            )
        }
    }
}

fn traffic(term: Term, used: u64, total: Option<u64>) -> String {
    let (text, tone) = traffic_text(used, total);
    term.paint(tone, &text)
}

pub(crate) fn expiry_text(expire: u64, now: u64, zone: &TimeZone) -> (String, Tone) {
    if expire == 0 {
        return ("бессрочно".to_owned(), Tone::Plain);
    }
    let date = format_date(expire, zone);
    if expire <= now {
        return (format!("истёк {date}"), Tone::Red);
    }
    let left = expire - now;
    let tone = if left < WARN_BEFORE_EXPIRY {
        Tone::Yellow
    } else {
        Tone::Plain
    };
    (format!("до {date}, осталось {}", span(left)), tone)
}

fn expiry(term: Term, expire: u64, now: u64, zone: &TimeZone) -> String {
    let (text, tone) = expiry_text(expire, now, zone);
    term.paint(tone, &text)
}

pub(crate) fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Proxy => "прокси",
        Mode::Gateway => "шлюз",
    }
}

fn xray_text(term: Term, xray: &XrayStatus) -> String {
    if !xray.running {
        return term.paint(Tone::Red, "не запущен");
    }
    let pid = xray
        .pid
        .map_or_else(String::new, |pid| format!(" (pid {pid})"));
    let restarts = if xray.restarts > 0 {
        term.paint(Tone::Yellow, &format!(", перезапусков: {}", xray.restarts))
    } else {
        String::new()
    };
    format!("{}{pid}{restarts}", term.paint(Tone::Green, "работает"))
}

fn header_lines(term: Term, status: &Status) -> Vec<String> {
    let mut lines = vec![
        term.paint(
            Tone::Header,
            &format!("raycat {}", sanitize(&status.version)),
        ),
        field(term, 2, "режим:", mode_name(status.mode)),
        field(term, 2, "работает:", &span(status.uptime_secs)),
        field(term, 2, "xray:", &xray_text(term, &status.xray)),
    ];
    match status.kill_switch {
        Some(true) => lines.push(field(
            term,
            2,
            "kill switch:",
            &term.paint(Tone::Green, "включён"),
        )),
        Some(false) => lines.push(field(
            term,
            2,
            "kill switch:",
            &term.paint(Tone::Red, "выключен"),
        )),
        None => {}
    }
    lines
}

fn node_lines(term: Term, node: Option<&CurrentNode>) -> Vec<String> {
    let mut lines = vec![term.paint(Tone::Header, "Текущий узел")];
    let Some(node) = node else {
        lines.push(format!(
            "  {}",
            term.paint(Tone::Yellow, "не выбран: узлов пока нет")
        ));
        return lines;
    };
    lines.push(field(
        term,
        2,
        "имя:",
        &term.paint(Tone::Bold, &sanitize(&node.name)),
    ));
    lines.push(field(term, 2, "подписка:", &sanitize(&node.subscription)));
    let latency = node
        .latency_ms
        .map_or_else(|| "нет данных".to_owned(), |ms| format!("{ms} мс"));
    lines.push(field(term, 2, "задержка:", &latency));
    let choice = if node.pinned {
        term.paint(
            Tone::Yellow,
            "закреплён вручную (вернуть автоматику: raycat use auto)",
        )
    } else {
        "автоматический".to_owned()
    };
    lines.push(field(term, 2, "выбор:", &choice));
    if let Some(reason) = &node.reason {
        lines.push(field(term, 2, "причина:", &sanitize(reason)));
    }
    lines
}

fn subscription_lines(
    term: Term,
    sub: &SubscriptionStatus,
    now: u64,
    zone: &TimeZone,
) -> Vec<String> {
    let provider = sub
        .title
        .as_deref()
        .filter(|text| !text.is_empty())
        .map_or_else(String::new, |text| format!("  «{}»", sanitize(text)));
    let title = format!(
        "  {}{provider}",
        term.paint(Tone::Bold, &sanitize(&sub.name))
    );
    let mut lines = vec![title, field(term, 4, "узлов:", &sub.nodes.to_string())];
    if let Some(used) = sub.used_bytes {
        lines.push(field(
            term,
            4,
            "трафик:",
            &traffic(term, used, sub.total_bytes),
        ));
    }
    if let Some(expire) = sub.expire {
        lines.push(field(term, 4, "срок:", &expiry(term, expire, now, zone)));
    }
    let updated = match sub.updated_at {
        Some(then) => format!(
            "{}, следующее обновление {}",
            ago(now, then),
            sub.next_update
                .map_or_else(|| "не запланировано".to_owned(), |next| ahead(now, next))
        ),
        None => term.paint(Tone::Yellow, "ещё не получена"),
    };
    lines.push(field(term, 4, "обновлена:", &updated));
    if sub.updating {
        lines.push(field(
            term,
            4,
            "сейчас:",
            &term.paint(Tone::Green, "идёт обновление"),
        ));
    }
    if let Some(error) = &sub.last_error {
        lines.push(field(
            term,
            4,
            "ошибка:",
            &term.paint(Tone::Red, &sanitize(error)),
        ));
    }
    lines
}

/// `raycat status`.
pub(crate) fn status(term: Term, status: &Status, now: u64, zone: &TimeZone) -> String {
    let mut lines = header_lines(term, status);
    lines.push(String::new());
    lines.extend(node_lines(term, status.node.as_ref()));
    lines.push(String::new());
    lines.push(term.paint(Tone::Header, "Подписки"));
    if status.subscriptions.is_empty() {
        lines.push("  подписок нет".to_owned());
    }
    for (index, sub) in status.subscriptions.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(subscription_lines(term, sub, now, zone));
    }
    lines.join("\n")
}

fn node_columns() -> Vec<Column> {
    vec![
        Column {
            title: "",
            align: Align::Left,
            shrink: 0,
            min: 0,
        },
        Column {
            title: "Подписка",
            align: Align::Left,
            shrink: 1,
            min: 8,
        },
        Column {
            title: "Узел",
            align: Align::Left,
            shrink: 2,
            min: 10,
        },
        Column {
            title: "Статус",
            align: Align::Left,
            shrink: 0,
            min: 0,
        },
        Column {
            title: "Задержка",
            align: Align::Right,
            shrink: 0,
            min: 0,
        },
        Column {
            title: "Провалы",
            align: Align::Right,
            shrink: 0,
            min: 0,
        },
        Column {
            title: "Трафик",
            align: Align::Left,
            shrink: 0,
            min: 0,
        },
    ]
}

/// Маркер строки узла: закреплён, выбран или пусто.
pub(crate) fn node_marker(node: &Node) -> (&'static str, Tone) {
    if node.pinned {
        ("★", Tone::Yellow)
    } else if node.selected {
        ("▶", Tone::Green)
    } else {
        ("", Tone::Plain)
    }
}

pub(crate) fn node_status(status: NodeStatus) -> (&'static str, Tone) {
    match status {
        NodeStatus::Alive => ("жив", Tone::Green),
        NodeStatus::Dead => ("не отвечает", Tone::Red),
        NodeStatus::Unknown => ("не проверен", Tone::Dim),
    }
}

pub(crate) fn latency_text(latency_ms: Option<u64>) -> String {
    latency_ms.map_or_else(|| "—".to_owned(), |ms| format!("{ms} мс"))
}

pub(crate) fn node_traffic(node: &Node) -> String {
    match (node.uplink_bytes, node.downlink_bytes) {
        (Some(up), Some(down)) => format!("↑{} ↓{}", format_bytes(up), format_bytes(down)),
        _ => "—".to_owned(),
    }
}

fn node_row(node: &Node) -> Vec<Cell> {
    let (mark, mark_tone) = node_marker(node);
    let name_tone = if node.selected {
        Tone::Bold
    } else {
        Tone::Plain
    };
    let (status_text, status_tone) = node_status(node.status);
    let failures_tone = if node.failures > 0 {
        Tone::Yellow
    } else {
        Tone::Dim
    };
    vec![
        Cell::new(mark, mark_tone),
        Cell::new(sanitize(&node.subscription), Tone::Plain),
        Cell::new(sanitize(&node.name), name_tone),
        Cell::new(status_text, status_tone),
        Cell::new(latency_text(node.latency_ms), Tone::Plain),
        Cell::new(node.failures.to_string(), failures_tone),
        Cell::new(node_traffic(node), Tone::Plain),
    ]
}

/// `raycat nodes`: по умолчанию живые узлы и выбранный, с `all` — все.
pub(crate) fn nodes(term: Term, nodes: &Nodes, all: bool) -> String {
    if nodes.nodes.is_empty() {
        return "Узлов нет: подписки ещё не получены или в них нет узлов (см. raycat status)"
            .to_owned();
    }
    let shown: Vec<&Node> = nodes
        .nodes
        .iter()
        .filter(|node| all || node.status == NodeStatus::Alive || node.selected)
        .collect();
    if shown.is_empty() {
        return format!(
            "Живых узлов нет (всего: {}). Показать все: raycat nodes --all",
            nodes.nodes.len()
        );
    }
    let rows: Vec<Vec<Cell>> = shown.iter().copied().map(node_row).collect();
    let mut text = table(term, &node_columns(), &rows);
    text.push('\n');
    text.push_str(&term.paint(Tone::Dim, "▶ выбран   ★ закреплён вручную"));
    let hidden = nodes.nodes.len() - shown.len();
    if hidden > 0 {
        text.push('\n');
        text.push_str(&term.paint(
            Tone::Dim,
            &format!("Скрыто узлов: {hidden} (все: raycat nodes --all)"),
        ));
    }
    text
}

/// Сколько подписок не обновилось.
pub(crate) fn failed(updates: &Updates) -> usize {
    updates.results.iter().filter(|result| !result.ok).count()
}

/// Итог `raycat update`: по строке на подписку.
pub(crate) fn updates(term: Term, updates: &Updates) -> String {
    if updates.results.is_empty() {
        return "Подписок для обновления нет".to_owned();
    }
    updates
        .results
        .iter()
        .map(|result| {
            let (mark, tone) = if result.ok {
                ("✓", Tone::Green)
            } else {
                ("✗", Tone::Red)
            };
            format!(
                "{} {}: {}",
                term.paint(tone, mark),
                sanitize(&result.subscription),
                sanitize(&result.message)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn node_or_dash(node: Option<&str>) -> String {
    node.map_or_else(|| "—".to_owned(), sanitize)
}

/// Текст события без времени и его тон; строки от провайдера очищены.
pub(crate) fn event_text(event: &Event) -> (Tone, String) {
    match event {
        Event::Hello { version } => (
            Tone::Dim,
            format!("подключено к демону, версия {}", sanitize(version)),
        ),
        Event::NodeChanged { from, to, reason } => (
            Tone::Plain,
            format!(
                "узел: {} → {}: {}",
                node_or_dash(from.as_deref()),
                node_or_dash(to.as_deref()),
                sanitize(reason)
            ),
        ),
        Event::Pin { node: Some(node) } => {
            (Tone::Yellow, format!("закреплён узел {}", sanitize(node)))
        }
        Event::Pin { node: None } => (Tone::Plain, "закрепление снято".to_owned()),
        Event::SubscriptionUpdated {
            subscription,
            nodes,
        } => (
            Tone::Green,
            format!(
                "подписка «{}» обновлена, узлов: {nodes}",
                sanitize(subscription)
            ),
        ),
        Event::SubscriptionFailed {
            subscription,
            error,
        } => (
            Tone::Red,
            format!("подписка «{}»: {}", sanitize(subscription), sanitize(error)),
        ),
        Event::Xray { state, message } => {
            let (tone, name) = match state {
                XrayState::Started => (Tone::Green, "xray запущен"),
                XrayState::Exited => (Tone::Red, "xray завершился"),
            };
            (tone, format!("{name}: {}", sanitize(message)))
        }
        Event::Warning { level, message } => {
            let tone = if level.eq_ignore_ascii_case("error") {
                Tone::Red
            } else {
                Tone::Yellow
            };
            (
                tone,
                format!("{}: {}", sanitize(&level.to_uppercase()), sanitize(message)),
            )
        }
    }
}

/// Строка `raycat events`: время получения и описание.
pub(crate) fn event_line(
    term: Term,
    time: u64,
    now: u64,
    zone: &TimeZone,
    event: &Event,
) -> String {
    let (tone, text) = event_text(event);
    format!(
        "{}  {}",
        term.paint(Tone::Dim, &format_moment(time, now, zone)),
        term.paint(tone, &text)
    )
}

#[cfg(test)]
mod tests {
    use raycat_proto::UpdateResult;

    use super::*;

    const NOW: u64 = 1_790_596_800;

    fn plain() -> Term {
        Term::new(false, None)
    }

    fn subscription() -> SubscriptionStatus {
        SubscriptionStatus {
            name: "main".to_owned(),
            url: "https://sub.example.com/…1234".to_owned(),
            title: Some("Мой VPN".to_owned()),
            used_bytes: Some(3 * 1024 * 1024),
            total_bytes: Some(100 * 1024 * 1024 * 1024),
            expire: Some(NOW + 40 * DAY),
            nodes: 12,
            updated_at: Some(NOW - 2 * 3_600),
            next_update: Some(NOW + 3 * 3_600),
            last_error: None,
            updating: false,
        }
    }

    fn sample_status() -> Status {
        Status {
            version: "0.1.0".to_owned(),
            mode: Mode::Gateway,
            uptime_secs: 312,
            kill_switch: Some(true),
            xray: XrayStatus {
                running: true,
                pid: Some(4127),
                restarts: 0,
            },
            node: Some(CurrentNode {
                id: "main/NL-1".to_owned(),
                subscription: "main".to_owned(),
                name: "NL-1".to_owned(),
                latency_ms: Some(31),
                pinned: false,
                reason: Some("выбран лучший живой узел «NL-1»".to_owned()),
            }),
            subscriptions: vec![subscription()],
        }
    }

    fn node(name: &str, status: NodeStatus, selected: bool) -> Node {
        Node {
            id: format!("main/{name}"),
            subscription: "main".to_owned(),
            name: name.to_owned(),
            tag: "node-001-main".to_owned(),
            status,
            latency_ms: (status == NodeStatus::Alive).then_some(31),
            failures: u32::from(status == NodeStatus::Dead) * 3,
            alive_for_secs: None,
            last_error: None,
            selected,
            pinned: false,
            uplink_bytes: Some(10 * 1024),
            downlink_bytes: Some(200 * 1024),
        }
    }

    fn sample_nodes() -> Nodes {
        Nodes {
            selected: Some("main/NL-1".to_owned()),
            nodes: vec![
                node("NL-1", NodeStatus::Alive, true),
                node("DE-2", NodeStatus::Alive, false),
                node("US-3", NodeStatus::Dead, false),
                node("FI-4", NodeStatus::Unknown, false),
            ],
        }
    }

    #[test]
    fn status_tells_the_whole_story() {
        let text = status(plain(), &sample_status(), NOW, &TimeZone::UTC);
        let expected = [
            "raycat 0.1.0",
            "  режим:       шлюз",
            "  работает:    5 мин 12 с",
            "  xray:        работает (pid 4127)",
            "  kill switch: включён",
            "",
            "Текущий узел",
            "  имя:         NL-1",
            "  подписка:    main",
            "  задержка:    31 мс",
            "  выбор:       автоматический",
            "  причина:     выбран лучший живой узел «NL-1»",
            "",
            "Подписки",
            "  main  «Мой VPN»",
            "    узлов:       12",
            "    трафик:      3.0 МиБ из 100.0 ГиБ (0%)",
            "    срок:        до 07.11.2026, осталось 40 дн",
            "    обновлена:   2 ч назад, следующее обновление через 3 ч",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn status_without_a_node_or_kill_switch() {
        let mut data = sample_status();
        data.mode = Mode::Proxy;
        data.kill_switch = None;
        data.node = None;
        data.subscriptions.clear();
        data.xray = XrayStatus {
            running: false,
            pid: None,
            restarts: 2,
        };
        let text = status(plain(), &data, NOW, &TimeZone::UTC);
        assert!(text.contains("режим:       прокси"));
        assert!(!text.contains("kill switch"));
        assert!(text.contains("xray:        не запущен"));
        assert!(text.contains("не выбран: узлов пока нет"));
        assert!(text.contains("подписок нет"));
    }

    #[test]
    fn status_shows_pin_restarts_and_subscription_trouble() {
        let mut data = sample_status();
        data.xray.restarts = 2;
        if let Some(node) = &mut data.node {
            node.pinned = true;
        }
        let mut sub = subscription();
        sub.last_error = Some("панель ответила 403".to_owned());
        sub.updating = true;
        sub.title = None;
        sub.expire = Some(0);
        sub.total_bytes = Some(0);
        sub.updated_at = None;
        data.subscriptions = vec![sub];
        let text = status(plain(), &data, NOW, &TimeZone::UTC);
        assert!(text.contains("работает (pid 4127), перезапусков: 2"));
        assert!(text.contains("закреплён вручную (вернуть автоматику: raycat use auto)"));
        assert!(text.contains("ошибка:      панель ответила 403"));
        assert!(text.contains("идёт обновление"));
        assert!(text.contains("срок:        бессрочно"));
        assert!(text.contains("3.0 МиБ (без ограничения)"));
        assert!(text.contains("обновлена:   ещё не получена"));
    }

    #[test]
    fn expiry_and_traffic_warn_before_they_hurt() {
        let on = Term::new(true, None);
        let utc = &TimeZone::UTC;
        assert!(expiry(on, NOW + 2 * 3_600, NOW, utc).contains("\x1b[93m"));
        assert!(expiry(on, NOW + 2 * 3_600, NOW, utc).contains("осталось 2 ч"));
        assert!(expiry(on, NOW + 30 * DAY, NOW, utc).starts_with("до "));
        assert!(expiry(on, NOW - DAY, NOW, utc).contains("\x1b[91mистёк"));
        assert_eq!(expiry(plain(), 0, NOW, utc), "бессрочно");
        assert!(traffic(on, 95, Some(100)).contains("\x1b[93m"));
        assert!(traffic(on, 100, Some(100)).contains("\x1b[91m"));
        assert!(!traffic(on, 10, Some(100)).contains('\x1b'));
        assert_eq!(traffic(plain(), 0, None), "0 Б");
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(ago(NOW, NOW - 3), "только что");
        assert_eq!(ago(NOW, NOW - 330), "5 мин 30 с назад");
        assert_eq!(ago(NOW, NOW - 5 * DAY), "5 дн назад");
        assert_eq!(ago(NOW, NOW - 5 * DAY - 3 * 3_600), "5 дн 3 ч назад");
        assert_eq!(ahead(NOW, NOW + 7_200), "через 2 ч");
        assert_eq!(ahead(NOW, NOW - 1), "скоро");
    }

    #[test]
    fn status_has_no_escape_codes_without_color() {
        let utc = &TimeZone::UTC;
        assert!(!status(plain(), &sample_status(), NOW, utc).contains('\x1b'));
        assert!(status(Term::new(true, None), &sample_status(), NOW, utc).contains('\x1b'));
    }

    #[test]
    fn provider_text_cannot_steer_the_terminal() {
        let mut data = sample_status();
        data.subscriptions[0].title = Some("\x1b]0;взлом\x07Мой".to_owned());
        data.subscriptions[0].last_error = Some("a\x1b[2Jb".to_owned());
        if let Some(node) = &mut data.node {
            node.name = "NL\x1b[31m".to_owned();
        }
        let text = status(plain(), &data, NOW, &TimeZone::UTC);
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\x07'));
    }

    #[test]
    fn nodes_list_the_alive_ones_and_the_selected_by_default() {
        let mut data = sample_nodes();
        data.nodes[0].pinned = true;
        let text = nodes(plain(), &data, false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "   Подписка  Узел  Статус  Задержка  Провалы  Трафик"
        );
        assert_eq!(
            lines[1],
            "★  main      NL-1  жив        31 мс        0  ↑10.0 КиБ ↓200.0 КиБ"
        );
        assert_eq!(
            lines[2],
            "   main      DE-2  жив        31 мс        0  ↑10.0 КиБ ↓200.0 КиБ"
        );
        assert_eq!(lines[3], "▶ выбран   ★ закреплён вручную");
        assert_eq!(lines[4], "Скрыто узлов: 2 (все: raycat nodes --all)");
        assert!(!text.contains("US-3"));
    }

    #[test]
    fn all_shows_dead_and_unchecked_nodes_too() {
        let text = nodes(plain(), &sample_nodes(), true);
        let dead = format!("US-3  не отвечает{}—{}3", " ".repeat(9), " ".repeat(8));
        assert!(text.contains(&dead), "{text}");
        assert!(text.contains("FI-4  не проверен"));
        assert!(!text.contains("Скрыто"));
        let selected = text.lines().nth(1).unwrap();
        assert!(selected.starts_with("▶  main"), "{selected:?}");
    }

    #[test]
    fn a_selected_dead_node_is_still_listed() {
        let mut data = sample_nodes();
        data.nodes[0].status = NodeStatus::Dead;
        data.nodes[1].status = NodeStatus::Dead;
        let text = nodes(plain(), &data, false);
        assert!(text.contains("NL-1"));
        assert!(!text.contains("DE-2"));
    }

    #[test]
    fn nodes_explain_an_empty_list() {
        let empty = Nodes {
            selected: None,
            nodes: Vec::new(),
        };
        assert!(nodes(plain(), &empty, false).contains("Узлов нет"));
        let mut dead = sample_nodes();
        for node in &mut dead.nodes {
            node.status = NodeStatus::Dead;
            node.selected = false;
        }
        let text = nodes(plain(), &dead, false);
        assert!(text.contains("Живых узлов нет (всего: 4)"));
        assert!(text.contains("--all"));
    }

    #[test]
    fn a_narrow_terminal_cuts_names_with_an_ellipsis() {
        let mut data = sample_nodes();
        data.nodes[0].name = "Германия, Франкфурт, очень длинное имя узла".to_owned();
        let text = nodes(Term::new(false, Some(80)), &data, false);
        for line in text.lines().take(3) {
            assert!(crate::term::display_width(line) <= 80, "{line:?}");
        }
        assert!(text.contains("Германия, Франкфу…"), "{text}");
    }

    #[test]
    fn updates_show_a_mark_per_subscription() {
        let data = Updates {
            results: vec![
                UpdateResult {
                    subscription: "main".to_owned(),
                    ok: true,
                    message: "узлов: 12".to_owned(),
                    nodes: Some(12),
                },
                UpdateResult {
                    subscription: "backup".to_owned(),
                    ok: false,
                    message: "панель ответила 403".to_owned(),
                    nodes: None,
                },
            ],
        };
        assert_eq!(
            updates(plain(), &data),
            "✓ main: узлов: 12\n✗ backup: панель ответила 403"
        );
        assert_eq!(failed(&data), 1);
        assert_eq!(
            updates(
                plain(),
                &Updates {
                    results: Vec::new()
                }
            ),
            "Подписок для обновления нет"
        );
    }

    #[test]
    fn every_event_has_a_line_with_the_time() {
        let at = NOW;
        let line = |event: &Event| event_line(plain(), at, at, &TimeZone::UTC, event);
        assert_eq!(
            line(&Event::Hello {
                version: "0.1.0".to_owned()
            }),
            "12:00:00  подключено к демону, версия 0.1.0"
        );
        assert_eq!(
            line(&Event::NodeChanged {
                from: Some("main/NL-1".to_owned()),
                to: None,
                reason: "сбой".to_owned()
            }),
            "12:00:00  узел: main/NL-1 → —: сбой"
        );
        assert!(line(&Event::Pin { node: None }).ends_with("закрепление снято"));
        assert!(
            line(&Event::Pin {
                node: Some("main/NL-1".to_owned())
            })
            .ends_with("закреплён узел main/NL-1")
        );
        assert!(
            line(&Event::SubscriptionUpdated {
                subscription: "main".to_owned(),
                nodes: 3
            })
            .ends_with("подписка «main» обновлена, узлов: 3")
        );
        assert!(
            line(&Event::SubscriptionFailed {
                subscription: "main".to_owned(),
                error: "403".to_owned()
            })
            .ends_with("подписка «main»: 403")
        );
        assert!(
            line(&Event::Xray {
                state: XrayState::Exited,
                message: "код 1".to_owned()
            })
            .ends_with("xray завершился: код 1")
        );
        assert!(
            line(&Event::Warning {
                level: "warn".to_owned(),
                message: "медленно".to_owned()
            })
            .ends_with("WARN: медленно")
        );
    }

    #[test]
    fn event_colors_follow_severity() {
        let on = Term::new(true, None);
        let failed = event_line(
            on,
            NOW,
            NOW,
            &TimeZone::UTC,
            &Event::SubscriptionFailed {
                subscription: "a".to_owned(),
                error: "x".to_owned(),
            },
        );
        assert!(failed.contains("\x1b[91m"));
        let error = event_line(
            on,
            NOW,
            NOW,
            &TimeZone::UTC,
            &Event::Warning {
                level: "error".to_owned(),
                message: "x".to_owned(),
            },
        );
        assert!(error.contains("\x1b[91m"));
    }

    #[test]
    fn event_text_is_sanitized() {
        let line = event_line(
            plain(),
            NOW,
            NOW,
            &TimeZone::UTC,
            &Event::Warning {
                level: "warn".to_owned(),
                message: "a\x1b[2Jb".to_owned(),
            },
        );
        assert!(!line.contains('\x1b'));
    }
}
