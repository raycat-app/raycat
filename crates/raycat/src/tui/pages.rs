//! Содержимое вкладок: обзор, узлы, подписки, журнал и настройки. Координаты абсолютные
//! (от левого верхнего угла экрана). Каждая вкладка регистрирует области нажатий мыши
//! в `App::hits` уже после рисования, чтобы не держать заимствования на время цикла.

use jiff::tz::TimeZone;
use raycat_proto::{Node, NodeCheck, NodeStatus, SubscriptionStatus};

use super::app::{App, Hit, InputMode, LEVELS, Setting, Shown, SortBy, Target};
use super::canvas::{Canvas, GAP, Seg, wrap};
use super::view::{
    bar, delay_tone, is_expired, speed_label, speed_part, sub_phrase, subscription_title,
    traffic_seg, tunnel_hint, xray_segs,
};
use crate::names::{NameStyle, country_code, display_name, node_reason, node_reason_short};
use crate::render;
use crate::term::{Tone, display_width, pad, truncate};
use crate::util::{format_moment, is_utc};

/// Рамка занимает две строки (верх и низ) и два столбца сбоку — по одной ячейке отступа.
const FRAME: usize = 2;
const STATUS_WIDTH: usize = 13;
const REASON_MAX: usize = 40;
/// Строки шапки подробностей узла: имя и кнопки. Остальное прокручивается.
const NODE_HEADER_ROWS: usize = 2;
/// Сколько символов масок видно в строке фильтров.
const HIDDEN_WIDTH: usize = 24;
/// Высота панелей «Туннель» и «Подписки» на обзоре: рамка, узел, задержка, две строки скорости.
const TUNNEL_ROWS: usize = FRAME + 4;
/// Ширина подписи скорости в панели туннеля, например `↓ 43,5 Мбит/с`.
const SPEED_LABEL: usize = 16;
/// Меньше стольких столбцов график не рисуется: он исчезает первым на узком экране.
const GRAPH_MIN: usize = 3;
/// Ниже этой ширины панели обзора идут друг под другом.
const SPLIT_MIN: usize = 90;
/// Ниже этой ширины подробности узла открываются окном поверх таблицы.
const NODE_SPLIT: usize = 120;
/// Ширина окна подробностей узла не больше этого числа ячеек.
const WINDOW_MAX: usize = 96;

/// Разделы вкладки «Настройки» в порядке показа. Заголовок раздела — строка списка,
/// которую выбор клавишами пропускает.
const SECTIONS: [(&str, &[Setting]); 4] = [
    ("Подключение", &[Setting::RuDirect, Setting::KillSwitch]),
    (
        "Выбор узла",
        &[
            Setting::CheckInterval,
            Setting::Failures,
            Setting::SwitchGain,
            Setting::ReturnDelay,
        ],
    ),
    ("Система", &[Setting::Xhttp, Setting::LogLevel]),
    ("Вид", &[Setting::Names]),
];
/// Ниже этой ширины правая панель подробностей не помещается: её текст идёт строками
/// под списком.
const SETTINGS_SPLIT: usize = 90;
/// Строки под списком на узкой ширине: пояснение, действие, файл и примечание.
const NARROW_ROWS: usize = 4;
/// Ширина подписи в панели подробностей.
const LABEL: usize = 17;
const FIELD_HINT: &str = "Enter — сохранить · Esc — отмена · например 90s, 5m или 3";

pub(super) type Chip = (String, Tone, Option<Target>);

/// Кнопки и переключатели в одну строку; каждый становится областью нажатия.
pub(super) fn chips(canvas: &mut Canvas<'_>, app: &mut App, y: usize, items: Vec<Chip>) {
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
    let count = app
        .status
        .as_ref()
        .map_or(0, |status| status.subscriptions.len());
    let sub_rows = FRAME + count.max(1);
    let wide = canvas.width >= SPLIT_MIN;
    // На широком экране «Туннель» и «Подписки» рядом и одной высоты.
    let row = if wide {
        TUNNEL_ROWS.max(sub_rows)
    } else {
        TUNNEL_ROWS
    };
    if y + row <= end {
        if wide {
            let left = canvas.width / 2;
            tunnel_panel(canvas, app, 0, y, left, row);
            subscription_panel(canvas, app, left, y, canvas.width - left, row, zone);
        } else {
            tunnel_panel(canvas, app, 0, y, canvas.width, row);
        }
        y += row;
    }
    if !wide && y + sub_rows <= end {
        subscription_panel(canvas, app, 0, y, canvas.width, sub_rows, zone);
        y += sub_rows;
    }
    let items = overview_buttons(app);
    if !items.is_empty() && y < end {
        chips(canvas, app, y, items);
        y += 1;
    }
    if y + FRAME < end {
        events(canvas, app, y, end - y, zone);
    }
}

