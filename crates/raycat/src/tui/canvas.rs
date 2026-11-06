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

// Палитра из 256 цветов: truecolor через ssh и `docker exec` не определить. Один акцент,
// три смысловых цвета одной светлоты и серые ступени. Цветных плашек с фоном нет: фон
// только у выделенной строки.

/// Акцент: название программы, активная вкладка, рамка панели в фокусе, кнопки.
const ACCENT: u8 = 111;
/// Фон выделенной строки.
const SELECTED: u8 = 236;
/// Рамки, разделители, пустая часть полоски.
const BORDER: u8 = 239;
/// Второстепенный текст и подписи.
const SECONDARY: u8 = 245;
const GREEN: u8 = 114;
const YELLOW: u8 = 222;
const RED: u8 = 210;
/// Клавиши в подсказках.
const BRIGHT: u8 = 255;

/// Заполненная и пустая части полоски.
pub(super) const GAUGE_FULL: char = '━';
pub(super) const GAUGE_EMPTY: char = '─';

/// Сколько из `cells` ячеек полоски заполнено при `percent` процентах.
pub(super) fn gauge_cells(percent: u128, cells: usize) -> usize {
    let total = u128::try_from(cells).unwrap_or(0);
    usize::try_from(percent.min(100) * total / 100).map_or(cells, |filled| filled.min(cells))
}

