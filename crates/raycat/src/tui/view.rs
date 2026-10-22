//! Каркас экрана: строка вкладок, шапка с состоянием и текущим узлом, баннер связи,
//! строка уведомлений и подсказок. Содержимое вкладок — в `pages`.

use jiff::tz::TimeZone;
use ratatui::Frame;
use raycat_proto::{SubscriptionStatus, XrayStatus};

use super::app::{App, Hit, InputMode, Link, Tab, Target};
use super::canvas::{Canvas, GAP, Palette, Seg, fit, highlight, wrap};
use super::pages;
use crate::names::{NameStyle, country_code, display_name};
use crate::render;
use crate::term::{Tone, display_width, pad};
use crate::util::{format_bytes, local_zone};

const MIN_WIDTH: usize = 44;
const MIN_HEIGHT: usize = 12;
const BOTTOM: usize = 2;
const BANNER_LINES: usize = 3;
const BANNER_HINT_LINES: usize = 2;
const BAR_CELLS: usize = 10;
/// Скорость ниже этого значения бит/с показывается как «—».
const MIN_SPEED: u64 = 1_000;
pub(super) const HEADER_LINES: usize = 2;
/// Строки, которые всегда остаются телу вкладки.
pub(super) const BODY_MIN: usize = 3;

const HELP: [(&str, &str); 15] = [
    ("1–5  Tab", "вкладки; Shift+Tab и ← → — перелистывание"),
    ("↑ ↓  j k", "выбор строки, прокрутка"),
    ("PgUp PgDn", "страница вверх и вниз"),
    ("Home End", "в начало и в конец (g G)"),
    ("Enter", "подробности узла или подписки; на «Настройках» — переключить"),
    ("p", "закрепить выбранный узел (на вкладке «Узлы» — и двойной клик)"),
    ("a", "вернуть автоматический выбор узла"),
    ("u", "обновить все подписки (на «Подписках» — выбранную)"),
    ("d", "скрыть мёртвые узлы (вкладка «Узлы»)"),
    ("s", "фильтр по подписке (вкладка «Узлы»)"),
    ("/", "поиск по узлам (Enter — применить, Esc — сбросить)"),
    ("w", "в журнале — только предупреждения и ошибки"),
    ("Esc", "закрыть подробности и сбросить фильтры; на верхнем уровне — выход"),
    ("?", "эта справка"),
    ("q  Ctrl+C", "выйти"),
];

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App, palette: Palette) {
    draw_at(frame, app, palette, local_zone());
}

pub(super) fn draw_at(frame: &mut Frame<'_>, app: &mut App, palette: Palette, zone: &TimeZone) {
    app.hits.clear();
    let mut canvas = Canvas::new(frame.buffer_mut(), palette);
    compose(&mut canvas, app, zone);
}

fn compose(canvas: &mut Canvas<'_>, app: &mut App, zone: &TimeZone) {
    if canvas.width < MIN_WIDTH || canvas.height < MIN_HEIGHT {
        let need = format!("{MIN_WIDTH}×{MIN_HEIGHT}");
        let full = format!(
            "Мало места: {}×{}, нужно {need}",
            canvas.width, canvas.height
        );
        let text = if display_width(&full) <= canvas.width {
            full
        } else {
            need
        };
        canvas.line(0, 0, vec![Seg::new(text, Tone::Yellow, 0)]);
        return;
    }
    if app.input == InputMode::Help {
        help(canvas);
        return;
    }
    tabs(canvas, app);
    let room = canvas.height.saturating_sub(BOTTOM + BODY_MIN);
    let mut y = 1;
    y += banner(canvas, app, y, room.saturating_sub(y));
    canvas.set_dim(matches!(app.link, Link::Down { .. }));
    y += header(canvas, app, y, room.saturating_sub(y));

    let end = canvas.height.saturating_sub(BOTTOM);
    if y < end {
        match app.tab {
            Tab::Overview => pages::overview(canvas, app, y, end, zone),
            Tab::Nodes => pages::nodes(canvas, app, y, end),
            Tab::Subscriptions => pages::subscriptions(canvas, app, y, end, zone),
            Tab::Journal => pages::journal(canvas, app, y, end, zone),
            Tab::Settings => pages::settings(canvas, app, y, end),
        }
    }
    canvas.set_dim(false);
    bottom(canvas, app);
}