fn overview_buttons(app: &App) -> Vec<Chip> {
    let mut items = Vec::new();
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

/// Панель туннеля: выбранный узел с задержкой и выбором, скорость с графиком. Без живых
/// узлов вместо скорости показывается пояснение.
fn tunnel_panel(
    canvas: &mut Canvas<'_>,
    app: &App,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    let border = canvas.chrome();
    let title = vec![Seg::new("Туннель", Tone::Bold, 0)];
    let mut inner = canvas.boxed(x, y, width, height, title, border);
    let Some(status) = &app.status else {
        inner.line(
            0,
            0,
            vec![Seg::new("данных от демона ещё нет", Tone::Dim, 0)],
        );
        return;
    };
    if let Some(node) = &status.node {
        let name = format!("► {}", display_name(&node.name, app.names));
        inner.line(0, 0, vec![Seg::new(name, Tone::Bold, 0)]);
        let choice = if node.pinned {
            "закреплён"
        } else {
            "авто"
        };
        let mut segs = vec![
            Seg::new(
                render::latency_text(node.latency_ms),
                delay_tone(node.latency_ms),
                0,
            ),
            Seg::new(choice, Tone::Dim, 1),
        ];
        segs.extend(xray_segs(&status.xray));
        inner.line(1, 0, segs);
    } else {
        inner.line(0, 0, vec![Seg::new("узел не выбран", Tone::Yellow, 0)]);
    }
    if let Some(hint) = tunnel_hint(app) {
        inner.line(2, 0, vec![Seg::new(hint, Tone::Dim, 0)]);
    } else {
        let down: Vec<u64> = app.speeds.iter().map(|speed| speed.down).collect();
        let up: Vec<u64> = app.speeds.iter().map(|speed| speed.up).collect();
        let now = app.speed();
        speed_row(&mut inner, 2, "↓", now.map(|speed| speed.down), &down);
        speed_row(&mut inner, 3, "↑", now.map(|speed| speed.up), &up);
    }
}

/// Строка скорости: стрелка, значение и график за последние замеры. Нет замера — `—`.
fn speed_row(inner: &mut Canvas<'_>, row: usize, arrow: &str, bits: Option<u64>, history: &[u64]) {
    let Some(bits) = bits else {
        inner.line(row, 0, vec![Seg::new(format!("{arrow} —"), Tone::Dim, 0)]);
        return;
    };
    let label = pad(&format!("{arrow} {}", speed_part(bits)), SPEED_LABEL);
    let cells = inner.width.saturating_sub(SPEED_LABEL + GAP);
    let mut segs = vec![Seg::new(label, Tone::Plain, 0)];
    if cells >= GRAPH_MIN && !history.is_empty() {
        segs.push(Seg::new(sparkline(history, cells), Tone::Plain, 1));
    }
    inner.line(row, 0, segs);
}

/// Столбики `▁▂▃▄▅▆▇` по последним значениям: высота относительно самого большого из них.
fn sparkline(values: &[u64], cells: usize) -> String {
    const BARS: [char; 7] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇'];
    let shown = &values[values.len().saturating_sub(cells)..];
    let max = shown.iter().copied().max().unwrap_or(0);
    shown
        .iter()
        .map(|&value| {
            let level = if max == 0 {
                0
            } else {
                usize::try_from(u128::from(value) * 6 / u128::from(max)).unwrap_or(6)
            };
            BARS[level.min(6)]
        })
        .collect()
}

/// Панель подписок: по строке на подписку — имя, полоска трафика, процент, срок и состояние.
fn subscription_panel(
    canvas: &mut Canvas<'_>,
    app: &App,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    zone: &TimeZone,
) {
    let subs: &[SubscriptionStatus] = match &app.status {
        Some(status) => &status.subscriptions,
        None => &[],
    };
    let border = canvas.chrome();
    let title = vec![
        Seg::new("Подписки", Tone::Bold, 0),
        Seg::new(subs.len().to_string(), Tone::Dim, 0),
    ];
    let mut inner = canvas.boxed(x, y, width, height, title, border);
    if subs.is_empty() {
        inner.line(0, 0, vec![Seg::new("подписок нет", Tone::Dim, 0)]);
        return;
    }
    let rows = inner.height;
    for (row, sub) in subs.iter().enumerate().take(rows) {
        inner.line(row, 0, sub_brief_segs(sub, app.now, zone));
    }
}

/// Строка подписки на обзоре. Полоска окрашена по заполнению: до 70% зелёная, до 90%
/// жёлтая, дальше красная.
fn sub_brief_segs(sub: &SubscriptionStatus, now: u64, zone: &TimeZone) -> Vec<Seg> {
    let mut segs = vec![Seg::new(sub.name.clone(), Tone::Bold, 0)];
    if let Some(used) = sub.used_bytes
        && let Some(total) = sub.total_bytes.filter(|total| *total > 0)
    {
        let percent = u128::from(used) * 100 / u128::from(total);
        let tone = match percent {
            0..=70 => Tone::Green,
            71..=90 => Tone::Yellow,
            _ => Tone::Red,
        };
        segs.push(Seg::new(format!("{} {percent}%", bar(percent)), tone, 1));
    }
    if let Some(expire) = sub.expire
        && !is_expired(sub, now)
    {
        let (text, tone) = render::expiry_text(expire, now, zone);
        segs.push(Seg::new(text, tone, 2));
    }
    let (phrase, tone) = sub_phrase(sub, now, zone);
    segs.push(Seg::new(phrase, tone, 0));
    segs
}

/// Последние события в рамке на всё оставшееся место: сколько строк помещается, столько и видно.
fn events(canvas: &mut Canvas<'_>, app: &App, y: usize, height: usize, zone: &TimeZone) {
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
    let wide = canvas.width >= NODE_SPLIT;
    app.details_window = !wide;
    filter_row(canvas, app, top);
    let y0 = top + 1;
    if y0 >= end {
        return;
    }
    let room = end - y0;
    if wide {
        // Подробности выбранного узла всегда справа: Enter для них не нужен.
        let table = canvas.width * 65 / 100;
        nodes_table(canvas, app, y0, end, table);
        if app.current().is_some() {
            node_details(canvas, app, table, y0, canvas.width - table, room);
        }
    } else {
        nodes_table(canvas, app, y0, end, canvas.width);
        if app.details && app.current().is_some() {
            node_window(canvas, app, y0, room);
        }
    }
}

/// Фильтры одной строкой над таблицей: включённые акцентом, выключенные серым, клавиша
/// в плашке. Каждый фильтр — область нажатия.
fn filter_row(canvas: &mut Canvas<'_>, app: &mut App, y: usize) {
    let subscription = app.filter.subscription.clone();
    let search = app.filter.text.trim().to_owned();
    let hidden = app.hidden.join(", ");
    let items = vec![
        (
            "d",
            if app.filter.hide_dead {
                "мёртвые скрыты".to_owned()
            } else {
                "мёртвые".to_owned()
            },
            app.filter.hide_dead,
            Target::DeadToggle,
        ),
        (
            "s",
            match subscription {
                Some(name) => format!("подписка: {name}"),
                None => "подписка".to_owned(),
            },
            app.filter.subscription.is_some(),
            Target::SubFilter,
        ),
        (
            "/",
            if search.is_empty() {
                "поиск".to_owned()
            } else {
                format!("поиск: {search}")
            },
            !search.is_empty(),
            Target::Search,
        ),
        (
            "h",
            if hidden.is_empty() {
                "скрытые".to_owned()
            } else {
                format!("скрытые: {}", truncate(&hidden, HIDDEN_WIDTH))
            },
            !hidden.is_empty(),
            Target::HiddenEdit,
        ),
        (
            "e",
            if app.filter.hide_excluded {
                "исключённые скрыты".to_owned()
            } else {
                "исключённые показаны".to_owned()
            },
            app.filter.hide_excluded,
            Target::ExcludedToggle,
        ),
    ];
    let key_style = canvas.key_style();
    let mut x = 0;
    for (key, label, active, target) in items {
        let chip = format!(" {key} ");
        let text = format!(" {label}");
        let chip_width = display_width(&chip);
        let width = chip_width + display_width(&text);
        canvas.put_style(x, y, &chip, chip_width, key_style);
        let tone = if active { Tone::Header } else { Tone::Dim };
        canvas.put(x + chip_width, y, &text, width - chip_width, tone);
        app.hits.push(Hit::new(x, y, width, target));
        x += width + GAP;
    }
}

fn nodes_table(canvas: &mut Canvas<'_>, app: &mut App, y0: usize, end: usize, panel: usize) {
    let room = end - y0;
    let rows = room.saturating_sub(FRAME + 1);
    app.set_rows(rows);
    let title = nodes_title(app);
    let speed = app
        .speed()
        .map_or_else(String::new, |speed| speed_label(speed.down, speed.up));
    let (sort, descending) = (app.filter.sort, app.filter.descending);
    let width = panel.saturating_sub(4);
    let mut found = Vec::new();
    // Выделенная строка закрашивается от края до края панели, рамку не трогает.
    if !app.visible.is_empty()
        && let Some(row) = app.cursor.checked_sub(app.offset).filter(|row| *row < rows)
    {
        let blank = " ".repeat(panel.saturating_sub(2));
        let style = canvas.selected();
        canvas.put_style(1, y0 + 2 + row, &blank, panel.saturating_sub(2), style);
    }
    {
        let shown: Vec<&Node> = app
            .visible
            .iter()
            .filter_map(|index| app.nodes.get(*index))
            .collect();
        let border = canvas.chrome();
        let mut inner = canvas.boxed(0, y0, panel, room, title, border);
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
                let flags = (current, node.excluded);
                draw_row(&mut inner, 1 + row, &cols, node, &speed, flags, app.names);
                found.push(Hit::new(0, y0 + 2 + row, panel, Target::NodeRow(index)));
            }
        }
    }
    app.hits.extend(found);
}

