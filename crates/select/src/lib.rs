//! Выбор узла: чистая логика без ввода-вывода и часов.
//!
//! Демон раз в `check_interval` передаёт [`Selector::step`] свежие данные о здоровье
//! узлов и текущее время, получает [`Decision`] и закрепляет выбор в xray.

mod model;
mod reason;
mod selector;
mod state;

pub use model::{Candidate, Decision, Health, NodeInfo, PinTarget, Settings, Snapshot, Status};
pub use reason::{Reason, Warning};
pub use selector::Selector;
