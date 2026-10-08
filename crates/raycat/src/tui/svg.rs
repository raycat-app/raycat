//! Снимок экрана для README. Буфер `TestBackend` переводится в SVG: отрезок строки с
//! одинаковым стилем становится одним `<text>`, его ширина задаётся через `textLength`,
//! поэтому колонки не съезжают, какой бы шрифт ни подставил браузер. Модуль только для
//! тестов: `cargo test -p raycat tui_screenshot` сверяет снимок с `assets/tui.svg`.

use std::path::Path;
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use raycat_proto::{
    CurrentNode, Event, Mode, Node, NodeStatus, Nodes, Status, SubscriptionStatus, XrayStatus,
};

use super::app::{App, Msg, Snapshot};
use super::canvas::Palette;
use super::view::draw_at;
use crate::term::display_width;

const COLS: u16 = 100;
const ROWS: u16 = 30;
const NOW: u64 = 1_790_596_800;
const DAY: u64 = 86_400;
const HOUR: u64 = 3_600;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const SELECTED: &str = "main/🇳🇱 Нидерланды 2";
// За две секунды: 2.1 Мбит/с отдачи и 48.3 Мбит/с приёма.
const UP_STEP: u64 = 525_000;
const DOWN_STEP: u64 = 12_075_000;
const COMMITTED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tui.svg");
const WRITTEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tui.svg");

// Всё в десятых долях пикселя: так SVG записывается целыми числами.
const CELL_W: usize = 84;
const CELL_H: usize = 180;
const BASELINE: usize = 130;
const FONT_SIZE: usize = 140;
const MARGIN: usize = 160;
const TITLE_BAR: usize = 360;
const PAD_X: usize = 140;
const PAD_TOP: usize = 100;
const PAD_BOTTOM: usize = 120;
const RADIUS: usize = 100;
const CONTENT_X: usize = MARGIN + PAD_X;
const CONTENT_Y: usize = MARGIN + TITLE_BAR + PAD_TOP;

const FONT: &str =
    "ui-monospace, SFMono-Regular, Menlo, Consolas, &quot;DejaVu Sans Mono&quot;, monospace";
const BACKGROUND: &str = "#1e1e2e";
const FOREGROUND: &str = "#cdd6f4";
const TITLE_BACKGROUND: &str = "#181825";
const WINDOW_BORDER: &str = "#313244";
const TITLE_TEXT: &str = "#6c7086";
const DOTS: [&str; 3] = ["#f38ba8", "#f9e2af", "#a6e3a1"];
// Catppuccin Mocha: 16 цветов ANSI по порядку.
const ANSI: [&str; 16] = [
    "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
    "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
];

#[derive(PartialEq, Eq)]
struct Look {
    fg: String,
    bg: String,
    bold: bool,
    dim: bool,
}

/// Подряд идущие ячейки с одинаковым стилем; `cells` — ширина в колонках, `tail` —
/// сколько из них пробелы в конце, их не выводим.
struct Run {
    x: usize,
    cells: usize,
    tail: usize,
    look: Look,
    text: String,
}

impl Run {
    fn new(x: u16, look: Look, symbol: &str, step: u16) -> Self {
        let mut run = Self {
            x: usize::from(x),
            cells: 0,
            tail: 0,
            look,
            text: String::new(),
        };
        run.push(symbol, step);
        run
    }

    fn push(&mut self, symbol: &str, step: u16) {
        let step = usize::from(step);
        self.text.push_str(symbol);
        self.cells += step;
        self.tail = if symbol == " " { self.tail + step } else { 0 };
    }

    fn origin(&self, row: usize) -> (usize, usize) {
        (CONTENT_X + self.x * CELL_W, CONTENT_Y + row * CELL_H)
    }

    fn fill(&self, row: usize) -> Option<String> {
        if self.look.bg == BACKGROUND {
            return None;
        }
        let (x, y) = self.origin(row);
        let width = self.cells * CELL_W;
        let color = &self.look.bg;
        Some(format!(
            r#"<rect x="{x}" y="{y}" width="{width}" height="{CELL_H}" fill="{color}"/>"#
        ))
    }

