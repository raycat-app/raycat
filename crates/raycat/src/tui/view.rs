//! Каркас экрана: шапка с состоянием туннеля, строка вкладок, баннер связи, строка
//! состояния и строка клавиш, окна поверх экрана. Содержимое вкладок — в `pages`.

use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use raycat_proto::{SubscriptionStatus, XrayStatus};

use super::app::{App, ExcludeDialog, Hit, InputMode, Link, Tab, Target};
use super::canvas::{Canvas, GAP, Palette, Seg, fit, wrap};
use super::pages;
use crate::render;
use crate::term::{Tone, display_width, pad};
use crate::util::{format_bytes, format_date, local_zone};

const MIN_WIDTH: usize = 44;
const MIN_HEIGHT: usize = 12;
/// Сколько строк может занять сообщение в строке состояния.
const STATUS_LINES: usize = 3;
const BANNER_LINES: usize = 3;
const BANNER_HINT_LINES: usize = 2;
const BAR_CELLS: usize = 10;
/// Скорость ниже этого значения бит/с показывается как «—».
const MIN_SPEED: u64 = 1_000;
/// Строки, которые всегда остаются телу вкладки.
pub(super) const BODY_MIN: usize = 3;
/// Шапка и строка вкладок вверху экрана.
const HEAD_ROWS: usize = 2;
/// Окно не шире этого числа ячеек, даже на широком экране.
const POPUP_MAX: usize = 96;
/// Ширина колонки клавиш в справке.
const KEY_COLUMN: usize = 12;
const SEPARATOR: &str = " · ";

const HELP: [(&str, &str); 20] = [
    ("1–5  Tab", "вкладки; Shift+Tab и ← → — перелистывание"),
    ("↑ ↓  j k", "выбор строки, прокрутка"),
    ("PgUp PgDn", "страница вверх и вниз"),
    ("Home End", "в начало и в конец (g G)"),
    (
        "Enter",
        "подробности узла или подписки; на «Настройках» — изменить строку",
    ),
    (
        "p",
        "закрепить выбранный узел (на вкладке «Узлы» — и двойной клик)",
    ),
    ("a", "вернуть автоматический выбор узла"),
    (
        "u  m  [ ]",
        "обновить подписки (на «Подписках» — выбранную); m — маски, [ ] — порядок подписок",
    ),
    ("d", "скрыть мёртвые узлы (вкладка «Узлы»)"),
    (
        "s",
        "фильтр по подписке (вкладка «Узлы»); на «Обзоре» — тест скорости",
    ),
    ("o", "сортировка узлов: задержка, имя, подписка, статус"),
    (
        "h",
        "скрытые узлы: маски через запятую, * — любая часть имени",
    ),
    ("t", "тест скорости через выбранный узел (Esc — отменить)"),
    (
        "x",
        "исключить узел из автовыбора маской или снять исключение",
    ),
    ("e", "скрыть или показать исключённые узлы (вкладка «Узлы»)"),
    ("/", "поиск по узлам (Enter — применить, Esc — сбросить)"),
    ("w", "в журнале — только предупреждения и ошибки"),
    (
        "Esc",
        "закрыть подробности и сбросить фильтры; на верхнем уровне — выход",
    ),
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
    tabs(canvas, app);
    let status = status_rows(app, canvas.width);
    // Строки состояния и строка клавиш внизу; тело вкладки занимает всё между шапкой и ними.
    let bottom_rows = status.len() + 1;
    let room = canvas.height.saturating_sub(bottom_rows + BODY_MIN);
    let mut y = HEAD_ROWS;
    y += banner(canvas, app, y, room.saturating_sub(y));
    canvas.set_dim(matches!(app.link, Link::Down { .. }));
    header(canvas, app);

    let end = canvas.height.saturating_sub(bottom_rows);
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
    bottom(canvas, app, status);
    overlay(canvas, app);
}

/// Вкладки во второй строке; каждая — область нажатия. Активная — плашкой акцента.
fn tabs(canvas: &mut Canvas<'_>, app: &mut App) {
    let mut x = 0;
    for (index, tab) in Tab::ALL.iter().enumerate() {
        let number = index + 1;
        let title = tab.title();
        let label = format!(" {number} {title} ");
        let width = display_width(&label);
        if *tab == app.tab {
            let style = canvas.accent_badge();
            canvas.put_style(x, 1, &label, width, style);
        } else {
            let digit = format!(" {number} ");
            let digit_width = display_width(&digit);
            canvas.put(x, 1, &digit, digit_width, Tone::Dim);
            let name = format!("{title} ");
            canvas.put(x + digit_width, 1, &name, width - digit_width, Tone::Plain);
        }
        app.hits.push(Hit::new(x, 1, width, Target::Tab(*tab)));
        x += width + 1;
    }
}

/// Шапка в первой строке: название и состояние туннеля плашками, справа серым режим,
/// выключатель и время работы. Текущий узел здесь не показывается.
fn header(canvas: &mut Canvas<'_>, app: &App) {
    let name = " raycat ";
    let name_width = display_width(name);
    let badge = canvas.accent_badge();
    canvas.put_style(0, 0, name, name_width, badge);
    let state = tunnel_seg(app);
    let plaque = format!(" {} ", state.text);
    let plaque_width = display_width(&plaque);
    let left = name_width + 1;
    let style = canvas.plaque(state.tone);
    canvas.put_style(left, 0, &plaque, plaque_width, style);
    let left_end = left + plaque_width;
    let room = canvas.width.saturating_sub(left_end + GAP);
    let right = header_right(app, room);
    let right_width = display_width(&right);
    if right_width > 0 && left_end + GAP + right_width <= canvas.width {
        canvas.put(
            canvas.width - right_width,
            0,
            &right,
            right_width,
            Tone::Dim,
        );
    }
}