/// Цвета по тонам. Без цвета (`NO_COLOR`) остаются только жирный, тусклый, подчёркнутый
/// и обратный.
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

    fn style(self, tone: Tone) -> Style {
        let base = Style::new();
        let bold = base.add_modifier(Modifier::BOLD);
        match tone {
            Tone::Plain => base,
            Tone::Bold => bold,
            Tone::Dim if self.color => base.fg(Color::Indexed(SECONDARY)),
            Tone::Dim => base.add_modifier(Modifier::DIM),
            Tone::Header => self.tint(bold, ACCENT),
            Tone::Green => self.tint(base, GREEN),
            Tone::Yellow => self.tint(base, YELLOW),
            Tone::Red => self.tint(bold, RED),
        }
    }

    fn border(self) -> Style {
        if self.color {
            Style::new().fg(Color::Indexed(BORDER))
        } else {
            Style::new().add_modifier(Modifier::DIM)
        }
    }

    fn active_border(self) -> Style {
        if self.color {
            Style::new().fg(Color::Indexed(ACCENT))
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }

    /// Состояние туннеля в шапке: цвет тона и жирный, без фона.
    fn state(self, tone: Tone) -> Style {
        let color = match tone {
            Tone::Green => GREEN,
            Tone::Yellow => YELLOW,
            Tone::Red => RED,
            _ => SECONDARY,
        };
        self.tint(Style::new().add_modifier(Modifier::BOLD), color)
    }

    /// Активная вкладка: акцент, жирный и подчёркивание.
    fn active_tab(self) -> Style {
        self.tint(
            Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ACCENT,
        )
    }

    /// График скорости: приём акцентом, отдача серым, обычным начертанием.
    fn graph(self, main: bool) -> Style {
        match (self.color, main) {
            (true, true) => Style::new().fg(Color::Indexed(ACCENT)),
            (true, false) => Style::new().fg(Color::Indexed(SECONDARY)),
            (false, true) => Style::new(),
            (false, false) => Style::new().add_modifier(Modifier::DIM),
        }
    }

    /// Клавиша в подсказке: ярко-белая и жирная. Подпись рядом — серая.
    fn key(self) -> Style {
        self.tint(Style::new().add_modifier(Modifier::BOLD), BRIGHT)
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

/// Кусок строки одного тона.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seg {
    pub(super) text: String,
    pub(super) tone: Tone,
    /// 0 — нужен всегда; чем больше число, тем раньше кусок пропадает на узком экране.
    pub(super) priority: u8,
}

impl Seg {
    pub(super) fn new(text: impl Into<String>, tone: Tone, priority: u8) -> Self {
        Self {
            text: text.into(),
            tone,
            priority,
        }
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
        }
    }

    /// Всё, что рисуется дальше, тускнеет: так выглядят устаревшие данные.
    pub(super) fn set_dim(&mut self, dim: bool) {
        self.dim = dim;
    }

    pub(super) fn style(&self, tone: Tone) -> Style {
        let style = self.palette.style(tone);
        if self.dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Рамка обычной панели.
    pub(super) fn chrome(&self) -> Style {
        let style = self.palette.border();
        if self.dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Рамка активной панели и окна.
    pub(super) fn active_frame(&self) -> Style {
        self.palette.active_border()
    }

    /// Название программы в шапке и кнопки окон: акцент, жирный.
    pub(super) fn accent(&self) -> Style {
        self.palette.style(Tone::Header)
    }

    /// Активная вкладка.
    pub(super) fn active_tab(&self) -> Style {
        self.palette.active_tab()
    }

    /// График скорости: `main` — приём (акцентом), иначе отдача (серым).
    pub(super) fn graph(&self, main: bool) -> Style {
        let style = self.palette.graph(main);
        if self.dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    /// Клавиша в подсказках нижней строки и в строке фильтров.
    pub(super) fn key_style(&self) -> Style {
        self.palette.key()
    }

    /// Состояние туннеля в шапке: цвет по тону.
    pub(super) fn state(&self, tone: Tone) -> Style {
        self.palette.state(tone)
    }

    /// Полоска на `cells` ячеек: заполненная часть цветом тона, пустая — цветом рамки.
    /// Без цвета заполненная часть жирная, пустая тусклая: их различает и толщина линии.
    pub(super) fn gauge(&mut self, x: usize, y: usize, percent: u128, cells: usize, tone: Tone) {
        let filled = gauge_cells(percent, cells);
        let full: String = std::iter::repeat_n(GAUGE_FULL, filled).collect();
        let empty: String = std::iter::repeat_n(GAUGE_EMPTY, cells - filled).collect();
        let style = self.style(tone).add_modifier(Modifier::BOLD);
        self.put_style(x, y, &full, filled, style);
        let rest = self.chrome();
        self.put_style(x + filled, y, &empty, cells - filled, rest);
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
        }
    }

    /// Рамка прямоугольника `width × height` в точке `x, y` с заголовком в верхней линии.
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
                self.put_style(x, y, "╭─", 2, border);
                let end = self.chain(y, x + 3, " · ", title, x + width - 4);
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

    /// Рамка на всю ширину строк `y..y + height` обычным цветом. Возвращает область внутри.
    pub(super) fn framed(&mut self, y: usize, height: usize, title: Vec<Seg>) -> Canvas<'_> {
        let width = self.width;
        let border = self.chrome();
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
            self.put(x, y, &text, room, seg.tone);
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
            let mut inner = canvas.framed(0, 3, title);
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
            let _inner = canvas.framed(0, 3, Vec::new());
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
            let border = canvas.chrome();
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
    fn borders_are_grey_with_color_and_dim_without() {
        assert_eq!(Palette::new(true).border().fg, Some(Color::Indexed(239)));
        assert_eq!(Palette::new(false).border().fg, None);
        assert!(
            Palette::new(false)
                .border()
                .add_modifier
                .contains(Modifier::DIM)
        );
    }

    #[test]
    fn a_selected_row_is_painted_edge_to_edge() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
        canvas.fill(0, canvas.selected());
        canvas.put_style(2, 0, "ab", 2, canvas.selected_style(Tone::Green));
        for x in 0..10 {
            assert_eq!(buffer[(x, 0)].bg, Color::Indexed(236), "ячейка {x}");
        }
        assert_eq!(buffer[(2, 0)].fg, Color::Indexed(114));
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
        assert_eq!(colored.style(Tone::Green).fg, Some(Color::Indexed(114)));
        assert_eq!(colored.style(Tone::Red).fg, Some(Color::Indexed(210)));
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

    #[test]
    fn a_gauge_fills_its_share_in_the_tone_and_leaves_the_rest_in_the_border_color() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        {
            let mut canvas = Canvas::new(&mut buffer, Palette::new(true));
            canvas.gauge(0, 0, 34, 10, Tone::Green);
        }
        assert_eq!(row(&buffer, 0), "━━━───────");
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(114));
        assert_eq!(buffer[(3, 0)].fg, Color::Indexed(239));
        assert_eq!(gauge_cells(0, 10), 0);
        assert_eq!(gauge_cells(100, 10), 10);
        assert_eq!(gauge_cells(250, 10), 10);
    }

    #[test]
    fn the_speed_graph_is_never_bold() {
        for color in [true, false] {
            for main in [true, false] {
                let style = Palette::new(color).graph(main);
                assert!(
                    !style.add_modifier.contains(Modifier::BOLD),
                    "{color} {main}"
                );
            }
        }
        let colored = Palette::new(true);
        assert_eq!(colored.graph(true).fg, Some(Color::Indexed(111)));
        assert_eq!(colored.graph(false).fg, Some(Color::Indexed(245)));
    }

    #[test]
    fn nothing_but_the_selected_row_has_a_background() {
        let colored = Palette::new(true);
        for tone in [Tone::Green, Tone::Yellow, Tone::Red, Tone::Dim] {
            assert_eq!(colored.state(tone).bg, None);
        }
        assert_eq!(colored.active_tab().bg, None);
        assert!(
            colored
                .active_tab()
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(colored.key().bg, None);
        assert_eq!(colored.state(Tone::Red).fg, Some(Color::Indexed(210)));
        let plain = Palette::new(false);
        assert_eq!(plain.state(Tone::Green).fg, None);
        assert_eq!(plain.active_tab().fg, None);
        assert_eq!(plain.key().fg, None);
    }
}