fn nodes_title(app: &App) -> Vec<Seg> {
    let total = app.nodes.len();
    let excluded = app.nodes.iter().filter(|node| node.excluded).count();
    let usable = app
        .visible
        .iter()
        .filter(|&&index| app.nodes.get(index).is_some_and(|node| !node.excluded))
        .count();
    let mut segs = vec![
        Seg::new("Узлы", Tone::Header, 0),
        Seg::new(format!("{usable} из {total}"), Tone::Dim, 0),
    ];
    if excluded > 0 {
        segs.push(Seg::new(format!("({excluded} исключено)"), Tone::Dim, 2));
    }
    if app.visible.len() < total {
        segs.push(Seg::new(
            format!("({} скрыто)", total - app.visible.len()),
            Tone::Dim,
            2,
        ));
    }
    segs
}

/// Подробности на узком экране: окно по центру поверх таблицы. Клик мимо закроет его.
fn node_window(canvas: &mut Canvas<'_>, app: &mut App, y0: usize, room: usize) {
    let height = if room > 6 { room - 2 } else { room };
    let width = canvas.width.saturating_sub(4).min(WINDOW_MAX);
    let x = (canvas.width - width) / 2;
    let y = y0 + (room - height) / 2;
    canvas.clear_rect(x, y, width, height);
    // Таблица под окном не принимает нажатий, кнопки окна остаются.
    app.hits.clear();
    node_details(canvas, app, x, y, width, height);
}