/// Серая часть шапки: режим, выключатель и время работы. Что не помещается, уходит первым.
fn header_right(app: &App, room: usize) -> String {
    let Some(status) = &app.status else {
        return String::new();
    };
    let mut segs = vec![Seg::new(render::mode_name(status.mode), Tone::Dim, 0)];
    if let Some(on) = status.kill_switch {
        let state = if on { "вкл" } else { "выкл" };
        segs.push(Seg::new(format!("kill switch {state}"), Tone::Dim, 1));
    }
    let uptime = status.uptime_secs + app.now.saturating_sub(app.status_at);
    segs.push(Seg::new(render::span(uptime), Tone::Dim, 2));
    let kept = fit(segs, room, display_width(SEPARATOR));
    kept.iter()
        .map(|seg| seg.text.as_str())
        .collect::<Vec<_>>()
        .join(SEPARATOR)
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

/// Состояние туннеля: по нему видно, идёт ли трафик через VPN. Связь с демоном — отдельно,
/// без неё состояния туннеля нет.
pub(super) fn tunnel_seg(app: &App) -> Seg {
    match (&app.link, &app.status) {
        (Link::Down { .. }, _) => Seg::new("● нет связи с демоном", Tone::Red, 0),
        (Link::Connecting, _) | (Link::Up, None) => Seg::new("● подключение…", Tone::Yellow, 0),
        (Link::Up, Some(status)) => {
            let text = render::tunnel_text(status.tunnel, status.mode, status.kill_switch);
            Seg::new(text.headline, text.tone, 0)
        }
    }
}

/// Пояснение к состоянию туннеля, когда живых узлов нет.
pub(super) fn tunnel_hint(app: &App) -> Option<&'static str> {
    let status = app
        .status
        .as_ref()
        .filter(|_| matches!(app.link, Link::Up))?;
    render::tunnel_text(status.tunnel, status.mode, status.kill_switch).hint
}

/// Перезапуски xray; состояние самого процесса и API показывает `tunnel_seg`.
pub(super) fn xray_segs(xray: &XrayStatus) -> Vec<Seg> {
    let mut segs = Vec::new();
    if xray.restarts > 0 {
        let text = format!("xray перезапускался: {}", xray.restarts);
        segs.push(Seg::new(text, Tone::Yellow, 1));
    }
    segs
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

pub(super) fn speed_part(bits: u64) -> String {
    if bits < MIN_SPEED {
        "—".to_owned()
    } else {
        render::speed_text(bits)
    }
}

/// Полоска из десяти ячеек: заполненная часть отвечает проценту.
pub(super) fn bar(percent: u128) -> String {
    let filled = usize::try_from(percent / 10).map_or(BAR_CELLS, |cells| cells.min(BAR_CELLS));
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_CELLS - filled))
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

/// Срок подписки прошёл. Бессрочная подписка (срок 0) не истекает.
pub(super) fn is_expired(sub: &SubscriptionStatus, now: u64) -> bool {
    sub.expire
        .is_some_and(|expire| expire != 0 && expire <= now)
}

/// Состояние подписки одним словом; ошибка уходит в отдельную вкладку. Для истёкшей
/// подписки дата идёт здесь, поэтому отдельный кусок «срок» на обзоре её не повторяет.
pub(super) fn sub_phrase(sub: &SubscriptionStatus, now: u64, zone: &TimeZone) -> (String, Tone) {
    // Заглушка провайдера называет причину: «истекла 25.10.2026», «лимит устройств».
    if let Some(note) = &sub.note {
        return (note.clone(), Tone::Red);
    }
    if sub.last_error.is_some() {
        return ("ошибка".to_owned(), Tone::Red);
    }
    if is_expired(sub, now) {
        let date = sub
            .expire
            .map_or_else(String::new, |expire| format_date(expire, zone));
        return (format!("истекла {date}"), Tone::Red);
    }
    if let (Some(used), Some(total)) = (sub.used_bytes, sub.total_bytes)
        && total > 0
        && used >= total
    {
        return ("трафик исчерпан".to_owned(), Tone::Red);
    }
    if sub.updating {
        return ("обновляется".to_owned(), Tone::Green);
    }
    if sub.updated_at.is_none() {
        return ("ещё не получена".to_owned(), Tone::Yellow);
    }
    ("работает".to_owned(), Tone::Green)
}

/// Подсказки «клавиша описание»: клавиша в плашке, описание обычным текстом.
fn normal_hints(app: &App) -> Vec<Seg> {
    let items: &[(&str, u8)] = match app.tab {
        Tab::Overview => &[
            ("u обновить", 1),
            ("s скорость", 2),
            ("a автовыбор", 3),
            ("2 узлы", 2),
            ("справка", 0),
            ("выход", 0),
        ],
        Tab::Nodes if app.details => &[
            ("Esc назад", 0),
            ("p закрепить", 1),
            ("t скорость", 1),
            ("справка", 0),
            ("выход", 0),
        ],
        Tab::Nodes => &[
            ("↑↓ выбор", 1),
            ("Enter подробности", 2),
            ("p закрепить", 1),
            ("t скорость", 2),
            ("d мёртвые", 3),
            ("s подписка", 4),
            ("o сортировка", 4),
            ("h скрытые", 4),
            ("x исключить", 3),
            ("e исключённые", 4),
            ("/ поиск", 3),
            ("справка", 0),
            ("выход", 0),
        ],
        Tab::Subscriptions => &[
            ("↑↓ выбор", 1),
            ("u обновить", 1),
            ("m маски", 1),
            ("[ ] приоритет", 2),
            ("Enter ошибка", 2),
            ("справка", 0),
            ("выход", 0),
        ],
        Tab::Journal => &[
            ("↑↓ листать", 1),
            ("w предупреждения", 2),
            ("End к концу", 3),
            ("справка", 0),
            ("выход", 0),
        ],
        Tab::Settings => &[
            ("↑↓ выбор", 1),
            ("Enter изменить", 1),
            ("Пробел переключить", 2),
            ("справка", 0),
            ("выход", 0),
        ],
    };
    items
        .iter()
        .map(|(text, priority)| Seg::new(*text, Tone::Dim, *priority))
        .collect()
}

/// Ширина подсказки: плашка клавиши с полями и подпись.
fn hint_width(key: &str, desc: &str) -> usize {
    let label = if desc.is_empty() {
        0
    } else {
        1 + display_width(desc)
    };
    display_width(key) + 2 + label
}

/// Какие подсказки помещаются в ширину: обязательные всегда, остальные по приоритету.
fn keep_hints(items: &[(&str, &str, u8)], width: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&index| items[index].2);
    let mut keep = vec![false; items.len()];
    let mut used = 0;
    for (count, index) in order.into_iter().enumerate() {
        let (key, desc, priority) = items[index];
        let need = hint_width(key, desc) + if count > 0 { GAP } else { 0 };
        if priority > 0 && used + need > width {
            break;
        }
        used += need;
        keep[index] = true;
    }
    keep
}

