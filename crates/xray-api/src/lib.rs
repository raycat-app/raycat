mod client;
mod error;

#[allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    unreachable_pub,
    unused_qualifications
)]
mod pb {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

pub use client::{BalancerInfo, OutboundHealth, OutboundTraffic, XrayApi};
pub use error::ApiError;
