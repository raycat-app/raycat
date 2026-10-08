//! Отрисовка: шапка, рамки подписок, узлов и журнала, строки подсказок. Раскладка
//! подстраивается под размер окна: сначала пропадают второстепенные колонки и
//! куски строк, потом секции.

use jiff::tz::TimeZone;
use ratatui::Frame;
use raycat_proto::{Node, SubscriptionStatus, XrayStatus};

use super::app::{App, InputMode, Link};
use super::canvas::{Canvas, GAP, Palette, Seg, fit, highlight, wrap};
use crate::render;
use crate::term::{Tone, display_width, pad, truncate};
use crate::util::{format_moment, is_utc, local_zone};

const MIN_WIDTH: usize = 44;
const MIN_HEIGHT: usize = 12;
const BOTTOM: usize = 2;
const HEADER_LINES: usize = 2;
/// Верхняя и нижняя линии рамки.
const FRAME: usize = 2;
/// Рамка узлов: строка заголовков таблицы и хотя бы две строки узлов.
const NODES_MIN: usize = 5;
const BANNER_LINES: usize = 3;
const BANNER_HINT_LINES: usize = 2;
const STATUS_WIDTH: usize = 13;

const HELP: [(&str, &str); 12] = [
    ("↑ ↓  j k", "выбрать узел"),
    ("PgUp PgDn", "страница вверх и вниз"),
    ("Home End", "в начало и в конец (g G)"),
    ("Enter", "закрепить выбранный узел"),
    ("a", "вернуть автоматический выбор узла"),
    ("u", "обновить все подписки"),
    (
        "U",
        "обновить подписку (выбранную Tab или узла под курсором)",
    ),
    ("/", "фильтр по тексту (Enter — применить, Esc — сбросить)"),
    ("Tab", "фильтр по подписке (Shift+Tab — в обратную сторону)"),
    ("Esc", "сбросить фильтры"),
    ("?", "эта справка"),
    ("q  Ctrl+C", "выйти"),
];

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App, palette: Palette) {
    draw_at(frame, app, palette, local_zone());
}

fn draw_at(frame: &mut Frame<'_>, app: &mut App, palette: Palette, zone: &TimeZone) {
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
    let top_limit = canvas.height.saturating_sub(BOTTOM + NODES_MIN);
    let mut y = banner(canvas, app, top_limit);
    canvas.set_dim(matches!(app.link, Link::Down { .. }));
    y = header(canvas, app, y, top_limit);

    let subs = subscription_lines(app, zone);
    let subs_want = if app.status.is_some() {
        FRAME + subs.len().max(1)
    } else {
        0
    };
    let body = canvas.height.saturating_sub(y + BOTTOM);
    let log_cap = (body / 5).clamp(3, 12);
    let log_want = FRAME + app.log.len().clamp(1, log_cap);
    let layout = plan(body, subs_want, log_want);
    let mut notes = Vec::new();
    if subs_want > 0 && layout.subs == 0 {
        notes.push("(подписки скрыты)");
    }
    if layout.log == 0 {
        notes.push("(журнал скрыт: мало строк)");
    }

    if layout.subs > 0 {
        subscriptions(canvas, y, layout.subs, subs, app);
        y += layout.subs;
    }
    nodes_section(canvas, y, layout.nodes, app, &notes);
    y += layout.nodes;
    if layout.log > 0 {
        log_section(canvas, y, layout.log, app, zone);
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
    let subs = if left > FRAME { left.min(subs_want) } else { 0 };
    left -= subs;
    let log = if left > FRAME { left.min(log_want) } else { 0 };
    left -= log;
    Plan {
        subs,
        nodes: nodes_min + left,
        log,
    }
}

fn banner(canvas: &mut Canvas<'_>, app: &App, limit: usize) -> usize {
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
        canvas.line(row, 0, segs);
    }
    count
}

fn header(canvas: &mut Canvas<'_>, app: &App, y: usize, limit: usize) -> usize {
    let count = limit.saturating_sub(y).min(HEADER_LINES);
    if count > 0 {
        header_top(canvas, app, y);
    }
    if count > 1 {
        header_node(canvas, app, y + 1);
    }
    y + count
}

fn link_seg(link: &Link) -> Seg {
    match link {
        Link::Up => Seg::new("● подключено", Tone::Green, 0),
        Link::Connecting => Seg::new("● подключение…", Tone::Yellow, 0),
        Link::Down { .. } => Seg::new("● нет связи", Tone::Red, 0),
    }
}