/// Клавиша, которую нажимает плашка. Стрелки и пары клавиш плашкой не нажимаются.
fn key_code(key: &str) -> Option<KeyCode> {
    match key {
        "Enter" => Some(KeyCode::Enter),
        "Esc" => Some(KeyCode::Esc),
        "End" => Some(KeyCode::End),
        "Пробел" => Some(KeyCode::Char(' ')),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            }
        }
    }
}

/// Строка клавиш: клавиша в плашке, подпись обычным текстом. Клик по плашке работает как
/// клавиша; пока открыто поле ввода, плашки не нажимаются.
fn keys_line(canvas: &mut Canvas<'_>, app: &mut App, y: usize, hints: &[Seg]) {
    let clickable = app.input == InputMode::Normal;
    let items: Vec<(&str, &str, u8)> = hints
        .iter()
        .map(|seg| {
            let (key, desc) = seg.text.split_once(' ').unwrap_or((seg.text.as_str(), ""));
            (key, desc, seg.priority)
        })
        .collect();
    let keep = keep_hints(&items, canvas.width);
    let style = canvas.key_style();
    let mut x = 0;
    let mut first = true;
    for (&(key, desc, _), kept) in items.iter().zip(keep) {
        if !kept {
            continue;
        }
        if !first {
            x += GAP;
        }
        first = false;
        let chip = format!(" {key} ");
        let chip_width = display_width(&chip);
        canvas.put_style(x, y, &chip, chip_width, style);
        if clickable && let Some(code) = key_code(key) {
            app.hits.push(Hit::new(x, y, chip_width, Target::Key(code)));
        }
        x += chip_width;
        if !desc.is_empty() {
            let text = format!(" {desc}");
            let room = canvas.width.saturating_sub(x);
            canvas.put(x, y, &text, room, Tone::Plain);
            x += display_width(&text);
        }
    }
}

fn filter_hint(app: &App) -> Vec<Seg> {
    vec![
        Seg::new(format!("/{}█", app.filter.text), Tone::Bold, 0),
        Seg::new("Enter — применить, Esc — сбросить и выйти", Tone::Dim, 1),
    ]
}

/// Варианты маски диалога исключения: выбранный отмечен стрелкой.
fn exclude_options(dialog: &ExcludeDialog) -> String {
    dialog
        .choices
        .iter()
        .enumerate()
        .map(|(index, mask)| {
            let mark = if index == dialog.selected { "►" } else { " " };
            let number = index + 1;
            format!("{mark}{number} «{mask}»")
        })
        .collect::<Vec<_>>()
        .join("   ")
}

/// Итог диалога исключения в одну строку окна.
fn exclude_summary(app: &App, dialog: &ExcludeDialog) -> String {
    if dialog.remove {
        let mask = dialog.choices.first().map_or("", String::as_str);
        format!(
            "Снять маску «{mask}»: вернётся узлов: {}",
            app.affected(&dialog.subscription, mask)
        )
    } else {
        let choice = dialog
            .choices
            .get(dialog.selected)
            .map_or("", String::as_str);
        format!(
            "Исключить «{}» маской «{choice}»: затронет узлов: {}",
            dialog.node,
            app.affected(&dialog.subscription, choice)
        )
    }
}

/// Строки состояния над подсказками. Сообщение переносится по словам, но не больше
/// `STATUS_LINES` строк; пустая строка состояния не даёт ни одной строки.
fn status_rows(app: &App, width: usize) -> Vec<Seg> {
    status_text(app).map_or_else(Vec::new, |(text, tone)| {
        wrap(&text, width, STATUS_LINES)
            .into_iter()
            .map(|line| Seg::new(line, tone, 0))
            .collect()
    })
}

/// Текст строки состояния и его тон; `None`, когда показывать нечего.
fn status_text(app: &App) -> Option<(String, Tone)> {
    if app.saving.is_some() {
        return Some(("* Сохраняю настройки…".to_owned(), Tone::Green));
    }
    if let Some(run) = &app.update {
        let target = run.target.as_deref().map_or_else(
            || "всех подписок".to_owned(),
            |name| format!("подписки «{name}»"),
        );
        let text = format!(
            "* Обновление {target}… {} с",
            app.now.saturating_sub(run.since)
        );
        return Some((text, Tone::Green));
    }
    if let Some(run) = &app.speed {
        let progress = match &run.progress {
            Some(progress) if progress.run == 0 => "узел применяется".to_owned(),
            Some(progress) => format!(
                "замер {} из {}, {}%",
                progress.run, progress.runs, progress.percent
            ),
            None => format!("{} с", app.now.saturating_sub(run.since)),
        };
        let text = format!(
            "* Тест скорости через «{}»: {progress} · Esc — отменить",
            run.target
        );
        return Some((text, Tone::Green));
    }
    app.notice
        .as_ref()
        .map(|notice| (notice.text.clone(), notice.tone))
}

/// Строки состояния над строкой клавиш; поле фильтра заменяет клавиши.
fn bottom(canvas: &mut Canvas<'_>, app: &mut App, status: Vec<Seg>) {
    let hints_y = canvas.height.saturating_sub(1);
    let top = hints_y.saturating_sub(status.len());
    for (row, seg) in status.into_iter().enumerate() {
        canvas.line(top + row, 0, vec![seg]);
    }
    if app.input == InputMode::Filter {
        let hint = filter_hint(app);
        canvas.line(hints_y, 0, hint);
    } else {
        let hints = normal_hints(app);
        keys_line(canvas, app, hints_y, &hints);
    }
}

/// Окно поверх экрана: одновременно открыто не больше одного. Нарисовано последним, чтобы
/// его кнопки были единственными областями нажатия.
fn overlay(canvas: &mut Canvas<'_>, app: &mut App) {
    if app.input == InputMode::Help {
        help_popup(canvas, app);
    } else if app.confirm.is_some() {
        confirm_popup(canvas, app);
    } else if app.exclude.is_some() {
        exclude_popup(canvas, app);
    } else if app.input == InputMode::Masks {
        masks_popup(canvas, app);
    } else if app.sub_edit.is_some() {
        sub_popup(canvas, app);
    }
}

/// Ширина окна: почти весь экран, но не шире `POPUP_MAX`.
fn popup_width(canvas: &Canvas<'_>) -> usize {
    canvas.width.saturating_sub(4).min(POPUP_MAX)
}

