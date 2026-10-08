//! Состояние TUI: данные демона, курсор, фильтры, журнал. Здесь нет ни терминала,
//! ни сети: клавиши и сообщения меняют состояние, а действия над демоном
//! возвращаются как [`Effect`] и выполняются снаружи.

use std::collections::VecDeque;
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use raycat_proto::{Event, Node, NodeStatus, Nodes, Pinned, Status, Updates};

use crate::render;
use crate::term::{Term, Tone, truncate};
use crate::util::sanitize;

pub(super) const LOG_CAP: usize = 200;
const TEXT_WIDTH: usize = 400;
const NOTICE_SECS: u64 = 8;
const RESULT_SECS: u64 = 20;
const FILTER_MAX: usize = 64;
const DEFAULT_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Link {
    Connecting,
    Up,
    Down {
        reason: String,
        /// Что сделать, если демон сообщил вторую строку.
        hint: Option<String>,
        retry_at: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputMode {
    Normal,
    Filter,
    Help,
}

/// Что нужно сделать с демоном; выполняет вызывающий, ответ приходит как [`Msg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    Pin(String),
    Unpin,
    Update(Option<String>),
}

pub(super) struct Snapshot {
    pub(super) status: Status,
    pub(super) nodes: Nodes,
}

pub(super) enum Msg {
    /// Поток событий открыт.
    Connected,
    Snapshot(Box<Snapshot>),
    Event(Event),
    Down {
        reason: String,
        retry_in: Duration,
    },
    Pinned(Result<Pinned, String>),
    Updated(Result<Updates, String>),
}

pub(super) struct LogEntry {
    pub(super) at: u64,
    pub(super) tone: Tone,
    pub(super) text: String,
}

pub(super) struct Notice {
    pub(super) text: String,
    pub(super) tone: Tone,
    until: u64,
}

pub(super) struct UpdateRun {
    /// Имя подписки; `None` — обновляются все.
    pub(super) target: Option<String>,
    pub(super) since: u64,
}

#[derive(Default)]
pub(super) struct Filter {
    pub(super) text: String,
    pub(super) subscription: Option<String>,
}

impl Filter {
    pub(super) fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || self.subscription.is_some()
    }
}

pub(super) struct App {
    pub(super) now: u64,
    pub(super) link: Link,
    pub(super) status: Option<Status>,
    /// Когда получен `status`: от этого считается время работы демона.
    pub(super) status_at: u64,
    pub(super) nodes: Vec<Node>,
    /// Индексы в `nodes` тех узлов, что прошли фильтр.
    pub(super) visible: Vec<usize>,
    pub(super) filter: Filter,
    pub(super) input: InputMode,
    /// Позиция курсора среди видимых узлов.
    pub(super) cursor: usize,
    /// Первая показанная строка таблицы.
    pub(super) offset: usize,
    pub(super) log: VecDeque<LogEntry>,
    pub(super) notice: Option<Notice>,
    pub(super) update: Option<UpdateRun>,
    pub(super) quit: bool,
    cursor_id: Option<String>,
    rows: usize,
}

fn clip(text: &str) -> String {
    truncate(&sanitize(text), TEXT_WIDTH)
}

/// Ошибка демона: первая строка — что случилось, вторая — что сделать. Делить надо до
/// очистки: `sanitize` превращает перевод строки в пробел.
fn split_lines(text: &str) -> (&str, Option<&str>) {
    match text.split_once('\n') {
        Some((first, rest)) => (first, Some(rest)),
        None => (text, None),
    }
}

impl App {
    pub(super) fn new(now: u64) -> Self {
        Self {
            now,
            link: Link::Connecting,
            status: None,
            status_at: now,
            nodes: Vec::new(),
            visible: Vec::new(),
            filter: Filter::default(),
            input: InputMode::Normal,
            cursor: 0,
            offset: 0,
            log: VecDeque::new(),
            notice: None,
            update: None,
            quit: false,
            cursor_id: None,
            rows: DEFAULT_ROWS,
        }
    }

