use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// Как приложения попадают в xray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Один inbound `mixed` (HTTP и SOCKS на одном порту) для контейнеров и LAN.
    /// С `auth` вход возможен только по логину и паролю.
    Proxy {
        listen: SocketAddr,
        auth: Option<Credentials>,
    },
    /// Прозрачный перехват: inbound TPROXY и метка собственных сокетов xray,
    /// по которой правила перехвата пропускают его трафик.
    Gateway { tproxy_port: u16, mark: u32 },
}

/// Логин и пароль входа в прокси. В `Debug` пароль не выводится.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsSettings {
    /// Настоящие резолверы; первый отвечает за адреса самих серверов узлов.
    pub resolvers: Vec<IpAddr>,
    /// Пул поддельных адресов (fakedns) в виде CIDR.
    pub fake_ip_pool: String,
}

impl Default for DnsSettings {
    fn default() -> Self {
        Self {
            resolvers: vec![
                IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            ],
            fake_ip_pool: "198.18.0.0/15".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSettings {
    pub url: String,
    /// Округляется вниз до секунд, минимум одна секунда.
    pub interval: Duration,
}

impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            url: "https://www.gstatic.com/generate_204".to_owned(),
            interval: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub dns: DnsSettings,
    pub probe: ProbeSettings,
    /// Порт API xray; слушается только `127.0.0.1`.
    pub api_port: u16,
    /// Алгоритм управления перегрузкой для TCP-соединений к узлам. Компилятор не
    /// проверяет, поддерживает ли его хост: это решает вызывающий.
    pub tcp_congestion: Option<String>,
    /// Число параллельных соединений XHTTP для узлов, где провайдер не задал `xmux`.
    pub xhttp_connections: Option<u8>,
    /// Правила раздельной маршрутизации по порядку, первое подошедшее побеждает. Пусто —
    /// весь трафик идёт через узел.
    pub rules: Vec<Rule>,
}

/// Действие правила: напрямую, через выбранный узел или отбросить соединение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Direct,
    Proxy,
    Block,
}

/// Домены и подсети с одним действием: совпадение с любым из них.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub domains: Vec<Domain>,
    pub subnets: Vec<Subnet>,
    pub action: Action,
}

/// Как сопоставляется имя домена с адресом соединения и запросом DNS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainKind {
    /// Только это имя.
    Full,
    /// Это имя и все его поддомены.
    Subdomains,
    /// Имя входит подстрокой в домен.
    Keyword,
}

/// Имя в нижнем регистре и способ его сравнения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    pub name: String,
    pub kind: DomainKind,
}

/// Подсеть: адрес сети и длина префикса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subnet {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Settings {
    /// Настройки по умолчанию для DNS и проверки узлов; настроек производительности нет.
    pub fn new(mode: Mode, api_port: u16) -> Self {
        Self {
            mode,
            dns: DnsSettings::default(),
            probe: ProbeSettings::default(),
            api_port,
            tcp_congestion: None,
            xhttp_connections: None,
            rules: Vec::new(),
        }
    }
}
