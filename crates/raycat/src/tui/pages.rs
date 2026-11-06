//! Содержимое вкладок: обзор, узлы, подписки, журнал и настройки. Координаты абсолютные
//! (от левого верхнего угла экрана). Каждая вкладка регистрирует области нажатий мыши
//! в `App::hits` уже после рисования, чтобы не держать заимствования на время цикла.

use jiff::tz::TimeZone;
use raycat_proto::{CurrentNode, Node, NodeStatus, SubscriptionStatus, XrayStatus};

use super::app::{App, Hit, InputMode, LEVELS, NODE_SPLIT, Setting, Shown, SortBy, Speed, Target};
use super::canvas::{Canvas, GAP, Seg, fit, wrap};
use super::view::{
    BAR_CELLS, brief_phrase, delay_tone, is_expired, speed_label, speed_part, sub_phrase,
    subscription_title, traffic_seg, tunnel_hint, xray_segs,
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
/// Высота панелей «Туннель» и «Подписки» на обзоре: рамка, узел и график в
/// `2 * GRAPH_ROWS` строк.
const TUNNEL_ROWS: usize = FRAME + 1 + 2 * GRAPH_ROWS;
/// Ширина подписи скорости в панели туннеля, например `↓ 43,5 Мбит/с`.
const SPEED_LABEL: usize = 16;
/// Меньше стольких столбцов график не рисуется: он исчезает первым на узком экране.
const GRAPH_MIN: usize = 3;
/// Строки графика по каждую сторону от черты: приём над ней, отдача под ней.
const GRAPH_ROWS: usize = 2;
/// Строка приёма в панели туннеля, последняя над чертой; отдача — первая под ней.
const DOWN_ROW: usize = GRAPH_ROWS;
/// Имя узла в панели туннеля не короче этого, пока справа есть что убрать.
const NAME_MIN: usize = 12;
const DOT: &str = " · ";
/// Ниже этой ширины панели идут друг под другом.
const SPLIT_MIN: usize = 90;
/// Ширина окна подробностей узла не больше этого числа ячеек.
const WINDOW_MAX: usize = 96;
/// Колонки журнала: время, уровень словом; текст идёт после них.
const TIME_WIDTH: usize = 8;
const LEVEL_WIDTH: usize = 9;
/// Колонки строки подписки на обзоре: имя, полоска с процентом, срок; состояние — справа.
/// На узкой панели срок пропадает раньше, чем имя станет короче `SUB_NAME_MIN`.
const SUB_NAME_WIDTH: usize = 14;
const SUB_NAME_MIN: usize = 6;
/// Имя на самой узкой панели, где срока уже нет.
const SUB_NAME_TIGHT: usize = 4;
/// `━━━━━━━━━━ 100%`: полоска, пробел и процент, всегда одной ширины.
const SUB_BAR_WIDTH: usize = 15;
/// Самая длинная строка срока, например `до 05.11 · 23 дн`.
const SUB_EXPIRY_WIDTH: usize = 17;
/// Имя подписки в списке не уже этого, иначе состояние в списке не показывается.
const SUB_LIST_NAME_MIN: usize = 6;
/// Ниже этой ширины экрана колонка подписки в таблице узлов получает заголовок `Подп.`.
const SUB_TITLE_SCREEN: usize = 80;
/// Доля таблицы узлов, когда справа панель подробностей.
const NODE_TABLE_PERCENT: usize = 70;

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
            overview_subs(canvas, app, left, y, canvas.width - left, row, zone);
        } else {
            tunnel_panel(canvas, app, 0, y, canvas.width, row);
        }
        y += row;
    }
    if !wide && y + sub_rows <= end {
        overview_subs(canvas, app, 0, y, canvas.width, sub_rows, zone);
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
        node_row(&mut inner, app, node, &status.xray);
    } else {
        inner.line(0, 0, vec![Seg::new("узел не выбран", Tone::Yellow, 0)]);
    }
    if let Some(hint) = tunnel_hint(app) {
        inner.line(1, 0, vec![Seg::new(hint, Tone::Dim, 0)]);
    } else {
        let now = app.speed();
        speed_row(&mut inner, DOWN_ROW, "↓", now.map(|speed| speed.down));
        speed_row(&mut inner, DOWN_ROW + 1, "↑", now.map(|speed| speed.up));
        if now.is_some() {
            speed_graph(&mut inner, app);
        }
    }
}

/// Первая строка панели туннеля: узел слева, справа задержка, выбор и перезапуски xray.
/// Справа уходит лишнее по приоритету, имя узла сокращается последним.
fn node_row(inner: &mut Canvas<'_>, app: &App, node: &CurrentNode, xray: &XrayStatus) {
    let choice = if node.pinned {
        "закреплён"
    } else {
        "авто"
    };
    let latency = render::latency_text(node.latency_ms);
    let mut segs = vec![
        Seg::new(latency, delay_tone(node.latency_ms), 0),
        Seg::new(choice, Tone::Dim, 1),
    ];
    segs.extend(xray_segs(xray));
    let room = inner.width.saturating_sub(NAME_MIN + GAP);
    let kept = fit(segs, room, display_width(DOT));
    let right = kept
        .iter()
        .map(|seg| display_width(&seg.text))
        .sum::<usize>()
        + display_width(DOT) * kept.len().saturating_sub(1);
    let at = inner.width.saturating_sub(right);
    inner.chain(0, at, DOT, kept, inner.width);
    let name = format!("► {}", display_name(&node.name, app.names));
    let room = at.saturating_sub(GAP);
    inner.put(0, 0, &truncate(&name, room), room, Tone::Bold);
}

