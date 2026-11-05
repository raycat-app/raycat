//! Рисование прямо в буфер ratatui. Всё, что попадает на экран, очищается от
//! управляющих символов и обрезается по границам: текст от провайдера не может ни
//! выйти за область, ни управлять терминалом.

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::term::{Tone, char_width, display_width, truncate};
use crate::util::sanitize;

pub(super) const GAP: usize = 2;

// Цвета btop → индекс 256-цветной палитры (через ssh и `docker exec` truecolor не работает).
/// Заголовок активной панели (btop title `#eeeeee`).
const TITLE: u8 = 255;
/// Заголовок неактивной панели.
const TITLE_DIM: u8 = 250;
/// Горячая клавиша (btop hi_fg `#b54040`; на тёмном фоне 131 читается плохо).
const HOTKEY: u8 = 167;
/// Фон выделенной строки (btop selected_bg `#6a2f2f`).
const SELECTED: u8 = 52;
/// Второстепенный текст.
const SECONDARY: u8 = 245;
/// Разделители и пустые ячейки полосок (btop div_line `#303030`). Он же фон плашки клавиши.
const DIVIDER: u8 = 237;
/// Хорошо, начало градиента (btop `#77ca9b`).
const GREEN: u8 = 115;
/// Внимание, середина градиента (btop `#cbc06c`).
const YELLOW: u8 = 179;
/// Плохо, конец градиента (btop `#dc4c4c`).
const RED: u8 = 167;
/// Тёмный текст на цветной плашке.
const INK: u8 = 16;

/// Три опорные точки градиента в RGB; между ними линейная интерполяция.
pub(super) type Ramp = [(u8, u8, u8); 3];
/// Полоски трафика и задержки: зелёный → жёлтый → красный.
pub(super) const HEAT: Ramp = [(119, 202, 155), (203, 192, 108), (220, 76, 76)];
/// ↓ загрузка, как `net` в btop.
pub(super) const DOWNLOAD: Ramp = [(79, 67, 163), (176, 169, 222), (241, 240, 255)];
/// ↑ отдача, как `net` в btop.
pub(super) const UPLOAD: Ramp = [(146, 53, 53), (217, 117, 117), (242, 196, 196)];
/// Уровни столбиков графика от самого низкого к самому высокому.
pub(super) const SPARK: [char; 7] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇'];
/// Уровни куба xterm для каждого канала RGB.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Панель с собственным цветом рамки: как `cpu_box`, `mem_box`, `net_box`, `proc_box` в btop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    /// Туннель на обзоре.
    Tunnel,
    /// Подписки на обзоре и во вкладке.
    Subs,
    /// События на обзоре и журнал.
    Events,
    /// Узлы: список и подробности.
    Nodes,
    /// Настройки.
    Settings,
}

impl Panel {
    /// Цвет рамки; он же цвет плашки вкладки, на которой панель главная.
    pub(super) fn color(self) -> u8 {
        match self {
            Self::Tunnel => 65,
            Self::Subs => 100,
            Self::Events => 61,
            Self::Nodes => 95,
            Self::Settings => 67,
        }
    }
}

/// Какой цвет у ячейки шкалы: полоска идёт по градиенту, график — по своей высоте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Paint {
    /// Обычный текст, один цвет тона.
    Text,
    /// Полоска `█░`: заполненные ячейки по градиенту, пустые тусклые.
    Gauge,
    /// Столбики скачивания: цвет по высоте столбика.
    Download,
    /// Столбики отдачи: цвет по высоте столбика.
    Upload,
}

impl Paint {
    /// Опорные цвета для столбиков; у полоски и текста их нет.
    fn ramp(self) -> Option<Ramp> {
        match self {
            Self::Text | Self::Gauge => None,
            Self::Download => Some(DOWNLOAD),
            Self::Upload => Some(UPLOAD),
        }
    }
}

