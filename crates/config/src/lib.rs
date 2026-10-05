//! Настройки raycat: модель, загрузка из TOML, переопределения из окружения, проверка.

mod envvars;
mod error;
mod link;
mod load;
mod model;
mod pattern;
mod raw;
mod units;
mod validate;

pub use error::{Error, Problem};
pub use load::Env;
pub use model::{
    App, Config, DEFAULT_UPDATE_INTERVAL, Device, Dns, Lan, LogLevel, Logging, Mode, Pin,
    Platform, Routing, Secret, Selection, Subscription, TcpCongestion, Xray,
};
pub use pattern::Pattern;
