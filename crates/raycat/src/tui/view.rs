//! Каркас экрана: строка вкладок, шапка с состоянием и текущим узлом, баннер связи,
//! строка уведомлений и подсказок. Содержимое вкладок — в `pages`.

use jiff::tz::TimeZone;
use ratatui::Frame;
use raycat_proto::{SubscriptionStatus, XrayStatus};

use super::app::{App, ExcludeDialog, Hit, InputMode, Link, Tab, Target};
use super::canvas::{Canvas, GAP, Palette, Seg, fit, highlight, wrap};
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
    if app.input == InputMode::Help {
        help(canvas);
        return;
    }
    tabs(canvas, app);
    let status = status_rows(app, canvas.width);
    // Строки состояния и строка подсказок внизу; тело вкладки занимает всё выше них.
    let bottom_rows = status.len().max(1) + 1;
    let room = canvas.height.saturating_sub(bottom_rows + BODY_MIN);
    let mut y = 1;
    y += banner(canvas, app, y, room.saturating_sub(y));
    canvas.set_dim(matches!(app.link, Link::Down { .. }));
    y += header(canvas, app, y, room.saturating_sub(y));

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

/// Общая строка состояния. Узел, задержка и скорость — в карточке обзора.
fn header(canvas: &mut Canvas<'_>, app: &App, y: usize, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    header_top(canvas, app, y);
    1
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

fn header_top(canvas: &mut Canvas<'_>, app: &App, y: usize) {
    let mut segs = vec![Seg::new("raycat", Tone::Bold, 0)];
    if let Some(status) = &app.status {
        segs.push(Seg::new(render::mode_name(status.mode), Tone::Plain, 0));
    }
    segs.push(tunnel_seg(app));
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

/// Подсказки «клавиша описание»: клавиша жирным, описание тусклым.
fn normal_hints(app: &App) -> Vec<Seg> {
    let items: &[(&str, u8)] = match app.tab {
        Tab::Overview => &[
            ("u обновить", 1),
            ("s скорость", 2),
            ("a автовыбор", 3),
            ("2 узлы", 2),
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Nodes if app.details => &[
            ("Esc назад", 0),
            ("p закрепить", 1),
            ("t скорость", 1),
            ("? справка", 0),
            ("q выход", 0),
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
            ("? справка", 0),
            ("q выход", 0),
        ],
        Tab::Subscriptions => &[
            ("↑↓ выбор", 1),
            ("u обновить", 1),
            ("m маски", 1),
            ("[ ] приоритет", 2),
            ("Enter ошибка", 2),
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
        Tab::Settings => &[
            ("↑↓ выбор", 1),
            ("Enter изменить", 1),
            ("Пробел переключить", 2),
            ("? справка", 0),
            ("q выход", 0),
        ],
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

fn masks_hint(app: &App) -> Vec<Seg> {
    vec![
        Seg::new(format!("скрыть: {}█", app.mask_edit), Tone::Bold, 0),
        Seg::new(
            "через запятую, * — любая часть имени; Enter — применить, Esc — отмена",
            Tone::Dim,
            1,
        ),
    ]
}

/// Итог диалога исключения в строке состояния.
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

/// Варианты маски и клавиши диалога исключения в строке подсказок.
fn exclude_hint(dialog: &ExcludeDialog) -> String {
    let options: Vec<String> = dialog
        .choices
        .iter()
        .enumerate()
        .map(|(index, mask)| {
            let mark = if index == dialog.selected { "►" } else { " " };
            format!("{mark}{} «{mask}»", index + 1)
        })
        .collect();
    format!(
        "{} · Enter — проверить и сохранить · Esc — отмена",
        options.join("   ")
    )
}

/// Подсказка поля правки на вкладке «Настройки».
fn field_hint() -> Vec<Seg> {
    vec![Seg::new(
        "Enter — сохранить · Esc — отмена · например 90s, 5m или 3",
        Tone::Dim,
        0,
    )]
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
    if let Some(confirm) = &app.confirm {
        return Some((confirm.question.clone(), Tone::Yellow));
    }
    if let Some(dialog) = &app.exclude {
        return Some((exclude_summary(app, dialog), Tone::Bold));
    }
    if let Some(edit) = &app.sub_edit {
        let affected = app.sub_affected(&edit.subscription, edit.kind, &edit.text);
        let text = format!(
            "Маски {}: {}█ · затронет узлов: {affected}",
            edit.kind.title(),
            edit.text
        );
        return Some((text, Tone::Bold));
    }
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
    if let Some(notice) = &app.notice {
        return Some((notice.text.clone(), notice.tone));
    }
    if matches!(app.link, Link::Up) {
        return Some(("готов".to_owned(), Tone::Dim));
    }
    None
}

/// Строки состояния и строка подсказок внизу экрана.
fn bottom(canvas: &mut Canvas<'_>, app: &mut App, status: Vec<Seg>) {
    let hints_y = canvas.height.saturating_sub(1);
    let top = hints_y.saturating_sub(status.len().max(1));
    for (row, seg) in status.into_iter().enumerate() {
        canvas.line(top + row, 0, vec![seg]);
    }
    if app.confirm.is_some() {
        let buttons = vec![
            (
                "[Enter] Подтвердить".to_owned(),
                Tone::Bold,
                Some(Target::ConfirmYes),
            ),
            (
                "[Esc] Отмена".to_owned(),
                Tone::Bold,
                Some(Target::ConfirmNo),
            ),
        ];
        pages::chips(canvas, app, hints_y, buttons);
    } else if let Some(dialog) = &app.exclude {
        canvas.line(
            hints_y,
            0,
            vec![Seg::new(exclude_hint(dialog), Tone::Dim, 0)],
        );
    } else if app.sub_edit.is_some() {
        let hint = "Tab — другой список · Enter — проверить и сохранить · Esc — отмена";
        canvas.line(hints_y, 0, vec![Seg::new(hint, Tone::Dim, 0)]);
    } else if app.input == InputMode::Filter {
        canvas.line(hints_y, 0, filter_hint(app));
    } else if app.input == InputMode::Masks {
        canvas.line(hints_y, 0, masks_hint(app));
    } else if app.input == InputMode::Field {
        canvas.line(hints_y, 0, field_hint());
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
    let legend = vec![Seg::new("► выбран   * закреплён вручную", Tone::Dim, 0)];
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
    use raycat_proto::{
        CurrentNode, Event, Mode, Node, NodeCheck, NodeStatus, Nodes, SettingItem, SettingValue,
        SettingsView, Status, Tunnel, XHTTP_PROVIDER,
    };

    use super::super::app::{Confirm, Edit, Msg, Setting, Snapshot, Tab};
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
            assert!(lines[0].contains(word), "{word}: {:?}", lines[0]);
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
        assert!(has_line(&lines, "Подписки · всего 1"), "{lines:?}");
        assert!(has_line(&lines, "[u] Обновить подписки"), "{lines:?}");
        assert!(has_line(&lines, "работает"), "{lines:?}");
        assert!(has_line(&lines, "[a] Автовыбор"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::UpdateAll));
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
    fn speed_below_one_kilobit_is_a_dash() {
        assert_eq!(speed_label(27, 23), "↓ — ↑ —");
        assert_eq!(speed_label(5_200_000, 300), "↓ 5.2 Мбит/с ↑ —");
    }

    #[test]
    fn without_a_daemon_the_banner_sits_under_the_tabs() {
        let mut app = App::new(NOW);
        let lines = screen(&mut app, 80, 20);
        assert_eq!(lines[1], "raycat · ● подключение…");
        assert!(has_line(&lines, "данных от демона ещё нет"), "{lines:?}");
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
        assert_eq!(lines[3], "raycat · ● нет связи с демоном");
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
        assert!(
            lines[3].starts_with("raycat · шлюз · ● нет связи с демоном"),
            "{lines:?}"
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
        assert!(text.contains("► выбран   * закреплён вручную"));
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
        assert_eq!(lines[18], "* Обновление всех подписок… 5 с");
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
        assert!(lines[1].starts_with("raycat · прокси · ● VPN работает"));
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
        status.tunnel = Tunnel::XrayDown;
        status.node = None;
        let mut app = app_with(status, Vec::new());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[1].contains("● xray не запущен"), "{lines:?}");
        assert!(has_line(&lines, "узел не выбран"), "{lines:?}");

        let mut status = sample_status();
        status.xray.restarts = 2;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[1].contains("● VPN работает"), "{lines:?}");
        assert!(lines[1].contains("xray перезапускался: 2"), "{lines:?}");
    }

    #[test]
    fn a_stuck_xray_is_a_problem_in_the_header() {
        let mut status = sample_status();
        status.xray.api_stuck = true;
        status.xray.api_silent_secs = Some(120);
        status.tunnel = Tunnel::XrayStuck;
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 80, 20);
        assert!(lines[1].contains("● xray не отвечает"), "{lines:?}");
    }

    #[test]
    fn without_live_nodes_the_card_says_what_happens_to_the_traffic() {
        let mut status = sample_status();
        status.tunnel = Tunnel::NoLiveNodes;
        status.kill_switch = Some(true);
        let mut app = app_with(status, sample_nodes());
        let lines = screen(&mut app, 100, 30);
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
        assert_eq!(buffer[(0, 3)].style().fg, Some(Color::DarkGray));
    }

    #[test]
    fn the_header_hides_the_uptime_when_it_does_not_fit() {
        let mut app = sample_app();
        app.set_now(NOW);
        let lines = screen(&mut app, 60, 20);
        assert!(!lines[1].contains("время работы"), "{:?}", lines[1]);
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

    #[test]
    fn the_settings_tab_shows_each_value_with_its_source_and_hint() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('5'));
        app.apply(Msg::Settings(Ok(settings_view())));
        let lines = screen(&mut app, 100, 30);
        for word in [
            "Россия напрямую",
            "[вкл]",
            "[3]",
            "[как у провайдера]",
            "error  [warn]  info  debug",
            "из файла",
            "по умолчанию",
            "российские домены и подсети идут напрямую",
            "Файл настроек: не найден",
            "Остальные настройки — в файле, raycat config покажет итог",
        ] {
            assert!(has_line(&lines, word), "{word}: {lines:?}");
        }
    }

    #[test]
    fn a_field_shows_its_text_with_a_cursor_and_what_keys_do() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('5'));
        app.apply(Msg::Settings(Ok(settings_view())));
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
    fn a_question_takes_the_status_line_and_offers_two_buttons() {
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
        assert!(lines[22].contains("xray перезапустится"), "{lines:?}");
        assert!(lines[23].contains("[Enter] Подтвердить"), "{lines:?}");
        assert!(lines[23].contains("[Esc] Отмена"), "{lines:?}");
        assert!(app.hits.iter().any(|hit| hit.target == Target::ConfirmYes));
        assert!(app.hits.iter().any(|hit| hit.target == Target::ConfirmNo));
    }

    #[test]
    fn a_narrow_settings_tab_keeps_names_and_values_and_drops_the_source_first() {
        let mut app = sample_app();
        press(&mut app, KeyCode::Char('5'));
        app.apply(Msg::Settings(Ok(settings_view())));
        let lines = screen(&mut app, 44, 30);
        for word in ["Россия напрямую", "[вкл]", "[как у провайдера]", "[warn]"]
        {
            assert!(has_line(&lines, word), "{word}: {lines:?}");
        }
        assert!(!has_line(&lines, "из файла"), "{lines:?}");
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
        assert!(lines[23].contains("? справка"), "{lines:?}");
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
            lines[19].contains("? справка") && lines[19].contains("q выход"),
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
            || "–—…●○▲▼↑↓←→►█░√".contains(c)
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
        screens.push(screen(&mut app, 80, 24));
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
    fn the_connection_card_has_no_blank_row_without_a_hint() {
        let mut app = sample_app();
        let lines = screen(&mut app, 80, 20);
        let node = lines
            .iter()
            .position(|line| line.contains("► NL-1"))
            .unwrap();
        assert!(lines[node + 1].starts_with('╰'), "{lines:?}");
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
        assert!(lines[27].starts_with('╰'), "{lines:?}");
    }
}
