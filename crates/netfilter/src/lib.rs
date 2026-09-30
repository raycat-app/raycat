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
//! Не перехватывается ICMP: без kill switch `ping` идёт напрямую. Пересылаемый
//! (`forward`) трафик тоже не перехватывается: шлюзом служит сетевое пространство
//! самого контейнера или хоста.

mod cidr;
mod dns;
mod exec;
mod install;
mod rules;
mod ruleset;

pub use cidr::{Cidr, CidrError};
pub use dns::{dns_leak, dns_leak_message, docker_dns_via_host};
pub use install::{install, remove};
pub use rules::{
    DEFAULT_BYPASS, DEFAULT_INTERCEPT_MARK, DEFAULT_OWN_MARK, DEFAULT_ROUTE_TABLE,
    DEFAULT_RULE_PRIORITY, DEFAULT_TPROXY_PORT, Rules,
};
pub use ruleset::ruleset;
