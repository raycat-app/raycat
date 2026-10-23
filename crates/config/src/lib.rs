//! Настройки raycat: модель, загрузка из TOML, переопределения из окружения, проверка.

mod envvars;
mod error;
mod link;
mod load;
mod model;
pub mod names;
mod pattern;
mod raw;
mod show;
mod suggest;
mod units;
mod validate;

pub use error::{Error, Problem};
pub use load::Env;
pub use model::{
    Action, App, Config, DEFAULT_UPDATE_INTERVAL, Device, Dns, DomainMatch, Lan, LogLevel, Logging,
    Mode, Pin, Platform, ProxyAuth, Routing, Rule, Secret, Selection, Subscription, TcpCongestion,
    Xray,
};
pub use pattern::Pattern;
pub use show::effective;

#[cfg(feature = "schema")]
#[doc(hidden)]
pub fn settings_schema() -> schemars::Schema {
    schemars::schema_for!(raw::Raw)
}
