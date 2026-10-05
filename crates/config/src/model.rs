use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use crate::link;
use crate::pattern::Pattern;

pub const DEFAULT_UPDATE_INTERVAL: Duration = Duration::from_secs(12 * 3_600);

/// Строка, которая не попадает в `Debug`: ссылка подписки, seed, machine-id.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub device: Device,
    /// В порядке приоритета: первая — основная.
    pub subscriptions: Vec<Subscription>,
    pub selection: Selection,
    pub mode: Mode,
    pub dns: Dns,
    pub routing: Routing,
    pub xray: Xray,
    pub log: Logging,
}

/// Что сообщается провайдеру об устройстве; `None` — решает демон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub seed: Option<Secret>,
    pub machine_id: Option<Secret>,
    pub hostname: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Happ,
    Incy,
}

impl App {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Happ => "happ",
            Self::Incy => "incy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Android,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Android => "android",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub name: String,
    pub url: Secret,
    pub app: App,
    pub platform: Platform,
    /// Своё устройство для этой подписки вместо общего.
    pub seed: Option<Secret>,
    /// `None` — интервал из ответа провайдера, иначе [`DEFAULT_UPDATE_INTERVAL`].
    pub update_interval: Option<Duration>,
    pub allow: Vec<Pattern>,
    pub deny: Vec<Pattern>,
    /// Предпочтительные узлы по порядку.
    pub priority: Vec<Pattern>,
}

impl Subscription {
    /// Ссылка, безопасная для логов: `https://хост/…abcd`.
    pub fn masked_url(&self) -> String {
        link::mask(self.url.expose())
    }

    /// Узел участвует в выборе: он в белом списке (если тот не пуст) и не в чёрном.
    pub fn allows(&self, node: &str) -> bool {
        let allowed = self.allow.is_empty() || self.allow.iter().any(|p| p.matches(node));
        allowed && !self.deny.iter().any(|p| p.matches(node))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub check_url: String,
    pub check_interval: Duration,
    pub failures: u32,
    pub switch_gain: Duration,
    pub return_delay: Duration,
    pub pin: Option<Pin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub subscription: String,
    pub node: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Proxy { listen: SocketAddr },
    Gateway { kill_switch: bool, lan: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dns {
    pub resolvers: Vec<IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routing {
    /// Применять маршрутизацию, которую присылает провайдер.
    pub provider: bool,
}

/// Алгоритм управления перегрузкой TCP для исходящих соединений к узлам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TcpCongestion {
    /// Демон включает BBR, только если хост это гарантированно позволяет.
    Auto,
    Off,
    /// Имя алгоритма ядра (`bbr`, `cubic`, …); ставится без проверки возможностей хоста.
    Algorithm(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xray {
    pub path: PathBuf,
    /// В байтах.
    pub memory_limit: u64,
    pub tcp_congestion: TcpCongestion,
    /// Число параллельных соединений XHTTP; `None` — как у провайдера.
    pub xhttp_connections: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logging {
    pub level: LogLevel,
}