/// Строка скорости: стрелка и значение. Нет замера — `—`.
fn speed_row(inner: &mut Canvas<'_>, row: usize, arrow: &str, bits: Option<u64>) {
    let Some(bits) = bits else {
        inner.line(row, 0, vec![Seg::new(format!("{arrow} —"), Tone::Dim, 0)]);
        return;
    };
    let label = pad(&format!("{arrow} {}", speed_part(bits)), SPEED_LABEL);
    inner.line(row, 0, vec![Seg::new(label, Tone::Plain, 0)]);
}

/// График справа от скоростей, как сеть в btop, зеркально относительно черты между
/// строками скорости: приём растёт вверх, отдача вниз, обе на `GRAPH_ROWS` строк. Масштаб
/// общий, как у btop с синхронизацией: высоты сторон можно сравнивать. Строки у черты
/// темнее, у края светлее. Новые замеры справа, старые уходят влево; пока истории мало,
/// слева пусто.
fn speed_graph(inner: &mut Canvas<'_>, app: &App) {
    let cells = inner.width.saturating_sub(SPEED_LABEL + GAP);
    if cells < GRAPH_MIN || app.speeds.is_empty() {
        return;
    }
    let shown = app.speeds.len().saturating_sub(cells * 2);
    // Нечётное число замеров: слева нулевой, чтобы последний замер был в правом столбце.
    let pad = (app.speeds.len() - shown) % 2;
    let zero = Speed { down: 0, up: 0 };
    let recent = std::iter::repeat_n(zero, pad).chain(app.speeds.iter().skip(shown).copied());
    let down: Vec<u64> = recent.clone().map(|speed| speed.down).collect();
    let up: Vec<u64> = recent.map(|speed| speed.up).collect();
    let max = down.iter().chain(&up).copied().max().unwrap_or(0);
    let x = SPEED_LABEL + GAP + cells.saturating_sub(down.len() / 2);
    let top = DOWN_ROW + 1 - GRAPH_ROWS;
    for (index, row) in graph_up(&down, cells, GRAPH_ROWS, max).iter().enumerate() {
        let style = inner.graph(true, index == 0);
        inner.put_style(x, top + index, row, cells, style);
    }
    for (index, row) in graph_down(&up, cells, GRAPH_ROWS, max).iter().enumerate() {
        let style = inner.graph(false, index + 1 == GRAPH_ROWS);
        inner.put_style(x, DOWN_ROW + 1 + index, row, cells, style);
    }
}

/// Точки шрифта Брайля левого и правого столбца ячейки снизу вверх; сверху вниз — те же
/// в обратном порядке.
const BRAILLE_LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
const BRAILLE_RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];

/// Уровни последних замеров в точках, от 0 до `dots`: высота относительно `max`, с
/// округлением; ненулевой замер — хотя бы одна точка. В ячейке два замера.
fn graph_levels(values: &[u64], cells: usize, dots: usize, max: u64) -> Vec<usize> {
    let shown = &values[values.len().saturating_sub(cells * 2)..];
    let max = u128::from(max);
    let scale = u128::try_from(dots).unwrap_or(0);
    shown
        .iter()
        .map(|&value| {
            // Все нули: делить не на что, точек нет.
            let level = (u128::from(value) * scale + max / 2)
                .checked_div(max)
                .map_or(0, |level| usize::try_from(level).unwrap_or(dots));
            if value > 0 { level.max(1) } else { level }
        })
        .collect()
}

/// График точками Брайля, как в btop, растущий вверх: `rows` строк сверху вниз. В ячейке
/// два замера (левый и правый столбец) и четыре точки по высоте.
fn graph_up(values: &[u64], cells: usize, rows: usize, max: u64) -> Vec<String> {
    let levels = graph_levels(values, cells, rows * 4, max);
    (0..rows)
        .map(|row| {
            // Сколько точек графика ниже этой строки.
            let below = (rows - 1 - row) * 4;
            braille_row(&levels, below, false)
        })
        .collect()
}

/// То же, растущее вниз от верхнего края: `rows` строк сверху вниз.
fn graph_down(values: &[u64], cells: usize, rows: usize, max: u64) -> Vec<String> {
    let levels = graph_levels(values, cells, rows * 4, max);
    (0..rows)
        .map(|row| braille_row(&levels, row * 4, true))
        .collect()
}

