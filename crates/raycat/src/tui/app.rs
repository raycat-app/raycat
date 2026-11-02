//! Состояние TUI: вкладки, данные демона, курсор, фильтры, журнал и области нажатий
//! мыши. Здесь нет ни терминала, ни сети: клавиши и сообщения меняют состояние, а
//! действия над демоном возвращаются как [`Effect`] и выполняются снаружи.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use raycat_proto::{
    Event, Node, NodeStatus, Nodes, Pinned, Reloaded, SettingChange, SettingItem, SettingValue,
    SettingsView, SpeedtestProgress, SpeedtestResult, Status, Updates, XHTTP_PROVIDER,
};

use raycat_config::names::without_code;
use raycat_config::{Pattern, escape_literal};

use crate::names::{NameStyle, display_name};
use crate::render;
use crate::term::{Term, Tone, truncate};
use crate::util::sanitize;

use super::prefs::{Prefs, split_masks};

pub(super) const LOG_CAP: usize = 200;
const TEXT_WIDTH: usize = 400;
const NOTICE_SECS: u64 = 8;
const RESULT_SECS: u64 = 20;
const FILTER_MAX: usize = 64;
const MASKS_MAX: usize = 512;
const DEFAULT_ROWS: usize = 10;
const WHEEL_ROWS: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const RESTART_TEXT: &str = "xray перезапустится: связь прервётся на несколько секунд";
/// Уровни журнала от подробного к важному: по кругу в порядке этого списка.
pub(super) const LEVELS: [&str; 4] = ["error", "warn", "info", "debug"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Overview,
    Nodes,
    Subscriptions,
    Journal,
    Settings,
}

impl Tab {
    pub(super) const ALL: [Self; 5] = [
        Self::Overview,
        Self::Nodes,
        Self::Subscriptions,
        Self::Journal,
        Self::Settings,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Overview => "Обзор",
            Self::Nodes => "Узлы",
            Self::Subscriptions => "Подписки",
            Self::Journal => "Журнал",
            Self::Settings => "Настройки",
        }
    }
}

/// Что делает нажатие мыши: переключает, выбирает или запускает действие.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Tab(Tab),
    NodeRow(usize),
    SubRow(usize),
    Pin,
    Autoselect,
    UpdateAll,
    UpdateSub,
    DeadToggle,
    SubFilter,
    Search,
    WarningsToggle,
    ExcludedToggle,
    Sort(SortBy),
    HiddenEdit,
    /// Тест скорости через текущий узел.
    Speed,
    /// Тест скорости через узел, открытый в подробностях.
    SpeedNode,
    /// Строка «Настроек»: выбор клавишами и кликом по названию или пояснению.
    SettingRow(usize),
    /// Значение строки «Настроек»: переключает, меняет по кругу или открывает поле.
    SettingCell(usize),
    /// Один из уровней журнала на строке «Уровень журнала».
    SettingLevel(&'static str),
    ConfirmYes,
    ConfirmNo,
    /// Правка масок выбранной подписки.
    SubMasks,
    /// Подписка на одно место выше (приоритет выше).
    SubUp,
    /// Подписка на одно место ниже.
    SubDown,
}

/// Порядок узлов в таблице; `Priority` — порядок из подписок, как его отдаёт демон.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SortBy {
    #[default]
    Priority,
    Latency,
    Name,
    Subscription,
    Status,
}

impl SortBy {
    fn next(self) -> Self {
        match self {
            Self::Priority => Self::Latency,
            Self::Latency => Self::Name,
            Self::Name => Self::Subscription,
            Self::Subscription => Self::Status,
            Self::Status => Self::Priority,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Priority => "как в подписках",
            Self::Latency => "задержка",
            Self::Name => "имя",
            Self::Subscription => "подписка",
            Self::Status => "статус",
        }
    }
}

/// Область нажатия шириной в одну строку экрана, координаты от левого верхнего угла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Hit {
    x: u16,
    y: u16,
    width: u16,
    pub(super) target: Target,
}

impl Hit {
    pub(super) fn new(x: usize, y: usize, width: usize, target: Target) -> Self {
        Self {
            x: to_u16(x),
            y: to_u16(y),
            width: to_u16(width),
            target,
        }
    }

    fn contains(self, column: u16, row: u16) -> bool {
        row == self.y && (self.x..self.x.saturating_add(self.width)).contains(&column)
    }
}

fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

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
    /// Правка масок скрытия.
    Masks,
    /// Правка значения строки «Настроек».
    Field,
    /// Правка списка масок выбранной подписки.
    SubMasks,
    Help,
}

/// Список масок подписки: белый список, чёрный список или предпочтительные узлы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListKind {
    Allow,
    Deny,
    Priority,
}

impl ListKind {
    /// Порядок переключения клавишей Tab.
    const ALL: [Self; 3] = [Self::Allow, Self::Deny, Self::Priority];

    /// Ключ подписки в API и в файле.
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Priority => "priority",
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Allow => "allow (только эти узлы)",
            Self::Deny => "deny (не брать эти узлы)",
            Self::Priority => "priority (предпочтительные)",
        }
    }

    fn step(self, forward: bool) -> Self {
        let index = Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0);
        let count = Self::ALL.len();
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        Self::ALL[next]
    }
}

/// Правка масок подписки: список через запятую, Tab переключает список.
pub(super) struct SubEdit {
    pub(super) subscription: String,
    pub(super) kind: ListKind,
    pub(super) text: String,
}

/// Изменение настроек демона: проверяется, при необходимости спрашивает подтверждение и
/// только потом сохраняется. Исключение узла — тоже такое изменение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Edit {
    pub(super) changes: Vec<SettingChange>,
    /// Что записать в журнал после сохранения, например «Россия напрямую: вкл».
    pub(super) done: String,
    /// Строка «Настроек», к которой относится ошибка; `None` — исключение узла.
    pub(super) row: Option<usize>,
    /// Что спросить до сохранения, даже если xray не перезапустится.
    pub(super) warn: Option<String>,
}

/// Вопрос перед сохранением: Enter применяет изменение, Esc отменяет.
pub(super) struct Confirm {
    pub(super) question: String,
    pub(super) edit: Edit,
}

/// Что нужно сделать снаружи состояния; ответ приходит как [`Msg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    Pin(String),
    Unpin,
    Update(Option<String>),
    SavePrefs(Prefs),
    /// Тест скорости через узел; `None` — через текущий.
    SpeedTest(Option<String>),
    CancelSpeedTest,
    /// Настройки демона: их читают для исключений и для вкладки «Настройки».
    LoadSettings,
    /// Проверка изменения без записи: демон говорит, понадобится ли перезапуск xray.
    Check(Edit),
    /// Сохранение изменения в файл и применение.
    Apply(Edit),
}

/// Строка вкладки «Настройки», в порядке показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Setting {
    RuDirect,
    KillSwitch,
    CheckInterval,
    Failures,
    SwitchGain,
    ReturnDelay,
    Xhttp,
    LogLevel,
    /// Значки в именах: настройка вида TUI, а не демона.
    Names,
}

impl Setting {
    pub(super) const ALL: [Self; 9] = [
        Self::RuDirect,
        Self::KillSwitch,
        Self::CheckInterval,
        Self::Failures,
        Self::SwitchGain,
        Self::ReturnDelay,
        Self::Xhttp,
        Self::LogLevel,
        Self::Names,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::RuDirect => "Россия напрямую",
            Self::KillSwitch => "Kill switch",
            Self::CheckInterval => "Проверка узлов каждые",
            Self::Failures => "Сбоев до переключения",
            Self::SwitchGain => "Порог переключения",
            Self::ReturnDelay => "Возврат на приоритетный",
            Self::Xhttp => "Соединения XHTTP",
            Self::LogLevel => "Уровень журнала",
            Self::Names => "Значки в именах",
        }
    }

    /// Пояснение в строке: коротко, что делает настройка.
    pub(super) fn hint(self) -> &'static str {
        match self {
            Self::RuDirect => "российские домены и подсети идут напрямую, мимо VPN",
            Self::KillSwitch => "не пускать трафик мимо VPN, пока он недоступен",
            Self::CheckInterval => "как часто проверять узлы, от 5s до 10m",
            Self::Failures => "проверок подряд без ответа до недоступности",
            Self::SwitchGain => "на сколько узел должен быть быстрее, например 150ms",
            Self::ReturnDelay => "через сколько вернуться на приоритетный узел",
            Self::Xhttp => "параллельные соединения для узлов XHTTP, от 1 до 16",
            Self::LogLevel => "что писать в журнал демона: error, warn, info или debug",
            Self::Names => "код страны вместо флага, например DE",
        }
    }

    /// Номер строки на экране.
    pub(super) fn row(self) -> usize {
        Self::ALL
            .iter()
            .position(|setting| *setting == self)
            .unwrap_or_default()
    }

    /// Таблица и ключ в файле настроек; `None` — настройка вида TUI.
    fn place(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::RuDirect => Some(("routing", "ru_direct")),
            Self::KillSwitch => Some(("mode", "kill_switch")),
            Self::CheckInterval => Some(("selection", "check_interval")),
            Self::Failures => Some(("selection", "failures")),
            Self::SwitchGain => Some(("selection", "switch_gain")),
            Self::ReturnDelay => Some(("selection", "return_delay")),
            Self::Xhttp => Some(("xray", "xhttp_connections")),
            Self::LogLevel => Some(("log", "level")),
            Self::Names => None,
        }
    }

    /// Что спросить до сохранения, даже если xray не перезапустится.
    fn warning(self, value: &SettingValue) -> Option<&'static str> {
        if self == Self::KillSwitch && *value == SettingValue::Bool(false) {
            Some("Выключить kill switch: трафик пойдёт мимо VPN, если связь пропадёт")
        } else {
            None
        }
    }
}

/// Что показывает строка «Настроек»: значение, откуда оно взято и можно ли его менять.
pub(super) struct Shown {
    pub(super) value: String,
    pub(super) source: String,
    pub(super) enabled: bool,
}

/// Значение для показа. «Как у провайдера» — отдельная строка, а не число.
fn value_text(value: &SettingValue, setting: Setting) -> String {
    match value {
        SettingValue::Bool(true) => "вкл".to_owned(),
        SettingValue::Bool(false) => "выкл".to_owned(),
        SettingValue::Number(number) => number.to_string(),
        SettingValue::Text(text) => {
            if setting == Setting::Xhttp && text.as_str() == XHTTP_PROVIDER {
                "как у провайдера".to_owned()
            } else {
                text.clone()
            }
        }
        SettingValue::Texts(texts) => texts.join(", "),
    }
}

/// Следующий уровень журнала по кругу; неизвестный начинает с первого.
fn next_level(value: &SettingValue) -> &'static str {
    let current = if let SettingValue::Text(text) = value {
        LEVELS.iter().position(|level| *level == text.as_str())
    } else {
        None
    };
    LEVELS[current.map_or(0, |index| (index + 1) % LEVELS.len())]
}

/// Текст поля в значение строки; `Err` — что не так, по-русски.
fn parse_field(setting: Setting, text: &str) -> Result<SettingValue, &'static str> {
    let number = |problem: &'static str| {
        text.parse::<i64>()
            .map(SettingValue::Number)
            .map_err(|_| problem)
    };
    match setting {
        Setting::Failures => number("нужно целое число, например 3"),
        Setting::Xhttp if text.is_empty() || text.to_lowercase() == "как у провайдера" => {
            Ok(SettingValue::Text(XHTTP_PROVIDER.to_owned()))
        }
        Setting::Xhttp => number("нужно число или «как у провайдера»"),
        _ if text.is_empty() => Err("введите время, например 90s или 5m"),
        _ => Ok(SettingValue::Text(text.to_owned())),
    }
}

