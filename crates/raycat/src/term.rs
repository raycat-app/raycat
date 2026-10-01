//! Терминал: цвета, ширина и таблицы. Цвета включены только на терминале и без
//! `NO_COLOR`; на не-терминале (канал, файл) ничего не обрезается.

use std::io::{self, IsTerminal as _};

use raycat_config::Env;

const GAP: &str = "  ";
const FALLBACK_WIDTH: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Plain,
    Header,
    Bold,
    Dim,
    Green,
    Yellow,
    Red,
}

impl Tone {
    fn code(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Header => Some("1;92"),
            Self::Bold => Some("1"),
            Self::Dim => Some("2"),
            Self::Green => Some("92"),
            Self::Yellow => Some("93"),
            Self::Red => Some("91"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Term {
    color: bool,
    width: Option<usize>,
}

impl Term {
    pub(crate) fn new(color: bool, width: Option<usize>) -> Self {
        Self { color, width }
    }

    pub(crate) fn detect(env: &Env) -> Self {
        let tty = io::stdout().is_terminal();
        Self::new(color_allowed(tty, env), tty.then(|| terminal_width(env)))
    }

    pub(crate) fn paint(self, tone: Tone, text: &str) -> String {
        match tone.code() {
            Some(code) if self.color && !text.is_empty() => format!("\x1b[{code}m{text}\x1b[0m"),
            _ => text.to_owned(),
        }
    }
}

fn color_allowed(tty: bool, env: &Env) -> bool {
    let no_color = env.get("NO_COLOR").is_some_and(|value| !value.is_empty());
    let dumb = env.get("TERM").is_some_and(|term| term == "dumb");
    tty && !no_color && !dumb
}

#[allow(unsafe_code)]
fn ioctl_width() -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ пишет только в переданную структуру winsize.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (result == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

fn terminal_width(env: &Env) -> usize {
    ioctl_width()
        .or_else(|| env.get("COLUMNS").and_then(|value| value.parse().ok()))
        .filter(|width| *width > 0)
        .unwrap_or(FALLBACK_WIDTH)
}

/// Ширина символа в ячейках терминала: без таблиц Unicode, по основным диапазонам.
/// Пара региональных индикаторов (флаг) занимает две ячейки, как и в терминалах.
fn char_width(c: char) -> usize {
    match u32::from(c) {
        0x0300..=0x036F
        | 0x200B..=0x200F
        | 0x2060
        | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F
        | 0x1F3FB..=0x1F3FF => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF
        | 0x1F900..=0x1F9FF
        | 0x1FA70..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

pub(crate) fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Обрезает текст до `max` ячеек, последним знаком ставя «…».
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let width = char_width(c);
        if used + width > max - 1 {
            break;
        }
        out.push(c);
        used += width;
    }
    out.push('…');
    out
}

/// Дополняет текст пробелами справа до `width` ячеек.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(display_width(text));
    format!("{text}{}", " ".repeat(fill))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Right,
}

pub(crate) struct Column {
    pub(crate) title: &'static str,
    pub(crate) align: Align,
    /// Чем больше, тем раньше колонку сужают на узком терминале; 0 — не сужать.
    pub(crate) shrink: u8,
    /// Ниже этой ширины колонку не сужают.
    pub(crate) min: usize,
}

pub(crate) struct Cell {
    pub(crate) text: String,
    pub(crate) tone: Tone,
}

impl Cell {
    pub(crate) fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

fn shrink(widths: &mut [usize], columns: &[Column], limit: usize) {
    let gaps = GAP.len() * columns.len().saturating_sub(1);
    let mut excess = (widths.iter().sum::<usize>() + gaps).saturating_sub(limit);
    let mut order: Vec<usize> = (0..columns.len())
        .filter(|index| columns.get(*index).is_some_and(|column| column.shrink > 0))
        .collect();
    order.sort_by_key(|index| std::cmp::Reverse(columns.get(*index).map(|column| column.shrink)));
    for index in order {
        let (Some(column), Some(width)) = (columns.get(index), widths.get_mut(index)) else {
            continue;
        };
        let floor = column.min.max(display_width(column.title));
        let cut = width.saturating_sub(floor).min(excess);
        *width -= cut;
        excess -= cut;
    }
}

fn line(term: Term, columns: &[Column], widths: &[usize], row: &[Cell]) -> String {
    let mut parts = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let width = widths.get(index).copied().unwrap_or_default();
        let (text, tone) = row
            .get(index)
            .map_or(("", Tone::Plain), |cell| (cell.text.as_str(), cell.tone));
        let text = truncate(text, width);
        let fill = " ".repeat(width.saturating_sub(display_width(&text)));
        let painted = term.paint(tone, &text);
        parts.push(match column.align {
            Align::Left => format!("{painted}{fill}"),
            Align::Right => format!("{fill}{painted}"),
        });
    }
    parts.join(GAP).trim_end().to_owned()
}

/// Таблица с заголовком. На узком терминале сужаются колонки с `shrink > 0`.
pub(crate) fn table(term: Term, columns: &[Column], rows: &[Vec<Cell>]) -> String {
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|cell| display_width(&cell.text))
                .fold(display_width(column.title), usize::max)
        })
        .collect();
    if let Some(limit) = term.width {
        shrink(&mut widths, columns, limit);
    }
    let header: Vec<Cell> = columns
        .iter()
        .map(|column| Cell::new(column.title, Tone::Dim))
        .collect();
    let mut lines = vec![line(term, columns, &widths, &header)];
    lines.extend(rows.iter().map(|row| line(term, columns, &widths, row)));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn columns() -> Vec<Column> {
        vec![
            Column {
                title: "Подписка",
                align: Align::Left,
                shrink: 1,
                min: 4,
            },
            Column {
                title: "Узел",
                align: Align::Left,
                shrink: 2,
                min: 6,
            },
            Column {
                title: "Мс",
                align: Align::Right,
                shrink: 0,
                min: 0,
            },
        ]
    }