/// Линейная интерполяция по трём опорным точкам, результат — ближайший цвет куба xterm.
/// `t` от 0 до 100: 0 — первая точка, 50 — средняя, 100 — последняя.
pub(super) fn gradient(ramp: Ramp, t: u8) -> u8 {
    let t = t.min(100);
    let [from, middle, to] = ramp;
    let rgb = if t <= 50 {
        mix(from, middle, t)
    } else {
        mix(middle, to, t - 50)
    };
    cube_index(rgb)
}

/// Смешивание двух цветов: `k` от 0 до 50, 0 — `from`, 50 — `to`.
fn mix(from: (u8, u8, u8), to: (u8, u8, u8), k: u8) -> (u8, u8, u8) {
    let lerp = |a: u8, b: u8| -> u8 {
        let a = i32::from(a);
        let b = i32::from(b);
        u8::try_from(a + (b - a) * i32::from(k) / 50).unwrap_or(u8::MAX)
    };
    (lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
}

/// Индекс куба xterm (16..=231) для цвета RGB: ближайший уровень каждого канала.
fn cube_index((r, g, b): (u8, u8, u8)) -> u8 {
    16 + 36 * cube_step(r) + 6 * cube_step(g) + cube_step(b)
}

/// Номер ближайшего уровня куба; при равном расстоянии берётся старший.
fn cube_step(value: u8) -> u8 {
    let mut best = (0_u8, u8::MAX);
    for (step, level) in (0_u8..).zip(CUBE) {
        let distance = value.abs_diff(level);
        if distance <= best.1 {
            best = (step, distance);
        }
    }
    best.0
}

/// Процент для ячейки с номером `index` в полоске из десяти ячеек.
fn cell_percent(index: u16) -> u8 {
    u8::try_from(index * 100 / 9).unwrap_or(100)
}

/// Цвета по тонам. Без цвета (`NO_COLOR`) остаются только жирный, тусклый и обратный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Palette {
    color: bool,
}

impl Palette {
    pub(super) fn new(color: bool) -> Self {
        Self { color }
    }

    fn tint(self, style: Style, color: u8) -> Style {
        if self.color {
            style.fg(Color::Indexed(color))
        } else {
            style
        }
    }

    fn title(self, active: bool) -> Style {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        match (self.color, active) {
            (false, _) => bold,
            (true, true) => bold.fg(Color::Indexed(TITLE)),
            (true, false) => bold.fg(Color::Indexed(TITLE_DIM)),
        }
    }

    fn style(self, tone: Tone) -> Style {
        let base = Style::new();
        let bold = base.add_modifier(Modifier::BOLD);
        match tone {
            Tone::Plain => base,
            Tone::Bold => bold,
            Tone::Dim if self.color => base.fg(Color::Indexed(SECONDARY)),
            Tone::Dim => base.add_modifier(Modifier::DIM),
            Tone::Header => self.title(true),
            Tone::Green => self.tint(base, GREEN),
            Tone::Yellow => self.tint(base, YELLOW),
            Tone::Red => self.tint(bold, RED),
        }
    }

