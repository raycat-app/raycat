//! Содержимое вкладок: обзор, узлы, подписки, журнал и настройки. Координаты абсолютные
//! (от левого верхнего угла экрана). Каждая вкладка регистрирует области нажатий мыши
//! в `App::hits` уже после рисования, чтобы не держать заимствования на время цикла.

use jiff::tz::TimeZone;
use raycat_proto::{Node, NodeStatus, SubscriptionStatus};

use super::app::{App, Hit, Link, SortBy, Target, names_label};
use super::canvas::{Canvas, GAP, Seg, highlight, wrap};
use super::view::{
    delay_tone, speed_label, sub_phrase, subscription_title, traffic_seg, xray_segs,
};
use crate::names::{NameStyle, country_code, display_name, node_reason};
use crate::render;
use crate::term::{Tone, display_width, pad, truncate};
use crate::util::{format_moment, is_utc};

/// Рамка занимает две строки (верх и низ) и два столбца сбоку — по одной ячейке отступа.
const FRAME: usize = 2;
/// Карточка подключения: рамка и две строки.
const CARD_ROWS: usize = 4;
/// Сколько последних событий показывает обзор.
const RECENT: usize = 5;
const STATUS_WIDTH: usize = 13;
const REASON_MAX: usize = 40;
/// Сколько символов масок видно на панели фильтров.
const HIDDEN_WIDTH: usize = 24;

type Chip = (String, Tone, Option<Target>);

/// Кнопки и переключатели в одну строку; каждый становится областью нажатия.
fn chips(canvas: &mut Canvas<'_>, app: &mut App, y: usize, items: Vec<Chip>) {
    let mut x = 0;
    for (text, tone, target) in items {
        let width = display_width(&text);
        canvas.put(x, y, &text, width, tone);
        if let Some(target) = target {
            app.hits.push(Hit::new(x, y, width, target));
        }
        x += width + GAP;
    }
}

pub(super) fn overview(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    top: usize,
    end: usize,
    zone: &TimeZone,
) {
    let mut y = top;
    if y + CARD_ROWS <= end {
        card(canvas, app, y);
        y += CARD_ROWS;
    }
    let count = app
        .status
        .as_ref()
        .map_or(0, |status| status.subscriptions.len());
    let sub_rows = FRAME + count.max(1);
    if y + sub_rows <= end {
        subscription_brief(canvas, app, y, sub_rows, zone);
        y += sub_rows;
    }
    if y < end {
        let items = overview_buttons(app);
        chips(canvas, app, y, items);
        y += 1;
    }
    if y + FRAME < end {
        events(canvas, app, y, end - y, zone);
    }
}

fn overview_buttons(app: &App) -> Vec<Chip> {
    let mut items = vec![
        (
            "[u] Обновить подписки".to_owned(),
            Tone::Bold,
            Some(Target::UpdateAll),
        ),
        (
            "[s] Тест скорости".to_owned(),
            Tone::Bold,
            Some(Target::Speed),
        ),
    ];
    if app
        .status
        .as_ref()
        .and_then(|status| status.node.as_ref())
        .is_some_and(|node| node.pinned)
    {
        items.push((
            "[a] Автовыбор".to_owned(),
            Tone::Bold,
            Some(Target::Autoselect),
        ));
    }
    items
}

fn card(canvas: &mut Canvas<'_>, app: &App, y: usize) {
    let mut inner = canvas.framed(y, CARD_ROWS, vec![Seg::new("Подключение", Tone::Bold, 0)]);
    inner.line(0, 0, state_segs(app));
    inner.line(1, 0, node_segs(app));
}

fn state_segs(app: &App) -> Vec<Seg> {
    let state = match (&app.link, &app.status) {
        (Link::Down { .. }, _) => Seg::new("● нет связи с демоном", Tone::Red, 0),
        (Link::Connecting, _) => Seg::new("● подключение…", Tone::Yellow, 0),
        (Link::Up, Some(status)) if status.node.is_none() => {
            if status.kill_switch == Some(true) {
                Seg::new("● kill switch держит трафик: узла нет", Tone::Yellow, 0)
            } else {
                Seg::new("● нет связи: узел не выбран", Tone::Red, 0)
            }
        }
        (Link::Up, _) => Seg::new("● подключено", Tone::Green, 0),
    };
    let mut segs = vec![state];
    if let Some(status) = &app.status {
        segs.push(Seg::new(render::mode_name(status.mode), Tone::Plain, 1));
        segs.extend(xray_segs(&status.xray));
    }
    segs
}