    fn label(&self, row: usize) -> Option<String> {
        let text = self.text.trim_end_matches(' ');
        if text.is_empty() {
            return None;
        }
        let (x, y) = self.origin(row);
        let baseline = y + BASELINE;
        let width = (self.cells - self.tail) * CELL_W;
        let color = &self.look.fg;
        let bold = if self.look.bold {
            r#" font-weight="bold""#
        } else {
            ""
        };
        let dim = if self.look.dim {
            r#" fill-opacity="0.6""#
        } else {
            ""
        };
        let text = escape(text);
        Some(format!(
            r#"<text x="{x}" y="{baseline}" textLength="{width}" xml:space="preserve" fill="{color}"{bold}{dim}>{text}</text>"#
        ))
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn px(units: usize) -> String {
    if units.is_multiple_of(10) {
        (units / 10).to_string()
    } else {
        format!("{}.{}", units / 10, units % 10)
    }
}

fn palette_index(color: Color) -> Option<usize> {
    let index = match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(index) => usize::from(index),
        _ => return None,
    };
    Some(index)
}

fn hex(color: Color) -> Option<String> {
    if let Color::Rgb(r, g, b) = color {
        return Some(format!("#{r:02x}{g:02x}{b:02x}"));
    }
    let index = palette_index(color)?;
    ANSI.get(index).copied().map(str::to_owned)
}

fn paint(color: Option<Color>, fallback: &str) -> String {
    color.and_then(hex).unwrap_or_else(|| fallback.to_owned())
}

fn look_of(cell: &Cell) -> Look {
    let style = cell.style();
    let mut fg = paint(style.fg, FOREGROUND);
    let mut bg = paint(style.bg, BACKGROUND);
    if style.add_modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    Look {
        fg,
        bg,
        bold: style.add_modifier.contains(Modifier::BOLD),
        dim: style.add_modifier.contains(Modifier::DIM),
    }
}

fn render(buffer: &Buffer) -> String {
    let cols = usize::from(buffer.area.width);
    let rows = usize::from(buffer.area.height);
    let window_width = 2 * PAD_X + cols * CELL_W;
    let window_height = TITLE_BAR + PAD_TOP + rows * CELL_H + PAD_BOTTOM;
    let total_width = window_width + 2 * MARGIN;
    let total_height = window_height + 2 * MARGIN;
    let width_attr = px(total_width);
    let height_attr = px(total_height);

    let mut fills = Vec::new();
    let mut texts = Vec::new();
    for row in 0..buffer.area.height {
        let mut runs: Vec<Run> = Vec::new();
        let mut x = 0;
        while x < buffer.area.width {
            let cell = &buffer[(x, row)];
            let symbol = cell.symbol();
            let step = u16::try_from(display_width(symbol).max(1)).unwrap_or(1);
            let look = look_of(cell);
            match runs.last_mut() {
                Some(run) if run.look == look => run.push(symbol, step),
                _ => runs.push(Run::new(x, look, symbol, step)),
            }
            x += step;
        }
        for run in &runs {
            fills.extend(run.fill(usize::from(row)));
            texts.extend(run.label(usize::from(row)));
        }
    }

    let title_center = MARGIN + window_width / 2;
    let title_base = MARGIN + TITLE_BAR / 2 + 45;
    let dot_y = MARGIN + TITLE_BAR / 2;
    let mut svg = vec![
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width_attr}" height="{height_attr}" viewBox="0 0 {total_width} {total_height}" font-family="{FONT}">"#
        ),
        format!(
            r#"<defs><clipPath id="window"><rect x="{MARGIN}" y="{MARGIN}" width="{window_width}" height="{window_height}" rx="{RADIUS}"/></clipPath></defs>"#
        ),
        format!(
            r#"<rect x="{MARGIN}" y="{MARGIN}" width="{window_width}" height="{window_height}" rx="{RADIUS}" fill="{BACKGROUND}" stroke="{WINDOW_BORDER}" stroke-width="10"/>"#
        ),
        format!(
            r#"<rect x="{MARGIN}" y="{MARGIN}" width="{window_width}" height="{TITLE_BAR}" fill="{TITLE_BACKGROUND}" clip-path="url(#window)"/>"#
        ),
    ];
    for (index, color) in DOTS.iter().enumerate() {
        let cx = MARGIN + 200 + index * 200;
        svg.push(format!(
            r#"<circle cx="{cx}" cy="{dot_y}" r="60" fill="{color}"/>"#
        ));
    }
    svg.push(format!(
        r#"<text x="{title_center}" y="{title_base}" text-anchor="middle" font-size="130" fill="{TITLE_TEXT}">raycat</text>"#
    ));
    svg.push(format!(r#"<g font-size="{FONT_SIZE}">"#));
    svg.extend(fills);
    svg.extend(texts);
    svg.push("</g>".to_owned());
    svg.push("</svg>".to_owned());
    let mut out = svg.join("\n");
    out.push('\n');
    out
}

fn node(subscription: &str, name: &str, status: NodeStatus, latency_ms: Option<u64>) -> Node {
    Node {
        id: format!("{subscription}/{name}"),
        subscription: subscription.to_owned(),
        name: name.to_owned(),
        tag: name.to_owned(),
        status,
        latency_ms,
        failures: 0,
        alive_for_secs: None,
        last_error: None,
        selected: false,
        pinned: false,
        uplink_bytes: Some(6 * MIB),
        downlink_bytes: Some(96 * MIB),
    }
}

/// `moved` — второй снимок: счётчики выбранного узла выросли за две секунды.
fn demo_nodes(moved: bool) -> Vec<Node> {
    let mut nodes = vec![
        node("main", "🇫🇮 Финляндия", NodeStatus::Alive, Some(44)),
        node("main", "🇩🇪 Германия 1", NodeStatus::Alive, Some(61)),
        node("main", "🇳🇱 Нидерланды 2", NodeStatus::Alive, Some(31)),
        node("main", "🇸🇪 Швеция", NodeStatus::Alive, Some(412)),
        node("main", "🇵🇱 Польша", NodeStatus::Unknown, None),
        node("backup", "🇩🇪 Германия 2", NodeStatus::Alive, Some(218)),
        node("backup", "🇳🇱 Нидерланды 1", NodeStatus::Alive, Some(47)),
        node("backup", "🇺🇸 США", NodeStatus::Dead, None),
        node("backup", "🇪🇪 Эстония", NodeStatus::Alive, Some(96)),
    ];
    let grow = u64::from(moved);
    let current = &mut nodes[2];
    current.selected = true;
    current.pinned = true;
    current.uplink_bytes = Some(9 * MIB + grow * UP_STEP);
    current.downlink_bytes = Some(96 * MIB + grow * DOWN_STEP);
    let dead = &mut nodes[7];
    dead.failures = 3;
    dead.last_error = Some("тайм-аут".to_owned());
    nodes
}

fn demo_status() -> Status {
    Status {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mode: Mode::Gateway,
        uptime_secs: 3 * HOUR + 12 * 60,
        kill_switch: Some(true),
        xray: XrayStatus {
            running: true,
            pid: Some(4127),
            restarts: 0,
        },
        node: Some(CurrentNode {
            id: SELECTED.to_owned(),
            subscription: "main".to_owned(),
            name: "🇳🇱 Нидерланды 2".to_owned(),
            latency_ms: Some(31),
            pinned: true,
            reason: None,
        }),
        subscriptions: vec![
            SubscriptionStatus {
                name: "main".to_owned(),
                url: "https://example.com/…a1b2".to_owned(),
                title: None,
                used_bytes: Some(34 * GIB),
                total_bytes: Some(100 * GIB),
                expire: Some(NOW + 23 * DAY),
                nodes: 5,
                updated_at: Some(NOW - 18 * 60),
                next_update: Some(NOW + 42 * 60),
                last_error: None,
                updating: false,
            },
            SubscriptionStatus {
                name: "backup".to_owned(),
                url: "https://example.net/…c3d4".to_owned(),
                title: None,
                used_bytes: Some(81 * GIB),
                total_bytes: Some(100 * GIB),
                expire: Some(NOW + 2 * DAY),
                nodes: 4,
                updated_at: Some(NOW - 3 * HOUR - 12 * 60),
                next_update: Some(NOW + 2 * HOUR),
                last_error: None,
                updating: false,
            },
        ],
    }
}

fn snapshot(nodes: Vec<Node>, at: Instant) -> Msg {
    Msg::Snapshot(Box::new(Snapshot {
        status: demo_status(),
        nodes: Nodes {
            selected: Some(SELECTED.to_owned()),
            nodes,
        },
        at,
    }))
}

fn demo_app() -> App {
    let start = Instant::now();
    let mut app = App::new(NOW);
    app.apply(snapshot(demo_nodes(false), start));
    app.apply(snapshot(demo_nodes(true), start + Duration::from_secs(2)));
    let events = [
        (
            NOW - 3_000,
            Event::Hello {
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        ),
        (
            NOW - 2_400,
            Event::SubscriptionUpdated {
                subscription: "main".to_owned(),
                nodes: 5,
            },
        ),
        (
            NOW - 1_500,
            Event::Pin {
                node: Some(SELECTED.to_owned()),
            },
        ),
        (
            NOW - 1_499,
            Event::NodeChanged {
                from: Some("main/🇩🇪 Германия 1".to_owned()),
                to: Some(SELECTED.to_owned()),
                reason: "закреплён вручную: «🇳🇱 Нидерланды 2»".to_owned(),
            },
        ),
        (
            NOW - 240,
            Event::Warning {
                level: "warn".to_owned(),
                message: "узлов, которые xray не поддерживает, пропущено: 1".to_owned(),
            },
        ),
    ];
    for (at, event) in events {
        app.set_now(at);
        app.apply(Msg::Event(event));
    }
    app.set_now(NOW);
    app
}

#[test]
fn tui_screenshot_matches_assets() {
    let mut app = demo_app();
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("тестовый терминал");
    terminal
        .draw(|frame| draw_at(frame, &mut app, Palette::new(true), &TimeZone::UTC))
        .expect("отрисовка");
    let actual = render(terminal.backend().buffer());
    let expected = std::fs::read_to_string(COMMITTED).unwrap_or_default();
    if actual == expected {
        return;
    }
    let written = Path::new(WRITTEN);
    if let Some(dir) = written.parent() {
        std::fs::create_dir_all(dir).expect("каталог target");
    }
    std::fs::write(written, actual).expect("запись снимка");
    panic!(
        "assets/tui.svg устарел: новый снимок записан в target/tui.svg, его нужно скопировать в assets/tui.svg"
    );
}