    fn row(a: &str, b: &str, c: &str) -> Vec<Cell> {
        vec![
            Cell::new(a, Tone::Plain),
            Cell::new(b, Tone::Plain),
            Cell::new(c, Tone::Plain),
        ]
    }

    #[test]
    fn color_needs_a_terminal_and_no_opt_out() {
        assert!(color_allowed(true, &env(&[])));
        assert!(!color_allowed(false, &env(&[])));
        assert!(!color_allowed(true, &env(&[("NO_COLOR", "1")])));
        assert!(!color_allowed(true, &env(&[("TERM", "dumb")])));
        assert!(color_allowed(true, &env(&[("NO_COLOR", "")])));
    }

    #[test]
    fn paint_wraps_only_when_color_is_on() {
        let on = Term::new(true, None);
        let off = Term::new(false, None);
        assert_eq!(on.paint(Tone::Red, "x"), "\x1b[91mx\x1b[0m");
        assert_eq!(off.paint(Tone::Red, "x"), "x");
        assert_eq!(on.paint(Tone::Plain, "x"), "x");
        assert_eq!(on.paint(Tone::Red, ""), "");
    }

    #[test]
    fn widths_follow_the_terminal_cells() {
        assert_eq!(display_width("Привет"), 6);
        assert_eq!(display_width("NL-1"), 4);
        assert_eq!(display_width("🚀"), 2);
        assert_eq!(display_width("🇩🇪"), 2);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("日本"), 4);
    }

    #[test]
    fn truncate_keeps_short_text_and_marks_cuts() {
        assert_eq!(truncate("Германия", 8), "Германия");
        assert_eq!(truncate("Германия 1", 8), "Германи…");
        assert_eq!(truncate("Германия", 1), "…");
        assert_eq!(truncate("Германия", 0), "");
    }

    #[test]
    fn truncate_never_splits_a_wide_symbol() {
        assert_eq!(truncate("ab🚀cd", 4), "ab…");
        assert_eq!(truncate("ab🚀cd", 5), "ab🚀…");
        assert!(display_width(&truncate("🚀🚀🚀🚀", 5)) <= 5);
    }

    #[test]
    fn pad_counts_cells() {
        assert_eq!(pad("абв", 5), "абв  ");
        assert_eq!(pad("абвгде", 3), "абвгде");
    }

    #[test]
    fn table_aligns_columns_and_trims_line_ends() {
        let rows = vec![row("main", "NL-1", "31"), row("резерв", "Германия", "5")];
        let text = table(Term::new(false, None), &columns(), &rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Подписка  Узел      Мс");
        assert_eq!(lines[1], "main      NL-1      31");
        assert_eq!(lines[2], "резерв    Германия   5");
        assert!(lines.iter().all(|line| !line.ends_with(' ')));
    }

    #[test]
    fn a_narrow_terminal_shrinks_the_widest_priority_first() {
        let rows = vec![row("main", "Очень длинное имя узла", "31")];
        let text = table(Term::new(false, Some(30)), &columns(), &rows);
        for line in text.lines() {
            assert!(display_width(line) <= 30, "{line:?}");
        }
        let body = text.lines().nth(1).unwrap();
        assert!(body.starts_with("main  "), "{body:?}");
        assert!(body.contains('…'), "{body:?}");
        assert!(body.ends_with("31"));
    }

    #[test]
    fn a_very_narrow_terminal_stops_at_the_minimum_widths() {
        let rows = vec![row("подписка-длинная", "Очень длинное имя узла", "31")];
        let text = table(Term::new(false, Some(5)), &columns(), &rows);
        let body = text.lines().nth(1).unwrap();
        assert!(body.contains('…'));
        assert!(body.ends_with("31"));
    }

    #[test]
    fn without_a_width_nothing_is_cut() {
        let rows = vec![row("main", "Очень длинное имя узла", "31")];
        let text = table(Term::new(false, None), &columns(), &rows);
        assert!(text.contains("Очень длинное имя узла"));
    }

    #[test]
    fn color_goes_around_the_text_not_the_padding() {
        let rows = vec![vec![
            Cell::new("a", Tone::Green),
            Cell::new("b", Tone::Plain),
            Cell::new("7", Tone::Red),
        ]];
        let text = table(Term::new(true, None), &columns(), &rows);
        let body = text.lines().nth(1).unwrap();
        assert!(body.starts_with("\x1b[92ma\x1b[0m       "), "{body:?}");
        assert!(body.ends_with("\x1b[91m7\x1b[0m"), "{body:?}");
    }
}