/// Вкладки в первой строке; каждая — область нажатия.
fn tabs(canvas: &mut Canvas<'_>, app: &mut App) {
    let mut x = 0;
    for (index, tab) in Tab::ALL.iter().enumerate() {
        let label = format!(" {} {} ", index + 1, tab.title());
        let width = display_width(&label);
        if *tab == app.tab {
            canvas.put_style(x, 0, &label, width, highlight());
        } else {
            canvas.put(x, 0, &label, width, Tone::Plain);
        }
        app.hits.push(Hit::new(x, 0, width, Target::Tab(*tab)));
        x += width + 1;
    }
}

fn banner(canvas: &mut Canvas<'_>, app: &App, y0: usize, limit: usize) -> usize {
    let lines: Vec<Vec<Seg>> = match &app.link {
        Link::Up | Link::Connecting => Vec::new(),
        Link::Down {
            reason,
            hint,
            retry_at,
        } => {
            let text = format!("демон недоступен: {reason}");
            let mut lines: Vec<Vec<Seg>> = wrap(&text, canvas.width, BANNER_LINES)
                .into_iter()
                .map(|line| vec![Seg::new(line, Tone::Red, 0)])
                .collect();
            if let Some(hint) = hint {
                let hint_lines = wrap(hint, canvas.width, BANNER_HINT_LINES);
                lines.extend(
                    hint_lines
                        .into_iter()
                        .map(|line| vec![Seg::new(line, Tone::Plain, 0)]),
                );
            }
            let wait = retry_at.saturating_sub(app.now);
            let retry = if wait == 0 {
                "повторное подключение…".to_owned()
            } else {
                format!("повторное подключение через {wait} с")
            };
            lines.push(vec![Seg::new(retry, Tone::Dim, 0)]);
            lines
        }
    };
    let count = lines.len().min(limit);
    for (row, segs) in lines.into_iter().take(count).enumerate() {
        canvas.line(y0 + row, 0, segs);
    }
    count
}

fn header(canvas: &mut Canvas<'_>, app: &App, y: usize, limit: usize) -> usize {
    let count = limit.min(HEADER_LINES);
    if count > 0 {
        header_top(canvas, app, y);
    }
    if count > 1 {
        header_node(canvas, app, y + 1);
    }
    count
}

fn link_seg(link: &Link) -> Seg {
    match link {
        Link::Up => Seg::new("● подключено", Tone::Green, 0),
        Link::Connecting => Seg::new("● подключение…", Tone::Yellow, 0),
        Link::Down { .. } => Seg::new("● нет связи", Tone::Red, 0),
    }
}

/// Xray показывается только когда с ним что-то не так.
pub(super) fn xray_segs(xray: &XrayStatus) -> Vec<Seg> {
    let mut segs = Vec::new();
    if !xray.running {
        segs.push(Seg::new("xray не запущен", Tone::Red, 0));
    } else if xray.api_stuck {
        let text = format!(
            "xray не отвечает ({} с)",
            xray.api_silent_secs.unwrap_or_default()
        );
        segs.push(Seg::new(text, Tone::Red, 0));
    }
    if xray.restarts > 0 {
        let text = format!("xray перезапускался: {}", xray.restarts);
        segs.push(Seg::new(text, Tone::Yellow, 1));
    }
    segs
}

fn header_top(canvas: &mut Canvas<'_>, app: &App, y: usize) {
    let mut segs = vec![Seg::new("raycat", Tone::Bold, 0)];
    if let Some(status) = &app.status {
        segs.push(Seg::new(render::mode_name(status.mode), Tone::Plain, 0));
    }
    segs.push(link_seg(&app.link));
    if let Some(status) = &app.status {
        segs.extend(xray_segs(&status.xray));
        if let Some(on) = status.kill_switch {
            let tone = if on { Tone::Green } else { Tone::Dim };
            segs.push(Seg::new("kill switch ●", tone, 0));
        }
    }
    let end = canvas.chain(y, 0, " · ", segs, canvas.width);
    let Some(status) = &app.status else {
        return;
    };
    let uptime = status.uptime_secs + app.now.saturating_sub(app.status_at);
    let text = format!("время работы {}", render::span(uptime));
    let width = display_width(&text);
    if end + GAP + width <= canvas.width {
        canvas.put(canvas.width - width, y, &text, width, Tone::Dim);
    }
}