/// Подробности выбранного узла в рамке по заданному прямоугольнику: справа от таблицы или
/// окном. Кнопки «закрепить» и «тест скорости» — вторая строка под именем.
fn node_details(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    let text_width = width.saturating_sub(4);
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
            format!(
                "Протокол: {}, транспорт: {}, безопасность: {}",
                node.protocol.as_deref().unwrap_or("—"),
                node.transport.as_deref().unwrap_or("—"),
                security_text(node.security.as_deref())
            ),
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
    ];
    lines.push(vec![Seg::new("Последние проверки:", Tone::Dim, 1)]);
    lines.extend(check_lines(node, app.now, text_width));
    lines.push(vec![Seg::new("Ошибка полностью:", Tone::Dim, 1)]);
    let error = node.last_error.as_deref().unwrap_or("—");
    // Текст ошибки целиком: панель прокручивается, обрезать его нельзя.
    for line in wrap(error, text_width, usize::MAX) {
        lines.push(vec![Seg::new(line, Tone::Dim, 0)]);
    }
    let pin_width = display_width("[p] закрепить");
    let speed_width = display_width("[t] тест скорости");
    let border = canvas.chrome();
    let mut inner = canvas.boxed(
        x,
        y,
        width,
        height,
        vec![Seg::new("Узел · подробности", Tone::Bold, 0)],
        border,
    );
    scroll_rows(&mut inner, lines, &mut app.details_offset);
    app.hits
        .push(Hit::new(x + 2, y + 2, pin_width, Target::Pin));
    app.hits.push(Hit::new(
        x + 2 + pin_width + GAP,
        y + 2,
        speed_width,
        Target::SpeedNode,
    ));
}

/// Выводит строки панели. Первые `NODE_HEADER_ROWS` неподвижны (имя и кнопки), остальные
/// идут от `offset`; смещение ограничивается концом тела, чтобы прокрутка не уходила дальше.
fn scroll_rows(inner: &mut Canvas<'_>, mut lines: Vec<Vec<Seg>>, offset: &mut usize) {
    let rows = inner.height;
    let body = lines.split_off(NODE_HEADER_ROWS.min(lines.len()));
    let visible = rows.saturating_sub(NODE_HEADER_ROWS);
    *offset = (*offset).min(body.len().saturating_sub(visible));
    let shown = lines
        .into_iter()
        .chain(body.into_iter().skip(*offset).take(visible));
    for (row, segs) in shown.take(rows).enumerate() {
        inner.line(row, 0, segs);
    }
}

/// Безопасность для показа: без шифрования в настройках xray пишут `none`, в строке — «нет».
fn security_text(security: Option<&str>) -> &str {
    match security {
        None | Some("none") => "нет",
        Some(other) => other,
    }
}