/// Одна строка графика: в каждой строке видны точки уровня выше `skip`, не больше четырёх.
fn braille_row(levels: &[usize], skip: usize, downward: bool) -> String {
    levels
        .chunks(2)
        .map(|pair| {
            let left = pair.first().map_or(0, |level| level.saturating_sub(skip));
            let right = pair.get(1).map_or(0, |level| level.saturating_sub(skip));
            let left = dots(&BRAILLE_LEFT, left, downward);
            let right = dots(&BRAILLE_RIGHT, right, downward);
            braille(left | right)
        })
        .collect()
}

/// `count` точек столбца (не больше четырёх) от нижнего края или, если `downward`, от
/// верхнего.
fn dots(column: &[u32; 4], count: usize, downward: bool) -> u32 {
    let take = count.min(4);
    if downward {
        column
            .iter()
            .rev()
            .take(take)
            .fold(0, |bits, dot| bits | dot)
    } else {
        column.iter().take(take).fold(0, |bits, dot| bits | dot)
    }
}

/// Символ Брайля по точкам; пустая ячейка — пробел.
fn braille(bits: u32) -> char {
    if bits == 0 {
        ' '
    } else {
        char::from_u32(0x2800 + bits).unwrap_or(' ')
    }
}

/// Панель подписок на обзоре: по строке на подписку, колонки совпадают у всех строк.
fn overview_subs(
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
    let phrases: Vec<(String, Tone)> = subs
        .iter()
        .take(rows)
        .map(|sub| brief_phrase(sub, app.now, zone))
        .collect();
    let phrase_width = phrases
        .iter()
        .map(|(phrase, _)| display_width(phrase))
        .max()
        .unwrap_or(0);
    let columns = brief_columns(inner.width, phrase_width);
    for (row, (sub, (phrase, tone))) in subs.iter().zip(&phrases).enumerate() {
        let shown = (phrase.as_str(), *tone);
        sub_brief_row(&mut inner, row, sub, shown, columns, app.now, zone);
    }
}

/// Где стоят колонки строки подписки на обзоре, в символах от левого края панели. Одни и
/// те же для всех строк панели.
#[derive(Debug, Clone, Copy)]
struct BriefColumns {
    name: usize,
    bar: usize,
    /// Срок есть, только если он помещается между полоской и состоянием.
    expiry: Option<usize>,
}

/// Колонки для панели с внутренней шириной `width`, где состояние занимает `phrase_width`.
/// Срок остаётся, пока имя не уже `SUB_NAME_MIN`; иначе срок уходит, а имя сужается дальше.
fn brief_columns(width: usize, phrase_width: usize) -> BriefColumns {
    let with_expiry =
        width.saturating_sub(SUB_BAR_WIDTH + SUB_EXPIRY_WIDTH + phrase_width + 3 * GAP);
    if with_expiry >= SUB_NAME_MIN {
        let name = with_expiry.min(SUB_NAME_WIDTH);
        let bar = name + GAP;
        let expiry = bar + SUB_BAR_WIDTH + GAP;
        return BriefColumns {
            name,
            bar,
            expiry: Some(expiry),
        };
    }
    let name = width
        .saturating_sub(SUB_BAR_WIDTH + phrase_width + 2 * GAP)
        .clamp(SUB_NAME_TIGHT, SUB_NAME_WIDTH);
    BriefColumns {
        name,
        bar: name + GAP,
        expiry: None,
    }
}

/// Строка подписки на обзоре: имя, полоска и процент (или серое «без лимита»), срок и
/// состояние справа. Колонки берутся из `columns`, поэтому у всех подписок они совпадают.
fn sub_brief_row(
    inner: &mut Canvas<'_>,
    row: usize,
    sub: &SubscriptionStatus,
    (phrase, tone): (&str, Tone),
    columns: BriefColumns,
    now: u64,
    zone: &TimeZone,
) {
    let name = truncate(&sub.name, columns.name);
    inner.put(0, row, &name, columns.name, Tone::Bold);
    match (sub.used_bytes, sub.total_bytes.filter(|total| *total > 0)) {
        (Some(used), Some(total)) => {
            let percent = u128::from(used) * 100 / u128::from(total);
            let bar_tone = match percent {
                0..=70 => Tone::Green,
                71..=90 => Tone::Yellow,
                _ => Tone::Red,
            };
            inner.gauge(columns.bar, row, percent, BAR_CELLS, bar_tone);
            let text = format!("{percent}%");
            let at = columns.bar + BAR_CELLS + 1;
            inner.put(at, row, &text, SUB_BAR_WIDTH - BAR_CELLS - 1, bar_tone);
        }
        (None, Some(_)) => inner.put(columns.bar, row, "—", SUB_BAR_WIDTH, Tone::Dim),
        (_, None) => inner.put(columns.bar, row, "без лимита", SUB_BAR_WIDTH, Tone::Dim),
    }
    if let Some(at) = columns.expiry
        && let Some(expire) = sub.expire
        && !is_expired(sub, now)
    {
        let (text, expiry_tone) = render::expiry_short(expire, now, zone);
        inner.put(at, row, &text, SUB_EXPIRY_WIDTH, expiry_tone);
    }
    let width = display_width(phrase);
    let at = inner.width.saturating_sub(width);
    inner.put(at, row, phrase, width, tone);
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
    app.screen_width = canvas.width;
    if top >= end {
        return;
    }
    let room = end - top;
    if wide {
        // Подробности выбранного узла всегда справа: Enter для них не нужен, скорость
        // показана там, поэтому в таблице колонки скорости нет.
        let table = canvas.width * NODE_TABLE_PERCENT / 100;
        nodes_table(canvas, app, top, end, table, false);
        if app.current().is_some() {
            node_details(canvas, app, table, top, canvas.width - table, room);
        }
    } else {
        nodes_table(canvas, app, top, end, canvas.width, true);
        if app.details && app.current().is_some() {
            node_window(canvas, app, top, room);
        }
    }
}