fn node_segs(app: &App) -> Vec<Seg> {
    let Some(status) = &app.status else {
        return vec![Seg::new("данных от демона ещё нет", Tone::Dim, 0)];
    };
    let Some(node) = &status.node else {
        return vec![Seg::new("узел не выбран", Tone::Yellow, 0)];
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
    let (down, up) = app.speed().map_or((0, 0), |speed| (speed.down, speed.up));
    segs.push(Seg::new(speed_label(down, up), Tone::Plain, 1));
    let choice = if node.pinned {
        "закреплён"
    } else {
        "авто"
    };
    segs.push(Seg::new(choice, Tone::Dim, 1));
    segs
}

fn subscription_brief(canvas: &mut Canvas<'_>, app: &App, y: usize, rows: usize, zone: &TimeZone) {
    let subs: &[SubscriptionStatus] = match &app.status {
        Some(status) => &status.subscriptions,
        None => &[],
    };
    let mut inner = canvas.framed(y, rows, subscription_title(subs.len()));
    if subs.is_empty() {
        inner.line(0, 0, vec![Seg::new("подписок нет", Tone::Dim, 0)]);
        return;
    }
    for (row, sub) in subs.iter().enumerate().take(rows.saturating_sub(FRAME)) {
        let mut segs = vec![Seg::new(sub.name.clone(), Tone::Bold, 0)];
        if let Some(used) = sub.used_bytes {
            segs.push(traffic_seg(used, sub.total_bytes));
        }
        if let Some(expire) = sub.expire {
            let (text, tone) = render::expiry_text(expire, app.now, zone);
            segs.push(Seg::new(text, tone, 2));
        }
        let (phrase, tone) = sub_phrase(sub, app.now);
        let phrase = if sub.last_error.is_some() {
            "ошибка — подробнее во вкладке «Подписки»"
        } else {
            phrase
        };
        segs.push(Seg::new(phrase, tone, 0));
        inner.line(row, 0, segs);
    }
}

/// Последние события в рамке: строк столько, сколько есть места, но не больше `RECENT`.
fn events(canvas: &mut Canvas<'_>, app: &App, y: usize, room: usize, zone: &TimeZone) {
    let height = room.min(FRAME + RECENT);
    let mut inner = canvas.framed(
        y,
        height,
        vec![Seg::new("Последние события", Tone::Bold, 0)],
    );
    if app.log.is_empty() {
        inner.line(0, 0, vec![Seg::new("пока пусто", Tone::Dim, 0)]);
        return;
    }
    let capacity = height.saturating_sub(FRAME);
    let recent: Vec<_> = app.log.iter().rev().take(capacity).collect();
    for (row, entry) in recent.into_iter().rev().enumerate() {
        let segs = vec![
            Seg::new(format_moment(entry.at, app.now, zone), Tone::Dim, 0),
            Seg::new(entry.text.clone(), entry.tone, 0),
        ];
        inner.line(row, 0, segs);
    }
}

pub(super) fn nodes(canvas: &mut Canvas<'_>, app: &mut App, top: usize, end: usize) {
    let panel = filter_panel(app);
    chips(canvas, app, top, panel);
    let y0 = top + 1;
    if y0 >= end {
        return;
    }
    if app.details && app.current().is_some() {
        node_details(canvas, app, y0, end);
    } else {
        nodes_table(canvas, app, y0, end);
    }
}

fn filter_panel(app: &App) -> Vec<Chip> {
    let dead = if app.filter.hide_dead { "✓" } else { "✗" };
    let subscription = app.filter.subscription.as_deref().unwrap_or("все");
    let search = match app.filter.text.trim() {
        "" => "—",
        text => text,
    };
    let hidden = match app.hidden.join(", ") {
        masks if masks.is_empty() => "нет".to_owned(),
        masks => truncate(&masks, HIDDEN_WIDTH),
    };
    vec![
        (
            format!("[d] Скрыть мёртвые {dead}"),
            Tone::Bold,
            Some(Target::DeadToggle),
        ),
        (
            format!("[s] Подписка: {subscription} ▾"),
            Tone::Plain,
            Some(Target::SubFilter),
        ),
        (
            format!("[/] Поиск: {search}"),
            Tone::Plain,
            Some(Target::Search),
        ),
        (
            format!("[h] Скрытые: {hidden}"),
            Tone::Plain,
            Some(Target::HiddenEdit),
        ),
    ]
}

fn nodes_table(canvas: &mut Canvas<'_>, app: &mut App, y0: usize, end: usize) {
    let room = end - y0;
    let rows = room.saturating_sub(FRAME + 1);
    app.set_rows(rows);
    let title = nodes_title(app);
    let speed = app
        .speed()
        .map_or_else(String::new, |speed| speed_label(speed.down, speed.up));
    let (sort, descending) = (app.filter.sort, app.filter.descending);
    let full = canvas.width;
    let width = full.saturating_sub(4);
    let mut found = Vec::new();
    {
        let shown: Vec<&Node> = app
            .visible
            .iter()
            .filter_map(|index| app.nodes.get(*index))
            .collect();
        let mut inner = canvas.framed(y0, room, title);
        if shown.is_empty() {
            let (text, tone) = if app.nodes.is_empty() {
                (
                    "Узлов нет: подписки ещё не получены или в них нет узлов",
                    Tone::Dim,
                )
            } else {
                ("Под фильтр ничего не подошло: Esc — сбросить", Tone::Yellow)
            };
            inner.line(0, 0, vec![Seg::new(text, tone, 0)]);
        } else {
            let cols = columns(&shown, &speed, width, app.names);
            for (x, col_width, by) in draw_titles(&mut inner, 0, &cols, sort, descending) {
                found.push(Hit::new(2 + x, y0 + 1, col_width, Target::Sort(by)));
            }
            for (row, node) in shown.iter().skip(app.offset).take(rows).enumerate() {
                let index = app.offset + row;
                let current = index == app.cursor;
                draw_row(&mut inner, 1 + row, &cols, node, &speed, current, app.names);
                found.push(Hit::new(0, y0 + 2 + row, full, Target::NodeRow(index)));
            }
        }
    }
    app.hits.extend(found);
}

fn nodes_title(app: &App) -> Vec<Seg> {
    let total = app.nodes.len();
    let shown = app.visible.len();
    let mut segs = vec![Seg::new(
        format!("Узлы · {shown} из {total}"),
        Tone::Bold,
        0,
    )];
    if shown < total {
        segs.push(Seg::new(
            format!("({} скрыто)", total - shown),
            Tone::Dim,
            2,
        ));
    }
    segs
}

/// Подробности выбранного узла вместо таблицы. Кнопка «закрепить» — первая строка
/// под именем.
fn node_details(canvas: &mut Canvas<'_>, app: &mut App, y0: usize, end: usize) {
    let room = end - y0;
    let width = canvas.width.saturating_sub(4);
    let Some(node) = app.current() else {
        return;
    };
    let (status_text, status_tone) = render::node_status_mark(node.status);
    let selected = if node.selected {
        "выбран"
    } else {
        "не выбран"
    };
    let choice = if node.pinned {
        "закреплён вручную"
    } else {
        "автоматический выбор"
    };
    let reason = node.last_error.as_deref().and_then(node_reason);
    let reason_tone = if node.last_error.is_some() {
        Tone::Red
    } else {
        Tone::Dim
    };
    let country = country_code(&node.name).unwrap_or_else(|| "—".to_owned());
    let mut lines: Vec<Vec<Seg>> = vec![
        vec![Seg::new(display_name(&node.name, app.names), Tone::Bold, 0)],
        vec![
            Seg::new("[p] закрепить", Tone::Bold, 0),
            Seg::new("[t] тест скорости", Tone::Bold, 0),
            Seg::new("[Esc] назад", Tone::Dim, 1),
        ],
        vec![
            Seg::new(format!("Подписка: {}", node.subscription), Tone::Plain, 0),
            Seg::new(format!("страна: {country}"), Tone::Plain, 1),
        ],
        vec![
            Seg::new(status_text, status_tone, 0),
            Seg::new(
                format!("задержка {}", render::latency_text(node.latency_ms)),
                delay_tone(node.latency_ms),
                0,
            ),
            Seg::new(format!("сбоев {}", node.failures), Tone::Plain, 0),
        ],
        vec![Seg::new(
            format!("Выбор: {selected}, {choice}"),
            Tone::Plain,
            0,
        )],
        vec![Seg::new(
            format!("Причина: {}", reason.unwrap_or("нет")),
            reason_tone,
            0,
        )],
        vec![Seg::new(
            format!("Трафик: {}", render::node_traffic(node)),
            Tone::Dim,
            1,
        )],
        vec![Seg::new(
            format!("Тег outbound: {}", node.tag),
            Tone::Dim,
            2,
        )],
        vec![Seg::new("Ошибка полностью:", Tone::Dim, 1)],
    ];
    let error = node.last_error.as_deref().unwrap_or("—");
    for line in wrap(error, width, 4) {
        lines.push(vec![Seg::new(line, Tone::Dim, 0)]);
    }
    let pin_width = display_width("[p] закрепить");
    let mut inner = canvas.framed(
        y0,
        room,
        vec![Seg::new("Узел · подробности", Tone::Bold, 0)],
    );
    for (row, segs) in lines.into_iter().enumerate() {
        inner.line(row, 0, segs);
    }
    app.hits.push(Hit::new(2, y0 + 2, pin_width, Target::Pin));
    let speed_width = display_width("[t] тест скорости");
    app.hits.push(Hit::new(
        2 + pin_width + GAP,
        y0 + 2,
        speed_width,
        Target::SpeedNode,
    ));
}

/// Колонки в порядке важности: на узком экране последние пропадают первыми. Скорость
/// есть только у выбранного узла и только когда её измерили.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Marker,
    Sub,
    Name,
    Country,
    Status,
    Latency,
    Failures,
    Reason,
    Speed,
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

fn specs(nodes: &[&Node], speed: &str, style: NameStyle) -> Vec<Spec> {
    let (mut sub, mut name, mut reason) = (0_usize, 0_usize, 0_usize);
    for node in nodes {
        sub = sub.max(display_width(&node.subscription));
        name = name.max(display_width(&display_name(&node.name, style)));
        reason = reason.max(
            node.last_error
                .as_deref()
                .and_then(node_reason)
                .map_or(0, display_width),
        );
    }
    let name = name.clamp(4, 48);
    let reason = reason.min(REASON_MAX);
    let speed_width = if speed.is_empty() {
        0
    } else {
        display_width(speed).max(display_width("Скорость"))
    };
    let spec = |kind, title, min, natural, right| Spec {
        kind,
        title,
        min,
        natural,
        right,
    };
    vec![
        spec(Kind::Marker, "", 2, 2, false),
        spec(Kind::Name, "Узел", name.min(12), name, false),
        spec(Kind::Status, "Статус", STATUS_WIDTH, STATUS_WIDTH, false),
        spec(Kind::Latency, "Задержка", 10, 10, true),
        spec(Kind::Country, "Страна", 6, 6, false),
        spec(Kind::Sub, "Подписка", 10, sub.clamp(10, 20), false),
        spec(Kind::Failures, "Сбои", 5, 5, true),
        spec(Kind::Reason, "Причина", reason.min(12), reason, false),
        spec(Kind::Speed, "Скорость", speed_width, speed_width, false),
    ]
}

/// Берёт колонки, пока помещаются минимальные ширины, и раздаёт остаток тем,
/// что могут расти: имени, подписке и причине.
fn columns(nodes: &[&Node], speed: &str, width: usize, style: NameStyle) -> Vec<Col> {
    let mut chosen: Vec<(Spec, usize)> = Vec::new();
    let mut used = 0;
    for spec in specs(nodes, speed, style) {
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
    for kind in [Kind::Name, Kind::Sub, Kind::Reason] {
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

fn cell(kind: Kind, node: &Node, speed: &str, style: NameStyle) -> (String, Tone) {
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
            (display_name(&node.name, style), tone)
        }
        Kind::Country => (
            country_code(&node.name).unwrap_or_else(|| "—".to_owned()),
            Tone::Plain,
        ),
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
        Kind::Reason => (
            node.last_error
                .as_deref()
                .and_then(node_reason)
                .unwrap_or("")
                .to_owned(),
            Tone::Red,
        ),
        Kind::Speed if node.selected => (speed.to_owned(), Tone::Plain),
        Kind::Speed => (String::new(), Tone::Plain),
    }
}

fn draw_row(
    canvas: &mut Canvas<'_>,
    y: usize,
    cols: &[Col],
    node: &Node,
    speed: &str,
    current: bool,
    style: NameStyle,
) {
    let marked = highlight();
    if current {
        canvas.fill(y, marked);
    }
    let mut x = 0;
    for col in cols {
        let (text, tone) = cell(col.kind, node, speed, style);
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

/// Заголовки столбцов; активный помечен стрелкой. Возвращает (x, ширина, порядок) для
/// столбцов, по которым можно сортировать.
fn draw_titles(
    canvas: &mut Canvas<'_>,
    y: usize,
    cols: &[Col],
    sort: SortBy,
    descending: bool,
) -> Vec<(usize, usize, SortBy)> {
    let mut x = 0;
    let mut clicks = Vec::new();
    for col in cols {
        let by = sort_of(col.kind);
        let (label, tone) = match by {
            Some(by) if by == sort => {
                let arrow = if descending { "▼" } else { "▲" };
                (format!("{} {arrow}", col.title), Tone::Bold)
            }
            _ => (col.title.to_owned(), Tone::Dim),
        };
        let shift = if col.right {
            col.width.saturating_sub(display_width(&label))
        } else {
            0
        };
        canvas.put(x + shift, y, &label, col.width, tone);
        if let Some(by) = by {
            clicks.push((x, col.width, by));
        }
        x += col.width + GAP;
    }
    clicks
}

fn sort_of(kind: Kind) -> Option<SortBy> {
    match kind {
        Kind::Name => Some(SortBy::Name),
        Kind::Sub => Some(SortBy::Subscription),
        Kind::Status => Some(SortBy::Status),
        Kind::Latency => Some(SortBy::Latency),
        _ => None,
    }
}

pub(super) fn subscriptions(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    top: usize,
    end: usize,
    zone: &TimeZone,
) {
    let buttons = vec![(
        "[u] Обновить подписку".to_owned(),
        Tone::Bold,
        Some(Target::UpdateSub),
    )];
    chips(canvas, app, top, buttons);
    let y0 = top + 1;
    if y0 >= end {
        return;
    }
    let room = end - y0;
    let count = app
        .status
        .as_ref()
        .map_or(0, |status| status.subscriptions.len());
    app.sub_cursor = app.sub_cursor.min(count.saturating_sub(1));
    let selected = app.sub_cursor;
    let list_rows = (count.max(1) + FRAME).min(room);
    let found = sub_list(canvas, app, y0, list_rows, selected, zone);
    app.hits.extend(found);
    if app.details {
        sub_details(canvas, app, y0 + list_rows, selected, zone);
    }
}

/// Список подписок: одна строка на подписку, выбранная помечена стрелкой.
fn sub_list(
    canvas: &mut Canvas<'_>,
    app: &App,
    y0: usize,
    rows: usize,
    selected: usize,
    zone: &TimeZone,
) -> Vec<Hit> {
    let full = canvas.width;
    let subs: &[SubscriptionStatus] = match &app.status {
        Some(status) => &status.subscriptions,
        None => &[],
    };
    let mut inner = canvas.framed(y0, rows, subscription_title(subs.len()));
    if subs.is_empty() {
        inner.line(0, 0, vec![Seg::new("подписок нет", Tone::Dim, 0)]);
        return Vec::new();
    }
    let mut found = Vec::new();
    for (index, sub) in subs.iter().enumerate().take(rows.saturating_sub(FRAME)) {
        let mark = if index == selected { "▶ " } else { "  " };
        let alive = app
            .nodes
            .iter()
            .filter(|node| node.subscription == sub.name && node.status == NodeStatus::Alive)
            .count();
        let mut segs = vec![
            Seg::new(mark, Tone::Green, 0),
            Seg::new(sub.name.clone(), Tone::Bold, 0),
        ];
        if let Some(title) = sub.title.as_deref().filter(|title| !title.is_empty()) {
            segs.push(Seg::new(format!("«{title}»"), Tone::Dim, 3));
        }
        segs.push(Seg::new(
            format!("узлов {alive}/{}", sub.nodes),
            Tone::Plain,
            2,
        ));
        if let Some(used) = sub.used_bytes {
            segs.push(traffic_seg(used, sub.total_bytes));
        }
        if let Some(expire) = sub.expire {
            let (text, tone) = render::expiry_text(expire, app.now, zone);
            segs.push(Seg::new(text, tone, 2));
        }
        let (phrase, tone) = sub_phrase(sub, app.now);
        segs.push(Seg::new(phrase, tone, 0));
        inner.line(index, 0, segs);
        found.push(Hit::new(0, y0 + 1 + index, full, Target::SubRow(index)));
    }
    found
}

/// Подробности выбранной подписки: провайдер, трафик, сроки, полная ошибка.
fn sub_details(canvas: &mut Canvas<'_>, app: &App, top: usize, selected: usize, zone: &TimeZone) {
    let Some(sub) = app
        .status
        .as_ref()
        .and_then(|status| status.subscriptions.get(selected))
    else {
        return;
    };
    let width = canvas.width.saturating_sub(4);
    let now = app.now;
    let provider = sub
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("не указан");
    let traffic = sub.used_bytes.map_or_else(
        || "—".to_owned(),
        |used| render::traffic_text(used, sub.total_bytes).0,
    );
    let expiry = sub.expire.map_or_else(
        || "—".to_owned(),
        |expire| render::expiry_text(expire, now, zone).0,
    );
    let updated = match sub.updated_at {
        Some(then) => format!(
            "{}, следующая {}",
            render::ago(now, then),
            sub.next_update.map_or_else(
                || "не запланирована".to_owned(),
                |next| render::ahead(now, next)
            )
        ),
        None => "ещё не получена".to_owned(),
    };
    let mut lines: Vec<Vec<Seg>> = vec![
        vec![Seg::new(format!("Провайдер: {provider}"), Tone::Plain, 0)],
        vec![Seg::new(format!("Трафик: {traffic}"), Tone::Plain, 0)],
        vec![Seg::new(format!("Срок: {expiry}"), Tone::Plain, 0)],
        vec![Seg::new(format!("Обновлена: {updated}"), Tone::Plain, 0)],
    ];
    match &sub.last_error {
        Some(error) => {
            lines.push(vec![Seg::new("Ошибка полностью:", Tone::Red, 0)]);
            for line in wrap(error, width, 4) {
                lines.push(vec![Seg::new(line, Tone::Red, 0)]);
            }
        }
        None => lines.push(vec![Seg::new("Ошибок нет", Tone::Dim, 0)]),
    }
    for (row, segs) in lines.into_iter().enumerate() {
        canvas.line(top + row, 2, segs);
    }
}

pub(super) fn journal(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    top: usize,
    end: usize,
    zone: &TimeZone,
) {
    let label = if app.journal_warnings {
        "[w] Только предупреждения: да"
    } else {
        "[w] Только предупреждения: нет"
    };
    let items = vec![(label.to_owned(), Tone::Bold, Some(Target::WarningsToggle))];
    chips(canvas, app, top, items);
    let y0 = top + 1;
    if y0 >= end {
        return;
    }
    let room = end - y0;
    let capacity = room.saturating_sub(FRAME);
    let (lines, back) = journal_lines(app, zone, canvas.width.saturating_sub(4), capacity);
    app.journal_back = back;
    let mut title = vec![Seg::new("Журнал", Tone::Bold, 0)];
    if is_utc(zone) {
        title.push(Seg::new("время UTC", Tone::Dim, 1));
    }
    let mut inner = canvas.framed(y0, room, title);
    if lines.is_empty() {
        let text = if app.journal_warnings {
            "предупреждений нет"
        } else {
            "пока пусто"
        };
        inner.line(0, 0, vec![Seg::new(text, Tone::Dim, 0)]);
        return;
    }
    let end_index = lines.len() - back;
    let start = end_index.saturating_sub(capacity);
    for (row, segs) in lines
        .into_iter()
        .skip(start)
        .take(end_index - start)
        .enumerate()
    {
        inner.line(row, 0, segs);
    }
}

/// Строки журнала с переносом длинных сообщений. Возвращает строки и прокрутку, ужатую
/// до допустимой (0 — последние строки видны).
fn journal_lines(
    app: &App,
    zone: &TimeZone,
    width: usize,
    capacity: usize,
) -> (Vec<Vec<Seg>>, usize) {
    let entries = app.journal_entries();
    let times: Vec<String> = entries
        .iter()
        .map(|entry| format_moment(entry.at, app.now, zone))
        .collect();
    let column = times
        .iter()
        .map(|time| display_width(time))
        .max()
        .unwrap_or(0)
        .max(8)
        + GAP;
    let text_width = width.saturating_sub(column).max(1);
    let mut lines: Vec<Vec<Seg>> = Vec::new();
    for (entry, time) in entries.iter().zip(times) {
        let texts = match wrap(&entry.text, text_width, usize::MAX) {
            texts if texts.is_empty() => vec![String::new()],
            texts => texts,
        };
        for (index, text) in texts.into_iter().enumerate() {
            let lead = if index == 0 {
                pad(&time, column)
            } else {
                " ".repeat(column)
            };
            lines.push(vec![
                Seg::new(lead, Tone::Dim, 0),
                Seg::new(text, entry.tone, 0),
            ]);
        }
    }
    let back = app.journal_back.min(lines.len().saturating_sub(capacity));
    (lines, back)
}

pub(super) fn settings(canvas: &mut Canvas<'_>, app: &mut App, top: usize, end: usize) {
    let mut y = top;
    for segs in summary_lines(app) {
        if y + 1 >= end {
            return;
        }
        canvas.line(y, 0, segs);
        y += 1;
    }
    y += 1;
    if y + 4 > end {
        return;
    }
    let label = format!("Значки в именах: {}", names_label(app.names));
    let explain = match app.names {
        NameStyle::Country => "флаги заменяются кодом страны (DE), эмодзи убираются",
        NameStyle::AsIs => "имена показываются без изменений, флаги зависят от терминала",
    };
    let width = display_width(&label);
    let mut inner = canvas.framed(y, 4, vec![Seg::new("Вид", Tone::Bold, 0)]);
    inner.line(0, 0, vec![Seg::new(label, Tone::Bold, 0)]);
    inner.line(1, 0, vec![Seg::new(explain, Tone::Dim, 0)]);
    app.hits
        .push(Hit::new(2, y + 1, width, Target::NamesToggle));
    y += 4;
    if y < end {
        let path = if app.settings_path.is_empty() {
            "не найден".to_owned()
        } else {
            app.settings_path.clone()
        };
        let text = format!("Настройки меняются в файле: {path}; raycat config покажет итог");
        canvas.line(y, 0, vec![Seg::new(text, Tone::Dim, 0)]);
    }
}

fn summary_lines(app: &App) -> Vec<Vec<Seg>> {
    let Some(status) = &app.status else {
        return vec![vec![Seg::new("данных от демона ещё нет", Tone::Dim, 0)]];
    };
    let kill = match status.kill_switch {
        Some(true) => "включён",
        Some(false) => "выключен",
        None => "нет (режим прокси)",
    };
    let uptime = status.uptime_secs + app.now.saturating_sub(app.status_at);
    let node = status.node.as_ref().map_or_else(
        || "не выбран".to_owned(),
        |node| display_name(&node.name, app.names),
    );
    vec![
        vec![Seg::new("Состояние демона", Tone::Bold, 0)],
        vec![Seg::new(
            format!("Версия: {}", status.version),
            Tone::Plain,
            0,
        )],
        vec![Seg::new(
            format!("Режим: {}", render::mode_name(status.mode)),
            Tone::Plain,
            0,
        )],
        vec![Seg::new(format!("Kill switch: {kill}"), Tone::Plain, 0)],
        vec![Seg::new(
            format!("Время работы: {}", render::span(uptime)),
            Tone::Plain,
            0,
        )],
        vec![Seg::new(
            format!("Подписок: {}", status.subscriptions.len()),
            Tone::Plain,
            0,
        )],
        vec![Seg::new(format!("Текущий узел: {node}"), Tone::Plain, 0)],
    ]
}