/// Ширина текста внутри окна: рамка и отступ по две ячейки с каждой стороны.
fn popup_text(canvas: &Canvas<'_>) -> usize {
    popup_width(canvas).saturating_sub(4)
}

/// Переносит текст по словам в строки окна одного тона.
fn wrapped(text: &str, width: usize, tone: Tone) -> Vec<(String, Tone)> {
    wrap(text, width, usize::MAX)
        .into_iter()
        .map(|line| (line, tone))
        .collect()
}

/// Окно по центру поверх экрана. Очищает свою область, рисует рамку акцентом, строки и
/// кнопки-плашки. Кнопки становятся областями нажатия, остальной экран нажатий не принимает.
fn popup(
    canvas: &mut Canvas<'_>,
    app: &mut App,
    heading: &str,
    lines: &[(String, Tone)],
    buttons: &[(&str, Target)],
) {
    app.hits.clear();
    let width = popup_width(canvas);
    let button_rows = if buttons.is_empty() { 0 } else { 2 };
    let height = (lines.len() + button_rows + 2).min(canvas.height);
    let x = (canvas.width - width) / 2;
    let y = (canvas.height - height) / 2;
    canvas.clear_rect(x, y, width, height);
    let border = canvas.active_frame();
    let title = vec![Seg::new(heading, Tone::Header, 0)];
    let mut inner = canvas.boxed(x, y, width, height, title, border);
    for (row, (text, tone)) in lines.iter().enumerate() {
        inner.line(row, 0, vec![Seg::new(text.as_str(), *tone, 0)]);
    }
    let chip = inner.key_style();
    let row = lines.len() + 1;
    let mut at = 0;
    for (label, target) in buttons {
        let text = format!(" {label} ");
        let text_width = display_width(&text);
        inner.put_style(at, row, &text, text_width, chip);
        app.hits
            .push(Hit::new(x + 2 + at, y + 1 + row, text_width, *target));
        at += text_width + GAP;
    }
}

fn help_popup(canvas: &mut Canvas<'_>, app: &mut App) {
    let text_width = popup_text(canvas).saturating_sub(KEY_COLUMN);
    let mut lines: Vec<(String, Tone)> = Vec::new();
    for (keys, text) in HELP {
        for (index, part) in wrap(text, text_width, usize::MAX).into_iter().enumerate() {
            let lead = if index == 0 {
                pad(keys, KEY_COLUMN)
            } else {
                " ".repeat(KEY_COLUMN)
            };
            lines.push((format!("{lead}{part}"), Tone::Plain));
        }
    }
    lines.push((String::new(), Tone::Plain));
    lines.push(("► выбран   * закреплён вручную".to_owned(), Tone::Dim));
    lines.push(("Любая клавиша закрывает справку".to_owned(), Tone::Dim));
    popup(canvas, app, "raycat tui: клавиши", &lines, &[]);
}

fn confirm_popup(canvas: &mut Canvas<'_>, app: &mut App) {
    let Some(question) = app.confirm.as_ref().map(|confirm| confirm.question.clone()) else {
        return;
    };
    let lines = wrapped(&question, popup_text(canvas), Tone::Plain);
    let buttons = [
        ("[Enter] Подтвердить", Target::ConfirmYes),
        ("[Esc] Отмена", Target::ConfirmNo),
    ];
    popup(canvas, app, "Подтверждение", &lines, &buttons);
}

fn exclude_popup(canvas: &mut Canvas<'_>, app: &mut App) {
    let Some(dialog) = app.exclude.as_ref() else {
        return;
    };
    let heading = if dialog.remove {
        "Снять исключение"
    } else {
        "Исключить узел"
    };
    let mut lines = wrapped(
        &exclude_summary(app, dialog),
        popup_text(canvas),
        Tone::Plain,
    );
    lines.push((String::new(), Tone::Plain));
    lines.push((exclude_options(dialog), Tone::Bold));
    let buttons = [
        ("[Enter] Проверить и сохранить", Target::Key(KeyCode::Enter)),
        ("[Esc] Отмена", Target::Key(KeyCode::Esc)),
    ];
    popup(canvas, app, heading, &lines, &buttons);
}

fn masks_popup(canvas: &mut Canvas<'_>, app: &mut App) {
    let mut lines = wrapped(
        "Через запятую, * — любая часть имени. Скрытые узлы не попадают в таблицу.",
        popup_text(canvas),
        Tone::Dim,
    );
    lines.push((String::new(), Tone::Plain));
    lines.push((format!("скрыть: {}█", app.mask_edit), Tone::Bold));
    let buttons = [
        ("[Enter] Применить", Target::Key(KeyCode::Enter)),
        ("[Esc] Отмена", Target::Key(KeyCode::Esc)),
    ];
    popup(canvas, app, "Скрытые узлы", &lines, &buttons);
}

fn sub_popup(canvas: &mut Canvas<'_>, app: &mut App) {
    let Some(edit) = app.sub_edit.as_ref() else {
        return;
    };
    let heading = format!("Маски подписки «{}»", edit.subscription);
    let affected = app.sub_affected(&edit.subscription, edit.kind, &edit.text);
    let mut lines = vec![
        (format!("Список: {}", edit.kind.title()), Tone::Plain),
        ("Tab — другой список".to_owned(), Tone::Dim),
        (String::new(), Tone::Plain),
        (format!("{}█", edit.text), Tone::Bold),
    ];
    let note = format!("через запятую, * — любая часть имени · затронет узлов: {affected}");
    lines.extend(wrapped(&note, popup_text(canvas), Tone::Dim));
    let buttons = [
        ("[Enter] Проверить и сохранить", Target::Key(KeyCode::Enter)),
        ("[Esc] Отмена", Target::Key(KeyCode::Esc)),
    ];
    popup(canvas, app, &heading, &lines, &buttons);
}