pub(super) struct Snapshot {
    pub(super) status: Status,
    pub(super) nodes: Nodes,
    /// Когда снимок получен: по разнице таких моментов меряется скорость.
    pub(super) at: Instant,
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
    Saved(Result<(), String>),
    SpeedProgress(Option<SpeedtestProgress>),
    SpeedDone(Result<SpeedtestResult, String>),
    Settings(Result<SettingsView, String>),
    /// Итог проверки изменения без записи.
    Checked {
        edit: Edit,
        result: Result<Reloaded, String>,
    },
    /// Итог сохранения: новые настройки демона и итог перечитывания.
    Applied {
        edit: Edit,
        result: Result<(Reloaded, SettingsView), String>,
    },
}

/// Диалог исключения узла: добавить маску в `deny` подписки или убрать её.
pub(super) struct ExcludeDialog {
    pub(super) subscription: String,
    pub(super) node: String,
    /// Варианты маски при добавлении; при снятии — одна текущая маска.
    pub(super) choices: Vec<String>,
    pub(super) selected: usize,
    pub(super) remove: bool,
}

/// Тест скорости, который идёт сейчас.
pub(super) struct SpeedRun {
    /// Что проверяем: id узла или «текущий узел».
    pub(super) target: String,
    pub(super) since: u64,
    pub(super) progress: Option<SpeedtestProgress>,
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
    /// Мёртвые узлы скрыты; выбранный узел остаётся видимым.
    pub(super) hide_dead: bool,
    /// Исключённые узлы скрыты (`deny` подписки).
    pub(super) hide_excluded: bool,
    /// Порядок таблицы узлов; сортировка относится к виду списка, как и фильтры.
    pub(super) sort: SortBy,
    pub(super) descending: bool,
}

impl Filter {
    pub(super) fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || self.subscription.is_some()
    }
}

/// Скорость выбранного узла в битах в секунду: приём и отдача.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Speed {
    pub(super) down: u64,
    pub(super) up: u64,
}

/// Счётчики выбранного узла на прошлом снимке и скорость по ним.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Meter {
    node: String,
    at: Instant,
    uplink: u64,
    downlink: u64,
    speed: Option<Speed>,
}

impl Meter {
    fn start(node: String, at: Instant, uplink: u64, downlink: u64) -> Self {
        Self {
            node,
            at,
            uplink,
            downlink,
            speed: None,
        }
    }

    /// Следующий снимок того же узла. Скорость за прошедший интервал усредняется
    /// с предыдущей пополам.
    fn advance(self, at: Instant, uplink: u64, downlink: u64) -> Self {
        let Some(elapsed) = at.checked_duration_since(self.at) else {
            return self;
        };
        let millis = elapsed.as_millis();
        if millis == 0 {
            return self;
        }
        let rate = Speed {
            down: bits_per_second(downlink.saturating_sub(self.downlink), millis),
            up: bits_per_second(uplink.saturating_sub(self.uplink), millis),
        };
        let speed = match self.speed {
            None => rate,
            Some(old) => Speed {
                down: u64::midpoint(rate.down, old.down),
                up: u64::midpoint(rate.up, old.up),
            },
        };
        Self {
            node: self.node,
            at,
            uplink,
            downlink,
            speed: Some(speed),
        }
    }
}

/// Бит в секунду: `bytes` байт за `millis` миллисекунд.
fn bits_per_second(bytes: u64, millis: u128) -> u64 {
    let bits = u128::from(bytes) * 8_000 / millis;
    u64::try_from(bits).unwrap_or(u64::MAX)
}