    pub(super) fn set_now(&mut self, now: u64) {
        self.now = now;
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.until <= now)
        {
            self.notice = None;
        }
    }

    /// Сколько строк таблицы помещается на экране; задаёт отрисовка.
    pub(super) fn set_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
        self.scroll_to_cursor();
    }

    pub(super) fn current(&self) -> Option<&Node> {
        self.visible
            .get(self.cursor)
            .and_then(|index| self.nodes.get(*index))
    }

    pub(super) fn subscription_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let from_status = self
            .status
            .iter()
            .flat_map(|status| status.subscriptions.iter().map(|sub| &sub.name));
        let from_nodes = self.nodes.iter().map(|node| &node.subscription);
        for name in from_status.chain(from_nodes) {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        match self.input {
            InputMode::Help => {
                self.input = InputMode::Normal;
                None
            }
            InputMode::Filter => {
                self.filter_key(key);
                None
            }
            InputMode::Normal => self.normal_key(key),
        }
    }

    fn normal_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(1),
            KeyCode::PageUp => self.move_up(self.page()),
            KeyCode::PageDown => self.move_down(self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.move_to(0),
            KeyCode::End | KeyCode::Char('G') => self.move_to(usize::MAX),
            KeyCode::Char('/') => self.input = InputMode::Filter,
            KeyCode::Char('?') | KeyCode::F(1) => self.input = InputMode::Help,
            KeyCode::Tab => self.cycle_subscription(true),
            KeyCode::BackTab => self.cycle_subscription(false),
            KeyCode::Esc => self.clear_filter(),
            KeyCode::Enter => return self.pin_current(),
            KeyCode::Char('a') => return self.unpin(),
            KeyCode::Char('u') => return self.start_update(None),
            KeyCode::Char('U') => return self.update_subscription(),
            _ => {}
        }
        None
    }

    fn filter_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                self.filter.text.clear();
                self.input = InputMode::Normal;
                self.refilter();
            }
            KeyCode::Enter => self.input = InputMode::Normal,
            KeyCode::Backspace => {
                self.filter.text.pop();
                self.refilter();
            }
            KeyCode::Up => self.move_up(1),
            KeyCode::Down => self.move_down(1),
            KeyCode::PageUp => self.move_up(self.page()),
            KeyCode::PageDown => self.move_down(self.page()),
            KeyCode::Char('u') if ctrl => {
                self.filter.text.clear();
                self.refilter();
            }
            KeyCode::Char(c)
                if !ctrl
                    && !alt
                    && !c.is_control()
                    && self.filter.text.chars().count() < FILTER_MAX =>
            {
                self.filter.text.push(c);
                self.refilter();
            }
            _ => {}
        }
    }

    pub(super) fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Connected => self.link = Link::Up,
            Msg::Snapshot(snapshot) => self.set_snapshot(*snapshot),
            Msg::Event(event) => {
                let (tone, text) = render::event_text(&event);
                self.push_log(tone, &text);
            }
            Msg::Down { reason, retry_in } => self.on_down(&reason, retry_in),
            Msg::Pinned(result) => self.on_pinned(result),
            Msg::Updated(result) => self.on_updated(result),
        }
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.link = Link::Up;
        self.status = Some(snapshot.status);
        self.status_at = self.now;
        self.nodes = snapshot.nodes.nodes;
        self.refilter();
    }

    fn on_down(&mut self, reason: &str, retry_in: Duration) {
        let (cause, hint) = split_lines(reason);
        let cause = clip(cause);
        if !matches!(self.link, Link::Down { .. }) {
            self.push_log(Tone::Red, &format!("демон недоступен: {cause}"));
        }
        self.link = Link::Down {
            reason: cause,
            hint: hint.map(clip).filter(|hint| !hint.trim().is_empty()),
            retry_at: self.now + retry_in.as_secs(),
        };
    }

    fn on_pinned(&mut self, result: Result<Pinned, String>) {
        match result {
            Ok(Pinned { node: Some(id) }) => {
                let dead = self
                    .nodes
                    .iter()
                    .any(|node| node.id == id && node.status == NodeStatus::Dead);
                if dead {
                    let text = format!("Узел «{id}» закреплён, но сейчас не отвечает");
                    self.notify(Tone::Yellow, &text, NOTICE_SECS);
                } else {
                    let text = format!("Узел «{id}» закреплён");
                    self.notify(Tone::Green, &text, NOTICE_SECS);
                }
            }
            Ok(Pinned { node: None }) => self.notify(
                Tone::Green,
                "Закрепление снято: узел выбирается автоматически",
                NOTICE_SECS,
            ),
            Err(error) => {
                let (cause, _) = split_lines(&error);
                self.fail(&format!("Не удалось изменить закрепление: {cause}"));
            }
        }
    }

    fn on_updated(&mut self, result: Result<Updates, String>) {
        self.update = None;
        match result {
            Ok(updates) if updates.results.is_empty() => {
                self.notify(Tone::Yellow, "Подписок для обновления нет", NOTICE_SECS);
            }
            Ok(updates) => {
                let tone = if render::failed(&updates) > 0 {
                    Tone::Red
                } else {
                    Tone::Green
                };
                let lines = render::updates(Term::new(false, None), &updates);
                let text = format!("Обновление завершено: {}", lines.replace('\n', "; "));
                self.notify(tone, &text, RESULT_SECS);
            }
            Err(error) => {
                let (cause, _) = split_lines(&error);
                self.fail(&format!("Не удалось обновить подписки: {cause}"));
            }
        }
    }

    fn fail(&mut self, text: &str) {
        self.push_log(Tone::Red, text);
        self.notify(Tone::Red, text, RESULT_SECS);
    }

    fn notify(&mut self, tone: Tone, text: &str, secs: u64) {
        self.notice = Some(Notice {
            text: clip(text),
            tone,
            until: self.now + secs,
        });
    }

    fn push_log(&mut self, tone: Tone, text: &str) {
        if self.log.len() >= LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            at: self.now,
            tone,
            text: clip(text),
        });
    }

    fn pin_current(&mut self) -> Option<Effect> {
        if let Some(node) = self.current() {
            return Some(Effect::Pin(node.id.clone()));
        }
        self.notify(
            Tone::Yellow,
            "Нет узла, который можно закрепить",
            NOTICE_SECS,
        );
        None
    }

    fn unpin(&mut self) -> Option<Effect> {
        // Без статуса не знаем, закреплён ли узел: спрашиваем демон.
        let automatic = self
            .status
            .as_ref()
            .is_some_and(|status| !status.node.as_ref().is_some_and(|node| node.pinned));
        if !automatic {
            return Some(Effect::Unpin);
        }
        self.notify(
            Tone::Yellow,
            "Узел и так выбирается автоматически",
            NOTICE_SECS,
        );
        None
    }

    /// Обновляет подписку, выбранную Tab, а без неё — подписку узла под курсором.
    fn update_subscription(&mut self) -> Option<Effect> {
        if let Some(name) = self.filter.subscription.clone() {
            return self.start_update(Some(name));
        }
        if let Some(node) = self.current() {
            let subscription = node.subscription.clone();
            return self.start_update(Some(subscription));
        }
        self.notify(
            Tone::Yellow,
            "Нет подписки для обновления: выберите её Tab или узел",
            NOTICE_SECS,
        );
        None
    }

    fn start_update(&mut self, target: Option<String>) -> Option<Effect> {
        if self.update.is_some() {
            self.notify(Tone::Yellow, "Обновление уже идёт", NOTICE_SECS);
            return None;
        }
        self.notice = None;
        self.update = Some(UpdateRun {
            target: target.clone(),
            since: self.now,
        });
        Some(Effect::Update(target))
    }

    fn clear_filter(&mut self) {
        if self.filter.is_active() || !self.filter.text.is_empty() {
            self.filter = Filter::default();
            self.refilter();
        }
    }

    fn cycle_subscription(&mut self, forward: bool) {
        let names = self.subscription_names();
        if names.is_empty() {
            return;
        }
        let slots = names.len() + 1;
        let current = self
            .filter
            .subscription
            .as_ref()
            .and_then(|name| names.iter().position(|candidate| candidate == name))
            .map_or(0, |index| index + 1);
        let next = if forward {
            (current + 1) % slots
        } else {
            (current + slots - 1) % slots
        };
        self.filter.subscription = next
            .checked_sub(1)
            .and_then(|index| names.get(index))
            .cloned();
        self.refilter();
    }

    fn page(&self) -> usize {
        self.rows.saturating_sub(1).max(1)
    }

    fn move_to(&mut self, index: usize) {
        self.cursor = index.min(self.visible.len().saturating_sub(1));
        self.cursor_id = self.current().map(|node| node.id.clone());
        self.scroll_to_cursor();
    }

    fn move_down(&mut self, by: usize) {
        self.move_to(self.cursor.saturating_add(by));
    }

    fn move_up(&mut self, by: usize) {
        self.move_to(self.cursor.saturating_sub(by));
    }

    fn scroll_to_cursor(&mut self) {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + self.rows {
            self.offset = self.cursor + 1 - self.rows;
        }
        self.offset = self
            .offset
            .min(self.visible.len().saturating_sub(self.rows));
    }

    fn position_where(&self, wanted: impl Fn(&Node) -> bool) -> Option<usize> {
        self.visible
            .iter()
            .position(|index| self.nodes.get(*index).is_some_and(&wanted))
    }

    /// Пересчитывает видимые узлы; курсор остаётся на том же узле, а если его нет,
    /// на той же позиции. Пока курсор не двигали, он стоит на выбранном узле.
    fn refilter(&mut self) {
        let needle = self.filter.text.trim().to_lowercase();
        let subscription = self.filter.subscription.as_deref();
        self.visible = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                subscription.is_none_or(|name| node.subscription == name)
                    && (needle.is_empty() || node.id.to_lowercase().contains(&needle))
            })
            .map(|(index, _)| index)
            .collect();
        let found = match &self.cursor_id {
            Some(id) => self.position_where(|node| node.id == *id),
            None => self.position_where(|node| node.selected),
        };
        self.cursor = found.unwrap_or(self.cursor);
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
        self.cursor_id = self.current().map(|node| node.id.clone());
        self.scroll_to_cursor();
    }
}

