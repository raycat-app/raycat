//! Что демон делает с сетевым стеком Linux в режиме шлюза.
//!
//! Схема (подробнее — в [`ruleset`]): исходящие tcp и udp помечаются в `output`,
//! ядро заново выбирает маршрут и по политике `fwmark → таблица` отправляет пакет
//! на loopback, где правило `prerouting` отдаёт его прозрачному сокету xray
//! (TPROXY). Собственные сокеты xray и демона несут другую метку и идут мимо.
//! Если xray не слушает, помеченный пакет всё равно попадает на loopback и получает
//! отказ от локального стека: наружу он не выходит. Kill switch дополнительно не
//! выпускает то, что перехват не затрагивает (ICMP, другие протоколы, IPv6).
//!
//! Шлюз для локальной сети ([`Lan`]) работает так же: пакеты устройств помечаются
//! в `prerouting` и там же достаются прозрачному сокету. Они доставляются локально,
//! поэтому пересылка (`ip_forward`) не нужна; при kill switch то, что всё же дошло
//! до `forward`, отбрасывается.
//!
//! Не перехватывается ICMP: без kill switch `ping` идёт напрямую.

mod cidr;
mod dns;
mod exec;
mod install;
mod lan;
mod rules;
mod ruleset;

pub use cidr::{Cidr, CidrError};
pub use dns::{dns_leak, dns_leak_message, docker_dns_via_host};
pub use install::{install, is_installed, remove};
pub use lan::resolve_lan;
pub use rules::{
    DEFAULT_BYPASS, DEFAULT_INTERCEPT_MARK, DEFAULT_OWN_MARK, DEFAULT_ROUTE_TABLE,
    DEFAULT_RULE_PRIORITY, DEFAULT_TPROXY_PORT, Lan, MAX_LAN_SUBNETS, Rules,
    interface_name_problem, lan_subnet_problem,
};
pub use ruleset::ruleset;