/// Порядок двух узлов. Задержку сравнивают только у живых: непроверенные и мёртвые
/// всегда в конце, независимо от направления.
fn order(a: &Node, b: &Node, by: SortBy, descending: bool) -> Ordering {
    let ordering = match by {
        SortBy::Priority => return Ordering::Equal,
        SortBy::Latency => return latency_order(a, b, descending),
        SortBy::Name => by_name(a, b),
        SortBy::Subscription => a
            .subscription
            .cmp(&b.subscription)
            .then_with(|| by_name(a, b)),
        SortBy::Status => status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| by_name(a, b)),
    };
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn latency_order(a: &Node, b: &Node, descending: bool) -> Ordering {
    let key = |node: &Node| -> (u8, u64) {
        match node.latency_ms.filter(|_| node.status == NodeStatus::Alive) {
            Some(ms) => (0, ms),
            None => (1, 0),
        }
    };
    let (x, y) = (key(a), key(b));
    if x.0 != y.0 {
        return x.0.cmp(&y.0);
    }
    let ordering = x.1.cmp(&y.1);
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn by_name(a: &Node, b: &Node) -> Ordering {
    display_name(&a.name, NameStyle::Country)
        .to_lowercase()
        .cmp(&display_name(&b.name, NameStyle::Country).to_lowercase())
}

fn status_rank(status: NodeStatus) -> u8 {
    match status {
        NodeStatus::Alive => 0,
        NodeStatus::Unknown => 1,
        NodeStatus::Dead => 2,
    }
}

/// Маски `deny` по подпискам из ответа демона; ключ без подписки игнорируется.
fn deny_lists(view: &SettingsView) -> Vec<(String, Vec<String>)> {
    view.items
        .iter()
        .filter(|item| item.key == "deny")
        .filter_map(|item| {
            let name = item.subscription.clone()?;
            match &item.value {
                SettingValue::Texts(masks) => Some((name, masks.clone())),
                _ => None,
            }
        })
        .collect()
}

/// Маски всех списков подписок из ответа демона: подписка, список и маски.
fn sub_lists(view: &SettingsView) -> Vec<(String, ListKind, Vec<String>)> {
    view.items
        .iter()
        .filter_map(|item| {
            let name = item.subscription.clone()?;
            let kind = match item.key.as_str() {
                "allow" => ListKind::Allow,
                "deny" => ListKind::Deny,
                "priority" => ListKind::Priority,
                _ => return None,
            };
            match &item.value {
                SettingValue::Texts(masks) => Some((name, kind, masks.clone())),
                _ => None,
            }
        })
        .collect()
}

/// Название вида для подсказок и строк.
pub(super) fn names_label(style: NameStyle) -> &'static str {
    match style {
        NameStyle::Country => "код страны",
        NameStyle::AsIs => "как есть",
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
    pub(super) tab: Tab,
    /// Открыта панель подробностей узла или подписки.
    pub(super) details: bool,
    /// Позиция курсора среди видимых узлов.
    pub(super) cursor: usize,
    /// Первая показанная строка таблицы узлов.
    pub(super) offset: usize,
    /// Выбранная строка на вкладке «Подписки».
    pub(super) sub_cursor: usize,
    /// Сколько строк журнала прокручено от конца; 0 — самые свежие.
    pub(super) journal_back: usize,
    pub(super) journal_warnings: bool,
    pub(super) names: NameStyle,
    /// Маски скрытых узлов (вид TUI); `*` — любая часть имени.
    pub(super) hidden: Vec<String>,
    /// Маски `deny` подписок, как их отдаёт демон: подписка и её маски.
    pub(super) deny: Vec<(String, Vec<String>)>,
    pub(super) exclude: Option<ExcludeDialog>,
    /// Текст поля правки масок.
    pub(super) mask_edit: String,
    pub(super) speed: Option<SpeedRun>,
    /// Файл настроек демона, показывается на вкладке «Настройки».
    pub(super) settings_path: String,
    /// Настройки демона из `GET /v1/settings`; `None`, пока их не получили.
    pub(super) view: Option<SettingsView>,
    /// Выбранная строка вкладки «Настройки».
    pub(super) setting_cursor: usize,
    /// Первая видимая строка «Настроек».
    pub(super) setting_offset: usize,
    /// Текст поля, которое правят на вкладке «Настройки».
    pub(super) field: String,
    /// Изменение ждёт подтверждения.
    pub(super) confirm: Option<Confirm>,
    /// Запрос на проверку или сохранение настроек идёт с этой секунды.
    pub(super) saving: Option<u64>,
    /// Ошибка у строки «Настроек»: номер строки и короткий текст.
    pub(super) setting_error: Option<(usize, String)>,
    /// Правка масок выбранной подписки.
    pub(super) sub_edit: Option<SubEdit>,
    /// Маски всех подписок из ответа демона: подписка, список и маски.
    pub(super) sub_lists: Vec<(String, ListKind, Vec<String>)>,
    pub(super) log: VecDeque<LogEntry>,
    pub(super) notice: Option<Notice>,
    pub(super) update: Option<UpdateRun>,
    /// Области нажатий последней отрисовки.
    pub(super) hits: Vec<Hit>,
    pub(super) quit: bool,
    meter: Option<Meter>,
    cursor_id: Option<String>,
    rows: usize,
    last_click: Option<(Target, Instant)>,
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
            tab: Tab::Overview,
            details: false,
            cursor: 0,
            offset: 0,
            sub_cursor: 0,
            journal_back: 0,
            journal_warnings: false,
            names: NameStyle::default(),
            hidden: Vec::new(),
            deny: Vec::new(),
            exclude: None,
            mask_edit: String::new(),
            speed: None,
            settings_path: String::new(),
            view: None,
            setting_cursor: 0,
            setting_offset: 0,
            field: String::new(),
            confirm: None,
            saving: None,
            setting_error: None,
            sub_edit: None,
            sub_lists: Vec::new(),
            log: VecDeque::new(),
            notice: None,
            update: None,
            hits: Vec::new(),
            quit: false,
            meter: None,
            cursor_id: None,
            rows: DEFAULT_ROWS,
            last_click: None,
        }
    }

    /// Скорость выбранного узла; `None`, пока нет двух снимков с его счётчиками.
    pub(super) fn speed(&self) -> Option<Speed> {
        self.meter.as_ref().and_then(|meter| meter.speed)
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

    /// Записи журнала, которые показывает вкладка: все или только предупреждения и ошибки.
    pub(super) fn journal_entries(&self) -> Vec<&LogEntry> {
        self.log
            .iter()
            .filter(|entry| {
                !self.journal_warnings || matches!(entry.tone, Tone::Yellow | Tone::Red)
            })
            .collect()
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        if self.exclude.is_some() {
            return self.exclude_key(key);
        }
        if self.confirm.is_some() {
            return self.confirm_key(key);
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
            InputMode::Masks => self.masks_key(key),
            InputMode::Field => self.field_key(key),
            InputMode::SubMasks => self.sub_masks_key(key),
            InputMode::Normal => self.normal_key(key),
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, at: Instant) -> Option<Effect> {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(WHEEL_ROWS),
            MouseEventKind::ScrollDown => self.scroll_down(WHEEL_ROWS),
            MouseEventKind::Down(MouseButton::Left) => {
                return self.press(mouse.column, mouse.row, at);
            }
            _ => {}
        }
        None
    }

    fn press(&mut self, column: u16, row: u16, at: Instant) -> Option<Effect> {
        if self.input == InputMode::Help {
            self.input = InputMode::Normal;
            return None;
        }
        let target = self
            .hits
            .iter()
            .find(|hit| hit.contains(column, row))?
            .target;
        // Пока ждут ответа на вопрос, нажатия мимо кнопок ничего не делают.
        if self.confirm.is_some() && !matches!(target, Target::ConfirmYes | Target::ConfirmNo) {
            return None;
        }
        if matches!(
            self.input,
            InputMode::Filter | InputMode::Masks | InputMode::Field | InputMode::SubMasks
        ) {
            self.input = InputMode::Normal;
            self.sub_edit = None;
        }
        self.click(target, at)
    }

    fn click(&mut self, target: Target, at: Instant) -> Option<Effect> {
        let double = self.last_click.is_some_and(|(previous, when)| {
            previous == target && at.duration_since(when) <= DOUBLE_CLICK
        });
        self.last_click = Some((target, at));
        match target {
            Target::Tab(tab) => return self.switch(tab),
            Target::NodeRow(index) => {
                self.move_to(index);
                if double {
                    return self.pin_current();
                }
            }
            Target::SubRow(index) => self.sub_cursor = index.min(self.last_sub()),
            Target::Pin => return self.pin_current(),
            Target::Autoselect => return self.unpin(),
            Target::UpdateAll => return self.start_update(None),
            Target::UpdateSub => return self.update_selected(),
            Target::DeadToggle => self.toggle_dead(),
            Target::SubFilter => self.next_subscription(),
            Target::Search => self.input = InputMode::Filter,
            Target::WarningsToggle => self.journal_warnings = !self.journal_warnings,
            Target::ExcludedToggle => self.toggle_excluded(),
            Target::Sort(by) => self.sort_click(by),
            Target::HiddenEdit => self.start_masks(),
            Target::Speed => return self.speedtest(None),
            Target::SpeedNode => return self.speed_selected(),
            Target::SettingRow(index) => self.select_setting(index),
            Target::SettingCell(index) => {
                self.select_setting(index);
                return self.activate(index);
            }
            Target::SettingLevel(level) => return self.choose_level(level),
            Target::ConfirmYes => return self.confirm_yes(),
            Target::ConfirmNo => self.cancel_confirm(),
            Target::SubMasks => self.start_sub_edit(),
            Target::SubUp => return self.move_sub(true),
            Target::SubDown => return self.move_sub(false),
        }
        None
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
            KeyCode::Esc => return self.escape(),
            KeyCode::Char('1') => return self.switch(Tab::Overview),
            KeyCode::Char('2') => return self.switch(Tab::Nodes),
            KeyCode::Char('3') => return self.switch(Tab::Subscriptions),
            KeyCode::Char('4') => return self.switch(Tab::Journal),
            KeyCode::Char('5') => return self.switch(Tab::Settings),
            KeyCode::Tab | KeyCode::Right => return self.step_tab(true),
            KeyCode::BackTab | KeyCode::Left => return self.step_tab(false),
            KeyCode::Char('m') if self.tab == Tab::Subscriptions => self.start_sub_edit(),
            KeyCode::Char('[') if self.tab == Tab::Subscriptions => return self.move_sub(true),
            KeyCode::Char(']') if self.tab == Tab::Subscriptions => return self.move_sub(false),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_up(self.page()),
            KeyCode::PageDown => self.scroll_down(self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.jump(false),
            KeyCode::End | KeyCode::Char('G') => self.jump(true),
            KeyCode::Char('/') if self.tab == Tab::Nodes => self.input = InputMode::Filter,
            KeyCode::Char('?') | KeyCode::F(1) => self.input = InputMode::Help,
            KeyCode::Enter => return self.enter(),
            KeyCode::Char('p') if self.tab == Tab::Nodes => return self.pin_current(),
            KeyCode::Char('a') => return self.unpin(),
            KeyCode::Char('u') => return self.update_key(),
            KeyCode::Char('d') if self.tab == Tab::Nodes => self.toggle_dead(),
            KeyCode::Char('s') if self.tab == Tab::Nodes => self.next_subscription(),
            KeyCode::Char('w') if self.tab == Tab::Journal => {
                self.journal_warnings = !self.journal_warnings;
            }
            KeyCode::Char('o') if self.tab == Tab::Nodes => self.cycle_sort(),
            KeyCode::Char('x') if self.tab == Tab::Nodes => return self.start_exclude(),
            KeyCode::Char('e') if self.tab == Tab::Nodes => self.toggle_excluded(),
            KeyCode::Char('h') if self.tab == Tab::Nodes => self.start_masks(),
            KeyCode::Char('t') if self.tab == Tab::Nodes => return self.speed_selected(),
            KeyCode::Char('s') if self.tab == Tab::Overview => return self.speedtest(None),
            KeyCode::Char(' ') if self.tab == Tab::Settings => {
                return self.activate(self.setting_cursor);
            }
            _ => {}
        }
        None
    }

    fn field_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.input = InputMode::Normal,
            KeyCode::Enter => return self.commit_field(),
            KeyCode::Backspace => {
                self.field.pop();
            }
            KeyCode::Char('u') if ctrl => self.field.clear(),
            KeyCode::Char(c)
                if !ctrl && !alt && !c.is_control() && self.field.chars().count() < FILTER_MAX =>
            {
                self.field.push(c);
            }
            _ => {}
        }
        None
    }

    fn masks_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.input = InputMode::Normal,
            KeyCode::Enter => {
                self.input = InputMode::Normal;
                self.hidden = split_masks(&self.mask_edit);
                self.mask_edit.clear();
                self.refilter();
                let text = if self.hidden.is_empty() {
                    "Скрытых узлов нет".to_owned()
                } else {
                    format!("Скрыто масками: {}", self.hidden.join(", "))
                };
                self.notify(Tone::Green, &text, NOTICE_SECS);
                return Some(Effect::SavePrefs(self.prefs()));
            }
            KeyCode::Backspace => {
                self.mask_edit.pop();
            }
            KeyCode::Char('u') if ctrl => self.mask_edit.clear(),
            KeyCode::Char(c)
                if !ctrl
                    && !alt
                    && !c.is_control()
                    && self.mask_edit.chars().count() < FILTER_MAX =>
            {
                self.mask_edit.push(c);
            }
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

    /// Применяет ответ. Если после ответа нужен ещё один запрос к демону, возвращает его.
    pub(super) fn apply(&mut self, msg: Msg) -> Option<Effect> {
        match msg {
            Msg::Connected => self.link = Link::Up,
            Msg::Snapshot(snapshot) => self.set_snapshot(*snapshot),
            Msg::Event(event) => {
                let (tone, text) = render::event_text(&event, self.names);
                self.push_log(tone, &text);
            }
            Msg::Down { reason, retry_in } => self.on_down(&reason, retry_in),
            Msg::Pinned(result) => self.on_pinned(result),
            Msg::Updated(result) => self.on_updated(result),
            Msg::SpeedProgress(progress) => {
                if let Some(run) = self.speed.as_mut() {
                    run.progress = progress;
                }
            }
            Msg::SpeedDone(result) => self.on_speed(result),
            Msg::Settings(result) => self.on_settings(result),
            Msg::Checked { edit, result } => return self.on_checked(edit, result),
            Msg::Applied { edit, result } => self.on_applied(edit, result),
            Msg::Saved(Ok(())) => {}
            Msg::Saved(Err(error)) => {
                let (cause, _) = split_lines(&error);
                self.fail(&format!("Не удалось сохранить вид: {cause}"));
            }
        }
        None
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.link = Link::Up;
        self.status = Some(snapshot.status);
        self.status_at = self.now;
        self.nodes = snapshot.nodes.nodes;
        self.measure(snapshot.at);
        self.refilter();
    }

    /// Смена узла и уменьшение счётчика (перезапуск xray) начинают измерение заново.
    fn measure(&mut self, at: Instant) {
        let reading = self
            .nodes
            .iter()
            .find(|node| node.selected)
            .and_then(|node| Some((node.id.clone(), node.uplink_bytes?, node.downlink_bytes?)));
        self.meter = match (reading, self.meter.take()) {
            (None, _) => None,
            (Some((node, uplink, downlink)), Some(meter))
                if meter.node == node && uplink >= meter.uplink && downlink >= meter.downlink =>
            {
                Some(meter.advance(at, uplink, downlink))
            }
            (Some((node, uplink, downlink)), _) => Some(Meter::start(node, at, uplink, downlink)),
        };
    }

    fn on_down(&mut self, reason: &str, retry_in: Duration) {
        // После обрыва интервал до следующего снимка не говорит о скорости.
        self.meter = None;
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

    /// На вкладке «Подписки» — выбранная подписка, в остальных местах — все.
    fn update_key(&mut self) -> Option<Effect> {
        if self.tab == Tab::Subscriptions {
            return self.update_selected();
        }
        self.start_update(None)
    }

    fn update_selected(&mut self) -> Option<Effect> {
        let Some(name) = self.subscription_names().get(self.sub_cursor).cloned() else {
            self.notify(Tone::Yellow, "Нет подписки для обновления", NOTICE_SECS);
            return None;
        };
        self.start_update(Some(name))
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

    fn toggle_names(&mut self) -> Effect {
        self.names = match self.names {
            NameStyle::Country => NameStyle::AsIs,
            NameStyle::AsIs => NameStyle::Country,
        };
        let text = format!("Значки в именах: {}", names_label(self.names));
        self.notify(Tone::Green, &text, NOTICE_SECS);
        Effect::SavePrefs(self.prefs())
    }

    fn prefs(&self) -> Prefs {
        Prefs {
            names: self.names,
            hidden: self.hidden.clone(),
        }
    }

    /// Щелчок по заголовку: тот же столбец меняет направление, другой — выбирает порядок.
    fn sort_click(&mut self, by: SortBy) {
        if self.filter.sort == by {
            self.filter.descending = !self.filter.descending;
        } else {
            self.filter.sort = by;
            self.filter.descending = false;
        }
        self.refilter();
    }

    fn cycle_sort(&mut self) {
        self.filter.sort = self.filter.sort.next();
        self.filter.descending = false;
        self.refilter();
        let text = format!("Сортировка: {}", self.filter.sort.label());
        self.notify(Tone::Green, &text, NOTICE_SECS);
    }

    fn start_masks(&mut self) {
        if self.tab == Tab::Nodes {
            self.mask_edit = self.hidden.join(", ");
            self.input = InputMode::Masks;
        }
    }

    fn speed_selected(&mut self) -> Option<Effect> {
        let Some(id) = self.current().map(|node| node.id.clone()) else {
            self.notify(Tone::Yellow, "Нет узла для теста скорости", NOTICE_SECS);
            return None;
        };
        self.speedtest(Some(id))
    }

    /// Запускает тест скорости, если другой не идёт. `None` — через текущий узел.
    fn speedtest(&mut self, node: Option<String>) -> Option<Effect> {
        if self.speed.is_some() {
            self.notify(Tone::Yellow, "Тест скорости уже идёт", NOTICE_SECS);
            return None;
        }
        self.notice = None;
        let target = node.clone().unwrap_or_else(|| "текущий узел".to_owned());
        self.speed = Some(SpeedRun {
            target,
            since: self.now,
            progress: None,
        });
        Some(Effect::SpeedTest(node))
    }

    fn on_speed(&mut self, result: Result<SpeedtestResult, String>) {
        if self.speed.take().is_none() {
            return;
        }
        match result {
            Ok(result) => {
                let best = result
                    .runs
                    .iter()
                    .map(|run| run.mbps)
                    .fold(0.0_f64, f64::max);
                let latency = result.runs.first().map(|run| run.ttfb_ms);
                let text = format!(
                    "Тест скорости «{}»: ↓ {best:.1} Мбит/с, задержка {}",
                    result.node,
                    render::latency_text(latency)
                );
                self.push_log(Tone::Green, &text);
                self.notify(Tone::Green, &text, RESULT_SECS);
            }
            Err(error) => {
                let (cause, _) = split_lines(&error);
                self.fail(&format!("Тест скорости не удался: {cause}"));
            }
        }
    }

    fn toggle_dead(&mut self) {
        if self.tab == Tab::Nodes {
            self.filter.hide_dead = !self.filter.hide_dead;
            self.refilter();
        }
    }

    fn next_subscription(&mut self) {
        let names = self.subscription_names();
        if names.is_empty() {
            return;
        }
        let current = self
            .filter
            .subscription
            .as_ref()
            .and_then(|name| names.iter().position(|candidate| candidate == name))
            .map_or(0, |index| index + 1);
        let next = (current + 1) % (names.len() + 1);
        self.filter.subscription = next
            .checked_sub(1)
            .and_then(|index| names.get(index))
            .cloned();
        self.refilter();
    }

    fn clear_filter(&mut self) {
        if self.filter.is_active() {
            self.filter.text.clear();
            self.filter.subscription = None;
            self.refilter();
        }
    }

    /// Esc отменяет тест скорости первым: запрос к демону прерывается.
    fn escape(&mut self) -> Option<Effect> {
        if self.speed.take().is_some() {
            self.notify(Tone::Yellow, "Тест скорости отменён", NOTICE_SECS);
            return Some(Effect::CancelSpeedTest);
        }
        if self.details {
            self.details = false;
        } else if self.tab == Tab::Nodes && self.filter.is_active() {
            self.clear_filter();
        } else {
            self.quit = true;
        }
        None
    }

    fn enter(&mut self) -> Option<Effect> {
        match self.tab {
            Tab::Nodes => {
                if self.current().is_some() {
                    self.details = !self.details;
                } else {
                    self.notify(Tone::Yellow, "Нет узла для подробностей", NOTICE_SECS);
                }
            }
            Tab::Subscriptions => self.details = !self.details,
            Tab::Settings => return self.activate(self.setting_cursor),
            Tab::Overview | Tab::Journal => {}
        }
        None
    }

    /// Переход на вкладку. Вход на «Настройки» перечитывает их: файл мог поменяться руками.
    fn switch(&mut self, tab: Tab) -> Option<Effect> {
        let entering = self.tab != tab;
        if entering {
            self.details = false;
        }
        self.tab = tab;
        (entering && tab == Tab::Settings).then_some(Effect::LoadSettings)
    }

    fn step_tab(&mut self, forward: bool) -> Option<Effect> {
        let count = Tab::ALL.len();
        let index = Tab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        Tab::ALL.get(next).and_then(|tab| self.switch(*tab))
    }

    fn scroll_up(&mut self, by: usize) {
        match self.tab {
            Tab::Nodes => self.move_up(by),
            Tab::Subscriptions => self.sub_cursor = self.sub_cursor.saturating_sub(by),
            Tab::Journal => self.journal_back = self.journal_back.saturating_add(by),
            Tab::Settings => self.select_setting(self.setting_cursor.saturating_sub(by)),
            Tab::Overview => {}
        }
    }

    fn scroll_down(&mut self, by: usize) {
        match self.tab {
            Tab::Nodes => self.move_down(by),
            Tab::Subscriptions => {
                self.sub_cursor = self.sub_cursor.saturating_add(by).min(self.last_sub());
            }
            Tab::Journal => self.journal_back = self.journal_back.saturating_sub(by),
            Tab::Settings => self.select_setting(self.setting_cursor.saturating_add(by)),
            Tab::Overview => {}
        }
    }

    /// Home и End: в начало или в конец списка текущей вкладки.
    fn jump(&mut self, end: bool) {
        match self.tab {
            Tab::Nodes => self.move_to(if end { usize::MAX } else { 0 }),
            Tab::Subscriptions => self.sub_cursor = if end { self.last_sub() } else { 0 },
            Tab::Journal => self.journal_back = if end { 0 } else { usize::MAX },
            Tab::Settings => self.select_setting(if end { usize::MAX } else { 0 }),
            Tab::Overview => {}
        }
    }

    /// Маска `deny` подписки, которая исключает узел; `None`, если узел в автовыборе.
    fn excluding_mask(&self, node: &Node) -> Option<String> {
        self.deny
            .iter()
            .filter(|(name, _)| *name == node.subscription)
            .flat_map(|(_, masks)| masks.iter())
            .find(|mask| Pattern::new(mask).matches(&node.name))
            .cloned()
    }

    pub(super) fn is_excluded(&self, node: &Node) -> bool {
        self.excluding_mask(node).is_some()
    }

    fn deny_of(&self, subscription: &str) -> Vec<String> {
        self.deny
            .iter()
            .find(|(name, _)| name.as_str() == subscription)
            .map(|(_, masks)| masks.clone())
            .unwrap_or_default()
    }

    /// Сколько узлов подписки задевает маска.
    pub(super) fn affected(&self, subscription: &str, mask: &str) -> usize {
        let pattern = Pattern::new(mask);
        self.nodes
            .iter()
            .filter(|node| node.subscription == subscription && pattern.matches(&node.name))
            .count()
    }

    /// `x`: исключить узел из автовыбора или снять исключение, если он уже исключён.
    fn start_exclude(&mut self) -> Option<Effect> {
        let Some(node) = self.current().cloned() else {
            self.notify(Tone::Yellow, "Нет узла для исключения", NOTICE_SECS);
            return None;
        };
        let remove = self.excluding_mask(&node);
        let choices = if let Some(mask) = &remove {
            vec![mask.clone()]
        } else {
            let mut choices = vec![escape_literal(&node.name)];
            let label = display_name(&node.name, NameStyle::Country);
            let word = without_code(&label)
                .split_whitespace()
                .next()
                .unwrap_or_default();
            let suggested = format!("{}*", escape_literal(word));
            if !word.is_empty() && !choices.contains(&suggested) {
                choices.push(suggested);
            }
            choices
        };
        self.exclude = Some(ExcludeDialog {
            subscription: node.subscription.clone(),
            node: node.id.clone(),
            choices,
            selected: 0,
            remove: remove.is_some(),
        });
        None
    }

    fn exclude_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let dialog = self.exclude.as_mut()?;
        let count = dialog.choices.len();
        match key.code {
            KeyCode::Esc => self.exclude = None,
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                dialog.selected = (dialog.selected + count - 1) % count;
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                dialog.selected = (dialog.selected + 1) % count;
            }
            KeyCode::Char('1') if count >= 1 => dialog.selected = 0,
            KeyCode::Char('2') if count >= 2 => dialog.selected = 1,
            KeyCode::Enter => return self.confirm_exclude(),
            _ => {}
        }
        None
    }

    /// Исключение или возврат узла: маска `deny` подписки. Сначала проверка, потом, если
    /// понадобится, вопрос про перезапуск xray.
    fn confirm_exclude(&mut self) -> Option<Effect> {
        let dialog = self.exclude.take()?;
        let choice = dialog.choices.get(dialog.selected)?.clone();
        let mut deny = self.deny_of(&dialog.subscription);
        let done = if dialog.remove {
            deny.retain(|mask| *mask != choice);
            format!("Исключение «{}» снято", dialog.node)
        } else {
            if !deny.contains(&choice) {
                deny.push(choice.clone());
            }
            format!("«{}» исключён маской «{choice}»", dialog.node)
        };
        let change = SettingChange {
            table: None,
            subscription: Some(dialog.subscription),
            key: "deny".to_owned(),
            value: SettingValue::Texts(deny),
        };
        self.begin_check(Edit {
            changes: vec![change],
            done,
            row: None,
            warn: None,
        })
    }

    fn on_settings(&mut self, result: Result<SettingsView, String>) {
        match result {
            Ok(view) => self.set_view(view),
            Err(error) => {
                let (cause, _) = split_lines(&error);
                self.push_log(
                    Tone::Yellow,
                    &format!("настройки демона не получены: {cause}"),
                );
            }
        }
    }

    fn toggle_excluded(&mut self) {
        if self.tab == Tab::Nodes {
            self.filter.hide_excluded = !self.filter.hide_excluded;
            self.refilter();
            let text = if self.filter.hide_excluded {
                "Исключённые узлы скрыты"
            } else {
                "Исключённые узлы показаны"
            };
            self.notify(Tone::Green, text, NOTICE_SECS);
        }
    }

    fn last_sub(&self) -> usize {
        self.subscription_names().len().saturating_sub(1)
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
        let hide_dead = self.filter.hide_dead;
        let masks: Vec<Pattern> = self.hidden.iter().map(|mask| Pattern::new(mask)).collect();
        let deny = self.deny.as_slice();
        let hide_excluded = self.filter.hide_excluded;
        self.visible = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                let alive = node.selected || node.status != NodeStatus::Dead;
                let masked = masks.iter().any(|mask| mask.matches(&node.name));
                let excluded = deny.iter().any(|(name, masks)| {
                    *name == node.subscription
                        && masks
                            .iter()
                            .any(|mask| Pattern::new(mask).matches(&node.name))
                });
                (!hide_dead || alive)
                    && !masked
                    && (!hide_excluded || !excluded)
                    && subscription.is_none_or(|name| node.subscription == name)
                    && (needle.is_empty() || node.id.to_lowercase().contains(&needle))
            })
            .map(|(index, _)| index)
            .collect();
        let by = self.filter.sort;
        let descending = self.filter.descending;
        let nodes = &self.nodes;
        self.visible
            .sort_by(|a, b| order(&nodes[*a], &nodes[*b], by, descending));
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