/// Фильтры одной строкой внутри панели узлов: включённые акцентом, выключенные серым,
/// клавиша в плашке. Каждый фильтр — область нажатия. Не умещающиеся справа пропускаются.
fn filter_row(canvas: &mut Canvas<'_>, app: &mut App, x0: usize, y: usize, limit: usize) {
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
    let mut x = x0;
    for (key, label, active, target) in items {
        let text = format!(" {label}");
        let key_width = display_width(key);
        let width = key_width + display_width(&text);
        if x + width > limit {
            break;
        }
        canvas.put_style(x, y, key, key_width, key_style);
        let tone = if active { Tone::Header } else { Tone::Dim };
        canvas.put(x + key_width, y, &text, width - key_width, tone);
        app.hits.push(Hit::new(x, y, width, target));
        x += width + 2 * GAP;
    }
}

/// Панель узлов: рамка, в ней фильтры, заголовки колонок и строки таблицы.
/// `show_speed` — показывать ли колонку скорости (на широком экране её даёт панель справа).
fn nodes_table(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    y0: usize,
    end: usize,
    panel: usize,
    show_speed: bool,
) {
    let room = end - y0;
    let sub_title = if canvas.width < SUB_TITLE_SCREEN {
        "Подп."
    } else {
        "Подписка"
    };
    // Внутри рамки: фильтры, заголовки колонок, затем строки.
    let rows = room.saturating_sub(FRAME + 2);
    app.set_rows(rows);
    let title = nodes_title(app);
    let speed = if show_speed {
        app.speed()
            .map_or_else(String::new, |speed| speed_label(speed.down, speed.up))
    } else {
        String::new()
    };
    let (sort, descending) = (app.filter.sort, app.filter.descending);
    let width = panel.saturating_sub(4);
    let mut found = Vec::new();
    // Выделенная строка закрашивается от края до края панели, рамку не трогает.
    if !app.visible.is_empty()
        && let Some(row) = app.cursor.checked_sub(app.offset).filter(|row| *row < rows)
    {
        let blank = " ".repeat(panel.saturating_sub(2));
        let style = canvas.selected();
        canvas.put_style(1, y0 + 3 + row, &blank, panel.saturating_sub(2), style);
    }
    filter_row(canvas, app, 2, y0 + 1, panel.saturating_sub(2));
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
            inner.line(1, 0, vec![Seg::new(text, tone, 0)]);
        } else {
            let cols = columns(&shown, &speed, width, app.names, sub_title);
            for (x, col_width, by) in draw_titles(&mut inner, 1, &cols, sort, descending) {
                found.push(Hit::new(2 + x, y0 + 2, col_width, Target::Sort(by)));
            }
            for (row, node) in shown.iter().skip(app.offset).take(rows).enumerate() {
                let index = app.offset + row;
                let current = index == app.cursor;
                let flags = (current, node.excluded);
                draw_row(&mut inner, 2 + row, &cols, node, &speed, flags, app.names);
                found.push(Hit::new(0, y0 + 3 + row, panel, Target::NodeRow(index)));
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
    let Some(node) = app.current() else {
        return;
    };
    let speed = if node.selected { app.speed() } else { None };
    let lines = node_lines(node, app, speed, width.saturating_sub(4));
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

/// Строки «подпись — значение» в колонке фиксированной ширины: подпись серым, значение
/// справа. Значение, которое не помещается, переносится под себя с тем же отступом.
fn pair(label: &str, value: &str, tone: Tone, width: usize) -> Vec<Vec<Seg>> {
    let room = width.saturating_sub(LABEL).max(1);
    value_parts(value, room)
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let lead = if index == 0 {
                pad(label, LABEL - GAP)
            } else {
                " ".repeat(LABEL - GAP)
            };
            vec![Seg::new(lead, Tone::Dim, 0), Seg::new(part, tone, 0)]
        })
        .collect()
}

/// Части значения по ширине; пустое значение даёт одну пустую часть, чтобы подпись осталась.
fn value_parts(value: &str, width: usize) -> Vec<String> {
    let parts = wrap(value, width, usize::MAX);
    if parts.is_empty() {
        vec![String::new()]
    } else {
        parts
    }
}

