//! Отрисовка: шапка, подписки, таблица узлов, журнал и строки внизу. Раскладка
//! подстраивается под размер окна: сначала пропадают второстепенные колонки и
//! куски строк, потом секции.

use ratatui::Frame;
use raycat_proto::{Node, Status, SubscriptionStatus, XrayStatus};

use super::app::{App, InputMode, Link};
use super::canvas::{Canvas, GAP, Palette, Seg, highlight, wrap};
use crate::render;
use crate::term::{Tone, display_width, pad, truncate};
use crate::util::format_clock;

const MIN_WIDTH: usize = 24;
const MIN_HEIGHT: usize = 8;
const BOTTOM: usize = 2;
const NODES_MIN: usize = 5;
const BANNER_LINES: usize = 3;

const HELP: [(&str, &str); 12] = [
    ("↑ ↓  j k", "выбрать узел"),
    ("PgUp PgDn", "страница вверх и вниз"),
    ("Home End", "в начало и в конец (g G)"),
    ("Enter", "закрепить выбранный узел"),
    ("a", "вернуть автоматический выбор узла"),
    ("u", "обновить все подписки"),
    ("U", "обновить подписку выбранного узла"),
    ("/", "фильтр по тексту (Enter — применить, Esc — сбросить)"),
    ("Tab", "фильтр по подписке (Shift+Tab — в обратную сторону)"),
    ("Esc", "сбросить фильтры"),
    ("?", "эта справка"),
    ("q  Ctrl+C", "выйти"),
];

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App, palette: Palette) {
    let mut canvas = Canvas::new(frame.buffer_mut(), palette);
    compose(&mut canvas, app);
}

fn compose(canvas: &mut Canvas<'_>, app: &mut App) {
    if canvas.width < MIN_WIDTH || canvas.height < MIN_HEIGHT {
        let text = format!("Окно слишком маленькое: нужно не меньше {MIN_WIDTH}×{MIN_HEIGHT}");
        canvas.line(0, 0, vec![Seg::new(text, Tone::Yellow, 0)]);
        return;
    }
    if app.input == InputMode::Help {
        help(canvas);
        return;
    }
    let top_limit = canvas.height.saturating_sub(BOTTOM + NODES_MIN);
    let mut y = banner(canvas, app, top_limit);
    canvas.set_dim(matches!(app.link, Link::Down { .. }));
    y = header(canvas, app, y, top_limit);

    let subs = subscription_lines(app);
    let subs_want = if app.status.is_some() {
        1 + subs.len().max(1)
    } else {
        0
    };
    let body = canvas.height.saturating_sub(y + BOTTOM);
    let log_cap = (body / 5).clamp(3, 12);
    let log_want = 1 + app.log.len().clamp(1, log_cap);
    let layout = plan(body, subs_want, log_want);

    if layout.subs > 0 {
        subscriptions(canvas, y, layout.subs, subs, app);
        y += layout.subs;
    }
    nodes_section(canvas, y, layout.nodes, app);
    y += layout.nodes;
    if layout.log > 0 {
        log_section(canvas, y, layout.log, app);
    }
    canvas.set_dim(false);
    bottom(canvas, app);
}

struct Plan {
    subs: usize,
    nodes: usize,
    log: usize,
}

/// Делит строки тела между секциями: таблице узлов сначала гарантируется минимум,
/// затем подпискам и журналу по запросу, остальное достаётся таблице.
fn plan(body: usize, subs_want: usize, log_want: usize) -> Plan {
    let nodes_min = body.min(NODES_MIN);
    let mut left = body - nodes_min;
    let subs = if left >= 2 { left.min(subs_want) } else { 0 };
    left -= subs;
    let log = if left >= 2 { left.min(log_want) } else { 0 };
    left -= log;
    Plan {
        subs,
        nodes: nodes_min + left,
        log,
    }
}