/// Есть ли в ответе демона эта таблица и ключ (маски подписок сюда не относятся).
fn is_setting(item: &SettingItem, table: &str, key: &str) -> bool {
    item.subscription.is_none() && item.table.as_deref() == Some(table) && item.key == key
}

impl App {
    /// Что показывает строка: значение, откуда оно взято и можно ли его менять.
    pub(super) fn shown(&self, setting: Setting) -> Shown {
        if setting == Setting::Names {
            return Shown {
                value: names_label(self.names).to_owned(),
                source: "вид TUI".to_owned(),
                enabled: true,
            };
        }
        match self.setting_item(setting) {
            Some(item) => Shown {
                value: value_text(&item.value, setting),
                source: item.source.clone(),
                enabled: true,
            },
            None => Shown {
                value: "—".to_owned(),
                source: self.missing_reason(setting).to_owned(),
                enabled: false,
            },
        }
    }

    /// Почему у строки нет значения: демон не ответил или kill switch вне режима шлюза.
    fn missing_reason(&self, setting: Setting) -> &'static str {
        if self.view.is_none() {
            "нет данных"
        } else if setting == Setting::KillSwitch {
            "только в режиме шлюза"
        } else {
            "нет в ответе демона"
        }
    }

    fn setting_item(&self, setting: Setting) -> Option<&SettingItem> {
        let (table, key) = setting.place()?;
        let view = self.view.as_ref()?;
        view.items.iter().find(|item| is_setting(item, table, key))
    }

    /// Новые настройки демона: по ним показываются строки и маски исключений.
    fn set_view(&mut self, view: SettingsView) {
        self.deny = deny_lists(&view);
        self.sub_lists = sub_lists(&view);
        self.view = Some(view);
        self.refilter();
    }

    /// Выбирает строку «Настроек». Ошибка прошлой попытки относилась к старой строке.
    fn select_setting(&mut self, index: usize) {
        self.setting_cursor = index.min(Setting::ALL.len() - 1);
        self.setting_error = None;
    }

    /// Держит выбранную строку на экране; `rows` — сколько строк помещается.
    pub(super) fn keep_setting_visible(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.setting_cursor < self.setting_offset {
            self.setting_offset = self.setting_cursor;
        } else if self.setting_cursor >= self.setting_offset + rows {
            self.setting_offset = self.setting_cursor + 1 - rows;
        }
    }

    /// `m` на вкладке «Подписки»: правка масок выбранной подписки, сначала `allow`.
    fn start_sub_edit(&mut self) {
        let Some(name) = self.subscription_names().get(self.sub_cursor).cloned() else {
            self.notify(Tone::Yellow, "Нет подписки для правки масок", NOTICE_SECS);
            return;
        };
        self.open_sub_edit(name, ListKind::Allow);
    }

    fn open_sub_edit(&mut self, subscription: String, kind: ListKind) {
        let text = self.sub_masks(&subscription, kind).join(", ");
        self.sub_edit = Some(SubEdit {
            subscription,
            kind,
            text,
        });
        self.input = InputMode::SubMasks;
    }

    fn sub_masks(&self, subscription: &str, kind: ListKind) -> Vec<String> {
        self.sub_lists
            .iter()
            .find(|(name, list, _)| name.as_str() == subscription && *list == kind)
            .map(|(_, _, masks)| masks.clone())
            .unwrap_or_default()
    }

    /// Сколько узлов подписки задевает список масок. Для `allow` — те, что он отсекает.
    pub(super) fn sub_affected(&self, subscription: &str, kind: ListKind, text: &str) -> usize {
        let masks: Vec<Pattern> = split_masks(text)
            .iter()
            .map(|mask| Pattern::new(mask))
            .collect();
        self.nodes
            .iter()
            .filter(|node| node.subscription == subscription)
            .filter(|node| {
                let hit = masks.iter().any(|pattern| pattern.matches(&node.name));
                match kind {
                    ListKind::Allow => !masks.is_empty() && !hit,
                    ListKind::Deny | ListKind::Priority => hit,
                }
            })
            .count()
    }

    fn sub_masks_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.close_sub_edit(),
            KeyCode::Enter => return self.save_sub_edit(),
            KeyCode::Tab => self.switch_sub_list(true),
            KeyCode::BackTab => self.switch_sub_list(false),
            KeyCode::Backspace => {
                if let Some(edit) = self.sub_edit.as_mut() {
                    edit.text.pop();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(edit) = self.sub_edit.as_mut() {
                    edit.text.clear();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                if let Some(edit) = self.sub_edit.as_mut()
                    && edit.text.chars().count() < MASKS_MAX
                {
                    edit.text.push(c);
                }
            }
            _ => {}
        }
        None
    }

    /// Tab переключает список. Несохранённый текст при этом сбрасывается к сохранённому.
    fn switch_sub_list(&mut self, forward: bool) {
        let Some(edit) = self.sub_edit.as_ref() else {
            return;
        };
        let kind = edit.kind.step(forward);
        let subscription = edit.subscription.clone();
        self.open_sub_edit(subscription, kind);
    }

    fn close_sub_edit(&mut self) {
        self.sub_edit = None;
        self.input = InputMode::Normal;
        self.notify(Tone::Yellow, "Маски не изменены", NOTICE_SECS);
    }

    /// Enter: маски уходят на проверку, дальше как у остальных изменений.
    fn save_sub_edit(&mut self) -> Option<Effect> {
        if self.saving.is_some() {
            self.notify(Tone::Yellow, "Настройки уже сохраняются", NOTICE_SECS);
            return None;
        }
        let edit = self.sub_edit.take()?;
        self.input = InputMode::Normal;
        let done = format!("«{}»: {} сохранены", edit.subscription, edit.kind.title());
        let change = SettingChange {
            table: None,
            subscription: Some(edit.subscription),
            key: edit.kind.key().to_owned(),
            value: SettingValue::Texts(split_masks(&edit.text)),
        };
        self.begin_check(Edit {
            changes: vec![change],
            done,
            row: None,
            warn: None,
        })
    }

    /// `[` и `]`: подписка на одно место выше или ниже. Порядок подписок — их приоритет.
    fn move_sub(&mut self, up: bool) -> Option<Effect> {
        let names: Vec<String> = self.status.as_ref().map_or_else(Vec::new, |status| {
            status
                .subscriptions
                .iter()
                .map(|sub| sub.name.clone())
                .collect()
        });
        let Some(name) = names.get(self.sub_cursor).cloned() else {
            self.notify(Tone::Yellow, "Нет подписки для перестановки", NOTICE_SECS);
            return None;
        };
        let target = if up {
            self.sub_cursor.checked_sub(1)
        } else {
            self.sub_cursor.checked_add(1)
        };
        let Some(to) = target.filter(|to| *to < names.len()) else {
            let text = if up {
                "Подписка уже первая"
            } else {
                "Подписка уже последняя"
            };
            self.notify(Tone::Yellow, text, NOTICE_SECS);
            return None;
        };
        let change = SettingChange {
            table: None,
            subscription: Some(name.clone()),
            key: "position".to_owned(),
            value: SettingValue::Number(i64::try_from(to).unwrap_or(i64::MAX)),
        };
        self.sub_cursor = to;
        self.begin_check(Edit {
            changes: vec![change],
            done: format!("«{name}» теперь на месте {}", to + 1),
            row: None,
            warn: None,
        })
    }

    /// Enter, пробел или клик по значению: переключает, меняет по кругу или открывает поле.
    fn activate(&mut self, index: usize) -> Option<Effect> {
        let setting = *Setting::ALL.get(index)?;
        if setting == Setting::Names {
            return Some(self.toggle_names());
        }
        if self.saving.is_some() {
            self.notify(Tone::Yellow, "Настройки уже сохраняются", NOTICE_SECS);
            return None;
        }
        let Some(item) = self.setting_item(setting) else {
            let text = if self.view.is_none() {
                "Настройки демона ещё не получены"
            } else if setting == Setting::KillSwitch {
                "Kill switch меняется только в режиме шлюза"
            } else {
                "Этой настройки нет в ответе демона"
            };
            self.notify(Tone::Yellow, text, NOTICE_SECS);
            return None;
        };
        let value = item.value.clone();
        match (setting, value) {
            (Setting::LogLevel, value) => {
                let next = SettingValue::Text(next_level(&value).to_owned());
                self.begin_edit(setting, next)
            }
            (_, SettingValue::Bool(on)) => self.begin_edit(setting, SettingValue::Bool(!on)),
            _ => {
                self.open_field(index);
                None
            }
        }
    }

    /// Клик по уровню журнала: сразу выбранный уровень, без перебора.
    fn choose_level(&mut self, level: &'static str) -> Option<Effect> {
        self.select_setting(Setting::LogLevel.row());
        if self.view.is_none() {
            self.notify(
                Tone::Yellow,
                "Настройки демона ещё не получены",
                NOTICE_SECS,
            );
            return None;
        }
        let wanted = SettingValue::Text(level.to_owned());
        if self
            .setting_item(Setting::LogLevel)
            .is_some_and(|item| item.value == wanted)
        {
            return None;
        }
        self.begin_edit(Setting::LogLevel, wanted)
    }

    /// Новое значение строки уходит на проверку; подтверждение спросим, если оно нужно.
    fn begin_edit(&mut self, setting: Setting, value: SettingValue) -> Option<Effect> {
        let (table, key) = setting.place()?;
        let done = format!("{}: {}", setting.title(), value_text(&value, setting));
        let warn = setting.warning(&value).map(str::to_owned);
        let edit = Edit {
            changes: vec![SettingChange {
                table: Some(table.to_owned()),
                subscription: None,
                key: key.to_owned(),
                value,
            }],
            done,
            row: Some(setting.row()),
            warn,
        };
        self.begin_check(edit)
    }

    /// Открывает поле правки с текущим значением; Enter его сохранит.
    fn open_field(&mut self, index: usize) {
        let Some(setting) = Setting::ALL.get(index).copied() else {
            return;
        };
        self.field = self
            .setting_item(setting)
            .map_or_else(String::new, |item| value_text(&item.value, setting));
        self.input = InputMode::Field;
    }

    /// Enter в поле: проверяет текст и отправляет изменение. Ошибка текста — у строки, поле
    /// остаётся открытым, чтобы его поправить.
    fn commit_field(&mut self) -> Option<Effect> {
        let index = self.setting_cursor;
        let setting = *Setting::ALL.get(index)?;
        match parse_field(setting, self.field.trim()) {
            Ok(value) => {
                self.input = InputMode::Normal;
                self.begin_edit(setting, value)
            }
            Err(problem) => {
                self.setting_error = Some((index, problem.to_owned()));
                None
            }
        }
    }

    /// Запускает проверку, если другой запрос не идёт.
    fn begin_check(&mut self, edit: Edit) -> Option<Effect> {
        if self.saving.is_some() {
            self.notify(Tone::Yellow, "Настройки уже сохраняются", NOTICE_SECS);
            return None;
        }
        self.saving = Some(self.now);
        self.setting_error = None;
        Some(Effect::Check(edit))
    }

    /// Итог проверки: без вопросов сразу сохраняем, иначе спрашиваем подтверждение.
    fn on_checked(&mut self, edit: Edit, result: Result<Reloaded, String>) -> Option<Effect> {
        match result {
            Ok(reloaded) => self.after_check(edit, &reloaded),
            Err(error) => {
                self.saving = None;
                self.setting_failed(edit.row, &error);
                None
            }
        }
    }

    fn after_check(&mut self, edit: Edit, reloaded: &Reloaded) -> Option<Effect> {
        let mut lines = Vec::new();
        if let Some(warn) = &edit.warn {
            lines.push(warn.clone());
        }
        if reloaded.xray_restart {
            lines.push(RESTART_TEXT.to_owned());
        }
        if lines.is_empty() {
            return Some(Effect::Apply(edit));
        }
        self.saving = None;
        self.confirm = Some(Confirm {
            question: lines.join("; "),
            edit,
        });
        None
    }

    fn on_applied(&mut self, edit: Edit, result: Result<(Reloaded, SettingsView), String>) {
        self.saving = None;
        match result {
            Ok((reloaded, view)) => self.on_saved(edit, &reloaded, view),
            Err(error) => self.setting_failed(edit.row, &error),
        }
    }

    /// Изменение сохранено: форма получает новые значения, журнал и строка состояния — итог.
    fn on_saved(&mut self, edit: Edit, reloaded: &Reloaded, view: SettingsView) {
        self.set_view(view);
        let mut text = edit.done;
        let mut tone = Tone::Green;
        if reloaded.xray_restart {
            text.push_str("; xray перезапущен, связь прервалась на несколько секунд");
            tone = Tone::Yellow;
        }
        if !reloaded.restart_required.is_empty() {
            let keys = reloaded.restart_required.join(", ");
            text = format!("{text}; для этого нужен raycat restart: {keys}");
            tone = Tone::Yellow;
        }
        self.push_log(tone, &text);
        self.notify(tone, &text, RESULT_SECS);
    }

    /// Ошибка сохранения или проверки: журнал и строка состояния получают весь текст в одну
    /// строку, у строки «Настроек» остаётся тот же текст, обрезанный по ширине.
    fn setting_failed(&mut self, row: Option<usize>, error: &str) {
        let text = error
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" — ");
        if let Some(row) = row {
            self.setting_error = Some((row, clip(&text)));
        }
        self.push_log(Tone::Red, &text);
        self.notify(Tone::Red, &text, RESULT_SECS);
    }

    fn confirm_key(&mut self, key: KeyEvent) -> Option<Effect> {
        match key.code {
            KeyCode::Enter => self.confirm_yes(),
            KeyCode::Esc => {
                self.cancel_confirm();
                None
            }
            _ => None,
        }
    }

    fn confirm_yes(&mut self) -> Option<Effect> {
        let confirm = self.confirm.take()?;
        self.saving = Some(self.now);
        Some(Effect::Apply(confirm.edit))
    }

    fn cancel_confirm(&mut self) {
        if self.confirm.take().is_some() {
            self.notify(Tone::Yellow, "Изменение отменено", NOTICE_SECS);
        }
    }
}