/// Последние проверки узла одной строкой с переносом: «2 мин назад: 31 мс · …».
fn check_lines(node: &Node, now: u64, width: usize) -> Vec<Vec<Seg>> {
    let text = if node.checks.is_empty() {
        "ещё не проверялся".to_owned()
    } else {
        node.checks
            .iter()
            .map(|check| check_text(check, now))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    wrap(&text, width, usize::MAX)
        .into_iter()
        .map(|line| vec![Seg::new(line, Tone::Dim, 0)])
        .collect()
}

fn check_text(check: &NodeCheck, now: u64) -> String {
    let when = render::ago(now, check.at);
    if check.alive {
        format!("{when}: {}", render::latency_text(check.latency_ms))
    } else {
        format!("{when}: сбой")
    }
}

/// Колонки в порядке важности: на узком экране последние пропадают первыми. Скорость
/// есть только у выбранного узла и только когда её измерили.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Marker,
    Sub,
    Name,
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
                .and_then(node_reason_short)
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

fn cell(kind: Kind, node: &Node, speed: &str, style: NameStyle, excluded: bool) -> (String, Tone) {
    match kind {
        Kind::Marker if excluded => ("ø".to_owned(), Tone::Red),
        Kind::Marker => {
            let (mark, tone) = render::node_marker(node);
            (mark.to_owned(), tone)
        }
        Kind::Sub => (node.subscription.clone(), Tone::Plain),
        Kind::Name => {
            // Мёртвые узлы серым, выбранный жирным.
            let tone = if node.status == NodeStatus::Dead {
                Tone::Dim
            } else if node.selected {
                Tone::Bold
            } else {
                Tone::Plain
            };
            (display_name(&node.name, style), tone)
        }
        Kind::Status if excluded => ("исключён".to_owned(), Tone::Dim),
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
                .and_then(node_reason_short)
                .unwrap_or("")
                .to_owned(),
            Tone::Red,
        ),
        Kind::Speed if node.selected => (speed.to_owned(), Tone::Plain),
        Kind::Speed => (String::new(), Tone::Plain),
    }
}