/// Текущий узел: имя, задержка, скорость и выбор. Код страны выводится отдельно только
/// в режиме «как есть»: в режиме кода он уже стоит вместо флага.
fn header_node(canvas: &mut Canvas<'_>, app: &App, y: usize) {
    let Some(status) = &app.status else {
        canvas.line(
            y,
            0,
            vec![Seg::new("данных от демона ещё нет", Tone::Dim, 0)],
        );
        return;
    };
    let Some(node) = &status.node else {
        canvas.line(
            y,
            0,
            vec![Seg::new("узел: не выбран, узлов пока нет", Tone::Yellow, 0)],
        );
        return;
    };
    let mut segs = vec![Seg::new(
        format!("▶ {}", display_name(&node.name, app.names)),
        Tone::Bold,
        0,
    )];
    if app.names == NameStyle::AsIs
        && let Some(code) = country_code(&node.name)
    {
        segs.push(Seg::new(code, Tone::Plain, 1));
    }
    segs.push(Seg::new(
        render::latency_text(node.latency_ms),
        delay_tone(node.latency_ms),
        0,
    ));
    if let Some(speed) = app.speed() {
        segs.push(Seg::new(speed_label(speed.down, speed.up), Tone::Plain, 1));
    }
    let choice = if node.pinned { "закреплён" } else { "авто" };
    segs.push(Seg::new(choice, Tone::Dim, 0));
    if let Some(reason) = &node.reason {
        segs.push(Seg::new(reason.clone(), Tone::Dim, 0));
    }
    canvas.line(y, 0, segs);
}

/// Без данных задержка тусклая, иначе цвет по порогам из `render`.
pub(super) fn delay_tone(latency: Option<u64>) -> Tone {
    if latency.is_none() {
        Tone::Dim
    } else {
        render::latency_tone(latency)
    }
}

/// Приём и отдача; в каждую сторону ниже 1 Кбит/с показывается «—».
pub(super) fn speed_label(down: u64, up: u64) -> String {
    format!("↓ {} ↑ {}", speed_part(down), speed_part(up))
}

fn speed_part(bits: u64) -> String {
    if bits < MIN_SPEED {
        "—".to_owned()
    } else {
        render::speed_text(bits)
    }
}

/// Полоска из десяти ячеек: заполненная часть отвечает проценту.
fn bar(percent: u128) -> String {
    let filled = usize::try_from(percent / 10).map_or(BAR_CELLS, |cells| cells.min(BAR_CELLS));
    format!("{}{}", "▰".repeat(filled), "▱".repeat(BAR_CELLS - filled))
}

/// Трафик подписки: полоска, процент и объёмы. Без лимита остаётся только объём.
pub(super) fn traffic_seg(used: u64, total: Option<u64>) -> Seg {
    let Some(total) = total.filter(|total| *total > 0) else {
        let (text, tone) = render::traffic_text(used, total);
        return Seg::new(text, tone, 1);
    };
    let percent = u128::from(used) * 100 / u128::from(total);
    let tone = if percent >= 95 {
        Tone::Red
    } else if percent >= 80 {
        Tone::Yellow
    } else {
        Tone::Green
    };
    let text = format!(
        "{} {percent}% {}/{}",
        bar(percent),
        format_bytes(used),
        format_bytes(total)
    );
    Seg::new(text, tone, 1)
}

/// Состояние подписки одним словом; ошибка уходит в отдельную вкладку.
pub(super) fn sub_phrase(sub: &SubscriptionStatus, now: u64) -> (&'static str, Tone) {
    if sub.last_error.is_some() {
        return ("ошибка", Tone::Red);
    }
    if sub.expire.is_some_and(|expire| expire != 0 && expire <= now) {
        return ("истекла", Tone::Red);
    }
    if let (Some(used), Some(total)) = (sub.used_bytes, sub.total_bytes)
        && total > 0
        && used >= total
    {
        return ("трафик исчерпан", Tone::Red);
    }
    if sub.updating {
        return ("обновляется", Tone::Green);
    }
    if sub.updated_at.is_none() {
        return ("ещё не получена", Tone::Yellow);
    }
    ("работает", Tone::Green)
}