fn banner(canvas: &mut Canvas<'_>, app: &App, limit: usize) -> usize {
    let lines: Vec<Vec<Seg>> = match &app.link {
        Link::Up => Vec::new(),
        Link::Connecting => vec![vec![Seg::new("Подключение к демону…", Tone::Yellow, 0)]],
        Link::Down { reason, retry_at } => {
            let text = format!("демон недоступен: {reason}");
            let mut lines: Vec<Vec<Seg>> = wrap(&text, canvas.width, BANNER_LINES)
                .into_iter()
                .map(|line| vec![Seg::new(line, Tone::Red, 0)])
                .collect();
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
        canvas.line(row, 0, segs);
    }
    count
}

fn header(canvas: &mut Canvas<'_>, app: &App, y: usize, limit: usize) -> usize {
    let lines = header_lines(app);
    let count = lines.len().min(limit.saturating_sub(y));
    for (row, segs) in lines.into_iter().take(count).enumerate() {
        canvas.line(y + row, 0, segs);
    }
    y + count
}

fn xray_segs(xray: &XrayStatus) -> Vec<Seg> {
    if !xray.running {
        return vec![Seg::new("xray: не запущен", Tone::Red, 0)];
    }
    let pid = xray
        .pid
        .map_or_else(String::new, |pid| format!(" (pid {pid})"));
    let mut segs = vec![Seg::new(format!("xray: работает{pid}"), Tone::Green, 0)];
    if xray.restarts > 0 {
        let text = format!("перезапусков: {}", xray.restarts);
        segs.push(Seg::new(text, Tone::Yellow, 2));
    }
    segs
}

fn node_segs(status: &Status) -> Vec<Seg> {
    let Some(node) = &status.node else {
        return vec![Seg::new("узел: не выбран, узлов пока нет", Tone::Yellow, 0)];
    };
    let choice = if node.pinned {
        Seg::new("закреплён вручную", Tone::Yellow, 2)
    } else {
        Seg::new("автоматический выбор", Tone::Dim, 2)
    };
    vec![
        Seg::new(format!("узел: {}", node.id), Tone::Bold, 0),
        Seg::new(render::latency_text(node.latency_ms), Tone::Plain, 1),
        choice,
    ]
}

fn header_lines(app: &App) -> Vec<Vec<Seg>> {
    let Some(status) = &app.status else {
        let text = "raycat: данных от демона ещё нет";
        return vec![vec![Seg::new(text, Tone::Dim, 0)]];
    };
    let uptime = status.uptime_secs + app.now.saturating_sub(app.status_at);
    let first = vec![
        Seg::new(format!("raycat {}", status.version), Tone::Header, 0),
        Seg::new(
            format!("режим: {}", render::mode_name(status.mode)),
            Tone::Plain,
            0,
        ),
        Seg::new(
            format!("время работы: {}", render::span(uptime)),
            Tone::Dim,
            1,
        ),
    ];
    let mut second = xray_segs(&status.xray);
    match status.kill_switch {
        Some(true) => second.push(Seg::new("kill switch: включён", Tone::Green, 1)),
        Some(false) => second.push(Seg::new("kill switch: выключен", Tone::Red, 0)),
        None => {}
    }
    let mut lines = vec![first, second, node_segs(status)];
    if let Some(reason) = status.node.as_ref().and_then(|node| node.reason.as_ref()) {
        lines.push(vec![Seg::new(format!("причина: {reason}"), Tone::Dim, 0)]);
    }
    lines
}

fn subscription_segs(sub: &SubscriptionStatus, now: u64) -> Vec<Seg> {
    let mut segs = vec![Seg::new(sub.name.clone(), Tone::Bold, 0)];
    if let Some(title) = sub.title.as_deref().filter(|title| !title.is_empty()) {
        segs.push(Seg::new(format!("«{title}»"), Tone::Dim, 4));
    }
    segs.push(Seg::new(format!("узлов: {}", sub.nodes), Tone::Plain, 2));
    if let Some(used) = sub.used_bytes {
        let (text, tone) = render::traffic_text(used, sub.total_bytes);
        segs.push(Seg::new(text, tone, 1));
    }
    if let Some(expire) = sub.expire {
        let (text, tone) = render::expiry_text(expire, now);
        segs.push(Seg::new(text, tone, 3));
    }
    match sub.updated_at {
        Some(then) => {
            let text = format!("обновлена {}", render::ago(now, then));
            segs.push(Seg::new(text, Tone::Plain, 2));
        }
        None => segs.push(Seg::new("ещё не получена", Tone::Yellow, 1)),
    }
    if let Some(next) = sub.next_update {
        let text = format!("следующая {}", render::ahead(now, next));
        segs.push(Seg::new(text, Tone::Dim, 5));
    }
    if sub.updating {
        segs.push(Seg::new("⟳ идёт обновление", Tone::Green, 0));
    }
    segs
}

fn subscription_lines(app: &App) -> Vec<Vec<Seg>> {
    let Some(status) = &app.status else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for sub in &status.subscriptions {
        lines.push(subscription_segs(sub, app.now));
        if let Some(error) = &sub.last_error {
            lines.push(vec![Seg::new(format!("  ✗ {error}"), Tone::Red, 0)]);
        }
    }
    lines
}

fn subscriptions(
    canvas: &mut Canvas<'_>,
    y0: usize,
    room: usize,
    lines: Vec<Vec<Seg>>,
    app: &App,
) {
    canvas.line(y0, 0, vec![Seg::new("Подписки", Tone::Header, 0)]);
    let capacity = room.saturating_sub(1);
    if lines.is_empty() {
        let text = if app.status.is_some() {
            "подписок нет"
        } else {
            "ждём данные от демона"
        };
        canvas.line(y0 + 1, 2, vec![Seg::new(text, Tone::Dim, 0)]);
        return;
    }
    let total = lines.len();
    let shown = if total > capacity {
        capacity.saturating_sub(1)
    } else {
        total
    };
    for (row, segs) in lines.into_iter().take(shown).enumerate() {
        canvas.line(y0 + 1 + row, 2, segs);
    }
    if shown < total {
        let text = format!("… и ещё строк: {}", total - shown);
        canvas.line(y0 + 1 + shown, 2, vec![Seg::new(text, Tone::Dim, 0)]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Marker,
    Sub,
    Name,
    Status,
    Latency,
    Failures,
    Error,
    Traffic,
}

struct Spec {
    kind: Kind,
    title: &'static str,
    min: usize,
    natural: usize,
    right: bool,
}

struct Col {
    kind: Kind,
    title: &'static str,
    width: usize,
    right: bool,
}

/// Колонки в порядке важности: на узком экране последние пропадают первыми.
fn specs(nodes: &[&Node]) -> Vec<Spec> {
    let (mut sub, mut name, mut error, mut traffic) = (0_usize, 0_usize, 0_usize, 0_usize);
    for node in nodes {
        sub = sub.max(display_width(&node.subscription));
        name = name.max(display_width(&node.name));
        error = error.max(node.last_error.as_deref().map_or(0, display_width));
        traffic = traffic.max(display_width(&render::node_traffic(node)));
    }
    let name = name.clamp(4, 48);
    let error = error.min(80);
    let traffic = traffic.max(6);
    let spec = |kind, title, min, natural, right| Spec {
        kind,
        title,
        min,
        natural,
        right,
    };
    vec![
        spec(Kind::Marker, "", 1, 1, false),
        spec(Kind::Name, "Узел", name.min(12), name, false),
        spec(Kind::Status, "Статус", 11, 11, false),
        spec(Kind::Latency, "Задержка", 8, 8, true),
        spec(Kind::Sub, "Подписка", 8, sub.clamp(8, 20), false),
        spec(Kind::Failures, "Провалы", 7, 7, true),
        spec(Kind::Error, "Ошибка", error.min(12), error, false),
        spec(Kind::Traffic, "Трафик", traffic, traffic, false),
    ]
}

/// Берёт колонки, пока помещаются минимальные ширины, и раздаёт остаток тем,
/// что могут расти: имени, подписке и ошибке.
fn columns(nodes: &[&Node], width: usize) -> Vec<Col> {
    let mut chosen: Vec<(Spec, usize)> = Vec::new();
    let mut used = 0;
    for spec in specs(nodes) {
        if spec.natural == 0 {
            continue;
        }
        let need = spec.min + if chosen.is_empty() { 0 } else { GAP };
        if used + need > width {
            break;
        }
        used += need;
        let start = spec.min;
        chosen.push((spec, start));
    }
    let mut extra = width.saturating_sub(used);
    for kind in [Kind::Name, Kind::Sub, Kind::Error] {
        if let Some((spec, current)) = chosen.iter_mut().find(|(spec, _)| spec.kind == kind) {
            let add = extra.min(spec.natural.saturating_sub(*current));
            *current += add;
            extra -= add;
        }
    }
    let mut cols: Vec<Col> = chosen
        .into_iter()
        .map(|(spec, width)| Col {
            kind: spec.kind,
            title: spec.title,
            width,
            right: spec.right,
        })
        .collect();
    cols.sort_by_key(|col| col.kind);
    cols
}

fn cell(kind: Kind, node: &Node) -> (String, Tone) {
    match kind {
        Kind::Marker => {
            let (mark, tone) = render::node_marker(node);
            (mark.to_owned(), tone)
        }
        Kind::Sub => (node.subscription.clone(), Tone::Plain),
        Kind::Name => {
            let tone = if node.selected {
                Tone::Bold
            } else {
                Tone::Plain
            };
            (node.name.clone(), tone)
        }
        Kind::Status => {
            let (text, tone) = render::node_status(node.status);
            (text.to_owned(), tone)
        }
        Kind::Latency => (render::latency_text(node.latency_ms), Tone::Plain),
        Kind::Failures => {
            let tone = if node.failures > 0 {
                Tone::Yellow
            } else {
                Tone::Dim
            };
            (node.failures.to_string(), tone)
        }
        Kind::Error => (node.last_error.clone().unwrap_or_default(), Tone::Red),
        Kind::Traffic => (render::node_traffic(node), Tone::Dim),
    }
}

fn draw_row(canvas: &mut Canvas<'_>, y: usize, cols: &[Col], node: &Node, current: bool) {
    let marked = highlight();
    if current {
        canvas.fill(y, marked);
    }
    let mut x = 0;
    for col in cols {
        let (text, tone) = cell(col.kind, node);
        let text = truncate(&text, col.width);
        let shift = if col.right {
            col.width.saturating_sub(display_width(&text))
        } else {
            0
        };
        if current {
            canvas.put_style(x + shift, y, &text, col.width, marked);
        } else {
            canvas.put(x + shift, y, &text, col.width, tone);
        }
        x += col.width + GAP;
    }
}

fn draw_titles(canvas: &mut Canvas<'_>, y: usize, cols: &[Col]) {
    let mut x = 0;
    for col in cols {
        let shift = if col.right {
            col.width.saturating_sub(display_width(col.title))
        } else {
            0
        };
        canvas.put(x + shift, y, col.title, col.width, Tone::Dim);
        x += col.width + GAP;
    }
}

fn nodes_title(app: &App, rows: usize) -> Vec<Seg> {
    let total = app.nodes.len();
    let shown = app.visible.len();
    let count = if shown < total {
        format!("показано {shown} из {total}")
    } else if shown > rows {
        format!(
            "{}–{} из {shown}",
            app.offset + 1,
            (app.offset + rows).min(shown)
        )
    } else {
        format!("всего {total}")
    };
    let mut segs = vec![
        Seg::new("Узлы", Tone::Header, 0),
        Seg::new(count, Tone::Dim, 2),
    ];
    let text = app.filter.text.trim();
    if !text.is_empty() {
        segs.push(Seg::new(format!("фильтр: «{text}»"), Tone::Yellow, 0));
    }
    if let Some(name) = &app.filter.subscription {
        segs.push(Seg::new(format!("подписка: {name}"), Tone::Yellow, 0));
    }
    segs
}

fn nodes_section(canvas: &mut Canvas<'_>, y0: usize, room: usize, app: &mut App) {
    let rows = room.saturating_sub(2);
    app.set_rows(rows);
    canvas.line(y0, 0, nodes_title(app, rows));
    if app.visible.is_empty() {
        let (text, tone) = if app.nodes.is_empty() {
            (
                "Узлов нет: подписки ещё не получены или в них нет узлов",
                Tone::Dim,
            )
        } else {
            ("Под фильтр ничего не подошло: Esc — сбросить", Tone::Yellow)
        };
        canvas.line(y0 + 1, 0, vec![Seg::new(text, tone, 0)]);
        return;
    }
    let shown: Vec<&Node> = app
        .visible
        .iter()
        .filter_map(|index| app.nodes.get(*index))
        .collect();
    let cols = columns(&shown, canvas.width);
    draw_titles(canvas, y0 + 1, &cols);
    for (row, node) in shown.iter().skip(app.offset).take(rows).enumerate() {
        let current = app.offset + row == app.cursor;
        draw_row(canvas, y0 + 2 + row, &cols, node, current);
    }
}

fn log_section(canvas: &mut Canvas<'_>, y0: usize, room: usize, app: &App) {
    let title = vec![
        Seg::new("Журнал", Tone::Header, 0),
        Seg::new("время UTC", Tone::Dim, 1),
    ];
    canvas.line(y0, 0, title);
    if app.log.is_empty() {
        canvas.line(y0 + 1, 0, vec![Seg::new("пока пусто", Tone::Dim, 0)]);
        return;
    }
    let capacity = room.saturating_sub(1);
    let first = app.log.len().saturating_sub(capacity);
    for (row, entry) in app.log.iter().skip(first).enumerate() {
        let segs = vec![
            Seg::new(format_clock(entry.at), Tone::Dim, 0),
            Seg::new(entry.text.clone(), entry.tone, 0),
        ];
        canvas.line(y0 + 1 + row, 0, segs);
    }
}

fn hint_segs(app: &App) -> Vec<Seg> {
    if app.input == InputMode::Filter {
        return vec![
            Seg::new(format!("/{}█", app.filter.text), Tone::Bold, 0),
            Seg::new("Enter — применить, Esc — сбросить", Tone::Dim, 1),
        ];
    }
    [
        ("↑↓ выбор", 0),
        ("Enter закрепить", 1),
        ("a авто", 2),
        ("u обновить", 3),
        ("U подписку", 5),
        ("/ фильтр", 2),
        ("Tab подписка", 6),
        ("? справка", 0),
        ("q выход", 1),
    ]
    .into_iter()
    .map(|(text, priority)| Seg::new(text, Tone::Dim, priority))
    .collect()
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
    }
    canvas.line(hints_y, 0, hint_segs(app));
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::{Color, Modifier};
    use raycat_proto::{CurrentNode, Event, Mode, NodeStatus, Nodes};

    use super::super::app::{Msg, Snapshot};
    use super::*;

    const NOW: u64 = 1_790_596_800;
    const DAY: u64 = 86_400;

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
        app.apply(Msg::Snapshot(Box::new(Snapshot {
            status,
            nodes: Nodes {
                selected: None,
                nodes,
            },
        })));
        app
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
            .draw(|frame| draw(frame, app, Palette::new(color)))
            .unwrap();
        terminal
    }

    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        rows(paint(app, width, height, false).backend().buffer())
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn a_full_screen_shows_everything_that_matters() {
        let mut app = sample_app();
        let pinned = format!(
            "★  main      NL-1  жив{}31 мс{}0",
            " ".repeat(13),
            " ".repeat(8)
        );
        let alive = format!(
            "   main      DE-2  жив{}45 мс{}0",
            " ".repeat(13),
            " ".repeat(8)
        );
        let dead = format!(
            "   main      US-3  не отвечает{}—{}3  тайм-аут",
            " ".repeat(9),
            " ".repeat(8)
        );
        let expected = [
            "raycat 0.1.0  режим: шлюз  время работы: 5 мин 12 с",
            "xray: работает (pid 4127)  kill switch: включён",
            "узел: main/NL-1  31 мс  закреплён вручную",
            "причина: выбран лучший живой узел",
            "Подписки",
            "  main  узлов: 3  3.0 МиБ из 100.0 ГиБ (0%)  обновлена 2 ч 0 мин назад",
            "Узлы  всего 3",
            "   Подписка  Узел  Статус       Задержка  Провалы  Ошибка",
            pinned.as_str(),
            alive.as_str(),
            dead.as_str(),
            "",
            "",
            "",
            "",
            "",
            "Журнал  время UTC",
            "12:00:00  подключено к демону, версия 0.1.0",
            "",
            "↑↓ выбор  Enter закрепить  a авто  u обновить  / фильтр  ? справка  q выход",
        ];
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines, expected, "\n{}", lines.join("\n"));
    }

    #[test]
    fn the_selected_row_is_highlighted_and_follows_the_cursor() {
        let mut app = sample_app();
        let terminal = paint(&mut app, 80, 20, false);
        let buffer = terminal.backend().buffer();
        let reversed = |y: u16| {
            buffer[(5, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed(8));
        assert!(!reversed(9));
        press(&mut app, KeyCode::Down);
        let terminal = paint(&mut app, 80, 20, false);
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(5, 8)].style().add_modifier.contains(Modifier::REVERSED));
        assert!(buffer[(5, 9)].style().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn without_a_daemon_the_screen_says_so() {
        let mut app = App::new(NOW);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[0], "Подключение к демону…");
        assert_eq!(lines[1], "raycat: данных от демона ещё нет");
        assert!(lines.iter().any(|line| line.starts_with("Узлов нет")));
        assert!(lines.last().unwrap().contains("q выход"));

        app.apply(Msg::Down {
            reason: "демон не запущен: сокета /run/raycat/raycat.sock нет".to_owned(),
            retry_in: Duration::from_secs(2),
        });
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[0],
            "демон недоступен: демон не запущен: сокета /run/raycat/raycat.sock нет"
        );
        assert_eq!(lines[1], "повторное подключение через 2 с");
        assert!(lines.iter().any(|line| line.contains("демон недоступен")));
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
        assert_eq!(lines[0], "демон недоступен: обрыв связи");
        assert_eq!(lines[1], "повторное подключение через 1 с");
        assert!(lines.iter().any(|line| line.contains("NL-1")));
        assert!(lines.iter().any(|line| line.contains("демон недоступен: обрыв")));
        let dimmed = |y: u16| buffer[(0, y)].style().add_modifier.contains(Modifier::DIM);
        assert!(!dimmed(0));
        assert!(dimmed(2));
    }

    #[test]
    fn a_long_reason_is_wrapped_over_a_few_lines() {
        let mut app = App::new(NOW);
        app.apply(Msg::Down {
            reason: "слово ".repeat(40),
            retry_in: Duration::from_secs(1),
        });
        let lines = screen(&mut app, 40, 20);
        assert!(lines[0].starts_with("демон недоступен: слово"));
        assert!(lines[1].starts_with("слово"));
        assert!(lines[2].ends_with('…'), "{:?}", lines[2]);
        assert_eq!(lines[3], "повторное подключение через 1 с");
    }

    #[test]
    fn a_narrow_terminal_hides_columns_and_cuts_names() {
        let mut nodes = sample_nodes();
        nodes[0].name = "Германия, Франкфурт, очень длинное имя узла".to_owned();
        let mut app = app_with(sample_status(), nodes);
        let lines = screen(&mut app, 40, 20);
        for line in &lines {
            assert!(display_width(line) <= 40, "{line:?}");
        }
        let header = lines.iter().find(|line| line.contains("Статус")).unwrap();
        assert!(!header.contains("Подписка"), "{header:?}");
        assert!(!header.contains("Провалы"), "{header:?}");
        assert!(header.contains("Задержка"), "{header:?}");
        let long = lines.iter().find(|line| line.contains("Германия")).unwrap();
        assert!(long.contains("Германия, Фра…"), "{long:?}");
        assert!(long.starts_with('★'), "{long:?}");
    }

    #[test]
    fn the_smallest_supported_terminal_still_shows_the_table() {
        let mut app = sample_app();
        let lines = screen(&mut app, 24, 8);
        for line in &lines {
            assert!(display_width(line) <= 24, "{line:?}");
        }
        assert!(lines.iter().any(|line| line.contains("NL-1")));
        assert!(lines.last().unwrap().starts_with("↑↓ выбор"));
    }

    #[test]
    fn a_terminal_below_the_minimum_gets_a_hint_instead_of_a_broken_screen() {
        let mut app = sample_app();
        let lines = screen(&mut app, 20, 5);
        assert!(lines[0].starts_with("Окно слишком"), "{:?}", lines[0]);
        assert!(lines[0].ends_with('…'));
        assert!(lines[1..].iter().all(String::is_empty));
        let lines = screen(&mut app, 80, 7);
        assert!(lines[0].starts_with("Окно слишком маленькое"));
    }

    #[test]
    fn flag_emoji_in_names_keep_the_columns_aligned() {
        let mut nodes = sample_nodes();
        nodes[0].name = "🇩🇪 Германия".to_owned();
        let mut app = app_with(sample_status(), nodes);
        let lines = screen(&mut app, 80, 20);
        let flagged = lines.iter().find(|line| line.contains("🇩🇪")).unwrap();
        assert!(flagged.contains("🇩🇪 Германия"), "{flagged:?}");
        assert!(flagged.contains("жив"));
        let plain = lines.iter().find(|line| line.contains("DE-2")).unwrap();
        let status_at = |line: &str| {
            let head = &line[..line.find("жив").unwrap()];
            display_width(head)
        };
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
        let terminal = paint(&mut app, 120, 30, false);
        let buffer = terminal.backend().buffer();
        assert!(!any_cell(buffer, |cell| cell
            .symbol()
            .chars()
            .any(char::is_control)));
        let lines = rows(buffer);
        assert!(lines.iter().any(|line| line.contains("NL [31m-1")));
        assert!(lines.iter().any(|line| line.contains("пан [1Aель")));
    }

    #[test]
    fn filter_prompt_and_title_show_what_is_applied() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "de".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let lines = screen(&mut app, 80, 20);
        assert!(lines.last().unwrap().starts_with("/de█"), "{:?}", lines.last());
        assert!(lines.iter().any(|line| line == "Узлы  показано 1 из 3  фильтр: «de»"));
        assert!(lines.iter().any(|line| line.contains("DE-2")));
        assert!(!lines.iter().any(|line| line.contains("US-3")));

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        let lines = screen(&mut app, 80, 20);
        assert!(
            lines
                .iter()
                .any(|line| line == "Узлы  всего 3  подписка: main"),
            "{lines:?}"
        );

        let mut empty = sample_app();
        press(&mut empty, KeyCode::Char('/'));
        press(&mut empty, KeyCode::Char('я'));
        let lines = screen(&mut empty, 80, 20);
        assert!(lines.iter().any(|line| line.starts_with("Под фильтр ничего не подошло")));
    }

    #[test]
    fn a_long_list_scrolls_with_the_cursor() {
        let nodes: Vec<Node> = (0..40)
            .map(|index| node(&format!("N{index:02}"), NodeStatus::Alive))
            .collect();
        let mut app = app_with(sample_status(), nodes);
        let lines = screen(&mut app, 80, 20);
        assert!(lines.iter().any(|line| line.contains("N00")));
        assert!(!lines.iter().any(|line| line.contains("N39")));
        assert!(lines.iter().any(|line| line.starts_with("Узлы  1–")), "{lines:?}");
        press(&mut app, KeyCode::End);
        let lines = screen(&mut app, 80, 20);
        assert!(lines.iter().any(|line| line.contains("N39")));
        assert!(!lines.iter().any(|line| line.contains("N00")));
        assert!(lines.iter().any(|line| line.ends_with("из 40")), "{lines:?}");
    }

    #[test]
    fn progress_and_results_show_in_the_status_line() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('u'));
        app.set_now(NOW + 5);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[18], "⟳ Обновление всех подписок… 5 с");
        press(&mut app, KeyCode::Char('U'));
        assert_eq!(app.update.as_ref().unwrap().target, None);

        app.apply(Msg::Updated(Err("демон не ответил вовремя".to_owned())));
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[18],
            "Не удалось обновить подписки: демон не ответил вовремя"
        );

        press(&mut app, KeyCode::Char('U'));
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[18], "⟳ Обновление подписки «main»… 0 с");
    }

    #[test]
    fn subscription_trouble_gets_its_own_line() {
        let mut status = sample_status();
        status.subscriptions[0].last_error = Some("панель ответила 403".to_owned());
        status.subscriptions[0].updating = true;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 100, 24);
        let at = lines.iter().position(|line| line == "Подписки").unwrap();
        assert!(lines[at + 1].contains("⟳ идёт обновление"), "{:?}", lines[at + 1]);
        assert_eq!(lines[at + 2], "    ✗ панель ответила 403");
    }

    #[test]
    fn the_log_shows_the_latest_entries_with_their_time() {
        let mut app = sample_app();
        for index in 0..30 {
            app.set_now(NOW + index);
            app.apply(Msg::Event(Event::Warning {
                level: "warn".to_owned(),
                message: format!("запись {index}"),
            }));
        }
        let lines = screen(&mut app, 80, 30);
        assert!(lines.iter().any(|line| line == "12:00:29  WARN: запись 29"));
        assert!(!lines.iter().any(|line| line.contains("запись 0")));
    }

    #[test]
    fn help_replaces_the_screen_and_lists_every_key() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        let lines = screen(&mut app, 80, 24);
        assert_eq!(lines[0], "raycat tui: клавиши");
        let text = lines.join("\n");
        for word in ["Enter", "PgUp", "Home", "Tab", "Esc", "Ctrl+C", "U  ", "закрепить"] {
            assert!(text.contains(word), "{word}: {text}");
        }
        assert!(text.contains("▶ выбран   ★ закреплён вручную"));
        assert!(!text.contains("Подписки"));
        let narrow = screen(&mut app, 30, 12);
        for line in &narrow {
            assert!(display_width(line) <= 30, "{line:?}");
        }
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
    fn a_warning_about_a_stopped_xray_and_kill_switch_is_loud() {
        let mut status = sample_status();
        status.xray = XrayStatus {
            running: false,
            pid: None,
            restarts: 3,
        };
        status.kill_switch = Some(false);
        status.node = None;
        let mut app = app_with(status, Vec::new());
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[1], "xray: не запущен  kill switch: выключен");
        assert_eq!(lines[2], "узел: не выбран, узлов пока нет");

        let mut status = sample_status();
        status.xray.restarts = 2;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[1],
            "xray: работает (pid 4127)  перезапусков: 2  kill switch: включён"
        );
    }

    #[test]
    fn the_layout_plan_never_exceeds_the_body() {
        for body in 0..60 {
            for subs in [0, 2, 9] {
                for log in [2, 8] {
                    let layout = plan(body, subs, log);
                    assert_eq!(layout.subs + layout.nodes + layout.log, body);
                    assert!(layout.nodes >= body.min(NODES_MIN));
                }
            }
        }
    }

    #[test]
    fn columns_are_chosen_by_priority_and_fit_the_width() {
        let nodes = sample_nodes();
        let refs: Vec<&Node> = nodes.iter().collect();
        for width in 24..160 {
            let cols = columns(&refs, width);
            let total: usize =
                cols.iter().map(|col| col.width).sum::<usize>() + GAP * cols.len().saturating_sub(1);
            assert!(total <= width, "{width}: {total}");
            assert!(cols.iter().any(|col| col.kind == Kind::Name));
        }
        let wide = columns(&refs, 200);
        assert!(wide.iter().any(|col| col.kind == Kind::Traffic));
        let narrow = columns(&refs, 30);
        assert!(!narrow.iter().any(|col| col.kind == Kind::Traffic));
    }
}