#[cfg(test)]
mod tests {
    use raycat_proto::{
        CurrentNode, Mode, SpeedtestRun, SubscriptionStatus, UpdateResult, XrayStatus,
    };

    use super::*;

    const NOW: u64 = 1_790_596_800;

    fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn nodes_tab(app: &mut App) {
        press(app, KeyCode::Char('2'));
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
                api_silent_secs: Some(0),
                api_stuck: false,
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
                    app: "happ".to_owned(),
                    platform: "windows".to_owned(),
                    update_interval_secs: None,
                    cut: 0,
                })
                .collect(),
        }
    }

    fn snapshot(nodes: Vec<Node>) -> Msg {
        snapshot_at(nodes, Instant::now())
    }

    fn snapshot_at(nodes: Vec<Node>, at: Instant) -> Msg {
        Msg::Snapshot(Box::new(Snapshot {
            status: status(&["main", "backup"]),
            nodes: Nodes {
                selected: None,
                nodes,
            },
            at,
        }))
    }

    /// Один узел, он выбран, со счётчиками трафика.
    fn metered(name: &str, uplink: u64, downlink: u64) -> Vec<Node> {
        let mut current = node("main", name, NodeStatus::Alive);
        current.selected = true;
        current.uplink_bytes = Some(uplink);
        current.downlink_bytes = Some(downlink);
        vec![current]
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

    /// Приложение на вкладке «Узлы».
    fn nodes_app(nodes: Vec<Node>) -> App {
        let mut app = app_with(nodes);
        nodes_tab(&mut app);
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
        let mut app = nodes_app(sample());
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
        let mut app = nodes_app(many(30));
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
    fn the_wheel_moves_the_node_cursor_and_the_journal() {
        let mut app = nodes_app(many(30));
        let wheel = |kind| MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let at = Instant::now();
        app.handle_mouse(wheel(MouseEventKind::ScrollDown), at);
        assert_eq!(app.cursor, 3);
        app.handle_mouse(wheel(MouseEventKind::ScrollUp), at);
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::Char('4'));
        app.handle_mouse(wheel(MouseEventKind::ScrollUp), at);
        assert_eq!(app.journal_back, WHEEL_ROWS);
    }

    #[test]
    fn a_taller_screen_pulls_the_window_back() {
        let mut app = nodes_app(many(30));
        app.set_rows(5);
        press(&mut app, KeyCode::End);
        assert_eq!(app.offset, 25);
        app.set_rows(20);
        assert_eq!(app.offset, 10);
    }

    #[test]
    fn the_cursor_follows_its_node_when_the_list_changes() {
        let mut app = nodes_app(sample());
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
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::End);
        app.apply(snapshot(sample().into_iter().take(3).collect()));
        assert_eq!(app.cursor, 2);
        app.apply(snapshot(Vec::new()));
        assert_eq!(app.cursor, 0);
        assert!(app.current().is_none());
        assert_eq!(press(&mut app, KeyCode::Char('p')), None);
        assert!(app.notice.is_some());
    }

    #[test]
    fn the_text_filter_matches_the_id_in_any_case() {
        let mut app = nodes_app(sample());
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
        let mut app = nodes_app(sample());
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
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "quaUa?");
        assert!(!app.quit);
        assert_eq!(app.filter.text, "quaUa?");
        assert_eq!(app.visible, Vec::<usize>::new());
        assert_eq!(press(&mut app, KeyCode::Enter), None);
    }

    #[test]
    fn ctrl_u_clears_the_filter_line_and_the_length_is_limited() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "abc");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.filter.text, "");
        typed(&mut app, &"x".repeat(FILTER_MAX + 20));
        assert_eq!(app.filter.text.chars().count(), FILTER_MAX);
    }

    #[test]
    fn s_cycles_through_subscriptions_and_back_to_all() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.filter.subscription.as_deref(), Some("main"));
        assert_eq!(visible_ids(&app).len(), 3);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.filter.subscription.as_deref(), Some("backup"));
        assert_eq!(visible_ids(&app).len(), 2);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.filter.subscription, None);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter.subscription, None);
        assert_eq!(visible_ids(&app).len(), 5);
    }

    #[test]
    fn subscription_filter_and_text_filter_combine() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "nl");
        assert_eq!(visible_ids(&app), ["main/NL-1"]);
    }

    #[test]
    fn d_hides_dead_nodes_but_keeps_the_selected_one() {
        let mut nodes = sample();
        nodes[2].selected = true;
        let mut app = nodes_app(nodes);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.filter.hide_dead);
        assert!(!visible_ids(&app).contains(&"main/Германия 3") || app.nodes[2].selected);
        assert_eq!(visible_ids(&app).len(), 5);
        let mut plain = nodes_app(sample());
        press(&mut plain, KeyCode::Char('d'));
        assert_eq!(
            visible_ids(&plain),
            ["main/NL-1", "main/DE-2", "backup/NL-1", "backup/FI-5"]
        );
        press(&mut plain, KeyCode::Char('d'));
        assert_eq!(visible_ids(&plain).len(), 5);
    }

    #[test]
    fn enter_opens_details_and_escape_closes_them_first() {
        let mut app = nodes_app(sample());
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(app.details);
        press(&mut app, KeyCode::Esc);
        assert!(!app.details);
        assert!(!app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn enter_pins_with_p_and_a_unpins() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
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
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "fi-");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Some(Effect::Pin("backup/FI-5".to_owned()))
        );
    }

    #[test]
    fn a_double_click_on_a_node_pins_it_and_one_click_only_selects() {
        let mut app = nodes_app(sample());
        app.hits.push(Hit::new(0, 4, 80, Target::NodeRow(1)));
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let t0 = Instant::now();
        assert_eq!(app.handle_mouse(click(4), t0), None);
        assert_eq!(app.cursor, 1);
        assert_eq!(
            app.handle_mouse(click(4), t0 + Duration::from_millis(200)),
            Some(Effect::Pin("main/DE-2".to_owned()))
        );
    }

    #[test]
    fn a_click_on_a_tab_switches_to_it() {
        let mut app = app_with(sample());
        app.hits.push(Hit::new(10, 0, 8, Target::Tab(Tab::Journal)));
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click, Instant::now());
        assert_eq!(app.tab, Tab::Journal);
    }

    #[test]
    fn tabs_switch_with_digits_tab_and_arrows() {
        let mut app = app_with(sample());
        assert_eq!(app.tab, Tab::Overview);
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(app.tab, Tab::Settings);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, Tab::Overview);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.tab, Tab::Settings);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.tab, Tab::Journal);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.tab, Tab::Settings);
    }

    #[test]
    fn update_keys_ask_for_all_or_for_the_chosen_subscription() {
        let mut app = nodes_app(sample());
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            Some(Effect::Update(None))
        );
        assert!(app.update.is_some());
        app.apply(Msg::Updated(Ok(Updates {
            results: Vec::new(),
        })));
        assert!(app.update.is_none());
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            Some(Effect::Update(Some("backup".to_owned())))
        );
    }

    #[test]
    fn u_without_subscriptions_says_so() {
        let mut app = App::new(NOW);
        press(&mut app, KeyCode::Char('3'));
        assert_eq!(press(&mut app, KeyCode::Char('u')), None);
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Нет подписки для обновления"
        );
    }

    #[test]
    fn a_second_update_waits_for_the_first() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(press(&mut app, KeyCode::Char('u')), None);
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
    fn a_failed_settings_save_is_reported() {
        let mut app = App::new(NOW);
        app.apply(Msg::Saved(Err("нет каталога\nдетали".to_owned())));
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Не удалось сохранить вид: нет каталога"
        );
    }

    #[test]
    fn the_names_setting_flips_and_asks_to_be_saved() {
        let mut app = App::new(NOW);
        press(&mut app, KeyCode::Char('5'));
        app.setting_cursor = Setting::Names.row();
        assert_eq!(app.names, NameStyle::Country);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::SavePrefs(Prefs {
                names: NameStyle::AsIs,
                hidden: Vec::new(),
            }))
        );
        assert_eq!(app.names, NameStyle::AsIs);
        assert_eq!(
            app.notice.as_ref().unwrap().text,
            "Значки в именах: как есть"
        );
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
    fn the_log_is_a_ring_buffer_and_the_warnings_filter_keeps_them() {
        let mut app = App::new(NOW);
        for index in 0..LOG_CAP + 25 {
            app.apply(Msg::Event(Event::Warning {
                level: "warn".to_owned(),
                message: format!("запись {index}"),
            }));
        }
        assert_eq!(app.log.len(), LOG_CAP);
        assert!(app.log.front().unwrap().text.ends_with("запись 25"));
        app.apply(Msg::Event(Event::Hello {
            version: "0.1.0".to_owned(),
        }));
        assert_eq!(app.journal_entries().len(), LOG_CAP);
        app.journal_warnings = true;
        assert_eq!(app.journal_entries().len(), LOG_CAP - 1);
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
    fn speed_needs_two_snapshots_and_counts_bits_per_second() {
        let t0 = Instant::now();
        let mut app = App::new(NOW);
        app.apply(snapshot_at(metered("NL-1", 1_000, 2_000), t0));
        assert_eq!(app.speed(), None);
        app.apply(snapshot_at(
            metered("NL-1", 26_000, 252_000),
            t0 + Duration::from_secs(2),
        ));
        assert_eq!(
            app.speed(),
            Some(Speed {
                down: 1_000_000,
                up: 100_000,
            })
        );
    }

    #[test]
    fn speed_is_averaged_with_the_previous_one() {
        let t0 = Instant::now();
        let mut app = App::new(NOW);
        app.apply(snapshot_at(metered("NL-1", 1_000, 2_000), t0));
        app.apply(snapshot_at(
            metered("NL-1", 26_000, 252_000),
            t0 + Duration::from_secs(2),
        ));
        app.apply(snapshot_at(
            metered("NL-1", 26_000, 252_000),
            t0 + Duration::from_secs(4),
        ));
        assert_eq!(
            app.speed(),
            Some(Speed {
                down: 500_000,
                up: 50_000,
            })
        );
    }

    #[test]
    fn a_new_node_or_a_smaller_counter_starts_the_speed_over() {
        let t0 = Instant::now();
        let mut app = App::new(NOW);
        app.apply(snapshot_at(metered("NL-1", 1_000, 2_000), t0));
        app.apply(snapshot_at(
            metered("NL-1", 26_000, 252_000),
            t0 + Duration::from_secs(2),
        ));
        assert!(app.speed().is_some());
        app.apply(snapshot_at(
            metered("DE-2", 26_500, 252_500),
            t0 + Duration::from_secs(3),
        ));
        assert_eq!(app.speed(), None);
        app.apply(snapshot_at(
            metered("DE-2", 27_000, 253_000),
            t0 + Duration::from_secs(5),
        ));
        assert!(app.speed().is_some());
        app.apply(snapshot_at(
            metered("DE-2", 500, 1_000),
            t0 + Duration::from_secs(6),
        ));
        assert_eq!(app.speed(), None);
    }

    #[test]
    fn snapshots_at_the_same_moment_and_a_lost_daemon_leave_no_speed() {
        let t0 = Instant::now();
        let mut app = App::new(NOW);
        app.apply(snapshot_at(metered("NL-1", 1_000, 2_000), t0));
        app.apply(snapshot_at(metered("NL-1", 26_000, 252_000), t0));
        assert_eq!(app.speed(), None);
        app.apply(snapshot_at(
            metered("NL-1", 26_000, 252_000),
            t0 + Duration::from_secs(2),
        ));
        assert!(app.speed().is_some());
        app.apply(Msg::Down {
            reason: "обрыв связи".to_owned(),
            retry_in: Duration::from_secs(1),
        });
        assert_eq!(app.speed(), None);
    }

    #[test]
    fn sorting_by_latency_puts_dead_and_unchecked_nodes_last() {
        let mut nodes = sample();
        nodes[0].latency_ms = Some(90);
        nodes[1].latency_ms = Some(20);
        let mut app = nodes_app(nodes);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.filter.sort, SortBy::Latency);
        assert_eq!(
            visible_ids(&app),
            [
                "main/DE-2",
                "backup/FI-5",
                "main/NL-1",
                "main/Германия 3",
                "backup/NL-1"
            ]
        );
    }

    #[test]
    fn sorting_by_name_and_a_header_click_reverses_it() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.filter.sort, SortBy::Name);
        assert_eq!(
            visible_ids(&app),
            [
                "main/DE-2",
                "backup/FI-5",
                "main/NL-1",
                "backup/NL-1",
                "main/Германия 3"
            ]
        );
        app.click(Target::Sort(SortBy::Name), Instant::now());
        assert!(app.filter.descending);
        assert_eq!(visible_ids(&app)[0], "main/Германия 3");
        app.click(Target::Sort(SortBy::Status), Instant::now());
        assert_eq!(
            (app.filter.sort, app.filter.descending),
            (SortBy::Status, false)
        );
    }

    #[test]
    fn masks_hide_nodes_by_their_safe_name_and_are_saved() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.input, InputMode::Masks);
        typed(&mut app, "FI*, Германия*");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::SavePrefs(Prefs {
                names: NameStyle::Country,
                hidden: vec!["FI*".to_owned(), "Германия*".to_owned()],
            }))
        );
        assert_eq!(app.input, InputMode::Normal);
        assert_eq!(visible_ids(&app), ["main/NL-1", "main/DE-2", "backup/NL-1"]);
    }

    #[test]
    fn a_mask_hides_a_flagged_name_by_its_text() {
        let mut nodes = sample();
        nodes[1].name = "🇩🇪 Обход 1".to_owned();
        let mut app = nodes_app(nodes);
        press(&mut app, KeyCode::Char('h'));
        typed(&mut app, "Обход*");
        press(&mut app, KeyCode::Enter);
        let ids = visible_ids(&app);
        assert!(!ids.contains(&"main/DE-2"), "{ids:?}");
        assert_eq!(ids.len(), 4, "{ids:?}");
    }

    #[test]
    fn a_speed_test_runs_once_and_escape_cancels_it() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Down);
        assert_eq!(
            press(&mut app, KeyCode::Char('t')),
            Some(Effect::SpeedTest(Some("main/DE-2".to_owned())))
        );
        assert!(app.speed.is_some());
        assert_eq!(press(&mut app, KeyCode::Char('t')), None);
        assert!(app.notice.as_ref().unwrap().text.contains("уже идёт"));
        assert_eq!(press(&mut app, KeyCode::Esc), Some(Effect::CancelSpeedTest));
        assert!(app.speed.is_none());
        assert!(!app.quit);
    }

    #[test]
    fn a_speed_result_is_summarized_in_the_journal() {
        let mut app = app_with(sample());
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Effect::SpeedTest(None))
        );
        app.apply(Msg::SpeedProgress(Some(SpeedtestProgress {
            run: 1,
            runs: 2,
            percent: 40,
        })));
        let progress = app.speed.as_ref().unwrap().progress.as_ref().unwrap();
        assert_eq!(progress.percent, 40);
        app.apply(Msg::SpeedDone(Ok(SpeedtestResult {
            node: "main/NL-1".to_owned(),
            url: "https://speed.example.com/".to_owned(),
            runs: vec![SpeedtestRun {
                streams: 1,
                bytes: 1_000,
                seconds: 1.0,
                mbps: 12.5,
                ttfb_ms: 31,
            }],
            hint: None,
        })));
        assert!(app.speed.is_none());
        assert_eq!(
            app.log.back().unwrap().text,
            "Тест скорости «main/NL-1»: ↓ 12.5 Мбит/с, задержка 31 мс"
        );
    }

    #[test]
    fn x_excludes_the_node_only_after_a_choice_and_enter() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('x'));
        let dialog = app.exclude.as_ref().unwrap();
        assert!(!dialog.remove);
        assert_eq!(dialog.choices, ["NL-1", "NL-1*"]);
        press(&mut app, KeyCode::Right);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Check(deny_edit(
                "main",
                &["NL-1*"],
                "«main/NL-1» исключён маской «NL-1*»",
            )))
        );
        assert!(app.exclude.is_none());
        assert!(app.saving.is_some());
    }

    #[test]
    fn x_on_an_excluded_node_offers_to_remove_the_mask_that_excludes_it() {
        let mut app = nodes_app(sample());
        app.deny = vec![("main".to_owned(), vec!["NL*".to_owned()])];
        press(&mut app, KeyCode::Char('x'));
        let dialog = app.exclude.as_ref().unwrap();
        assert!(dialog.remove);
        assert_eq!(dialog.choices, ["NL*"]);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Check(deny_edit(
                "main",
                &[],
                "Исключение «main/NL-1» снято"
            )))
        );
    }

    #[test]
    fn e_hides_the_excluded_nodes() {
        let mut app = nodes_app(sample());
        app.deny = vec![("main".to_owned(), vec!["DE*".to_owned()])];
        app.refilter();
        assert!(app.is_excluded(&app.nodes[1]));
        press(&mut app, KeyCode::Char('e'));
        assert!(!visible_ids(&app).contains(&"main/DE-2"));
        press(&mut app, KeyCode::Char('e'));
        assert!(visible_ids(&app).contains(&"main/DE-2"));
    }

    #[test]
    fn the_settings_answer_fills_the_exclusions() {
        let mut app = App::new(NOW);
        let view = raycat_proto::SettingsView {
            items: vec![raycat_proto::SettingItem {
                table: None,
                subscription: Some("main".to_owned()),
                key: "deny".to_owned(),
                value: raycat_proto::SettingValue::Texts(vec!["DE*".to_owned()]),
                source: "из файла".to_owned(),
            }],
        };
        app.apply(Msg::Settings(Ok(view)));
        assert_eq!(app.deny, [("main".to_owned(), vec!["DE*".to_owned()])]);
    }

    fn text(value: &str) -> SettingValue {
        SettingValue::Text(value.to_owned())
    }

    fn change(table: &str, key: &str, value: SettingValue) -> SettingChange {
        SettingChange {
            table: Some(table.to_owned()),
            subscription: None,
            key: key.to_owned(),
            value,
        }
    }

    fn item_of(table: &str, key: &str, value: SettingValue, source: &str) -> SettingItem {
        SettingItem {
            table: Some(table.to_owned()),
            subscription: None,
            key: key.to_owned(),
            value,
            source: source.to_owned(),
        }
    }

    /// Ответ `GET /v1/settings`: без `kill_switch` демон в режиме прокси.
    fn settings_view(kill_switch: bool) -> SettingsView {
        let mut items = vec![
            item_of(
                "routing",
                "ru_direct",
                SettingValue::Bool(false),
                "по умолчанию",
            ),
            item_of("selection", "check_interval", text("30s"), "по умолчанию"),
            item_of("selection", "failures", SettingValue::Number(3), "из файла"),
            item_of("selection", "switch_gain", text("150ms"), "по умолчанию"),
            item_of("selection", "return_delay", text("5m"), "по умолчанию"),
            item_of(
                "xray",
                "xhttp_connections",
                text(XHTTP_PROVIDER),
                "по умолчанию",
            ),
            item_of("log", "level", text("info"), "из файла"),
        ];
        if kill_switch {
            items.push(item_of(
                "mode",
                "kill_switch",
                SettingValue::Bool(true),
                "по умолчанию",
            ));
        }
        SettingsView { items }
    }

    fn reloaded(xray_restart: bool) -> Reloaded {
        Reloaded {
            changed: true,
            xray_restart,
            restart_required: Vec::new(),
        }
    }

    fn deny_edit(subscription: &str, masks: &[&str], done: &str) -> Edit {
        Edit {
            changes: vec![SettingChange {
                table: None,
                subscription: Some(subscription.to_owned()),
                key: "deny".to_owned(),
                value: SettingValue::Texts(masks.iter().map(|mask| (*mask).to_owned()).collect()),
            }],
            done: done.to_owned(),
            row: None,
            warn: None,
        }
    }

    /// Вкладка «Настройки» с ответом демона.
    fn settings_app(kill_switch: bool) -> App {
        let mut app = App::new(NOW);
        app.apply(Msg::Settings(Ok(settings_view(kill_switch))));
        press(&mut app, KeyCode::Char('5'));
        app
    }

    fn checked_edit(effect: Option<Effect>) -> Edit {
        match effect {
            Some(Effect::Check(edit)) => edit,
            other => panic!("ожидалась проверка изменения, пришло {other:?}"),
        }
    }

    #[test]
    fn a_switch_is_checked_and_saved_when_nothing_needs_a_question() {
        let mut app = settings_app(true);
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        assert_eq!(
            edit.changes,
            [change("routing", "ru_direct", SettingValue::Bool(true))]
        );
        assert_eq!(edit.row, Some(Setting::RuDirect.row()));
        assert_eq!(edit.done, "Россия напрямую: вкл");
        let saved = app.apply(Msg::Checked {
            edit: edit.clone(),
            result: Ok(reloaded(false)),
        });
        assert_eq!(saved, Some(Effect::Apply(edit)));
        assert!(app.confirm.is_none());
        assert!(app.saving.is_some());
    }

    #[test]
    fn switching_the_kill_switch_off_asks_and_enter_saves_it() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::KillSwitch.row();
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let asked = app.apply(Msg::Checked {
            edit: edit.clone(),
            result: Ok(reloaded(false)),
        });
        assert!(asked.is_none());
        let confirm = app.confirm.as_ref().unwrap();
        assert!(
            confirm.question.contains("трафик пойдёт мимо VPN"),
            "{}",
            confirm.question
        );
        assert!(app.saving.is_none());
        assert_eq!(press(&mut app, KeyCode::Char('q')), None);
        assert!(!app.quit, "пока ждут ответа, q не выходит");
        assert_eq!(press(&mut app, KeyCode::Enter), Some(Effect::Apply(edit)));
        assert!(app.confirm.is_none());
        assert!(app.saving.is_some());
    }

    #[test]
    fn a_restart_of_xray_is_asked_and_escape_cancels_it() {
        let mut app = settings_app(true);
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let asked = app.apply(Msg::Checked {
            edit,
            result: Ok(reloaded(true)),
        });
        assert!(asked.is_none());
        let question = app.confirm.as_ref().map(|confirm| confirm.question.clone());
        assert!(question.is_some_and(|text| text.contains("связь прервётся")));
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert!(app.confirm.is_none());
        assert!(app.saving.is_none());
        assert!(app.notice.as_ref().unwrap().text.contains("отменено"));
    }

    #[test]
    fn a_number_field_refuses_text_and_then_checks_the_number() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::Failures.row();
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.input, InputMode::Field);
        assert_eq!(app.field, "3");
        press(&mut app, KeyCode::Backspace);
        typed(&mut app, "x");
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.input, InputMode::Field, "поле остаётся открытым");
        let error = app.setting_error.clone().unwrap();
        assert_eq!(error.0, Setting::Failures.row());
        assert!(error.1.contains("целое число"), "{}", error.1);
        press(&mut app, KeyCode::Backspace);
        typed(&mut app, "5");
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        assert_eq!(
            edit.changes,
            [change("selection", "failures", SettingValue::Number(5))]
        );
        assert_eq!(app.input, InputMode::Normal);
    }

    #[test]
    fn escape_leaves_the_field_and_keeps_the_value() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::CheckInterval.row();
        press(&mut app, KeyCode::Enter);
        typed(&mut app, "9");
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.input, InputMode::Normal);
        assert_eq!(app.shown(Setting::CheckInterval).value, "30s");
        assert!(app.saving.is_none());
    }

    #[test]
    fn the_connections_field_takes_a_number_or_the_provider_choice() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::Xhttp.row();
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert_eq!(app.field, "как у провайдера");
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        assert_eq!(
            edit.changes,
            [change("xray", "xhttp_connections", text(XHTTP_PROVIDER))]
        );
        app.apply(Msg::Checked {
            edit,
            result: Err("тест".to_owned()),
        });
        press(&mut app, KeyCode::Enter);
        app.field = String::from("4");
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        assert_eq!(
            edit.changes,
            [change("xray", "xhttp_connections", SettingValue::Number(4))]
        );
    }

    #[test]
    fn the_log_level_steps_through_four_levels_and_a_click_picks_one() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::LogLevel.row();
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        assert_eq!(edit.changes, [change("log", "level", text("debug"))]);
        app.saving = None;
        let picked = app.click(Target::SettingLevel("warn"), Instant::now());
        let edit = checked_edit(picked);
        assert_eq!(edit.changes, [change("log", "level", text("warn"))]);
    }

    #[test]
    fn a_saved_setting_updates_the_form_and_says_what_happened() {
        let mut app = settings_app(true);
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let mut view = settings_view(true);
        view.items[0].value = SettingValue::Bool(true);
        let reloaded = Reloaded {
            changed: true,
            xray_restart: true,
            restart_required: vec!["mode.listen".to_owned()],
        };
        app.apply(Msg::Applied {
            edit,
            result: Ok((reloaded, view)),
        });
        assert!(app.saving.is_none());
        assert_eq!(app.shown(Setting::RuDirect).value, "вкл");
        let notice = app.notice.as_ref().unwrap();
        assert!(
            notice.text.starts_with("Россия напрямую: вкл"),
            "{}",
            notice.text
        );
        assert!(notice.text.contains("связь прервалась"), "{}", notice.text);
        assert!(notice.text.contains("mode.listen"), "{}", notice.text);
        assert_eq!(notice.tone, Tone::Yellow);
    }

    #[test]
    fn a_change_saved_but_not_applied_is_said_at_its_row() {
        let mut app = settings_app(true);
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let failure = "сохранено, но не применено: плохо\nподробности".to_owned();
        app.apply(Msg::Applied {
            edit,
            result: Err(failure),
        });
        assert!(app.saving.is_none());
        let (row, text) = app.setting_error.clone().unwrap();
        assert_eq!(row, Setting::RuDirect.row());
        assert_eq!(text, "сохранено, но не применено: плохо — подробности");
        let notice = app.notice.as_ref().unwrap();
        assert!(notice.text.starts_with("сохранено, но не применено: плохо"));
        assert_eq!(notice.tone, Tone::Red);
    }

    #[test]
    fn a_refused_check_is_shown_and_the_next_try_works() {
        let mut app = settings_app(true);
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let reason = "файл настроек только для чтения: /etc/raycat/config.toml".to_owned();
        app.apply(Msg::Checked {
            edit,
            result: Err(reason),
        });
        assert!(app.saving.is_none());
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .text
                .contains("только для чтения")
        );
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Check(_))
        ));
    }

    #[test]
    fn a_second_change_waits_while_the_first_one_is_saved() {
        let mut app = settings_app(true);
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Effect::Check(_))
        ));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .text
                .contains("уже сохраняются")
        );
    }

    #[test]
    fn without_the_daemon_or_outside_the_gateway_a_row_says_why() {
        let mut app = App::new(NOW);
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(press(&mut app, KeyCode::Enter), None);
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .text
                .contains("ещё не получены")
        );
        assert_eq!(app.shown(Setting::RuDirect).source, "нет данных");

        let mut proxy = settings_app(false);
        proxy.setting_cursor = Setting::KillSwitch.row();
        assert_eq!(press(&mut proxy, KeyCode::Enter), None);
        let text = &proxy.notice.as_ref().unwrap().text;
        assert!(text.contains("только в режиме шлюза"), "{text}");
        assert!(!proxy.shown(Setting::KillSwitch).enabled);
    }

    #[test]
    fn the_names_row_saves_only_the_view_preference() {
        let mut app = settings_app(true);
        app.setting_cursor = Setting::Names.row();
        assert!(matches!(
            press(&mut app, KeyCode::Char(' ')),
            Some(Effect::SavePrefs(_))
        ));
        assert_eq!(app.shown(Setting::Names).source, "вид TUI");
    }

    #[test]
    fn a_short_screen_scrolls_the_settings_to_the_cursor() {
        let mut app = settings_app(true);
        press(&mut app, KeyCode::End);
        assert_eq!(app.setting_cursor, Setting::ALL.len() - 1);
        app.keep_setting_visible(3);
        assert_eq!(app.setting_offset, 6);
        press(&mut app, KeyCode::Home);
        app.keep_setting_visible(3);
        assert_eq!(app.setting_offset, 0);
    }

    #[test]
    fn an_exclusion_that_restarts_xray_is_asked_before_it_is_saved() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('x'));
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let asked = app.apply(Msg::Checked {
            edit,
            result: Ok(reloaded(true)),
        });
        assert!(asked.is_none());
        assert!(app.confirm.is_some());
        let applied = press(&mut app, KeyCode::Enter);
        let expected = deny_edit("main", &["NL-1"], "«main/NL-1» исключён маской «NL-1»");
        assert_eq!(applied, Some(Effect::Apply(expected)));
    }

    #[test]
    fn an_exclusion_without_a_restart_is_saved_at_once_and_its_mask_applies() {
        let mut app = nodes_app(sample());
        press(&mut app, KeyCode::Char('x'));
        let edit = checked_edit(press(&mut app, KeyCode::Enter));
        let saved = app.apply(Msg::Checked {
            edit: edit.clone(),
            result: Ok(reloaded(false)),
        });
        assert_eq!(saved, Some(Effect::Apply(edit.clone())));
        let view = SettingsView {
            items: vec![SettingItem {
                table: None,
                subscription: Some("main".to_owned()),
                key: "deny".to_owned(),
                value: SettingValue::Texts(vec!["NL-1".to_owned()]),
                source: "из файла".to_owned(),
            }],
        };
        app.apply(Msg::Applied {
            edit,
            result: Ok((reloaded(false), view)),
        });
        assert_eq!(app.deny, [("main".to_owned(), vec!["NL-1".to_owned()])]);
        assert!(app.saving.is_none());
    }

    #[test]
    fn m_edits_the_masks_of_the_selected_subscription_and_tab_switches_the_list() {
        let mut app = app_with(sample());
        app.sub_lists = vec![("main".to_owned(), ListKind::Allow, vec!["DE*".to_owned()])];
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.input, InputMode::SubMasks);
        let edit = app.sub_edit.as_ref().unwrap();
        assert_eq!(edit.subscription, "main");
        assert_eq!(edit.kind, ListKind::Allow);
        assert_eq!(edit.text, "DE*");
        press(&mut app, KeyCode::Tab);
        let edit = app.sub_edit.as_ref().unwrap();
        assert_eq!(edit.kind, ListKind::Deny);
        assert_eq!(edit.text, "");
    }

    #[test]
    fn enter_checks_the_masks_and_escape_drops_them() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('m'));
        typed(&mut app, "NL*");
        let Some(Effect::Check(edit)) = press(&mut app, KeyCode::Enter) else {
            panic!("ожидалась проверка масок");
        };
        let expected = SettingChange {
            table: None,
            subscription: Some("main".to_owned()),
            key: "allow".to_owned(),
            value: SettingValue::Texts(vec!["NL*".to_owned()]),
        };
        assert_eq!(edit.changes, [expected]);
        assert_eq!(app.input, InputMode::Normal);
        assert!(app.sub_edit.is_none());

        app.saving = None;
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(press(&mut app, KeyCode::Esc), None);
        assert_eq!(app.input, InputMode::Normal);
        assert!(app.notice.as_ref().unwrap().text.contains("не изменены"));
    }

    #[test]
    fn the_brackets_move_a_subscription_one_place_and_say_when_it_cannot() {
        let mut app = app_with(sample());
        press(&mut app, KeyCode::Char('3'));
        assert_eq!(press(&mut app, KeyCode::Char('[')), None);
        assert!(app.notice.as_ref().unwrap().text.contains("уже первая"));
        let Some(Effect::Check(edit)) = press(&mut app, KeyCode::Char(']')) else {
            panic!("ожидалась проверка перестановки");
        };
        let expected = SettingChange {
            table: None,
            subscription: Some("main".to_owned()),
            key: "position".to_owned(),
            value: SettingValue::Number(1),
        };
        assert_eq!(edit.changes, [expected]);
        assert_eq!(app.sub_cursor, 1);
    }

    #[test]
    fn entering_the_settings_tab_asks_the_daemon_for_fresh_values() {
        let mut app = App::new(NOW);
        assert_eq!(press(&mut app, KeyCode::Char('5')), Some(Effect::LoadSettings));
        assert_eq!(press(&mut app, KeyCode::Char('5')), None);
        assert_eq!(press(&mut app, KeyCode::Char('3')), None);
        assert_eq!(press(&mut app, KeyCode::Char('5')), Some(Effect::LoadSettings));
    }

    #[test]
    fn subscription_names_come_from_the_status_first() {
        let mut app = app_with(sample());
        assert_eq!(app.subscription_names(), ["main", "backup"]);
        app.status = None;
        assert_eq!(app.subscription_names(), ["main", "backup"]);
    }
}