/// Заголовок подписок в таблице и подсказках; нужен и вкладкам.
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
    use raycat_proto::{
        CurrentNode, Event, Mode, Node, NodeCheck, NodeStatus, Nodes, SettingItem, SettingValue,
        SettingsView, Status, Tunnel, XHTTP_PROVIDER,
    };

    use super::super::app::{Confirm, Edit, InputMode, Msg, Setting, Snapshot, Tab};
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
            protocol: None,
            transport: None,
            security: None,
            checks: Vec::new(),
            uplink_bytes: Some(10 * 1024),
            downlink_bytes: Some(200 * 1024),
            excluded: false,
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
            tunnel: Tunnel::Working,
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
                app: "happ".to_owned(),
                platform: "windows".to_owned(),
                update_interval_secs: None,
                cut: 0,
                note: None,
                announce: None,
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
        for word in ["1 Обзор", "2 Узлы", "3 Подписки", "4 Журнал", "5 Настройки"]
        {
            assert!(lines[1].contains(word), "{word}: {:?}", lines[1]);
        }
        assert!(
            app.hits
                .iter()
                .any(|hit| hit.target == Target::Tab(Tab::Journal))
        );
    }

    #[test]
    fn the_overview_shows_the_connection_the_subscriptions_and_the_buttons() {
        let mut app = sample_app();
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "● VPN работает"), "{lines:?}");
        assert!(has_line(&lines, "Подписки · 1"), "{lines:?}");
        assert!(!has_line(&lines, "[u] Обновить подписки"), "{lines:?}");
        assert!(has_line(&lines, "работает"), "{lines:?}");
        assert!(has_line(&lines, "[a] Автовыбор"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::Autoselect));
    }

    #[test]
    fn the_tunnel_panel_draws_the_speed_graph_from_the_history() {
        let mut nodes = sample_nodes();
        nodes[0].uplink_bytes = Some(0);
        nodes[0].downlink_bytes = Some(0);
        let mut app = App::new(NOW);
        let base = Instant::now();
        app.apply(snapshot_at(sample_status(), nodes.clone(), base));
        nodes[0].downlink_bytes = Some(125_000);
        app.apply(snapshot_at(
            sample_status(),
            nodes,
            base + Duration::from_secs(1),
        ));
        assert_eq!(app.speeds.len(), 1);
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "↓ "), "{lines:?}");
        assert!(has_line(&lines, "Мбит/с"), "{lines:?}");
        assert!(has_line(&lines, "▇"), "{lines:?}");
    }

    #[test]
    fn without_a_speed_the_tunnel_panel_shows_a_dash_and_no_graph() {
        let mut app = sample_app();
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "↓ —"), "{lines:?}");
        assert!(!has_line(&lines, "▇"), "{lines:?}");
    }

    #[test]
    fn the_node_details_sit_beside_the_table_from_120_columns() {
        let mut app = sample_app();
        on_nodes(&mut app);
        let wide = screen(&mut app, 160, 30);
        assert!(has_line(&wide, "Узел · подробности"), "{wide:?}");
        assert!(has_line(&wide, "Протокол:"), "{wide:?}");
        let narrow = screen(&mut app, 100, 30);
        assert!(!has_line(&narrow, "Узел · подробности"), "{narrow:?}");
    }

    #[test]
    fn enter_opens_the_node_window_on_a_narrow_screen_and_esc_closes_it() {
        let mut app = sample_app();
        on_nodes(&mut app);
        press(&mut app, KeyCode::Enter);
        let open = screen(&mut app, 100, 30);
        assert!(has_line(&open, "Узел · подробности"), "{open:?}");
        press(&mut app, KeyCode::Esc);
        let closed = screen(&mut app, 100, 30);
        assert!(!has_line(&closed, "Узел · подробности"), "{closed:?}");
    }

    #[test]
    fn the_nodes_tab_shows_the_reason_and_the_safe_names() {
        let mut nodes = sample_nodes();
        nodes[0].name = "🇩🇪 Германия".to_owned();
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "DE Германия"), "{lines:?}");
        assert!(has_line(&lines, "Причина"), "{lines:?}");
        assert!(has_line(&lines, "нет ответа"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::NodeRow(0)));
    }

    #[test]
    fn a_selected_node_row_is_painted_to_the_panel_edges() {
        let mut app = sample_app();
        on_nodes(&mut app);
        let terminal = paint(&mut app, 100, 30, true);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        let y = lines.iter().position(|line| line.contains("NL-1")).unwrap();
        for x in 1..99 {
            assert_eq!(
                buffer[(x, u16::try_from(y).unwrap())].bg,
                Color::Indexed(238),
                "ячейка {x}"
            );
        }
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
        assert!(lines[0].contains("● подключение…"), "{lines:?}");
        assert!(has_line(&lines, "данных от демона ещё нет"), "{lines:?}");
        assert!(lines.last().unwrap().contains("выход"));

        app.apply(Msg::Down {
            reason: "демон не запущен: сокета /run/raycat/raycat.sock нет".to_owned(),
            retry_in: Duration::from_secs(2),
        });
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].contains("● нет связи с демоном"), "{lines:?}");
        assert!(lines[1].contains("2 Узлы"), "{lines:?}");
        assert_eq!(
            lines[2],
            "демон недоступен: демон не запущен: сокета /run/raycat/raycat.sock нет"
        );
        assert_eq!(lines[3], "повторное подключение через 2 с");
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
        assert_eq!(lines[2], "демон недоступен: обрыв связи");
        assert_eq!(lines[3], "повторное подключение через 1 с");
        assert!(has_line(&lines, "NL-1"));
        let dimmed = |y: u16| buffer[(0, y)].style().add_modifier.contains(Modifier::DIM);
        assert!(!dimmed(1));
        assert!(dimmed(4));
    }

    #[test]
    fn a_lost_link_shows_in_the_header_as_a_red_plaque() {
        let mut app = sample_app();
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        let terminal = paint(&mut app, 80, 24, true);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        assert!(lines[0].contains("raycat"), "{lines:?}");
        assert!(lines[0].contains("● нет связи с демоном"), "{lines:?}");
        assert_eq!(buffer[(10, 0)].symbol(), "●");
        assert_eq!(buffer[(10, 0)].style().bg, Some(Color::Indexed(203)));
    }

    #[test]
    fn a_long_reason_is_wrapped_over_a_few_lines() {
        let mut app = App::new(NOW);
        app.apply(Msg::Down {
            reason: "слово ".repeat(40),
            retry_in: Duration::from_secs(1),
        });
        let lines = screen(&mut app, 44, 20);
        assert!(lines[2].starts_with("демон недоступен: слово"));
        assert!(lines[3].starts_with("слово"));
        assert!(lines[4].ends_with('…'), "{:?}", lines[4]);
        assert_eq!(lines[5], "повторное подключение через 1 с");
    }

    #[test]
    fn the_smallest_supported_terminal_keeps_the_hints() {
        let mut app = sample_app();
        let lines = screen(&mut app, 44, 12);
        for line in &lines {
            assert!(display_width(line) <= 44, "{line:?}");
        }
        assert!(lines[1].contains("1 Обзор"));
        assert!(lines.last().unwrap().contains("справка"));
        assert!(lines.last().unwrap().contains("выход"));
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
                last.contains("справка") && last.contains("выход"),
                "{width}: {last:?}"
            );
        }
    }

    #[test]
    fn help_is_a_window_over_the_screen_and_lists_the_new_keys() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        let lines = screen(&mut app, 80, 40);
        let text = lines.join("\n");
        assert!(text.contains("raycat tui: клавиши"), "{text}");
        assert!(text.contains("2 Узлы"), "окно не закрывает экран: {text}");
        for word in ["Enter", "PgUp", "Shift+Tab", "Esc", "Ctrl+C", "закрепить"] {
            assert!(text.contains(word), "{word}: {text}");
        }
        assert!(text.contains("► выбран   * закреплён вручную"));
        let narrow = screen(&mut app, 44, 12);
        for line in &narrow {
            assert!(display_width(line) <= 44, "{line:?}");
        }
    }

    #[test]
    fn any_key_closes_the_help_window() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input, InputMode::Normal);
    }

    #[test]
    fn progress_and_results_show_in_the_status_line() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('u'));
        app.set_now(NOW + 5);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[18], "* Обновление всех подписок… 5 с");
        app.apply(Msg::Updated(Err("демон не ответил вовремя".to_owned())));
        let lines = screen(&mut app, 80, 20);
        assert_eq!(
            lines[18],
            "Не удалось обновить подписки: демон не ответил вовремя"
        );
    }

    #[test]
    fn the_status_line_has_no_ready_word_when_idle() {
        let mut app = sample_app();
        let lines = screen(&mut app, 80, 20);
        assert!(!has_line(&lines, "готов"), "{lines:?}");
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
        assert_eq!(delay_tone(Some(499)), Tone::Green);
        assert_eq!(delay_tone(Some(500)), Tone::Yellow);
        assert_eq!(delay_tone(Some(999)), Tone::Yellow);
        assert_eq!(delay_tone(Some(1000)), Tone::Red);
        assert_eq!(delay_tone(None), Tone::Dim);
    }

    #[test]
    fn a_proxy_screen_has_no_kill_switch() {
        let mut status = sample_status();
        status.mode = Mode::Proxy;
        status.kill_switch = None;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].contains("● VPN работает"), "{lines:?}");
        assert!(lines[0].contains("прокси"), "{lines:?}");
        assert!(!lines[0].contains("kill switch"), "{lines:?}");
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
        status.tunnel = Tunnel::XrayDown;
        status.node = None;
        let mut app = app_with(status, Vec::new());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].contains("● xray не запущен"), "{lines:?}");
        assert!(has_line(&lines, "узел не выбран"), "{lines:?}");

        let mut status = sample_status();
        status.xray.restarts = 2;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].contains("● VPN работает"), "{lines:?}");
        assert!(has_line(&lines, "xray перезапускался: 2"), "{lines:?}");
    }

    #[test]
    fn a_stuck_xray_is_a_problem_in_the_header() {
        let mut status = sample_status();
        status.xray.api_stuck = true;
        status.xray.api_silent_secs = Some(120);
        status.tunnel = Tunnel::XrayStuck;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[0].contains("● xray не отвечает"), "{lines:?}");
    }

    #[test]
    fn without_live_nodes_the_card_says_what_happens_to_the_traffic() {
        let mut status = sample_status();
        status.tunnel = Tunnel::NoLiveNodes;
        status.kill_switch = Some(true);
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 160, 30);
        assert!(has_line(&lines, "● нет живых узлов"), "{lines:?}");
        assert!(has_line(&lines, "мимо VPN он не уйдёт"), "{lines:?}");
    }

    #[test]
    fn checking_nodes_is_yellow_and_has_no_hint() {
        let mut status = sample_status();
        status.tunnel = Tunnel::Checking;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "● проверяю узлы"), "{lines:?}");
        assert!(!has_line(&lines, "не уйдёт"), "{lines:?}");
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
        assert_eq!(lines[2], "демон недоступен: raycat не запущен");
        assert_eq!(lines[3], "Запустите службу: sudo systemctl start raycat");
        assert_eq!(lines[4], "повторное подключение через 1 с");
        assert_eq!(buffer[(0, 2)].style().fg, Some(Color::Indexed(203)));
        assert_ne!(buffer[(0, 3)].style().fg, Some(Color::Indexed(203)));
    }

    #[test]
    fn a_flag_in_a_name_is_a_code_and_the_status_column_stays_aligned() {
        let mut nodes = sample_nodes();
        nodes[0].name = "🇩🇪 Германия".to_owned();
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        let lines = screen(&mut app, 100, 30);
        let flagged = lines
            .iter()
            .find(|line| line.contains("DE Германия"))
            .unwrap();
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
        assert_eq!(buffer[(0, 3)].symbol(), "╭");
        assert_eq!(buffer[(79, 3)].symbol(), "╮");
        assert_eq!(buffer[(0, 3)].style().fg, Some(Color::Indexed(240)));
    }

    #[test]
    fn the_header_drops_its_right_part_first_when_narrow() {
        let mut app = sample_app();
        let wide = screen(&mut app, 100, 20);
        assert!(wide[0].contains("kill switch вкл"), "{wide:?}");
        assert!(wide[0].contains("шлюз"), "{wide:?}");
        let narrow = screen(&mut app, 44, 20);
        assert!(!narrow[0].contains("kill switch"), "{narrow:?}");
    }

    fn text(value: &str) -> SettingValue {
        SettingValue::Text(value.to_owned())
    }

    fn setting_item(table: &str, key: &str, value: SettingValue, source: &str) -> SettingItem {
        SettingItem {
            table: Some(table.to_owned()),
            subscription: None,
            key: key.to_owned(),
            value,
            source: source.to_owned(),
        }
    }

    fn settings_view() -> SettingsView {
        SettingsView {
            items: vec![
                setting_item("routing", "ru_direct", SettingValue::Bool(true), "из файла"),
                setting_item(
                    "mode",
                    "kill_switch",
                    SettingValue::Bool(true),
                    "по умолчанию",
                ),
                setting_item("selection", "check_interval", text("30s"), "по умолчанию"),
                setting_item("selection", "switch_gain", text("150ms"), "по умолчанию"),
                setting_item("selection", "return_delay", text("5m"), "по умолчанию"),
                setting_item("selection", "failures", SettingValue::Number(3), "из файла"),
                setting_item(
                    "xray",
                    "xhttp_connections",
                    text(XHTTP_PROVIDER),
                    "по умолчанию",
                ),
                setting_item("log", "level", text("warn"), "из файла"),
            ],
        }
    }

    fn on_settings(app: &mut App) {
        press(app, KeyCode::Char('5'));
        app.apply(Msg::Settings(Ok(settings_view())));
    }

    #[test]
    fn the_settings_tab_shows_a_list_and_the_details_of_the_chosen_row() {
        let mut app = sample_app();
        on_settings(&mut app);
        let lines = screen(&mut app, 120, 30);
        for word in [
            "Россия напрямую",
            "● вкл",
            "как у провайдера",
            "warn",
            "Подключение",
            "Выбор узла",
            "Система",
            "Вид",
            "Сейчас",
            "По умолчанию",
            "из файла",
            "российские домены и подсети идут напрямую",
            "Файл настроек: не найден",
            "Остальное в файле, raycat config покажет итог",
        ] {
            assert!(has_line(&lines, word), "{word}: {lines:?}");
        }
    }

    #[test]
    fn a_section_has_an_empty_row_above_it_except_the_first() {
        let mut app = sample_app();
        on_settings(&mut app);
        let lines = screen(&mut app, 120, 40);
        assert!(lines[3].starts_with("│ Подключение"), "{lines:?}");
        let heading = lines
            .iter()
            .position(|line| line.starts_with("│ Выбор узла"))
            .unwrap();
        let above: String = lines[heading - 1].chars().skip(1).take(40).collect();
        assert!(above.trim().is_empty(), "{lines:?}");
    }

    #[test]
    fn the_selected_setting_row_is_painted_to_the_panel_edges() {
        let mut app = sample_app();
        on_settings(&mut app);
        let terminal = paint(&mut app, 100, 30, true);
        let buffer = terminal.backend().buffer();
        let lines = rows(buffer);
        let y = lines
            .iter()
            .position(|line| line.starts_with("│ Россия напрямую"))
            .unwrap();
        let y = u16::try_from(y).unwrap();
        for x in 1..54 {
            assert_eq!(buffer[(x, y)].bg, Color::Indexed(238), "ячейка {x}");
        }
    }

    #[test]
    fn a_field_shows_its_text_with_a_cursor_and_what_keys_do() {
        let mut app = sample_app();
        on_settings(&mut app);
        app.setting_cursor = Setting::CheckInterval.row();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('9'));
        let lines = screen(&mut app, 100, 30);
        assert!(has_line(&lines, "[30s9█]"), "{lines:?}");
        assert!(
            has_line(&lines, "Enter — сохранить · Esc — отмена"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_question_is_a_window_with_two_buttons() {
        let mut app = sample_app();
        let edit = Edit {
            changes: Vec::new(),
            done: String::new(),
            row: None,
            warn: None,
        };
        app.confirm = Some(Confirm {
            question: "xray перезапустится: связь прервётся на несколько секунд".to_owned(),
            edit,
        });
        let lines = screen(&mut app, 100, 24);
        assert!(has_line(&lines, "xray перезапустится"), "{lines:?}");
        assert!(has_line(&lines, "[Enter] Подтвердить"), "{lines:?}");
        assert!(has_line(&lines, "[Esc] Отмена"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::ConfirmYes));
        assert!(app.hits.iter().any(|hit| hit.target == Target::ConfirmNo));
    }

    #[test]
    fn a_narrow_settings_tab_keeps_names_and_values_and_drops_the_details() {
        let mut app = sample_app();
        on_settings(&mut app);
        let lines = screen(&mut app, 44, 30);
        for word in ["Россия напрямую", "● вкл", "как у провайдера", "warn"]
        {
            assert!(has_line(&lines, word), "{word}: {lines:?}");
        }
        assert!(!has_line(&lines, "из файла"), "{lines:?}");
        assert!(has_line(&lines, "Файл настроек"), "{lines:?}");
        assert!(
            lines.iter().all(|line| display_width(line) <= 44),
            "{lines:?}"
        );
    }

    #[test]
    fn the_subscriptions_panel_shows_the_selected_subscription() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('3'));
        let lines = screen(&mut app, 100, 30);
        for word in [
            "Подписка «main»",
            "эмуляция: happ · windows",
            "отсечено масками: 0",
            "Узлов живых",
            "[m] Маски",
        ] {
            assert!(has_line(&lines, word), "{word}: {lines:?}");
        }
    }

    #[test]
    fn the_node_details_show_protocol_security_and_the_last_checks() {
        let mut nodes = sample_nodes();
        nodes[0].protocol = Some("vless".to_owned());
        nodes[0].transport = Some("xhttp".to_owned());
        nodes[0].security = Some("reality".to_owned());
        nodes[0].checks = vec![
            NodeCheck {
                at: NOW - 120,
                alive: true,
                latency_ms: Some(31),
            },
            NodeCheck {
                at: NOW - 60,
                alive: false,
                latency_ms: None,
            },
        ];
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        press(&mut app, KeyCode::Enter);
        let lines = screen(&mut app, 100, 30);
        assert!(
            has_line(
                &lines,
                "Протокол: vless, транспорт: xhttp, безопасность: reality"
            ),
            "{lines:?}"
        );
        assert!(has_line(&lines, "31 мс"), "{lines:?}");
        assert!(has_line(&lines, "сбой"), "{lines:?}");
    }

    #[test]
    fn the_node_details_list_the_checks_before_the_full_error() {
        let mut nodes = sample_nodes();
        nodes[0].checks = vec![NodeCheck {
            at: NOW - 60,
            alive: true,
            latency_ms: Some(31),
        }];
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        press(&mut app, KeyCode::Enter);
        let lines = screen(&mut app, 100, 30);
        let line_of = |text: &str| lines.iter().position(|line| line.contains(text));
        let checks = line_of("Последние проверки:");
        let error = line_of("Ошибка полностью:");
        assert!(checks.is_some() && checks < error, "{lines:?}");
    }

    #[test]
    fn the_node_details_scroll_to_the_end_of_a_long_error() {
        let mut nodes = sample_nodes();
        nodes[0].last_error = Some(format!("{} ХВОСТ", "ошибка соединения ".repeat(40)));
        let mut app = app_with(sample_status(), nodes);
        on_nodes(&mut app);
        press(&mut app, KeyCode::Enter);
        let top = screen(&mut app, 100, 20);
        assert!(has_line(&top, "[p] закрепить"), "{top:?}");
        assert!(!has_line(&top, "ХВОСТ"), "{top:?}");
        press(&mut app, KeyCode::End);
        let end = screen(&mut app, 100, 20);
        assert!(has_line(&end, "ХВОСТ"), "{end:?}");
        assert!(has_line(&end, "[p] закрепить"), "{end:?}");
        press(&mut app, KeyCode::Home);
        let back = screen(&mut app, 100, 20);
        assert!(has_line(&back, "Выбор:"), "{back:?}");
        assert!(!has_line(&back, "ХВОСТ"), "{back:?}");
    }

    #[test]
    fn the_layout_stays_inside_the_terminal_on_every_tab() {
        let mut app = sample_app();
        on_settings(&mut app);
        for key in ['1', '2', '3', '4', '5'] {
            press(&mut app, KeyCode::Char(key));
            for (width, height) in [(44_u16, 12_u16), (80, 20), (120, 40)] {
                for line in screen(&mut app, width, height) {
                    assert!(display_width(&line) <= usize::from(width), "{line:?}");
                }
            }
        }
    }

    const WRAPPED: &str = "демон не ответил вовремя: файл настроек смонтирован отдельным файлом, его нельзя заменить: смонтируйте каталог или правьте файл на хосте и выполните raycat reload";

    #[test]
    fn a_long_status_message_wraps_above_the_hints_at_100_columns() {
        let mut app = sample_app();
        app.apply(Msg::Updated(Err(WRAPPED.to_owned())));
        let lines = screen(&mut app, 100, 24);
        let start = lines
            .iter()
            .position(|line| line.starts_with("Не удалось обновить"))
            .unwrap();
        let shown: Vec<&str> = lines[start..23]
            .iter()
            .map(|line| line.trim_end())
            .collect();
        assert_eq!(
            shown.join(" "),
            format!("Не удалось обновить подписки: {WRAPPED}")
        );
        assert!(lines[23].contains("справка"), "{lines:?}");
    }

    #[test]
    fn a_status_message_is_cut_only_after_three_lines_at_44_columns() {
        let mut app = sample_app();
        app.apply(Msg::Updated(Err(WRAPPED.to_owned())));
        let lines = screen(&mut app, 44, 20);
        let start = lines
            .iter()
            .position(|line| line.starts_with("Не удалось обновить"))
            .unwrap();
        assert_eq!(start, 16, "{lines:?}");
        assert!(lines[18].ends_with('…'), "{lines:?}");
        assert!(
            lines[19].contains("справка") && lines[19].contains("выход"),
            "{lines:?}"
        );
    }

    #[test]
    fn an_expired_subscription_gives_its_date_once() {
        let mut status = sample_status();
        let expire = NOW - DAY;
        let date = format_date(expire, &TimeZone::UTC);
        status.subscriptions[0].expire = Some(expire);
        status.subscriptions[0].note = Some(format!("истекла {date}"));
        let mut app = app_with(status, sample_nodes());
        let overview = screen(&mut app, 100, 30);
        assert!(
            has_line(&overview, &format!("истекла {date}")),
            "{overview:?}"
        );
        assert!(!has_line(&overview, "истёк"), "{overview:?}");
        assert_eq!(
            overview.join("\n").matches(date.as_str()).count(),
            1,
            "{overview:?}"
        );
        press(&mut app, KeyCode::Char('3'));
        let subs = screen(&mut app, 100, 30);
        let row = subs.iter().find(|line| line.contains("узлов 2/3")).unwrap();
        assert!(row.contains(&format!("истекла {date}")), "{row}");
        assert_eq!(row.matches(date.as_str()).count(), 1, "{row}");
    }

    /// Символы, которые есть в шрифте консоли Windows: ASCII, кириллица, Latin-1, рамки и
    /// символы того же набора, которыми пользуется TUI.
    fn in_console_font(c: char) -> bool {
        c.is_ascii()
            || ('\u{0400}'..='\u{04FF}').contains(&c)
            || ('\u{00A0}'..='\u{00FF}').contains(&c)
            || ('\u{2500}'..='\u{257F}').contains(&c)
            || "–—…●○▲▼↑↓←→►█░▁▂▃▄▅▆▇√".contains(c)
    }

    #[test]
    fn every_screen_uses_only_symbols_the_console_font_has() {
        let mut app = sample_app();
        app.apply(Msg::Settings(Ok(settings_view())));
        let mut screens = Vec::new();
        for key in ['1', '2', '3', '4', '5'] {
            press(&mut app, KeyCode::Char(key));
            screens.push(screen(&mut app, 100, 30));
        }
        press(&mut app, KeyCode::Char('?'));
        screens.push(screen(&mut app, 80, 40));
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Enter);
        screens.push(screen(&mut app, 100, 30));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('u'));
        screens.push(screen(&mut app, 100, 30));
        for line in screens.iter().flatten() {
            if let Some(symbol) = line.chars().find(|c| !in_console_font(*c)) {
                panic!("символ {symbol:?} нет в шрифте консоли: {line:?}");
            }
        }
    }

    #[test]
    fn the_events_card_takes_the_rest_of_the_screen() {
        let mut app = sample_app();
        for index in 0..10 {
            app.apply(Msg::Event(Event::Warning {
                level: "warn".to_owned(),
                message: format!("событие {index}"),
            }));
        }
        let lines = screen(&mut app, 80, 30);
        let shown = lines.iter().filter(|line| line.contains("событие")).count();
        assert!(shown > 5, "{lines:?}");
        let top = lines
            .iter()
            .position(|line| line.contains("Последние события"))
            .unwrap();
        let bottom = top
            + lines[top..]
                .iter()
                .position(|line| line.starts_with('╰'))
                .unwrap();
        assert_eq!(bottom, 28, "{lines:?}");
    }
}