/// Xray показывается только когда с ним что-то не так.
fn xray_segs(xray: &XrayStatus) -> Vec<Seg> {
    let mut segs = Vec::new();
    if !xray.running {
        segs.push(Seg::new("xray не запущен", Tone::Red, 0));
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
    let choice = if node.pinned { "закреплён" } else { "авто" };
    let mut segs = vec![
        Seg::new(format!("▶ {}", node.id), Tone::Bold, 0),
        Seg::new(
            render::latency_text(node.latency_ms),
            delay_tone(node.latency_ms),
            0,
        ),
        Seg::new(choice, Tone::Dim, 0),
    ];
    if let Some(reason) = &node.reason {
        segs.push(Seg::new(reason.clone(), Tone::Dim, 0));
    }
    canvas.line(y, 0, segs);
}

/// Без данных задержка тусклая, иначе цвет по порогам из `render`.
fn delay_tone(latency: Option<u64>) -> Tone {
    if latency.is_none() {
        Tone::Dim
    } else {
        render::latency_tone(latency)
    }
}

fn subscription_segs(sub: &SubscriptionStatus, now: u64, zone: &TimeZone) -> Vec<Seg> {
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
        let (text, tone) = render::expiry_text(expire, now, zone);
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

fn subscription_lines(app: &App, zone: &TimeZone) -> Vec<Vec<Seg>> {
    let Some(status) = &app.status else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for sub in &status.subscriptions {
        lines.push(subscription_segs(sub, app.now, zone));
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
    let count = app
        .status
        .as_ref()
        .map_or(0, |status| status.subscriptions.len());
    let title = vec![
        Seg::new("Подписки", Tone::Bold, 0),
        Seg::new(format!("всего {count}"), Tone::Dim, 2),
    ];
    let mut inner = canvas.framed(y0, room, title);
    if lines.is_empty() {
        inner.line(0, 0, vec![Seg::new("подписок нет", Tone::Dim, 0)]);
        return;
    }
    let capacity = room.saturating_sub(FRAME);
    let total = lines.len();
    let shown = if total > capacity {
        capacity.saturating_sub(1)
    } else {
        total
    };
    for (row, segs) in lines.into_iter().take(shown).enumerate() {
        inner.line(row, 0, segs);
    }
    if shown < total {
        let text = format!("… и ещё строк: {}", total - shown);
        inner.line(shown, 0, vec![Seg::new(text, Tone::Dim, 0)]);
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
        spec(Kind::Status, "Статус", STATUS_WIDTH, STATUS_WIDTH, false),
        spec(Kind::Latency, "Задержка", 8, 8, true),
        spec(Kind::Sub, "Подписка", 8, sub.clamp(8, 20), false),
        spec(Kind::Failures, "Сбои", 7, 7, true),
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
            let (text, tone) = render::node_status_mark(node.status);
            (text.to_owned(), tone)
        }
        Kind::Latency => (
            render::latency_text(node.latency_ms),
            delay_tone(node.latency_ms),
        ),
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

fn nodes_title(app: &App, rows: usize, notes: &[&str]) -> Vec<Seg> {
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
        Seg::new("Узлы", Tone::Bold, 0),
        Seg::new(count, Tone::Dim, 2),
    ];
    let text = app.filter.text.trim();
    if !text.is_empty() {
        segs.push(Seg::new(format!("фильтр: «{text}»"), Tone::Yellow, 0));
    }
    if let Some(name) = &app.filter.subscription {
        segs.push(Seg::new(format!("подписка: {name}"), Tone::Yellow, 0));
    }
    segs.extend(notes.iter().map(|note| Seg::new(*note, Tone::Yellow, 3)));
    segs
}

fn nodes_section(canvas: &mut Canvas<'_>, y0: usize, room: usize, app: &mut App, notes: &[&str]) {
    let rows = room.saturating_sub(FRAME + 1);
    app.set_rows(rows);
    let mut inner = canvas.framed(y0, room, nodes_title(app, rows, notes));
    if app.visible.is_empty() {
        let (text, tone) = if app.nodes.is_empty() {
            (
                "Узлов нет: подписки ещё не получены или в них нет узлов",
                Tone::Dim,
            )
        } else {
            ("Под фильтр ничего не подошло: Esc — сбросить", Tone::Yellow)
        };
        inner.line(0, 0, vec![Seg::new(text, tone, 0)]);
        return;
    }
    let shown: Vec<&Node> = app
        .visible
        .iter()
        .filter_map(|index| app.nodes.get(*index))
        .collect();
    let cols = columns(&shown, inner.width);
    draw_titles(&mut inner, 0, &cols);
    for (row, node) in shown.iter().skip(app.offset).take(rows).enumerate() {
        let current = app.offset + row == app.cursor;
        draw_row(&mut inner, 1 + row, &cols, node, current);
    }
}

fn log_section(canvas: &mut Canvas<'_>, y0: usize, room: usize, app: &App, zone: &TimeZone) {
    let mut title = vec![Seg::new("Журнал", Tone::Bold, 0)];
    if is_utc(zone) {
        title.push(Seg::new("время UTC", Tone::Dim, 1));
    }
    let mut inner = canvas.framed(y0, room, title);
    if app.log.is_empty() {
        inner.line(0, 0, vec![Seg::new("пока пусто", Tone::Dim, 0)]);
        return;
    }
    let capacity = room.saturating_sub(FRAME);
    let first = app.log.len().saturating_sub(capacity);
    for (row, entry) in app.log.iter().skip(first).enumerate() {
        let segs = vec![
            Seg::new(format_moment(entry.at, app.now, zone), Tone::Dim, 0),
            Seg::new(entry.text.clone(), entry.tone, 0),
        ];
        inner.line(row, 0, segs);
    }
}

fn filter_hint(app: &App) -> Vec<Seg> {
    vec![
        Seg::new(format!("/{}█", app.filter.text), Tone::Bold, 0),
        Seg::new("Enter — применить, Esc — сбросить и выйти", Tone::Dim, 1),
    ]
}

/// Подсказки «клавиша описание»: клавиша жирным, описание тусклым.
fn normal_hints() -> Vec<Seg> {
    // Справка и выход отбрасываются последними: без них экран не объяснить.
    [
        ("↑↓ выбор", 1),
        ("Enter закрепить", 1),
        ("a авто", 2),
        ("u обновить", 3),
        ("U подписку", 5),
        ("/ фильтр", 2),
        ("Tab подписка", 6),
        ("? справка", 0),
        ("q выход", 0),
    ]
    .into_iter()
    .map(|(text, priority)| Seg::new(text, Tone::Dim, priority))
    .collect()
}

fn keys_line(canvas: &mut Canvas<'_>, y: usize, hints: Vec<Seg>) {
    let kept = fit(hints, canvas.width, GAP);
    let mut x = 0;
    for (index, seg) in kept.iter().enumerate() {
        if index > 0 {
            x += GAP;
        }
        let (key, desc) = seg
            .text
            .split_once(' ')
            .unwrap_or((seg.text.as_str(), ""));
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
    if app.input == InputMode::Filter {
        canvas.line(hints_y, 0, filter_hint(app));
    } else {
        keys_line(canvas, hints_y, normal_hints());
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::{Color, Modifier};
    use raycat_proto::{CurrentNode, Event, Mode, NodeStatus, Nodes, Status};

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

    #[test]
    fn a_full_screen_shows_everything_that_matters() {
        let mut app = sample_app();
        let s = |n: usize| " ".repeat(n);
        let d = |n: usize| "─".repeat(n);
        let expected = vec![
            format!(
                "raycat · шлюз · ● подключено · kill switch ●{}время работы 5 мин 12 с",
                s(13)
            ),
            "▶ main/NL-1  31 мс  закреплён  выбран лучший живой узел".to_owned(),
            format!("╭─ Подписки · всего 1 {}╮", d(57)),
            format!(
                "│ main  узлов: 3  3.0 МиБ из 100.0 ГиБ (0%)  обновлена 2 ч назад{}│",
                s(15)
            ),
            format!("╰{}╯", d(78)),
            format!("╭─ Узлы · всего 3 {}╮", d(61)),
            format!(
                "│    Подписка  Узел  Статус{}Задержка     Сбои  Ошибка{}│",
                s(9),
                s(18)
            ),
            format!("│ ★  main      NL-1  ● жив{}31 мс{}0{}│", s(13), s(8), s(26)),
            format!("│    main      DE-2  ● жив{}45 мс{}0{}│", s(13), s(8), s(26)),
            format!(
                "│    main      US-3  ✗ не отвечает{}—{}3  тайм-аут{}│",
                s(9),
                s(8),
                s(16)
            ),
            format!("│{}│", s(78)),
            format!("│{}│", s(78)),
            format!("│{}│", s(78)),
            format!("│{}│", s(78)),
            format!("╰{}╯", d(78)),
            format!("╭─ Журнал · время UTC {}╮", d(57)),
            format!("│ 12:00:00  подключено к демону, версия 0.1.0{}│", s(34)),
            format!("╰{}╯", d(78)),
            String::new(),
            "↑↓ выбор  Enter закрепить  a авто  u обновить  / фильтр  ? справка  q выход"
                .to_owned(),
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
        assert!(reversed(7));
        assert!(!reversed(8));
        press(&mut app, KeyCode::Down);
        let terminal = paint(&mut app, 80, 20, false);
        let buffer = terminal.backend().buffer();
        assert!(
            !buffer[(5, 7)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            buffer[(5, 8)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn without_a_daemon_the_screen_says_so() {
        let mut app = App::new(NOW);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[0], "raycat · ● подключение…");
        assert_eq!(lines[1], "данных от демона ещё нет");
        assert!(lines.iter().any(|line| line.contains("Узлов нет")));
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
        assert_eq!(lines[2], "raycat · ● нет связи");
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
        let dimmed = |y: u16| buffer[(0, y)].style().add_modifier.contains(Modifier::DIM);
        assert!(!dimmed(0));
        assert!(dimmed(2));
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
            lines[2],
            format!(
                "raycat · шлюз · ● нет связи · kill switch ●{}время работы 5 мин 12 с",
                " ".repeat(14)
            )
        );
        assert_eq!(buffer[(16, 2)].symbol(), "●");
        assert_eq!(buffer[(16, 2)].style().fg, Some(Color::LightRed));
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
        let lines = screen(&mut app, 44, 20);
        for line in &lines {
            assert!(display_width(line) <= 44, "{line:?}");
        }
        let header = lines.iter().find(|line| line.contains("Статус")).unwrap();
        assert!(!header.contains("Подписка"), "{header:?}");
        assert!(!header.contains("Сбои"), "{header:?}");
        assert!(header.contains("Задержка"), "{header:?}");
        let long = lines.iter().find(|line| line.contains("Германия")).unwrap();
        assert!(long.starts_with("│ ★  Германия, Ф…"), "{long:?}");
    }

    #[test]
    fn the_smallest_supported_terminal_still_shows_the_table() {
        let mut app = sample_app();
        let lines = screen(&mut app, 44, 12);
        for line in &lines {
            assert!(display_width(line) <= 44, "{line:?}");
        }
        assert!(lines.iter().any(|line| line.contains("NL-1")));
        assert_eq!(lines.last().unwrap(), "↑↓ выбор  ? справка  q выход");
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
    fn hidden_sections_are_named_in_the_nodes_title() {
        let mut app = sample_app();
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|line| line.starts_with(
                "╭─ Узлы · всего 3 · (журнал скрыт: мало строк) "
            )),
            "{lines:?}"
        );
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|line| line.starts_with(
                "╭─ Узлы · всего 3 · (подписки скрыты) · (журнал скрыт: мало строк) "
            )),
            "{lines:?}"
        );
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
        assert_eq!(lines[0], "демон недоступен: raycat не запущен");
        assert_eq!(lines[1], "Запустите службу: sudo systemctl start raycat");
        assert_eq!(lines[2], "повторное подключение через 1 с");
        assert_eq!(buffer[(0, 0)].style().fg, Some(Color::LightRed));
        assert_ne!(buffer[(0, 1)].style().fg, Some(Color::LightRed));
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
        assert!(
            lines.last().unwrap().starts_with("/de█"),
            "{:?}",
            lines.last()
        );
        assert!(
            lines
                .last()
                .unwrap()
                .ends_with("Enter — применить, Esc — сбросить и выйти")
        );
        assert!(lines.iter().any(|line| line.starts_with(
            "╭─ Узлы · показано 1 из 3 · фильтр: «de» "
        )));
        assert!(lines.iter().any(|line| line.contains("DE-2")));
        assert!(!lines.iter().any(|line| line.contains("US-3")));

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        let lines = screen(&mut app, 80, 20);
        assert!(
            lines.iter().any(|line| line.starts_with(
                "╭─ Узлы · всего 3 · подписка: main "
            )),
            "{lines:?}"
        );

        let mut empty = sample_app();
        press(&mut empty, KeyCode::Char('/'));
        press(&mut empty, KeyCode::Char('я'));
        let lines = screen(&mut empty, 80, 20);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Под фильтр ничего не подошло"))
        );
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
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("╭─ Узлы · 1–7 из 40 ")),
            "{lines:?}"
        );
        press(&mut app, KeyCode::End);
        let lines = screen(&mut app, 80, 20);
        assert!(lines.iter().any(|line| line.contains("N39")));
        assert!(!lines.iter().any(|line| line.contains("N00")));
        assert!(
            lines.iter().any(|line| line.contains("34–40 из 40")),
            "{lines:?}"
        );
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
        let at = lines
            .iter()
            .position(|line| line.starts_with("╭─ Подписки"))
            .unwrap();
        assert!(
            lines[at + 1].contains("⟳ идёт обновление"),
            "{:?}",
            lines[at + 1]
        );
        assert!(
            lines[at + 2].starts_with("│   ✗ панель ответила 403"),
            "{:?}",
            lines[at + 2]
        );
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
        assert!(
            lines
                .iter()
                .any(|line| line.contains("12:00:29  WARN: запись 29"))
        );
        assert!(!lines.iter().any(|line| line.contains("запись 0")));
    }

    #[test]
    fn help_replaces_the_screen_and_lists_every_key() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        let lines = screen(&mut app, 80, 24);
        assert_eq!(lines[0], "raycat tui: клавиши");
        let text = lines.join("\n");
        for word in [
            "Enter",
            "PgUp",
            "Home",
            "Tab",
            "Esc",
            "Ctrl+C",
            "U  ",
            "закрепить",
        ] {
            assert!(text.contains(word), "{word}: {text}");
        }
        assert!(text.contains("▶ выбран   ★ закреплён вручную"));
        assert!(text.contains("обновить подписку (выбранную Tab или узла под курсором)"));
        assert!(!text.contains("Подписки"));
        let narrow = screen(&mut app, 44, 12);
        for line in &narrow {
            assert!(display_width(line) <= 44, "{line:?}");
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
    fn frames_are_rounded_and_grey() {
        let mut app = sample_app();
        let terminal = paint(&mut app, 80, 20, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 2)].symbol(), "╭");
        assert_eq!(buffer[(79, 2)].symbol(), "╮");
        assert_eq!(buffer[(0, 3)].symbol(), "│");
        assert_eq!(buffer[(0, 4)].symbol(), "╰");
        assert_eq!(buffer[(0, 2)].style().fg, Some(Color::DarkGray));
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
    fn node_status_has_a_mark_and_a_color() {
        assert_eq!(
            render::node_status_mark(NodeStatus::Alive),
            ("● жив", Tone::Green)
        );
        assert_eq!(
            render::node_status_mark(NodeStatus::Dead),
            ("✗ не отвечает", Tone::Red)
        );
        assert_eq!(
            render::node_status_mark(NodeStatus::Unknown),
            ("○ не проверен", Tone::Dim)
        );
    }

    #[test]
    fn a_proxy_screen_has_no_kill_switch() {
        let mut status = sample_status();
        status.mode = Mode::Proxy;
        status.kill_switch = None;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].starts_with("raycat · прокси · ● подключено"));
        assert!(!lines[0].contains("kill switch"));
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
        assert_eq!(
            lines[0],
            "raycat · шлюз · ● подключено · xray не запущен · kill switch ●"
        );
        assert_eq!(lines[1], "узел: не выбран, узлов пока нет");

        let mut status = sample_status();
        status.xray.restarts = 2;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[0],
            "raycat · шлюз · ● подключено · xray перезапускался: 2 · kill switch ●"
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
            let total: usize = cols.iter().map(|col| col.width).sum::<usize>()
                + GAP * cols.len().saturating_sub(1);
            assert!(total <= width, "{width}: {total}");
            assert!(cols.iter().any(|col| col.kind == Kind::Name));
        }
        let wide = columns(&refs, 200);
        assert!(wide.iter().any(|col| col.kind == Kind::Traffic));
        let narrow = columns(&refs, 30);
        assert!(!narrow.iter().any(|col| col.kind == Kind::Traffic));
    }
}
