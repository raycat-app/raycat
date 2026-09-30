use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Raw {
    pub(crate) device: RawDevice,
    pub(crate) subscription: Vec<RawSubscription>,
    pub(crate) selection: RawSelection,
    pub(crate) mode: RawMode,
    pub(crate) dns: RawDns,
    pub(crate) routing: RawRouting,
    pub(crate) xray: RawXray,
    pub(crate) log: RawLog,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawDevice {
    pub(crate) seed: Option<String>,
    pub(crate) machine_id: Option<String>,
    pub(crate) hostname: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) os_version: Option<String>,
    pub(crate) locale: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawSubscription {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) allow_http: Option<bool>,
    pub(crate) app: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) seed: Option<String>,
    pub(crate) update_interval: Option<String>,
    pub(crate) allow: Vec<String>,
    pub(crate) deny: Vec<String>,
    pub(crate) priority: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawSelection {
    pub(crate) check_url: Option<String>,
    pub(crate) check_interval: Option<String>,
    pub(crate) failures: Option<i64>,
    pub(crate) switch_gain: Option<String>,
    pub(crate) return_delay: Option<String>,
    pub(crate) pin: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawMode {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) listen: Option<String>,
    pub(crate) kill_switch: Option<bool>,
    pub(crate) lan: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawDns {
    pub(crate) resolvers: Option<Vec<String>>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawRouting {
    pub(crate) provider: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawXray {
    pub(crate) path: Option<String>,
    pub(crate) memory_limit: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RawLog {
    pub(crate) level: Option<String>,
}