/// Строки панели подробностей узла: имя, кнопки, пары «подпись — значение», причина и
/// ошибка только когда она есть, проверки по одной на строку и тег внизу серым.
fn node_lines(node: &Node, app: &App, speed: Option<Speed>, width: usize) -> Vec<Vec<Seg>> {
    let (status_text, status_tone) = render::node_status_mark(node.status);
    let choice = match (node.selected, node.pinned) {
        (false, _) => "нет",
        (true, true) => "закреплён",
        (true, false) => "авто",
    };
    let country = country_code(&node.name).unwrap_or_else(|| "—".to_owned());
    let protocol = node.protocol.as_deref().unwrap_or("—");
    let transport = node.transport.as_deref().unwrap_or("—");
    let security = security_text(node.security.as_deref());
    let traffic = render::node_traffic(node);
    let latency = render::latency_text(node.latency_ms);
    let mut lines: Vec<Vec<Seg>> = vec![
        vec![Seg::new(display_name(&node.name, app.names), Tone::Bold, 0)],
        vec![
            Seg::new("[p] закрепить", Tone::Bold, 0),
            Seg::new("[t] тест скорости", Tone::Bold, 0),
            Seg::new("[Esc] назад", Tone::Dim, 1),
        ],
    ];
    lines.extend(pair("Подписка", &node.subscription, Tone::Plain, width));
    lines.extend(pair("Страна", &country, Tone::Plain, width));
    lines.extend(pair("Статус", status_text, status_tone, width));
    lines.extend(pair(
        "Задержка",
        &latency,
        delay_tone(node.latency_ms),
        width,
    ));
    lines.extend(pair("Сбои", &node.failures.to_string(), Tone::Plain, width));
    lines.extend(pair("Выбор", choice, Tone::Plain, width));
    lines.extend(pair("Протокол", protocol, Tone::Plain, width));
    lines.extend(pair("Транспорт", transport, Tone::Plain, width));
    lines.extend(pair("Безопасность", security, Tone::Plain, width));
    lines.extend(pair("Трафик", &traffic, Tone::Dim, width));
    if let Some(speed) = speed {
        let down = speed_part(speed.down);
        let up = speed_part(speed.up);
        lines.extend(pair("Скорость ↓", &down, Tone::Plain, width));
        lines.extend(pair("Скорость ↑", &up, Tone::Plain, width));
    }
    if let Some(reason) = node.last_error.as_deref().and_then(node_reason) {
        lines.extend(pair("Причина", reason, Tone::Red, width));
    }
    lines.push(vec![Seg::new("Последние проверки:", Tone::Dim, 0)]);
    lines.extend(check_rows(node, app.now));
    if let Some(error) = node.last_error.as_deref() {
        lines.push(vec![Seg::new("Ошибка полностью:", Tone::Dim, 0)]);
        for line in wrap(error, width, usize::MAX) {
            lines.push(vec![Seg::new(line, Tone::Dim, 0)]);
        }
    }
    lines.push(vec![Seg::new(
        format!("Тег outbound: {}", node.tag),
        Tone::Dim,
        0,
    )]);
    lines
}

/// Проверки узла по одной на строку: маркер `●` или `×`, давность и задержка или сбой.
fn check_rows(node: &Node, now: u64) -> Vec<Vec<Seg>> {
    if node.checks.is_empty() {
        return vec![vec![Seg::new("ещё не проверялся", Tone::Dim, 0)]];
    }
    node.checks
        .iter()
        .map(|check| {
            let when = render::ago(now, check.at);
            let (marker, marker_tone, what, what_tone) = if check.alive {
                (
                    "●",
                    Tone::Green,
                    render::latency_text(check.latency_ms),
                    delay_tone(check.latency_ms),
                )
            } else {
                ("×", Tone::Red, "сбой".to_owned(), Tone::Red)
            };
            vec![
                Seg::new(marker, marker_tone, 0),
                Seg::new(when, Tone::Dim, 0),
                Seg::new(what, what_tone, 0),
            ]
        })
        .collect()
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

fn specs(nodes: &[&Node], speed: &str, style: NameStyle, sub_title: &'static str) -> Vec<Spec> {
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
    // Имя — по самому длинному имени, но не шире 80; подписка — по содержимому, но не уже
    // своего заголовка.
    let name = name.clamp(4, 80);
    let sub = sub.clamp(display_width(sub_title), 20);
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
        spec(Kind::Sub, sub_title, sub, sub, false),
        spec(Kind::Failures, "Сбои", 5, 5, true),
        spec(Kind::Reason, "Причина", reason.min(12), reason, false),
        spec(Kind::Speed, "Скорость", speed_width, speed_width, false),
    ]
}