/// Строка таблицы. Выделенная строка закрашивается внутри панели, а её края до рамки
/// кладёт вызывающий; ячейкам добавляется фон и жирность, цвета статуса остаются.
fn draw_row(
    canvas: &mut Canvas<'_>,
    y: usize,
    cols: &[Col],
    node: &Node,
    speed: &str,
    (current, excluded): (bool, bool),
    style: NameStyle,
) {
    if current {
        canvas.fill(y, canvas.selected());
    }
    let mut x = 0;
    for col in cols {
        let (text, tone) = cell(col.kind, node, speed, style, excluded);
        let text = truncate(&text, col.width);
        let shift = if col.right {
            col.width.saturating_sub(display_width(&text))
        } else {
            0
        };
        let cell_style = if current {
            canvas.selected_style(tone)
        } else {
            canvas.style(tone)
        };
        canvas.put_style(x + shift, y, &text, col.width, cell_style);
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
    let buttons = vec![
        (
            "[u] Обновить".to_owned(),
            Tone::Bold,
            Some(Target::UpdateSub),
        ),
        ("[m] Маски".to_owned(), Tone::Bold, Some(Target::SubMasks)),
        ("[ выше".to_owned(), Tone::Plain, Some(Target::SubUp)),
        ("ниже ]".to_owned(), Tone::Plain, Some(Target::SubDown)),
    ];
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
    let panel_top = y0 + list_rows;
    if end > panel_top + FRAME {
        sub_details(canvas, app, panel_top, end - panel_top, selected, zone);
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
        let mark = if index == selected { "► " } else { "  " };
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
        if let Some(expire) = sub.expire
            && !is_expired(sub, app.now)
        {
            let (text, tone) = render::expiry_text(expire, app.now, zone);
            segs.push(Seg::new(text, tone, 2));
        }
        let (phrase, tone) = sub_phrase(sub, app.now, zone);
        segs.push(Seg::new(phrase, tone, 0));
        inner.line(index, 0, segs);
        found.push(Hit::new(0, y0 + 1 + index, full, Target::SubRow(index)));
    }
    found
}

/// Панель выбранной подписки в рамке: провайдер, трафик, сроки, обновления, узлы, маски и
/// состояние. Ошибка короткой фразой; полный текст — когда открыт (Enter).
fn sub_details(
    canvas: &mut Canvas<'_>,
    app: &App,
    top: usize,
    height: usize,
    selected: usize,
    zone: &TimeZone,
) {
    let Some(sub) = app
        .status
        .as_ref()
        .and_then(|status| status.subscriptions.get(selected))
    else {
        return;
    };
    let width = canvas.width.saturating_sub(4);
    let now = app.now;
    let alive = app
        .nodes
        .iter()
        .filter(|node| node.subscription == sub.name && node.status == NodeStatus::Alive)
        .count();
    let provider = sub
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("не указан");
    let traffic = match sub.used_bytes {
        Some(used) => traffic_seg(used, sub.total_bytes),
        None => Seg::new("—", Tone::Dim, 0),
    };
    let (expiry, expiry_tone) = sub.expire.map_or_else(
        || ("—".to_owned(), Tone::Dim),
        |expire| render::expiry_text(expire, now, zone),
    );
    let interval = sub.update_interval_secs.map_or_else(
        || "по ответу провайдера".to_owned(),
        |secs| format!("каждые {}", render::span(secs)),
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
    let (phrase, phrase_tone) = sub_phrase(sub, now, zone);
    let mut lines: Vec<Vec<Seg>> = vec![
        vec![
            Seg::new(format!("Провайдер: {provider}"), Tone::Plain, 0),
            Seg::new(
                format!("эмуляция: {} · {}", sub.app, sub.platform),
                Tone::Dim,
                2,
            ),
        ],
        vec![Seg::new("Трафик:", Tone::Plain, 0), traffic],
        vec![
            Seg::new("Срок:", Tone::Plain, 0),
            Seg::new(expiry, expiry_tone, 0),
        ],
        vec![Seg::new(format!("Обновление: {interval}"), Tone::Plain, 0)],
        vec![Seg::new(format!("Обновлена: {updated}"), Tone::Plain, 0)],
        vec![
            Seg::new(
                format!("Узлов живых: {alive} из {}", sub.nodes),
                Tone::Plain,
                0,
            ),
            Seg::new(format!("отсечено масками: {}", sub.cut), Tone::Plain, 1),
        ],
        vec![Seg::new(format!("Ссылка: {}", sub.url), Tone::Dim, 1)],
        vec![
            Seg::new("Состояние:", Tone::Plain, 0),
            Seg::new(phrase, phrase_tone, 0),
        ],
    ];
    lines.extend(announce_lines(sub, width));
    match &sub.last_error {
        Some(error) if app.details => {
            lines.push(vec![Seg::new("Ошибка полностью:", Tone::Red, 0)]);
            for line in wrap(error, width, 4) {
                lines.push(vec![Seg::new(line, Tone::Red, 0)]);
            }
        }
        Some(_) => lines.push(vec![Seg::new(
            "Ошибка при обновлении: Enter покажет текст полностью",
            Tone::Red,
            0,
        )]),
        None => lines.push(vec![Seg::new("Ошибок нет", Tone::Dim, 0)]),
    }
    let mut inner = canvas.framed(
        top,
        height,
        vec![Seg::new(format!("Подписка «{}»", sub.name), Tone::Bold, 0)],
    );
    let rows = inner.height;
    for (row, segs) in lines.into_iter().take(rows).enumerate() {
        inner.line(row, 0, segs);
    }
}

/// Объявление провайдера из последнего ответа: сначала заголовок, потом текст с переносом.
fn announce_lines(sub: &SubscriptionStatus, width: usize) -> Vec<Vec<Seg>> {
    let Some(announce) = &sub.announce else {
        return Vec::new();
    };
    let mut lines = vec![vec![Seg::new("Объявление провайдера:", Tone::Plain, 0)]];
    for line in wrap(announce, width, usize::MAX) {
        lines.push(vec![Seg::new(line, Tone::Dim, 0)]);
    }
    lines
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

/// Строки списка «Настроек»: пустая строка, заголовки разделов и сами настройки.
#[derive(Debug, Clone, Copy)]
enum Row {
    /// Пустая строка перед заголовком раздела; выбор её пропускает.
    Blank,
    Head(&'static str),
    Item(Setting),
}

fn list_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, (heading, settings)) in SECTIONS.into_iter().enumerate() {
        if index > 0 {
            rows.push(Row::Blank);
        }
        rows.push(Row::Head(heading));
        rows.extend(settings.iter().map(|setting| Row::Item(*setting)));
    }
    rows
}

/// Переключатели показываются как `● вкл` и `○ выкл`; остальные значения обычным текстом.
fn is_switch(setting: Setting) -> bool {
    matches!(setting, Setting::RuDirect | Setting::KillSwitch)
}

/// Значение строки в списке и его тон.
fn list_value(setting: Setting, shown: &Shown) -> (String, Tone) {
    if !shown.enabled {
        return (shown.value.clone(), Tone::Dim);
    }
    if is_switch(setting) {
        return if shown.value == "вкл" {
            ("● вкл".to_owned(), Tone::Green)
        } else {
            ("○ выкл".to_owned(), Tone::Dim)
        };
    }
    (shown.value.clone(), Tone::Plain)
}

/// Что делает клавиша действия над выбранной строкой.
fn action_hint(setting: Setting) -> &'static str {
    match setting {
        Setting::LogLevel => "Enter — следующий уровень",
        _ if is_switch(setting) || setting == Setting::Names => "Пробел — переключить",
        _ => "Enter — изменить",
    }
}

/// Вкладка «Настройки»: слева список по разделам, справа подробности выбранной строки.
/// На узком экране подробности идут строками под списком.
pub(super) fn settings(canvas: &mut Canvas<'_>, app: &mut App, top: usize, end: usize) {
    let wide = canvas.width >= SETTINGS_SPLIT;
    let text_rows = if wide { 0 } else { NARROW_ROWS };
    if end < top + 3 + text_rows {
        return;
    }
    let height = end - top;
    let list_height = height - text_rows;
    let list_width = if wide {
        canvas.width * 55 / 100
    } else {
        canvas.width
    };
    settings_list(canvas, app, top, list_width, list_height);
    if wide {
        let details_width = canvas.width - list_width;
        settings_details(canvas, app, top, list_width, details_width, height);
    } else {
        settings_text(canvas, app, top + list_height);
    }
}

/// Список настроек в рамке: разделы серым, строки с названием слева и значением справа.
/// Выбранная строка закрашена на всю ширину панели.
fn settings_list(canvas: &mut Canvas<'_>, app: &mut App, top: usize, width: usize, height: usize) {
    let rows = list_rows();
    let inner_rows = height - 2;
    // Прокрутка идёт по настройкам. Строк-разделителей и заголовков в списке всего
    // `rows.len() - Setting::ALL.len()`, поэтому столько строк остаётся под настройки.
    let decor = rows.len() - Setting::ALL.len();
    app.keep_setting_visible(inner_rows.saturating_sub(decor).max(1));
    let first = rows
        .iter()
        .position(|row| matches!(row, Row::Item(setting) if setting.row() == app.setting_offset))
        .unwrap_or(0);
    // Перед первой показанной настройкой возвращаем её заголовок и пустую строку над ним.
    let mut start = first;
    if start > 0 && matches!(rows.get(start - 1), Some(Row::Head(_))) {
        start -= 1;
    }
    if start > 0 && matches!(rows.get(start - 1), Some(Row::Blank)) {
        start -= 1;
    }
    let cursor = app.setting_cursor;
    let visible = &rows[start..];
    if let Some(row) = visible
        .iter()
        .take(inner_rows)
        .position(|row| matches!(row, Row::Item(setting) if setting.row() == cursor))
    {
        let blank = " ".repeat(width - 2);
        let style = canvas.selected();
        canvas.put_style(1, top + 1 + row, &blank, width - 2, style);
    }
    let border = canvas.active_frame();
    let title = vec![Seg::new("Настройки", Tone::Header, 0)];
    let mut inner = canvas.boxed(0, top, width, height, title, border);
    let inner_width = inner.width;
    for (row, entry) in visible.iter().take(inner_rows).enumerate() {
        match *entry {
            Row::Blank => {}
            Row::Head(heading) => inner.line(row, 0, vec![Seg::new(heading, Tone::Dim, 0)]),
            Row::Item(setting) => {
                let selected = setting.row() == cursor;
                let shown = app.shown(setting);
                let (value, tone) = list_value(setting, &shown);
                let value_width = display_width(&value);
                let x = inner_width.saturating_sub(value_width);
                let name_room = x.saturating_sub(1);
                let name = truncate(setting.title(), name_room);
                let name_tone = if shown.enabled {
                    Tone::Plain
                } else {
                    Tone::Dim
                };
                let (name_style, value_style) = if selected {
                    (inner.selected_style(name_tone), inner.selected_style(tone))
                } else {
                    (inner.style(name_tone), inner.style(tone))
                };
                inner.put_style(0, row, &name, name_room, name_style);
                inner.put_style(x, row, &value, value_width, value_style);
                if shown.enabled {
                    app.hits.push(Hit::new(
                        2 + x,
                        top + 1 + row,
                        value_width,
                        Target::SettingCell(setting.row()),
                    ));
                }
                app.hits.push(Hit::new(
                    0,
                    top + 1 + row,
                    width,
                    Target::SettingRow(setting.row()),
                ));
            }
        }
    }
}

/// Панель подробностей выбранной строки: пояснение, значения, действие, поле правки,
/// варианты уровня журнала и внизу путь к файлу настроек.
fn settings_details(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    top: usize,
    left: usize,
    width: usize,
    height: usize,
) {
    let Some(setting) = Setting::ALL.get(app.setting_cursor).copied() else {
        return;
    };
    let shown = app.shown(setting);
    let border = canvas.chrome();
    let title = vec![Seg::new(setting.title(), Tone::Bold, 0)];
    let mut inner = canvas.boxed(left, top, width, height, title, border);
    let inner_width = inner.width;
    let mut row = 0;
    for line in wrap(setting.hint(), inner_width, 2) {
        inner.line(row, 0, vec![Seg::new(line, Tone::Plain, 0)]);
        row += 1;
    }
    row += 1;
    let (value, tone) = list_value(setting, &shown);
    detail(&mut inner, row, "Сейчас", &value, tone);
    detail(
        &mut inner,
        row + 1,
        "По умолчанию",
        setting.default_text(),
        Tone::Plain,
    );
    detail(&mut inner, row + 2, "Откуда", &shown.source, Tone::Plain);
    row += 4;
    if app.input == InputMode::Field {
        let text = format!("[{}█]", app.field);
        detail(&mut inner, row, "Новое значение", &text, Tone::Bold);
        inner.line(row + 1, 0, vec![Seg::new(FIELD_HINT, Tone::Dim, 0)]);
        row += 2;
    } else {
        inner.line(row, 0, vec![Seg::new(action_hint(setting), Tone::Dim, 0)]);
        row += 1;
    }
    if setting == Setting::LogLevel && shown.enabled {
        row += 1;
        level_choices(&mut inner, app, &shown.value, row, (left + 2, top + 1));
        row += 1;
    }
    if let Some((_, error)) = app
        .setting_error
        .as_ref()
        .filter(|(index, _)| *index == setting.row())
    {
        row += 1;
        for line in wrap(error, inner_width, 2) {
            inner.line(row, 0, vec![Seg::new(line, Tone::Red, 0)]);
            row += 1;
        }
    }
    if let Some(footer) = inner.height.checked_sub(2) {
        let path = if app.settings_path.is_empty() {
            "не найден".to_owned()
        } else {
            app.settings_path.clone()
        };
        inner.line(
            footer,
            0,
            vec![Seg::new(format!("Файл настроек: {path}"), Tone::Dim, 0)],
        );
        inner.line(
            footer + 1,
            0,
            vec![Seg::new(
                "Остальное в файле, raycat config покажет итог",
                Tone::Dim,
                0,
            )],
        );
    }
}

/// Строка панели подробностей: подпись слева, значение с отступом.
fn detail(inner: &mut Canvas<'_>, row: usize, label: &str, value: &str, tone: Tone) {
    inner.put(0, row, &pad(label, LABEL), LABEL, Tone::Dim);
    let room = inner.width.saturating_sub(LABEL);
    inner.put(LABEL, row, value, room, tone);
}

/// Варианты уровня журнала в строке: текущий в скобках, каждый нажимается отдельно.
/// `origin` — экранные координаты левого верхнего угла области внутри рамки.
fn level_choices(
    inner: &mut Canvas<'_>,
    app: &mut App,
    current: &str,
    row: usize,
    origin: (usize, usize),
) {
    let mut at = 0;
    for level in LEVELS {
        let chosen = current == level;
        let text = if chosen {
            format!("[{level}]")
        } else {
            level.to_owned()
        };
        let width = display_width(&text);
        let tone = if chosen { Tone::Bold } else { Tone::Dim };
        inner.put(at, row, &text, width, tone);
        app.hits.push(Hit::new(
            origin.0 + at,
            origin.1 + row,
            width,
            Target::SettingLevel(level),
        ));
        at += width + GAP;
    }
}

/// Узкий экран: подробности строками под списком — пояснение или ошибка, действие,
/// файл настроек и примечание.
fn settings_text(canvas: &mut Canvas<'_>, app: &App, top: usize) {
    let Some(setting) = Setting::ALL.get(app.setting_cursor).copied() else {
        return;
    };
    let error = app
        .setting_error
        .as_ref()
        .filter(|(index, _)| *index == app.setting_cursor)
        .map(|(_, text)| text.clone());
    let (first, first_tone) = match error {
        Some(text) => (text, Tone::Red),
        None => (setting.hint().to_owned(), Tone::Dim),
    };
    let (action, action_tone) = if app.input == InputMode::Field {
        (
            format!("Новое значение: [{}█] · {FIELD_HINT}", app.field),
            Tone::Bold,
        )
    } else {
        (action_hint(setting).to_owned(), Tone::Dim)
    };
    let path = if app.settings_path.is_empty() {
        "не найден".to_owned()
    } else {
        app.settings_path.clone()
    };
    let lines = [
        (first, first_tone),
        (action, action_tone),
        (format!("Файл настроек: {path}"), Tone::Dim),
        (
            "Остальное в файле, raycat config покажет итог".to_owned(),
            Tone::Dim,
        ),
    ];
    for (row, (text, tone)) in lines.into_iter().enumerate() {
        canvas.line(top + row, 0, vec![Seg::new(text, tone, 0)]);
    }
}