    /// Рамка панели своим цветом. Активная жирная; без цвета активная жирная, неактивная тусклая.
    fn frame(self, panel: Panel, active: bool) -> Style {
        let style = if self.color {
            Style::new().fg(Color::Indexed(panel.color()))
        } else if active {
            Style::new()
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        if active {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }

    /// Плашка: текст `ink` на цветном фоне `bg`.
    fn badge(self, bg: u8, ink: u8) -> Style {
        if self.color {
            Style::new()
                .fg(Color::Indexed(ink))
                .bg(Color::Indexed(bg))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// Плашка клавиши: горячая буква на тёмном фоне.
    fn key(self) -> Style {
        if self.color {
            Style::new()
                .fg(Color::Indexed(HOTKEY))
                .bg(Color::Indexed(DIVIDER))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// Выделение: фон на всю строку, цвета текста сохраняются.
    fn selected(self, style: Style) -> Style {
        if self.color {
            style
                .bg(Color::Indexed(SELECTED))
                .add_modifier(Modifier::BOLD)
        } else {
            style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }
}

/// Цвет плашки состояния туннеля: зелёный, жёлтый или красный; остальное — серое.
fn plaque_color(tone: Tone) -> u8 {
    match tone {
        Tone::Green => GREEN,
        Tone::Yellow => YELLOW,
        Tone::Red => RED,
        _ => SECONDARY,
    }
}

/// Кусок строки одного тона.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seg {
    pub(super) text: String,
    pub(super) tone: Tone,
    /// 0 — нужен всегда; чем больше число, тем раньше кусок пропадает на узком экране.
    pub(super) priority: u8,
    /// Раскраска по ячейкам: полоска или столбики графика.
    pub(super) paint: Paint,
}

impl Seg {
    pub(super) fn new(text: impl Into<String>, tone: Tone, priority: u8) -> Self {
        Self {
            text: text.into(),
            tone,
            priority,
            paint: Paint::Text,
        }
    }

    /// Тот же кусок, но с раскраской по ячейкам.
    pub(super) fn painted(mut self, paint: Paint) -> Self {
        self.paint = paint;
        self
    }
}

/// Оставляет куски, которые помещаются в `width` с разделителем шириной `gap`:
/// обязательные (приоритет 0) всегда, остальные по возрастанию приоритета, пока есть
/// место. Порядок кусков сохраняется.
pub(super) fn fit(segs: Vec<Seg>, width: usize, gap: usize) -> Vec<Seg> {
    let mut order: Vec<(u8, usize)> = segs
        .iter()
        .enumerate()
        .map(|(index, seg)| (seg.priority, index))
        .collect();
    order.sort_unstable();
    let mut keep = vec![false; segs.len()];
    let mut used = 0;
    let mut count = 0;
    for (priority, index) in order {
        let Some(seg) = segs.get(index) else {
            continue;
        };
        let need = display_width(&seg.text) + if count > 0 { gap } else { 0 };
        if priority > 0 && used + need > width {
            break;
        }
        used += need;
        count += 1;
        if let Some(flag) = keep.get_mut(index) {
            *flag = true;
        }
    }
    segs.into_iter()
        .zip(keep)
        .filter_map(|(seg, kept)| kept.then_some(seg))
        .collect()
}

fn split_word(word: &str, width: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut used = 0;
    for c in word.chars() {
        let cell = char_width(c);
        if used + cell > width && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            used = 0;
        }
        piece.push(c);
        used += cell;
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// Переносит текст по словам на строки шириной не больше `width`; всё, что не
/// поместилось в `max_lines`, заменяется «…» в конце последней строки.
pub(super) fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in sanitize(text).split_whitespace() {
        for piece in split_word(word, width) {
            let piece_width = display_width(&piece);
            if used > 0 && used + 1 + piece_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            line.push_str(&piece);
            used += piece_width;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = truncate(&format!("{last}…"), width);
        }
    }
    lines
}

fn clean(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        Cow::Owned(sanitize(text))
    } else {
        Cow::Borrowed(text)
    }
}

/// Область экрана с координатами от её левого верхнего угла.
pub(super) struct Canvas<'a> {
    buf: &'a mut Buffer,
    area: Rect,
    pub(super) width: usize,
    pub(super) height: usize,
    palette: Palette,
    dim: bool,
    /// Заголовок рисуется неактивным (тусклее); включается только на время заголовка рамки.
    title_dim: bool,
}

impl<'a> Canvas<'a> {
    pub(super) fn new(buf: &'a mut Buffer, palette: Palette) -> Self {
        let area = buf.area;
        Self {
            buf,
            area,
            width: usize::from(area.width),
            height: usize::from(area.height),
            palette,
            dim: false,
            title_dim: false,
        }
    }

    /// Всё, что рисуется дальше, тускнеет: так выглядят устаревшие данные.
    pub(super) fn set_dim(&mut self, dim: bool) {
        self.dim = dim;
    }

    pub(super) fn style(&self, tone: Tone) -> Style {
        let style = if self.title_dim && matches!(tone, Tone::Header) {
            self.palette.title(false)
        } else {
            self.palette.style(tone)
        };
        if self.dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Рамка панели своим цветом: активная панель жирная, неактивная обычная.
    pub(super) fn frame(&self, panel: Panel, active: bool) -> Style {
        let style = self.palette.frame(panel, active);
        if self.dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Плашка активной вкладки: цвет её панели, текст яркий и жирный.
    pub(super) fn tab_badge(&self, panel: Panel) -> Style {
        self.palette.badge(panel.color(), TITLE)
    }

    /// Плашка названия программы.
    pub(super) fn brand_badge(&self) -> Style {
        self.palette.badge(SELECTED, TITLE)
    }

    /// Плашка клавиши в нижней строке и кнопки окон.
    pub(super) fn key_style(&self) -> Style {
        self.palette.key()
    }

    /// Плашка состояния туннеля: цвет по тону.
    pub(super) fn plaque(&self, tone: Tone) -> Style {
        self.palette.badge(plaque_color(tone), INK)
    }

    /// Фон выделенной строки без цвета текста.
    pub(super) fn selected(&self) -> Style {
        self.palette.selected(Style::new())
    }

    /// Текст выделенной строки: цвет тона остаётся, фон и жирность добавляются.
    pub(super) fn selected_style(&self, tone: Tone) -> Style {
        self.palette.selected(self.style(tone))
    }

    /// Прямоугольник внутри текущей области; размер обрезается по ней.
    pub(super) fn sub(&mut self, x: usize, y: usize, width: usize, height: usize) -> Canvas<'_> {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);
        let area = Rect::new(
            self.area
                .x
                .saturating_add(u16::try_from(x).unwrap_or_default()),
            self.area
                .y
                .saturating_add(u16::try_from(y).unwrap_or_default()),
            u16::try_from(width).unwrap_or_default(),
            u16::try_from(height).unwrap_or_default(),
        );
        Canvas {
            buf: &mut *self.buf,
            area,
            width,
            height,
            palette: self.palette,
            dim: self.dim,
            title_dim: false,
        }
    }

    /// Рамка прямоугольника `width × height` в точке `x, y` с заголовком в верхней линии.
    /// Жирная рамка (активная панель) даёт яркий заголовок, обычная — тусклый.
    /// Возвращает область внутри рамки с отступом в одну ячейку с каждой стороны и
    /// в две ячейки по бокам.
    pub(super) fn boxed(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        title: Vec<Seg>,
        border: Style,
    ) -> Canvas<'_> {
        if height >= 2 && width >= 8 {
            let edge = "─".repeat(width - 2);
            let right = x + width - 1;
            if title.is_empty() {
                self.put_style(x, y, &format!("╭{edge}╮"), width, border);
            } else {
                let active = border.add_modifier.contains(Modifier::BOLD);
                self.put_style(x, y, "╭─", 2, border);
                self.title_dim = !active;
                let end = self.chain(y, x + 3, " · ", title, x + width - 4);
                self.title_dim = false;
                self.put_style(end, y, " ", 1, border);
                let fill = (x + width - 2).saturating_sub(end);
                self.put_style(end + 1, y, &"─".repeat(fill), fill, border);
                self.put_style(right, y, "╮", 1, border);
            }
            for row in y + 1..y + height - 1 {
                self.put_style(x, row, "│", 1, border);
                self.put_style(right, row, "│", 1, border);
            }
            self.put_style(x, y + height - 1, &format!("╰{edge}╯"), width, border);
        }
        self.sub(
            x + 2,
            y + 1,
            width.saturating_sub(4),
            height.saturating_sub(2),
        )
    }

    /// Рамка на всю ширину строк `y..y + height` цветом `border`. Возвращает область внутри.
    pub(super) fn framed(
        &mut self,
        y: usize,
        height: usize,
        title: Vec<Seg>,
        border: Style,
    ) -> Canvas<'_> {
        let width = self.width;
        self.boxed(0, y, width, height, title, border)
    }

    /// Очищает прямоугольник: под окном не должно остаться следов экрана.
    pub(super) fn clear_rect(&mut self, x: usize, y: usize, width: usize, height: usize) {
        let blank = " ".repeat(width);
        for row in y..y + height {
            self.put_style(x, row, &blank, width, Style::reset());
        }
    }

    pub(super) fn put_style(&mut self, x: usize, y: usize, text: &str, max: usize, style: Style) {
        if y >= self.height || x >= self.width || max == 0 {
            return;
        }
        let room = max.min(self.width - x);
        let (Ok(dx), Ok(dy)) = (u16::try_from(x), u16::try_from(y)) else {
            return;
        };
        self.buf.set_stringn(
            self.area.x.saturating_add(dx),
            self.area.y.saturating_add(dy),
            clean(text),
            room,
            style,
        );
    }

    pub(super) fn put(&mut self, x: usize, y: usize, text: &str, max: usize, tone: Tone) {
        let style = self.style(tone);
        self.put_style(x, y, text, max, style);
    }

    /// Кусок, раскрашенный по ячейкам: полоска или столбики. Без раскраски — как `put`.
    pub(super) fn put_painted(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        max: usize,
        tone: Tone,
        paint: Paint,
    ) {
        if paint == Paint::Text {
            self.put(x, y, text, max, tone);
            return;
        }
        let base = self.style(tone);
        let mut at = x;
        let mut cell = 0_u16;
        for c in text.chars() {
            let width = char_width(c);
            if at + width > x + max {
                break;
            }
            let style = match self.paint_color(paint, c, &mut cell) {
                Some(color) => self.palette.tint(base, color),
                None => base,
            };
            let mut buf = [0_u8; 4];
            self.put_style(at, y, c.encode_utf8(&mut buf), width, style);
            at += width;
        }
    }

    /// Цвет одного символа куба xterm; `None` — символ рисуется цветом тона.
    /// Полоска считает свои ячейки в `cell`: заполненная — по градиенту, пустая — `DIVIDER`.
    fn paint_color(&self, paint: Paint, c: char, cell: &mut u16) -> Option<u8> {
        match paint {
            Paint::Text => None,
            Paint::Gauge => match c {
                '█' => {
                    let color = gradient(HEAT, cell_percent(*cell));
                    *cell += 1;
                    Some(color)
                }
                '░' => {
                    *cell += 1;
                    Some(DIVIDER)
                }
                _ => None,
            },
            Paint::Download | Paint::Upload => {
                let level = SPARK.iter().position(|bar| *bar == c)?;
                let percent = u8::try_from(level * 100 / (SPARK.len() - 1)).unwrap_or(100);
                paint.ramp().map(|ramp| gradient(ramp, percent))
            }
        }
    }

    /// Закрашивает всю строку `y` стилем, чтобы выделение шло от края до края.
    pub(super) fn fill(&mut self, y: usize, style: Style) {
        let blank = " ".repeat(self.width);
        self.put_style(0, y, &blank, self.width, style);
    }

    /// Строка из кусков с отступом; лишние куски убираются, последний обрезается.
    pub(super) fn line(&mut self, y: usize, indent: usize, segs: Vec<Seg>) {
        let gap = " ".repeat(GAP);
        self.chain(y, indent, &gap, segs, self.width);
    }

    /// Куски через `sep` (рисуется тусклым) от `indent` до `max` не включительно.
    /// Лишние куски убираются, последний обрезается. Возвращает x, где кончилась строка.
    pub(super) fn chain(
        &mut self,
        y: usize,
        indent: usize,
        sep: &str,
        segs: Vec<Seg>,
        max: usize,
    ) -> usize {
        let limit = max.min(self.width);
        let kept = fit(segs, limit.saturating_sub(indent), display_width(sep));
        let mut x = indent;
        for (index, seg) in kept.iter().enumerate() {
            if index > 0 {
                self.put(x, y, sep, limit.saturating_sub(x), Tone::Dim);
                x += display_width(sep);
            }
            let room = limit.saturating_sub(x);
            if room == 0 {
                break;
            }
            let text = truncate(&seg.text, room);
            self.put_painted(x, y, &text, room, seg.tone, seg.paint);
            x += display_width(&text);
        }
        x.min(limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, priority: u8) -> Seg {
        Seg::new(text, Tone::Plain, priority)
    }

    fn texts(segs: &[Seg]) -> Vec<&str> {
        segs.iter().map(|seg| seg.text.as_str()).collect()
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_owned())
            .collect()
    }

    #[test]
    fn fit_drops_low_priority_pieces_first() {
        let segs = vec![
            seg("главное", 0),
            seg("второе", 2),
            seg("первое", 1),
            seg("третье", 3),
        ];
        let all = fit(segs.clone(), 100, GAP);
        assert_eq!(texts(&all), ["главное", "второе", "первое", "третье"]);
        let some = fit(segs.clone(), 7 + 2 + 6, GAP);
        assert_eq!(texts(&some), ["главное", "первое"]);
        let must = fit(segs, 3, GAP);
        assert_eq!(texts(&must), ["главное"]);
    }

    #[test]
    fn fit_stops_at_the_first_piece_that_does_not_fit() {
        let segs = vec![seg("а", 0), seg("длинный кусок", 1), seg("б", 2)];
        assert_eq!(texts(&fit(segs, 6, GAP)), ["а"]);
    }

    #[test]
    fn a_frame_puts_its_title_into_the_top_line() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 22, 3));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
            let title = vec![
                Seg::new("Узлы", Tone::Bold, 0),
                Seg::new("всего 3", Tone::Dim, 2),
            ];
            let border = canvas.frame(Panel::Nodes, true);
            let mut inner = canvas.framed(0, 3, title, border);
            inner.line(0, 0, vec![seg("ab", 0)]);
        }
        assert_eq!(row(&buffer, 0), "╭─ Узлы · всего 3 ───╮");
        assert_eq!(row(&buffer, 1), format!("│ ab{}│", " ".repeat(17)));
        assert_eq!(row(&buffer, 2), format!("╰{}╯", "─".repeat(20)));
    }

    #[test]
    fn a_frame_without_a_title_is_a_plain_box() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 3));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
            let border = canvas.frame(Panel::Nodes, false);
            let _inner = canvas.framed(0, 3, Vec::new(), border);
        }
        assert_eq!(row(&buffer, 0), "╭──────╮");
        assert_eq!(row(&buffer, 1), "│      │");
        assert_eq!(row(&buffer, 2), "╰──────╯");
    }

    #[test]
    fn a_box_away_from_the_left_edge_draws_inside_its_own_corners() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 3));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
            let border = canvas.frame(Panel::Tunnel, false);
            let _inner = canvas.boxed(4, 0, 8, 3, Vec::new(), border);
        }
        assert_eq!(row(&buffer, 0), "    ╭──────╮");
        assert_eq!(row(&buffer, 2), "    ╰──────╯");
    }

    #[test]
    fn chain_joins_pieces_with_a_separator_and_reports_the_end() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
        let end = canvas.chain(0, 0, " · ", vec![seg("a", 0), seg("bc", 0)], 12);
        assert_eq!(end, 6);
        assert_eq!(row(&buffer, 0), "a · bc      ");
    }

    #[test]
    fn each_panel_has_its_own_frame_colour_and_bold_when_active() {
        let colour = Palette::new(true);
        let expected = [
            (Panel::Tunnel, 65),
            (Panel::Subs, 100),
            (Panel::Events, 61),
            (Panel::Nodes, 95),
            (Panel::Settings, 67),
        ];
        for (panel, index) in expected {
            assert_eq!(colour.frame(panel, false).fg, Some(Color::Indexed(index)));
            assert!(
                !colour
                    .frame(panel, false)
                    .add_modifier
                    .contains(Modifier::BOLD)
            );
            assert!(
                colour
                    .frame(panel, true)
                    .add_modifier
                    .contains(Modifier::BOLD)
            );
        }
    }

    #[test]
    fn an_inactive_frame_without_color_is_dim_and_an_active_one_bold() {
        let plain = Palette::new(false);
        assert_eq!(plain.frame(Panel::Nodes, false).fg, None);
        assert!(
            plain
                .frame(Panel::Nodes, false)
                .add_modifier
                .contains(Modifier::DIM)
        );
        assert!(
            plain
                .frame(Panel::Nodes, true)
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn active_titles_are_bright_and_inactive_ones_dimmer() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 3));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
            let active = canvas.frame(Panel::Nodes, true);
            let title_a = vec![Seg::new("А", Tone::Header, 0)];
            let _inner = canvas.boxed(0, 0, 10, 3, title_a, active);
            let idle = canvas.frame(Panel::Nodes, false);
            let title_b = vec![Seg::new("Б", Tone::Header, 0)];
            let _other = canvas.boxed(10, 0, 10, 3, title_b, idle);
        }
        let title = |x: u16| {
            let style = buffer[(x, 0)].style();
            (buffer[(x, 0)].symbol().to_owned(), style.fg)
        };
        assert_eq!(title(3), ("А".to_owned(), Some(Color::Indexed(255))));
        assert_eq!(title(13), ("Б".to_owned(), Some(Color::Indexed(250))));
    }

    #[test]
    fn a_selected_row_is_painted_edge_to_edge() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.fill(0, canvas.selected());
        canvas.put_style(2, 0, "ab", 2, canvas.selected_style(Tone::Green));
        for x in 0..10 {
            assert_eq!(buffer[(x, 0)].bg, Color::Indexed(52), "ячейка {x}");
        }
        assert_eq!(buffer[(2, 0)].fg, Color::Indexed(115));
    }

    #[test]
    fn the_gradient_runs_green_yellow_red() {
        assert_eq!(gradient(HEAT, 0), 115);
        assert_eq!(gradient(HEAT, 50), 179);
        assert_eq!(gradient(HEAT, 100), 167);
    }

    #[test]
    fn the_cube_index_covers_the_corners() {
        assert_eq!(cube_index((0, 0, 0)), 16);
        assert_eq!(cube_index((255, 255, 255)), 231);
    }

    #[test]
    fn a_gauge_is_painted_cell_by_cell() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.put_painted(0, 0, "██████████ 45%", 14, Tone::Plain, Paint::Gauge);
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(115));
        assert_eq!(buffer[(9, 0)].fg, Color::Indexed(167));
        assert_ne!(buffer[(4, 0)].fg, buffer[(0, 0)].fg);
        assert_eq!(buffer[(13, 0)].symbol(), "%");
        assert_eq!(buffer[(13, 0)].fg, Color::Reset);
    }

    #[test]
    fn an_empty_gauge_cell_is_the_divider_colour() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.put_painted(0, 0, "█░░░", 4, Tone::Plain, Paint::Gauge);
        assert_eq!(buffer[(1, 0)].fg, Color::Indexed(237));
        assert_eq!(buffer[(3, 0)].fg, Color::Indexed(237));
    }

    #[test]
    fn sparkline_columns_take_the_colour_of_their_height() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.put_painted(0, 0, "▁▇", 2, Tone::Plain, Paint::Download);
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(gradient(DOWNLOAD, 0)));
        assert_eq!(buffer[(1, 0)].fg, Color::Indexed(gradient(DOWNLOAD, 100)));
        assert_ne!(buffer[(0, 0)].fg, buffer[(1, 0)].fg);
    }

    #[test]
    fn without_color_no_cell_gets_an_indexed_colour() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 2));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
            canvas.put_painted(0, 0, "███░░░ 9%", 24, Tone::Green, Paint::Gauge);
            canvas.put_painted(0, 1, "▁▄▇", 3, Tone::Plain, Paint::Upload);
            canvas.put_style(12, 0, "k", 1, canvas.key_style());
            canvas.put_style(14, 0, "t", 1, canvas.tab_badge(Panel::Subs));
            canvas.put_style(16, 0, "b", 1, canvas.brand_badge());
            canvas.fill(1, canvas.selected());
        }
        for y in 0..2 {
            for x in 0..24 {
                let cell = &buffer[(x, y)];
                assert!(!matches!(cell.fg, Color::Indexed(_)), "{x} {y}");
                assert!(!matches!(cell.bg, Color::Indexed(_)), "{x} {y}");
            }
        }
    }

    #[test]
    fn a_selected_row_without_color_is_reversed() {
        let plain = Palette::new(false);
        let selected = plain.selected(Style::new());
        assert!(selected.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(selected.bg, None);
    }

    #[test]
    fn wrap_breaks_on_words_and_cuts_long_words() {
        assert_eq!(
            wrap("раз два три четыре", 9, 5),
            ["раз два", "три", "четыре"]
        );
        assert_eq!(wrap("абвгдежзик", 4, 5), ["абвг", "дежз", "ик"]);
        assert_eq!(wrap("текст", 0, 5), Vec::<String>::new());
        assert_eq!(wrap("текст", 5, 0), Vec::<String>::new());
    }

    #[test]
    fn wrap_marks_the_cut_with_an_ellipsis() {
        let lines = wrap("раз два три четыре пять шесть", 8, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'), "{lines:?}");
        assert!(lines.iter().all(|line| display_width(line) <= 8));
    }

    #[test]
    fn wrap_neutralizes_control_characters() {
        let lines = wrap("a\x1b[2Jb", 20, 3);
        assert!(lines.iter().all(|line| !line.contains('\x1b')));
    }

    #[test]
    fn drawing_is_clipped_to_the_buffer() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
        canvas.put(4, 0, "абвгд", 10, Tone::Plain);
        canvas.put(0, 5, "ниже", 4, Tone::Plain);
        canvas.put(9, 0, "правее", 4, Tone::Plain);
        canvas.put(0, 1, "ноль", 0, Tone::Plain);
        assert_eq!(row(&buffer, 0), "    аб");
        assert_eq!(row(&buffer, 1), "      ");
    }

    #[test]
    fn control_characters_never_reach_the_buffer() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
        canvas.put(0, 0, "a\x1b[2Jb\x07", 12, Tone::Plain);
        let text = row(&buffer, 0);
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
        assert!(text.starts_with("a [2Jb"), "{text:?}");
    }

    #[test]
    fn a_line_cuts_the_last_piece_with_an_ellipsis() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(false));
        canvas.line(0, 1, vec![seg("узел", 0), seg("очень длинный текст", 0)]);
        assert_eq!(row(&buffer, 0), " узел  очен…");
    }

    #[test]
    fn without_color_only_weight_remains() {
        let plain = Palette::new(false);
        assert_eq!(plain.style(Tone::Green), Style::new());
        assert_eq!(plain.style(Tone::Yellow), Style::new());
        assert_eq!(plain.style(Tone::Red).fg, None);
        assert!(plain.style(Tone::Red).add_modifier.contains(Modifier::BOLD));
        let colored = Palette::new(true);
        assert_eq!(colored.style(Tone::Green).fg, Some(Color::Indexed(115)));
        assert_eq!(colored.style(Tone::Red).fg, Some(Color::Indexed(167)));
    }

    #[test]
    fn stale_data_is_dimmed() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.set_dim(true);
        assert!(
            canvas
                .style(Tone::Green)
                .add_modifier
                .contains(Modifier::DIM)
        );
        canvas.set_dim(false);
        assert!(
            !canvas
                .style(Tone::Green)
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
}