/// Берёт колонки, пока помещаются минимальные ширины, и раздаёт остаток сначала имени,
/// потом причине. Подписка занимает ровно свою ширину.
fn columns(
    nodes: &[&Node],
    speed: &str,
    width: usize,
    style: NameStyle,
    sub_title: &'static str,
) -> Vec<Col> {
    let mut chosen: Vec<(Spec, usize)> = Vec::new();
    let mut used = 0;
    for spec in specs(nodes, speed, style, sub_title) {
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
    for kind in [Kind::Name, Kind::Reason] {
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

/// Подписка в списке и её подробности справа: как во вкладке «Настройки», пары «подпись —
/// значение». От 90 колонок список и подробности рядом; ниже — друг под другом.
pub(super) fn subscriptions(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    top: usize,
    end: usize,
    zone: &TimeZone,
) {
    if top >= end {
        return;
    }
    let room = end - top;
    let count = app
        .status
        .as_ref()
        .map_or(0, |status| status.subscriptions.len());
    app.sub_cursor = app.sub_cursor.min(count.saturating_sub(1));
    let selected = app.sub_cursor;
    if canvas.width >= SPLIT_MIN {
        let left = canvas.width * 55 / 100;
        let found = sub_list(canvas, app, (0, top, left, room), selected, zone);
        app.hits.extend(found);
        let details = (left, top, canvas.width - left, room);
        sub_details(canvas, app, details, selected, zone);
    } else {
        let list_rows = (count.max(1) + FRAME).min(room);
        let found = sub_list(
            canvas,
            app,
            (0, top, canvas.width, list_rows),
            selected,
            zone,
        );
        app.hits.extend(found);
        let panel_top = top + list_rows;
        if end > panel_top + FRAME {
            let details = (0, panel_top, canvas.width, end - panel_top);
            sub_details(canvas, app, details, selected, zone);
        }
    }
}

/// Список подписок: имя слева, состояние одним словом цветом и число живых узлов справа;
/// выбранная строка на всю ширину. `area` — x, y, ширина и высота панели.
fn sub_list(
    canvas: &mut Canvas<'_>,
    app: &App,
    area: (usize, usize, usize, usize),
    selected: usize,
    zone: &TimeZone,
) -> Vec<Hit> {
    let (x, y, width, height) = area;
    let subs: &[SubscriptionStatus] = match &app.status {
        Some(status) => &status.subscriptions,
        None => &[],
    };
    let border = canvas.active_frame();
    let title = subscription_title(subs.len());
    let inner_rows = height.saturating_sub(FRAME);
    if selected < inner_rows && !subs.is_empty() {
        let blank = " ".repeat(width.saturating_sub(2));
        let style = canvas.selected();
        canvas.put_style(
            x + 1,
            y + 1 + selected,
            &blank,
            width.saturating_sub(2),
            style,
        );
    }
    let mut inner = canvas.boxed(x, y, width, height, title, border);
    if subs.is_empty() {
        inner.line(0, 0, vec![Seg::new("подписок нет", Tone::Dim, 0)]);
        return Vec::new();
    }
    let inner_width = inner.width;
    let rows: Vec<(&SubscriptionStatus, String, (String, Tone))> = subs
        .iter()
        .take(inner_rows)
        .map(|sub| {
            let alive = app
                .nodes
                .iter()
                .filter(|node| node.subscription == sub.name && node.status == NodeStatus::Alive)
                .count();
            let value = format!("узлов {alive}/{}", sub.nodes);
            (sub, value, list_status(sub, app.now, zone))
        })
        .collect();
    let value_width = rows
        .iter()
        .map(|(_, value, _)| display_width(value))
        .max()
        .unwrap_or(0);
    let status_width = rows
        .iter()
        .map(|(_, _, (status, _))| display_width(status))
        .max()
        .unwrap_or(0);
    // Состояние стоит колонкой левее чисел узлов; если на имя не остаётся места, его нет.
    let status_x = inner_width.saturating_sub(value_width + GAP + status_width);
    let show_status = status_x >= SUB_LIST_NAME_MIN + GAP;
    let mut found = Vec::new();
    for (index, (sub, value, (status, status_tone))) in rows.iter().enumerate() {
        let is_selected = index == selected;
        let row_width = display_width(value);
        let value_x = inner_width.saturating_sub(row_width);
        let name_room = if show_status {
            status_x.saturating_sub(GAP)
        } else {
            value_x.saturating_sub(GAP)
        };
        let name = truncate(&sub.name, name_room);
        let (name_style, value_style, status_style) = if is_selected {
            (
                inner.selected_style(Tone::Bold),
                inner.selected_style(Tone::Plain),
                inner.selected_style(*status_tone),
            )
        } else {
            (
                inner.style(Tone::Bold),
                inner.style(Tone::Plain),
                inner.style(*status_tone),
            )
        };
        inner.put_style(0, index, &name, name_room, name_style);
        if show_status {
            inner.put_style(status_x, index, status, status_width, status_style);
        }
        inner.put_style(value_x, index, value, row_width, value_style);
        found.push(Hit::new(x, y + 1 + index, width, Target::SubRow(index)));
    }
    found
}

/// Состояние подписки в списке одним словом: истёкшая, с ошибкой обновления или как на
/// обзоре.
fn list_status(sub: &SubscriptionStatus, now: u64, zone: &TimeZone) -> (String, Tone) {
    if is_expired(sub, now) {
        ("истекла".to_owned(), Tone::Red)
    } else if sub.last_error.is_some() {
        ("ошибка".to_owned(), Tone::Red)
    } else {
        sub_phrase(sub, now, zone)
    }
}

/// Подробности выбранной подписки: пары «подпись — значение», затем объявление провайдера
/// и ошибка. Полный текст ошибки — когда открыт (Enter). `area` — x, y, ширина и высота.
fn sub_details(
    canvas: &mut Canvas<'_>,
    app: &App,
    area: (usize, usize, usize, usize),
    selected: usize,
    zone: &TimeZone,
) {
    let (x, y, width, height) = area;
    let Some(sub) = app
        .status
        .as_ref()
        .and_then(|status| status.subscriptions.get(selected))
    else {
        return;
    };
    let now = app.now;
    let alive = app
        .nodes
        .iter()
        .filter(|node| node.subscription == sub.name && node.status == NodeStatus::Alive)
        .count();
    let provider = match sub.title.as_deref().filter(|title| !title.is_empty()) {
        Some(title) => display_name(title, app.names),
        None => "не указан".to_owned(),
    };
    let (traffic, traffic_tone) = match sub.used_bytes {
        Some(used) => {
            let seg = traffic_seg(used, sub.total_bytes);
            (seg.text, seg.tone)
        }
        None => ("—".to_owned(), Tone::Dim),
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
    let border = canvas.chrome();
    let title = vec![Seg::new(format!("Подписка «{}»", sub.name), Tone::Bold, 0)];
    let mut inner = canvas.boxed(x, y, width, height, title, border);
    let inner_width = inner.width;
    let emulation = format!("{} · {}", sub.app, sub.platform);
    let cut = sub.cut.to_string();
    let alive_text = format!("{alive} из {}", sub.nodes);
    let mut row = 0;
    row += detail(&mut inner, row, "Провайдер", &provider, Tone::Plain);
    row += detail(&mut inner, row, "Эмуляция", &emulation, Tone::Dim);
    row += detail(&mut inner, row, "Трафик", &traffic, traffic_tone);
    row += detail(&mut inner, row, "Срок", &expiry, expiry_tone);
    row += detail(&mut inner, row, "Обновление", &interval, Tone::Plain);
    row += detail(&mut inner, row, "Обновлена", &updated, Tone::Plain);
    row += detail(&mut inner, row, "Узлов живых", &alive_text, Tone::Plain);
    row += detail(&mut inner, row, "Отсечено масками", &cut, Tone::Plain);
    row += detail(&mut inner, row, "Ссылка", &sub.url, Tone::Dim);
    row += detail(&mut inner, row, "Состояние", &phrase, phrase_tone);
    let mut rest: Vec<Vec<Seg>> = match &sub.announce {
        Some(announce) => announce_lines(&display_name(announce, app.names), inner_width),
        None => Vec::new(),
    };
    match &sub.last_error {
        Some(error) if app.details => {
            rest.push(vec![Seg::new("Ошибка полностью:", Tone::Red, 0)]);
            for line in wrap(error, inner_width, 4) {
                rest.push(vec![Seg::new(line, Tone::Red, 0)]);
            }
        }
        Some(_) => rest.push(vec![Seg::new(
            "Ошибка при обновлении: Enter покажет текст полностью",
            Tone::Red,
            0,
        )]),
        None => rest.push(vec![Seg::new("Ошибок нет", Tone::Dim, 0)]),
    }
    for (offset, segs) in rest.into_iter().enumerate() {
        inner.line(row + offset, 0, segs);
    }
}

/// Объявление провайдера: сначала заголовок, потом текст с переносом.
fn announce_lines(text: &str, width: usize) -> Vec<Vec<Seg>> {
    let mut lines = vec![vec![Seg::new("Объявление провайдера:", Tone::Plain, 0)]];
    for line in wrap(text, width, usize::MAX) {
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
    if top >= end {
        return;
    }
    let room = end - top;
    let capacity = room.saturating_sub(FRAME);
    let (lines, back) = journal_lines(app, zone, canvas.width.saturating_sub(4), capacity);
    app.journal_back = back;
    let mut title = vec![Seg::new("Журнал", Tone::Bold, 0)];
    if is_utc(zone) {
        title.push(Seg::new("время UTC", Tone::Dim, 1));
    }
    if app.journal_warnings {
        title.push(Seg::new("только предупреждения", Tone::Header, 0));
    }
    let mut inner = canvas.framed(top, room, title);
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

/// Слово уровня в журнале по цвету записи: ошибка, внимание, готово или обычная запись.
fn level_word(tone: Tone) -> (&'static str, Tone) {
    match tone {
        Tone::Red => ("ошибка", Tone::Red),
        Tone::Yellow => ("внимание", Tone::Yellow),
        Tone::Green => ("готово", Tone::Green),
        _ => ("инфо", Tone::Dim),
    }
}

/// Строки журнала: время и уровень словом в колонках фиксированной ширины, текст с переносом.
/// Продолжение переноса выравнивается под текст. Возвращает строки и прокрутку, ужатую до
/// допустимой (0 — последние строки видны).
fn journal_lines(
    app: &App,
    zone: &TimeZone,
    width: usize,
    capacity: usize,
) -> (Vec<Vec<Seg>>, usize) {
    let column = TIME_WIDTH + GAP + LEVEL_WIDTH + GAP;
    let text_width = width.saturating_sub(column).max(1);
    let mut lines: Vec<Vec<Seg>> = Vec::new();
    for entry in app.journal_entries() {
        let time = format_moment(entry.at, app.now, zone);
        let (level, level_tone) = level_word(entry.tone);
        let texts = match wrap(&entry.text, text_width, usize::MAX) {
            texts if texts.is_empty() => vec![String::new()],
            texts => texts,
        };
        for (index, text) in texts.into_iter().enumerate() {
            if index == 0 {
                lines.push(vec![
                    Seg::new(pad(&time, TIME_WIDTH), Tone::Dim, 0),
                    Seg::new(pad(level, LEVEL_WIDTH), level_tone, 0),
                    Seg::new(text, entry.tone, 0),
                ]);
            } else {
                let lead = " ".repeat(TIME_WIDTH + GAP + LEVEL_WIDTH);
                lines.push(vec![
                    Seg::new(lead, Tone::Plain, 0),
                    Seg::new(text, entry.tone, 0),
                ]);
            }
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
    row += detail(&mut inner, row, "Сейчас", &value, tone);
    row += detail(
        &mut inner,
        row,
        "По умолчанию",
        setting.default_text(),
        Tone::Plain,
    );
    let in_file = file_state(&shown.source);
    row += detail(&mut inner, row, "В файле", in_file, Tone::Plain);
    row += 1;
    if app.input == InputMode::Field {
        let text = format!("[{}█]", app.field);
        row += detail(&mut inner, row, "Новое значение", &text, Tone::Bold);
        inner.line(row, 0, vec![Seg::new(FIELD_HINT, Tone::Dim, 0)]);
        row += 1;
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

/// Строки панели подробностей: подпись слева, значение с отступом. Значение, которое не
/// помещается, переносится на следующие строки под ним. Возвращает число занятых строк.
fn detail(inner: &mut Canvas<'_>, row: usize, label: &str, value: &str, tone: Tone) -> usize {
    let room = inner.width.saturating_sub(LABEL).max(1);
    let parts = value_parts(value, room);
    inner.put(0, row, &pad(label, LABEL), LABEL, Tone::Dim);
    for (index, part) in parts.iter().enumerate() {
        inner.put(LABEL, row + index, part, room, tone);
    }
    parts.len()
}

/// Задана ли настройка в файле. Демон называет источник значения «из файла» или «по
/// умолчанию»; рядом со строкой «По умолчанию» второе читалось бы как повтор.
fn file_state(source: &str) -> &str {
    match source {
        "из файла" => "задано",
        "по умолчанию" => "не задано",
        other => other,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_download_graph_grows_up_in_two_rows_and_keeps_the_latest_values() {
        // Полный левый столбец и половина правого: внизу всё, вверху только левый.
        assert_eq!(graph_up(&[8, 4], 1, 2, 8), ["⡇", "⣿"]);
        assert_eq!(graph_up(&[0, 0], 1, 2, 0), [" ", " "]);
        // Маленький замер рядом с большим — одна точка, а не пустота.
        assert_eq!(graph_up(&[1, 100], 1, 2, 100), ["⢸", "⣸"]);
        // Видны только последние замеры: по два на ячейку.
        assert_eq!(graph_up(&[8, 0, 0, 8, 8], 1, 2, 8), ["⣿", "⣿"]);
        // Нечётное число замеров: последняя ячейка с одним левым столбцом.
        assert_eq!(graph_up(&[8, 8, 8], 2, 2, 8), ["⣿⡇", "⣿⡇"]);
    }

    #[test]
    fn the_upload_graph_grows_down_from_the_top_of_its_row() {
        // Левый столбец целиком, правый — две верхние точки.
        assert_eq!(graph_down(&[4, 2], 1, 1, 4), ["⡟"]);
        // Одна точка сверху слева и полный правый столбец.
        assert_eq!(graph_down(&[1, 4], 1, 1, 4), ["⢹"]);
        assert_eq!(graph_down(&[0, 0], 1, 1, 0), [" "]);
        // Общий масштаб: та же отдача рядом с вдвое большим приёмом вдвое ниже.
        assert_eq!(graph_down(&[1, 4], 1, 1, 8), ["⠙"]);
    }

    #[test]
    fn the_overview_columns_do_not_overlap_in_40_62_and_80_column_panels() {
        // Внутренняя ширина: панель минус рамка и отступы по две ячейки с каждой стороны.
        for width in [36, 58, 76] {
            for phrase_width in [8, 15] {
                let columns = brief_columns(width, phrase_width);
                let phrase_at = width - phrase_width;
                assert!(
                    columns.bar + SUB_BAR_WIDTH <= phrase_at,
                    "{width} {phrase_width}: {columns:?}"
                );
                if let Some(expiry) = columns.expiry {
                    assert!(
                        expiry + SUB_EXPIRY_WIDTH + GAP <= phrase_at,
                        "{width} {phrase_width}: {columns:?}"
                    );
                }
            }
        }
    }
}