/// Подсказки «клавиша описание»: клавиша жирным, описание тусклым.
fn normal_hints(app: &App) -> Vec<Seg> {
    let items: &[(&str, u8)] = match app.tab {
        Tab::Overview => &[
            ("u обновить", 1),
            ("a автовыбор", 3),
            ("2 узлы", 2),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Nodes if app.details => &[
            ("Esc назад", 0),
            ("p закрепить", 1),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Nodes => &[
            ("↑↓ выбор", 1),
            ("Enter подробности", 2),
            ("p закрепить", 1),
            ("d мёртвые", 3),
            ("s подписка", 4),
            ("/ поиск", 3),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Subscriptions => &[
            ("↑↓ выбор", 1),
            ("Enter подробности", 2),
            ("u обновить", 1),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Journal => &[
            ("↑↓ листать", 1),
            ("w предупреждения", 2),
            ("End к концу", 3),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Settings => &[("Enter переключить", 1), ("? справка", 0), ("q выход", 0)],
    };
    items
        .iter()
        .map(|(text, priority)| Seg::new(*text, Tone::Dim, *priority))
        .collect()
}

fn keys_line(canvas: &mut Canvas<'_>, y: usize, hints: Vec<Seg>) {
    let kept = fit(hints, canvas.width, GAP);
    let mut x = 0;
    for (index, seg) in kept.iter().enumerate() {
        if index > 0 {
            x += GAP;
        }
        let (key, desc) = seg.text.split_once(' ').unwrap_or((seg.text.as_str(), ""));
        let room = canvas.width.saturating_sub(x);
        canvas.put(x, y, key, room, Tone::Bold);
        x += display_width(key);
        if !desc.is_empty() {
            let room = canvas.width.saturating_sub(x + 1);
            canvas.put(x + 1, y, desc, room, Tone::Dim);
            x += 1 + display_width(desc);
        }
    }
}

fn filter_hint(app: &App) -> Vec<Seg> {
    vec![
        Seg::new(format!("/{}█", app.filter.text), Tone::Bold, 0),
        Seg::new("Enter — применить, Esc — сбросить и выйти", Tone::Dim, 1),
    ]
}

fn bottom(canvas: &mut Canvas<'_>, app: &App) {
    let status_y = canvas.height.saturating_sub(BOTTOM);
    let hints_y = canvas.height.saturating_sub(1);
    if let Some(run) = &app.update {
        let target = run.target.as_deref().map_or_else(
            || "всех подписок".to_owned(),
            |name| format!("подписки «{name}»"),
        );
        let text = format!(
            "⟳ Обновление {target}… {} с",
            app.now.saturating_sub(run.since)
        );
        canvas.line(status_y, 0, vec![Seg::new(text, Tone::Green, 0)]);
    } else if let Some(notice) = &app.notice {
        let segs = vec![Seg::new(notice.text.clone(), notice.tone, 0)];
        canvas.line(status_y, 0, segs);
    } else if matches!(app.link, Link::Up) {
        canvas.line(status_y, 0, vec![Seg::new("готов", Tone::Dim, 0)]);
    }
    if app.input == InputMode::Filter {
        canvas.line(hints_y, 0, filter_hint(app));
    } else {
        keys_line(canvas, hints_y, normal_hints(app));
    }
}

fn help(canvas: &mut Canvas<'_>) {
    let title = vec![Seg::new("raycat tui: клавиши", Tone::Header, 0)];
    canvas.line(0, 0, title);
    for (row, (keys, text)) in HELP.iter().enumerate() {
        let segs = vec![
            Seg::new(pad(keys, 12), Tone::Bold, 0),
            Seg::new(*text, Tone::Plain, 0),
        ];
        canvas.line(2 + row, 0, segs);
    }
    let legend = vec![Seg::new("▶ выбран   ★ закреплён вручную", Tone::Dim, 0)];
    canvas.line(3 + HELP.len(), 0, legend);
    let close = vec![Seg::new("Любая клавиша закрывает справку", Tone::Dim, 0)];
    canvas.line(5 + HELP.len(), 0, close);
}

/// Подписи подписок в таблице и подсказках; нужны и вкладкам.
pub(super) fn subscription_title(count: usize) -> Vec<Seg> {
    vec![
        Seg::new("Подписки", Tone::Bold, 0),
        Seg::new(format!("всего {count}"), Tone::Dim, 2),
    ]
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::{Color, Modifier};
    use raycat_proto::{CurrentNode, Event, Mode, Node, NodeStatus, Nodes, Status};

    use super::super::app::{Msg, Snapshot, Tab};
    use super::*;

    const NOW: u64 = 1_790_596_800;
    const DAY: u64 = 86_400;
    const GIB: u64 = 1024 * 1024 * 1024;

    fn node(name: &str, status: NodeStatus) -> Node {
        Node {
            id: format!("main/{name}"),
            subscription: "main".to_owned(),
            name: name.to_owned(),
            tag: "node-001".to_owned(),
            status,
            latency_ms: (status == NodeStatus::Alive).then_some(31),
            failures: 0,
            alive_for_secs: None,
            last_error: None,
            selected: false,
            pinned: false,
            uplink_bytes: Some(10 * 1024),
            downlink_bytes: Some(200 * 1024),
        }
    }

    fn sample_nodes() -> Vec<Node> {
        let mut first = node("NL-1", NodeStatus::Alive);
        first.selected = true;
        first.pinned = true;
        let mut second = node("DE-2", NodeStatus::Alive);
        second.latency_ms = Some(45);
        let mut dead = node("US-3", NodeStatus::Dead);
        dead.failures = 3;
        dead.last_error = Some("тайм-аут".to_owned());
        vec![first, second, dead]
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
                api_silent_secs: Some(0),
                api_stuck: false,
            },
            node: Some(CurrentNode {
                id: "main/NL-1".to_owned(),
                subscription: "main".to_owned(),
                name: "NL-1".to_owned(),
                latency_ms: Some(31),
                pinned: true,
                reason: Some("выбран лучший живой узел".to_owned()),
            }),
            subscriptions: vec![SubscriptionStatus {
                name: "main".to_owned(),
                url: "https://sub.example.com/…1234".to_owned(),
                title: Some("Мой VPN".to_owned()),
                used_bytes: Some(3 * 1024 * 1024),
                total_bytes: Some(100 * 1024 * 1024 * 1024),
                expire: Some(NOW + 40 * DAY),
                nodes: 3,
                updated_at: Some(NOW - 2 * 3_600),
                next_update: Some(NOW + 3 * 3_600),
                last_error: None,
                updating: false,
            }],
        }
    }

    fn app_with(status: Status, nodes: Vec<Node>) -> App {
        let mut app = App::new(NOW);
        app.apply(snapshot_at(status, nodes, Instant::now()));
        app
    }

    fn snapshot_at(status: Status, nodes: Vec<Node>, at: Instant) -> Msg {
        Msg::Snapshot(Box::new(Snapshot {
            status,
            nodes: Nodes {
                selected: None,
                nodes,
            },
            at,
        }))
    }

    fn sample_app() -> App {
        let mut app = app_with(sample_status(), sample_nodes());
        app.apply(Msg::Event(Event::Hello {
            version: "0.1.0".to_owned(),
        }));
        app
    }

    fn rows(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < area.width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += u16::try_from(display_width(symbol).max(1)).unwrap_or(1);
                }
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn any_cell(buffer: &Buffer, test: impl Fn(&Cell) -> bool) -> bool {
        let area = buffer.area;
        (0..area.height).any(|y| (0..area.width).any(|x| test(&buffer[(x, y)])))
    }

    fn paint(app: &mut App, width: u16, height: u16, color: bool) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_at(frame, app, Palette::new(color), &TimeZone::UTC))
            .unwrap();
        terminal
    }

    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        rows(paint(app, width, height, false).backend().buffer())
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn on_nodes(app: &mut App) {
        press(app, KeyCode::Char('2'));
    }

    fn has_line(lines: &[String], text: &str) -> bool {
        lines.iter().any(|line| line.contains(text))
    }

    #[test]
    fn the_tab_row_names_every_tab_and_is_clickable() {
        let mut app = sample_app();
        let lines = screen(&mut app, 100, 30);
        for word in ["1 Обзор", "2 Узлы", "3 Подписки", "4 Журнал", "5 Настройки"] {
            assert!(lines[0].contains(word), "{word}: {:?}", lines[0]);
        }
        assert!(app.hits.iter().any(|hit| hit.target == Target::Tab(Tab::Journal)));
    }

    #[test]
    fn the_overview_shows_the_connection_the_subscriptions_and_the_buttons() {
        let mut app = sample_app();
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "● подключено"), "{lines:?}");
        assert!(has_line(&lines, "Подписки · всего 1"), "{lines:?}");
        assert!(has_line(&lines, "[u] Обновить подписки"), "{lines:?}");
        assert!(has_line(&lines, "работает"), "{lines:?}");
        assert!(has_line(&lines, "[a] Автовыбор"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::UpdateAll));
    }

    #[test]
    fn the_nodes_tab_shows_country_reason_and_the_safe_names() {
        let mut nodes = sample_nodes();
        nodes[0].name = "🇩🇪 Германия".to_owned();
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "DE Германия"), "{lines:?}");
        assert!(has_line(&lines, "Страна"), "{lines:?}");
        assert!(has_line(&lines, "Причина"), "{lines:?}");
        assert!(has_line(&lines, "не отвечает (тайм-аут)"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::NodeRow(0)));
    }

    #[test]
    fn speed_below_one_kilobit_is_a_dash() {
        assert_eq!(speed_label(27, 23), "↓ — ↑ —");
        assert_eq!(speed_label(5_200_000, 300), "↓ 5.2 Мбит/с ↑ —");
    }

    #[test]
    fn without_a_daemon_the_banner_sits_under_the_tabs() {
        let mut app = App::new(NOW);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[1], "raycat · ● подключение…");
        assert_eq!(lines[2], "данных от демона ещё нет");
        assert!(lines.last().unwrap().contains("q выход"));

        app.apply(Msg::Down {
            reason: "демон не запущен: сокета /run/raycat/raycat.sock нет".to_owned(),
            retry_in: Duration::from_secs(2),
        });
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[1],
            "демон недоступен: демон не запущен: сокета /run/raycat/raycat.sock нет"
        );
        assert_eq!(lines[2], "повторное подключение через 2 с");
        assert_eq!(lines[3], "raycat · ● нет связи");
    }

    #[test]
    fn a_lost_daemon_keeps_the_last_data_but_dims_it() {
        let mut app = sample_app();
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        let terminal = paint(&mut app, 80, 24, false);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        assert_eq!(lines[1], "демон недоступен: обрыв связи");
        assert_eq!(lines[2], "повторное подключение через 1 с");
        assert!(has_line(&lines, "NL-1"));
        let dimmed = |y: u16| buffer[(0, y)].style().add_modifier.contains(Modifier::DIM);
        assert!(!dimmed(1));
        assert!(dimmed(4));
    }

    #[test]
    fn a_lost_link_shows_in_the_header_in_red() {
        let mut app = sample_app();
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        let terminal = paint(&mut app, 80, 24, true);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        assert_eq!(
            lines[3],
            format!(
                "raycat · шлюз · ● нет связи · kill switch ●{}время работы 5 мин 12 с",
                " ".repeat(14)
            )
        );
        assert_eq!(buffer[(16, 3)].symbol(), "●");
        assert_eq!(buffer[(16, 3)].style().fg, Some(Color::LightRed));
    }

    #[test]
    fn a_long_reason_is_wrapped_over_a_few_lines() {
        let mut app = App::new(NOW);
        app.apply(Msg::Down {
            reason: "слово ".repeat(40),
            retry_in: Duration::from_secs(1),
        });
        let lines = screen(&mut app, 44, 20);
        assert!(lines[1].starts_with("демон недоступен: слово"));
        assert!(lines[2].starts_with("слово"));
        assert!(lines[3].ends_with('…'), "{:?}", lines[3]);
        assert_eq!(lines[4], "повторное подключение через 1 с");
    }

    #[test]
    fn the_smallest_supported_terminal_keeps_the_hints() {
        let mut app = sample_app();
        let lines = screen(&mut app, 44, 12);
        for line in &lines {
            assert!(display_width(line) <= 44, "{line:?}");
        }
        assert!(lines[0].contains("1 Обзор"));
        assert!(lines.last().unwrap().contains("? справка"));
        assert!(lines.last().unwrap().contains("q выход"));
    }

    #[test]
    fn a_terminal_below_the_minimum_says_its_size_and_the_needed_one() {
        let mut app = sample_app();
        let lines = screen(&mut app, 20, 5);
        assert_eq!(lines[0], "44×12");
        assert!(lines[1..].iter().all(String::is_empty));
        let lines = screen(&mut app, 80, 7);
        assert_eq!(lines[0], "Мало места: 80×7, нужно 44×12");
    }

    #[test]
    fn the_exit_and_help_hints_survive_every_width() {
        let mut app = sample_app();
        for width in 44..=80_u16 {
            let lines = screen(&mut app, width, 20);
            let last = lines.last().unwrap();
            assert!(
                last.contains("? справка") && last.contains("q выход"),
                "{width}: {last:?}"
            );
        }
    }

    #[test]
    fn help_replaces_the_screen_and_lists_the_new_keys() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        let lines = screen(&mut app, 80, 24);
        assert_eq!(lines[0], "raycat tui: клавиши");
        let text = lines.join("\n");
        for word in ["Enter", "PgUp", "Shift+Tab", "Esc", "Ctrl+C", "закрепить"] {
            assert!(text.contains(word), "{word}: {text}");
        }
        assert!(text.contains("▶ выбран   ★ закреплён вручную"));
        let narrow = screen(&mut app, 44, 12);
        for line in &narrow {
            assert!(display_width(line) <= 44, "{line:?}");
        }
    }

    #[test]
    fn progress_and_results_show_in_the_status_line() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('u'));
        app.set_now(NOW + 5);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[18], "⟳ Обновление всех подписок… 5 с");
        app.apply(Msg::Updated(Err("демон не ответил вовремя".to_owned())));
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[18],
            "Не удалось обновить подписки: демон не ответил вовремя"
        );
    }

    #[test]
    fn the_status_line_says_ready_only_when_connected_and_idle() {
        let mut app = App::new(NOW);
        assert_eq!(screen(&mut app, 80, 20)[18], "");
        let app = &mut sample_app();
        assert_eq!(screen(app, 80, 20)[18], "готов");
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        assert_eq!(screen(app, 80, 20)[18], "");
    }

    #[test]
    fn colors_follow_the_palette_and_vanish_without_it() {
        let mut app = sample_app();
        let colored = paint(&mut app, 80, 20, true);
        let has_color = |terminal: &Terminal<TestBackend>| {
            any_cell(terminal.backend().buffer(), |cell| {
                !matches!(cell.style().fg, None | Some(Color::Reset))
            })
        };
        assert!(has_color(&colored));
        let plain = paint(&mut app, 80, 20, false);
        assert!(!has_color(&plain));
    }

    #[test]
    fn latency_is_colored_by_its_thresholds() {
        assert_eq!(delay_tone(Some(31)), Tone::Green);
        assert_eq!(delay_tone(Some(149)), Tone::Green);
        assert_eq!(delay_tone(Some(150)), Tone::Yellow);
        assert_eq!(delay_tone(Some(399)), Tone::Yellow);
        assert_eq!(delay_tone(Some(400)), Tone::Red);
        assert_eq!(delay_tone(None), Tone::Dim);
    }

    #[test]
    fn a_proxy_screen_has_no_kill_switch() {
        let mut status = sample_status();
        status.mode = Mode::Proxy;
        status.kill_switch = None;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[1].starts_with("raycat · прокси · ● подключено"));
        assert!(!lines[1].contains("kill switch"));
    }

    #[test]
    fn a_warning_about_a_stopped_xray_and_kill_switch_is_loud() {
        let mut status = sample_status();
        status.xray = XrayStatus {
            running: false,
            pid: None,
            restarts: 3,
            api_silent_secs: None,
            api_stuck: false,
        };
        status.kill_switch = Some(false);
        status.node = None;
        let mut app = app_with(status, Vec::new());
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[1],
            "raycat · шлюз · ● подключено · xray не запущен · kill switch ●"
        );
        assert_eq!(lines[2], "узел: не выбран, узлов пока нет");

        let mut status = sample_status();
        status.xray.restarts = 2;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[1],
            "raycat · шлюз · ● подключено · xray перезапускался: 2 · kill switch ●"
        );
    }

    #[test]
    fn a_stuck_xray_is_a_problem_in_the_header() {
        let mut status = sample_status();
        status.xray.api_stuck = true;
        status.xray.api_silent_secs = Some(120);
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[1].contains("xray не отвечает (120 с)"));
    }

    #[test]
    fn a_two_line_error_shows_the_cause_in_red_and_the_hint_plain() {
        let mut app = App::new(NOW);
        app.apply(Msg::Down {
            reason: "raycat не запущен\nЗапустите службу: sudo systemctl start raycat".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        let terminal = paint(&mut app, 80, 24, true);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        assert_eq!(lines[1], "демон недоступен: raycat не запущен");
        assert_eq!(lines[2], "Запустите службу: sudo systemctl start raycat");
        assert_eq!(lines[3], "повторное подключение через 1 с");
        assert_eq!(buffer[(0, 1)].style().fg, Some(Color::LightRed));
        assert_ne!(buffer[(0, 2)].style().fg, Some(Color::LightRed));
    }

    #[test]
    fn a_flag_in_a_name_is_a_code_and_the_status_column_stays_aligned() {
        let mut nodes = sample_nodes();
        nodes[0].name = "🇩🇪 Германия".to_owned();
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        let lines = screen(&mut app, 100, 30);
        let flagged = lines.iter().find(|line| line.contains("DE Германия")).unwrap();
        let plain = lines.iter().find(|line| line.contains("DE-2")).unwrap();
        let status_at = |line: &str| display_width(&line[..line.find("жив").unwrap()]);
        assert_eq!(status_at(flagged), status_at(plain));
    }

    #[test]
    fn provider_text_cannot_reach_the_terminal_as_control_codes() {
        let mut nodes = sample_nodes();
        nodes[0].name = "NL\x1b[31m-1".to_owned();
        nodes[2].last_error = Some("a\x1b]0;взлом\x07b".to_owned());
        let mut status = sample_status();
        status.subscriptions[0].title = Some("\x1b[2JМой".to_owned());
        status.subscriptions[0].last_error = Some("пан\x1b[1Aель".to_owned());
        if let Some(node) = &mut status.node {
            node.reason = Some("причина\x07".to_owned());
        }
        let mut app = app_with(status, nodes);
        app.apply(Msg::Event(Event::Warning {
            level: "warn".to_owned(),
            message: "x\x1b[2Jy".to_owned(),
        }));
        on_nodes(&mut app);
        let terminal = paint(&mut app, 120, 30, false);
        let buffer = terminal.backend().buffer();
        assert!(!any_cell(buffer, |cell| cell
            .symbol()
            .chars()
            .any(char::is_control)));
        let lines = rows(buffer);
        assert!(has_line(&lines, "NL [31m-1"), "{lines:?}");
    }

    #[test]
    fn frames_have_rounded_corners_on_the_subscriptions_tab() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('3'));
        let terminal = paint(&mut app, 80, 20, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 4)].symbol(), "╭");
        assert_eq!(buffer[(79, 4)].symbol(), "╮");
        assert_eq!(buffer[(0, 4)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn the_header_hides_the_uptime_when_it_does_not_fit() {
        let mut app = sample_app();
        app.set_now(NOW);
        let lines = screen(&mut app, 60, 20);
        assert!(!lines[1].contains("время работы"), "{:?}", lines[1]);
    }

    #[test]
    fn the_layout_stays_inside_the_terminal_on_every_tab() {
        let mut app = sample_app();
        for key in ['1', '2', '3', '4', '5'] {
            press(&mut app, KeyCode::Char(key));
            for (width, height) in [(44_u16, 12_u16), (80, 20), (120, 40)] {
                for line in screen(&mut app, width, height) {
                    assert!(display_width(&line) <= usize::from(width), "{line:?}");
                }
            }
        }
    }
}