#[cfg(test)]
mod tests {
    use raycat_proto::{CurrentNode, Mode, SubscriptionStatus, UpdateResult, XrayStatus};

    use super::*;

    const NOW: u64 = 1_790_596_800;

    fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn set_pinned(app: &mut App, pinned: bool) {
        if let Some(status) = app.status.as_mut() {
            status.node = Some(CurrentNode {
                id: "main/DE-2".to_owned(),
                subscription: "main".to_owned(),
                name: "DE-2".to_owned(),
                latency_ms: Some(30),
                pinned,
                reason: None,
            });
        }
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn node(subscription: &str, name: &str, status: NodeStatus) -> Node {
        Node {
            id: format!("{subscription}/{name}"),
            subscription: subscription.to_owned(),
            name: name.to_owned(),
            tag: "node-001".to_owned(),
            status,
            latency_ms: (status == NodeStatus::Alive).then_some(30),
            failures: 0,
            alive_for_secs: None,
            last_error: None,
            selected: false,
            pinned: false,
            uplink_bytes: None,
            downlink_bytes: None,
        }
    }

    fn status(subscriptions: &[&str]) -> Status {
        Status {
            version: "0.1.0".to_owned(),
            mode: Mode::Gateway,
            uptime_secs: 100,
            kill_switch: Some(true),
            xray: XrayStatus {
                running: true,
                pid: Some(1),
                restarts: 0,
            },
            node: None,
            subscriptions: subscriptions
                .iter()
                .map(|name| SubscriptionStatus {
                    name: (*name).to_owned(),
                    url: "https://sub.example.com/…1234".to_owned(),
                    title: None,
                    used_bytes: None,
                    total_bytes: None,
                    expire: None,
                    nodes: 0,
                    updated_at: None,
                    next_update: None,
                    last_error: None,
                    updating: false,
                })
                .collect(),
        }
    }

    fn snapshot(nodes: Vec<Node>) -> Msg {
        Msg::Snapshot(Box::new(Snapshot {
            status: status(&["main", "backup"]),
            nodes: Nodes {
                selected: None,
                nodes,
            },
        }))
    }

    fn sample() -> Vec<Node> {
        vec![
            node("main", "NL-1", NodeStatus::Alive),
            node("main", "DE-2", NodeStatus::Alive),
            node("main", "Германия 3", NodeStatus::Dead),
            node("backup", "NL-1", NodeStatus::Unknown),
            node("backup", "FI-5", NodeStatus::Alive),
        ]
    }

    fn app_with(nodes: Vec<Node>) -> App {
        let mut app = App::new(NOW);
        app.apply(snapshot(nodes));
        app
    }

    fn visible_ids(app: &App) -> Vec<&str> {
        app.visible
            .iter()
            .filter_map(|index| app.nodes.get(*index))
            .map(|node| node.id.as_str())
            .collect()
    }

    #[test]
    fn the_cursor_starts_on_the_selected_node() {
        let mut nodes = sample();
        nodes[2].selected = true;
        let app = app_with(nodes);
        assert_eq!(app.current().unwrap().id, "main/Германия 3");
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn arrows_and_vim_keys_move_the_cursor_within_bounds() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.cursor, 1);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor, 4);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor, 4);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.cursor, 4);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.cursor, 0);
    }

    fn many(count: usize) -> Vec<Node> {
        (0..count)
            .map(|index| node("main", &format!("N{index:02}"), NodeStatus::Alive))
            .collect()
    }

    #[test]
    fn paging_scrolls_the_window_and_keeps_the_cursor_visible() {
        let mut app = app_with(many(30));
        app.set_rows(10);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.cursor, 9);
        assert_eq!(app.offset, 0);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.cursor, 18);
        assert_eq!(app.offset, 9);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor, 29);
        assert_eq!(app.offset, 20);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.cursor, 20);
        assert_eq!(app.offset, 20);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.offset, 19);
        press(&mut app, KeyCode::Home);
        assert_eq!((app.cursor, app.offset), (0, 0));
    }

    #[test]
    fn a_taller_screen_pulls_the_window_back() {
        let mut app = app_with(many(30));
        app.set_rows(5);
        press(&mut app, KeyCode::End);
        assert_eq!(app.offset, 25);
        app.set_rows(20);
        assert_eq!(app.offset, 10);
    }

    #[test]
    fn the_cursor_follows_its_node_when_the_list_changes() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.current().unwrap().id, "main/Германия 3");
        let mut shuffled = sample();
        shuffled.rotate_left(1);
        app.apply(snapshot(shuffled));
        assert_eq!(app.current().unwrap().id, "main/Германия 3");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn a_vanished_node_leaves_the_cursor_on_the_same_position() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::End);
        app.apply(snapshot(sample().into_iter().take(3).collect()));
        assert_eq!(app.cursor, 2);
        app.apply(snapshot(Vec::new()));
        assert_eq!(app.cursor, 0);
        assert!(app.current().is_none());
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.notice.is_some());
    }

    #[test]
    fn the_text_filter_matches_the_id_in_any_case() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input, InputMode::Filter);
        typed(&mut app, "NL");
        assert_eq!(visible_ids(&app), ["main/NL-1", "backup/NL-1"]);
        typed(&mut app, "-1");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.filter.text, "NL");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, InputMode::Normal);
        assert_eq!(visible_ids(&app).len(), 2);
        press(&mut app, KeyCode::Esc);
        assert_eq!(visible_ids(&app).len(), 5);
        assert!(!app.filter.is_active());
    }

    #[test]
    fn typing_a_subscription_name_filters_by_it() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "backup/");
        assert_eq!(visible_ids(&app), ["backup/NL-1", "backup/FI-5"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input, InputMode::Normal);
        assert_eq!(app.filter.text, "");
        assert_eq!(visible_ids(&app).len(), 5);
    }

    #[test]
    fn filter_input_takes_letters_that_are_commands_elsewhere() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "quaUa?");
        assert!(!app.quit);
        assert_eq!(app.filter.text, "quaUa?");
        assert_eq!(app.visible, Vec::<usize>::new());
        assert_eq!(press(&mut app, KeyCode::Enter), None);
    }

    #[test]
    fn ctrl_u_clears_the_filter_line_and_the_length_is_limited() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "abc");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.filter.text, "");
        typed(&mut app, &"x".repeat(FILTER_MAX + 20));
        assert_eq!(app.filter.text.chars().count(), FILTER_MAX);
    }

    #[test]
    fn tab_cycles_through_subscriptions_and_back_to_all() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.filter.subscription.as_deref(), Some("main"));
        assert_eq!(visible_ids(&app).len(), 3);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.filter.subscription.as_deref(), Some("backup"));
        assert_eq!(visible_ids(&app).len(), 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.filter.subscription, None);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.filter.subscription.as_deref(), Some("backup"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter.subscription, None);
        assert_eq!(visible_ids(&app).len(), 5);
    }

    #[test]
    fn subscription_filter_and_text_filter_combine() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "nl");
        assert_eq!(visible_ids(&app), ["main/NL-1"]);
    }

    #[test]
    fn enter_pins_the_highlighted_node_and_a_unpins() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Pin("main/DE-2".to_owned()))
        );
        set_pinned(&mut app, true);
        assert_eq!(press(&mut app, KeyCode::Char('a')), Some(Effect::Unpin));
    }

    #[test]
    fn a_without_a_pin_says_the_node_is_automatic_and_asks_nothing() {
        let mut app = app_with(sample());
        assert_eq!(press(&mut app, KeyCode::Char('a')), None);
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Узел и так выбирается автоматически"
        );
        set_pinned(&mut app, false);
        assert_eq!(press(&mut app, KeyCode::Char('a')), None);
        let mut fresh = App::new(NOW);
        assert_eq!(press(&mut fresh, KeyCode::Char('a')), Some(Effect::Unpin));
    }

    #[test]
    fn pin_works_on_the_filtered_list() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "fi-");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Pin("backup/FI-5".to_owned()))
        );
    }

    #[test]
    fn update_keys_ask_for_all_or_for_the_subscription_of_the_cursor() {
        let mut app = app_with(sample());
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            Some(Effect::Update(None))
        );
        assert!(app.update.is_some());
        app.apply(Msg::Updated(Ok(Updates {
            results: Vec::new(),
        })));
        assert!(app.update.is_none());
        press(&mut app, KeyCode::End);
        assert_eq!(
            press(&mut app, KeyCode::Char('U')),
            Some(Effect::Update(Some("backup".to_owned())))
        );
    }

    #[test]
    fn u_updates_the_subscription_chosen_by_tab_even_without_nodes() {
        let main_only: Vec<Node> = sample()
            .into_iter()
            .filter(|node| node.subscription == "main")
            .collect();
        let mut app = app_with(main_only);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.filter.subscription.as_deref(), Some("backup"));
        assert!(app.visible.is_empty());
        assert_eq!(
            press(&mut app, KeyCode::Char('U')),
            Some(Effect::Update(Some("backup".to_owned())))
        );
    }

    #[test]
    fn u_without_a_choice_and_without_nodes_says_how_to_choose() {
        let mut app = app_with(Vec::new());
        assert_eq!(press(&mut app, KeyCode::Char('U')), None);
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Нет подписки для обновления: выберите её Tab или узел"
        );
    }

    #[test]
    fn a_second_update_waits_for_the_first() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(press(&mut app, KeyCode::Char('u')), None);
        assert_eq!(press(&mut app, KeyCode::Char('U')), None);
        assert!(app.notice.as_ref().unwrap().text.contains("уже идёт"));
    }

    #[test]
    fn update_results_are_summarized_per_subscription() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('u'));
        app.apply(Msg::Updated(Ok(Updates {
            results: vec![
                UpdateResult {
                    subscription: "main".to_owned(),
                    ok: true,
                    message: "узлов: 12".to_owned(),
                    nodes: Some(12),
                },
                UpdateResult {
                    subscription: "backup".to_owned(),
                    ok: false,
                    message: "панель ответила 403".to_owned(),
                    nodes: None,
                },
            ],
        })));
        assert!(app.update.is_none());
        let notice = app.notice.as_ref().unwrap();
        assert_eq!(notice.tone, Tone::Red);
        assert_eq!(
            notice.text,
            "Обновление завершено: ✓ main: узлов: 12; ✗ backup: панель ответила 403"
        );
    }

    #[test]
    fn failed_actions_are_shown_and_logged_but_do_not_stop_anything() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('u'));
        app.apply(Msg::Updated(Err("демон не ответил вовремя".to_owned())));
        assert!(app.update.is_none());
        assert_eq!(app.notice.as_ref().unwrap().tone, Tone::Red);
        assert!(app.log.back().unwrap().text.contains("не ответил вовремя"));
        app.apply(Msg::Pinned(Err("узла «x» нет".to_owned())));
        assert!(app.notice.as_ref().unwrap().text.contains("узла «x» нет"));
        assert!(!app.quit);
    }

    #[test]
    fn pinning_a_dead_node_warns() {
        let mut app = app_with(sample());
        app.apply(Msg::Pinned(Ok(Pinned {
            node: Some("main/Германия 3".to_owned()),
        })));
        let notice = app.notice.as_ref().unwrap();
        assert_eq!(notice.tone, Tone::Yellow);
        assert!(notice.text.contains("не отвечает"));
        app.apply(Msg::Pinned(Ok(Pinned { node: None })));
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .text
                .contains("Закрепление снято")
        );
    }

    #[test]
    fn notices_expire() {
        let mut app = app_with(sample());
        app.apply(Msg::Pinned(Ok(Pinned { node: None })));
        app.set_now(NOW + NOTICE_SECS - 1);
        assert!(app.notice.is_some());
        app.set_now(NOW + NOTICE_SECS);
        assert!(app.notice.is_none());
    }

    #[test]
    fn quit_keys() {
        let mut app = App::new(NOW);
        press(&mut app, KeyCode::Char('x'));
        assert!(!app.quit);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);

        let mut app = App::new(NOW);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);

        let mut app = App::new(NOW);
        press(&mut app, KeyCode::Char('/'));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn help_opens_with_a_question_mark_and_closes_on_any_key() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input, InputMode::Help);
        assert_eq!(press(&mut app, KeyCode::Char('q')), None);
        assert_eq!(app.input, InputMode::Normal);
        assert!(!app.quit);
    }

    #[test]
    fn modified_and_released_keys_are_ignored() {
        let mut app = app_with(sample());
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(!app.quit && app.update.is_none());
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.handle_key(release);
        assert!(!app.quit);
    }

    #[test]
    fn the_log_is_a_ring_buffer() {
        let mut app = App::new(NOW);
        for index in 0..LOG_CAP + 25 {
            app.apply(Msg::Event(Event::Warning {
                level: "warn".to_owned(),
                message: format!("запись {index}"),
            }));
        }
        assert_eq!(app.log.len(), LOG_CAP);
        assert!(app.log.front().unwrap().text.ends_with("запись 25"));
        assert!(
            app.log
                .back()
                .unwrap()
                .text
                .ends_with(&format!("запись {}", LOG_CAP + 24))
        );
    }

    #[test]
    fn log_text_is_cleaned_and_bounded() {
        let mut app = App::new(NOW);
        app.apply(Msg::Event(Event::SubscriptionFailed {
            subscription: "main".to_owned(),
            error: format!("a\x1b[2Jb{}", "я".repeat(5_000)),
        }));
        let entry = app.log.back().unwrap();
        assert!(!entry.text.contains('\x1b'));
        assert_eq!(crate::term::display_width(&entry.text), TEXT_WIDTH);
        assert_eq!(entry.at, NOW);
    }

    #[test]
    fn events_become_log_lines_with_the_receive_time() {
        let mut app = App::new(NOW);
        app.apply(Msg::Event(Event::NodeChanged {
            from: Some("main/NL-1".to_owned()),
            to: None,
            reason: "сбой".to_owned(),
        }));
        let entry = app.log.back().unwrap();
        assert_eq!(entry.text, "узел: main/NL-1 → —: сбой");
        assert_eq!(entry.tone, Tone::Plain);
    }

    #[test]
    fn losing_the_daemon_keeps_the_data_and_is_logged_once() {
        let mut app = app_with(sample());
        assert_eq!(app.link, Link::Up);
        app.apply(Msg::Down {
            reason: "демон не запущен".to_owned(),
            retry_in: Duration::from_secs(2),
        });
        assert_eq!(
            app.link,
            Link::Down {
                reason: "демон не запущен".to_owned(),
                hint: None,
                retry_at: NOW + 2,
            }
        );
        assert_eq!(app.nodes.len(), 5);
        app.set_now(NOW + 3);
        app.apply(Msg::Down {
            reason: "демон не запущен".to_owned(),
            retry_in: Duration::from_secs(4),
        });
        assert_eq!(app.log.len(), 1);
        assert!(
            app.log[0]
                .text
                .starts_with("демон недоступен: демон не запущен")
        );
        app.apply(Msg::Connected);
        assert_eq!(app.link, Link::Up);
        app.apply(Msg::Down {
            reason: "обрыв".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        assert_eq!(app.log.len(), 2);
    }

    #[test]
    fn a_two_line_error_keeps_the_hint_out_of_the_journal() {
        let mut app = app_with(sample());
        app.apply(Msg::Down {
            reason: "raycat не запущен\nЗапустите службу: sudo systemctl start raycat".to_owned(),
            retry_in: Duration::from_secs(2),
        });
        assert_eq!(
            app.link,
            Link::Down {
                reason: "raycat не запущен".to_owned(),
                hint: Some("Запустите службу: sudo systemctl start raycat".to_owned()),
                retry_at: NOW + 2,
            }
        );
        assert_eq!(
            app.log.back().unwrap().text,
            "демон недоступен: raycat не запущен"
        );
    }

    #[test]
    fn a_failed_action_shows_only_the_first_line() {
        let mut app = app_with(sample());
        let error = "raycat не запущен\nЗапустите службу: sudo systemctl start raycat";
        app.apply(Msg::Updated(Err(error.to_owned())));
        let text = "Не удалось обновить подписки: raycat не запущен";
        assert_eq!(app.log.back().unwrap().text, text);
        assert_eq!(app.notice.as_ref().unwrap().text, text);
        app.apply(Msg::Pinned(Err(error.to_owned())));
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Не удалось изменить закрепление: raycat не запущен"
        );
    }

    #[test]
    fn a_snapshot_brings_the_link_up_and_remembers_the_time() {
        let mut app = App::new(NOW);
        assert_eq!(app.link, Link::Connecting);
        app.set_now(NOW + 5);
        app.apply(snapshot(sample()));
        assert_eq!(app.link, Link::Up);
        assert_eq!(app.status_at, NOW + 5);
    }

    #[test]
    fn subscription_names_come_from_the_status_first() {
        let mut app = app_with(sample());
        assert_eq!(app.subscription_names(), ["main", "backup"]);
        app.status = None;
        assert_eq!(app.subscription_names(), ["main", "backup"]);
    }
}
