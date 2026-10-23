//! Перечитывание настроек без перезапуска raycat. Файл читается и проверяется целиком.
//! Ключи, которые применяются на лету, идут тем же путём, что обновление подписки: конфиг
//! xray собирается из кэша, и xray перезапускается, только если его конфиг изменился.
//! Остальные ключи демон держит прежними до перезапуска (см. `restart_keys`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use raycat_config::edit::{self, Change, Target};
use raycat_config::{Config, Env, LogLevel, Mode, Subscription};
use raycat_proto::{
    Reloaded, SettingChange, SettingItem, SettingValue, SettingsPatch, SettingsView,
};
use tokio::time::Instant;
use toml_edit::Value;

use super::{Daemon, stored_pin};
use crate::api::Refusal;
use crate::gateway;
use crate::log;
use crate::paths;
use crate::plan;
use crate::selection;
use crate::updater::Source;

/// Что `PATCH /v1/settings` умеет менять: таблица и ключ.
const EDITABLE: &[(&str, &str)] = &[
    ("routing", "ru_direct"),
    ("mode", "kill_switch"),
    ("selection", "check_interval"),
    ("selection", "failures"),
    ("selection", "switch_gain"),
    ("selection", "return_delay"),
    ("xray", "xhttp_connections"),
    ("log", "level"),
];

/// Ключи подписки, которые меняет `PATCH`: маски.
const SUBSCRIPTION_KEYS: &[&str] = &["allow", "deny", "priority"];

/// Файл настроек: где лежит и с каким окружением его читать. Перечитывание всегда берёт
/// файл заново, поэтому правка руками и `PATCH` для демона одинаковы.
pub(crate) struct ConfigFile {
    pub(crate) path: Option<PathBuf>,
    pub(crate) env: Env,
    pub(crate) root: bool,
}

impl ConfigFile {
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self {
            path: None,
            env: Env::new(),
            root: false,
        }
    }

    fn resolved(&self) -> Option<PathBuf> {
        paths::config_file(self.path.as_deref(), &self.env, Path::exists)
    }

    fn load(&self) -> Result<Config, Refusal> {
        Config::load(self.resolved().as_deref(), &self.env, self.root)
            .map_err(|error| Refusal::Invalid(error.to_string()))
    }

    fn parse(&self, text: &str) -> Result<Config, Refusal> {
        Config::from_toml_str(text, &self.env).map_err(|error| Refusal::Invalid(error.to_string()))
    }

    /// Путь файла и его текст.
    fn text(&self) -> Result<(PathBuf, String), Refusal> {
        let file = self.resolved().ok_or_else(|| {
            Refusal::NotFound("файл настроек не найден: создайте его командой raycat init".to_owned())
        })?;
        let text = fs::read_to_string(&file).map_err(|error| {
            Refusal::Unavailable(format!("не удалось прочитать {}: {error}", file.display()))
        })?;
        Ok((file, text))
    }
}

fn is_proxy(mode: Mode) -> bool {
    matches!(mode, Mode::Proxy { .. })
}

fn same_names(old: &Config, new: &Config) -> bool {
    old.subscriptions.len() == new.subscriptions.len()
        && old
            .subscriptions
            .iter()
            .zip(&new.subscriptions)
            .all(|(before, after)| before.name == after.name)
}

/// Подписка та же по адресу, приложению, платформе и устройству: маски можно менять на лету.
fn same_identity(before: &Subscription, after: &Subscription) -> bool {
    before.url == after.url
        && before.app == after.app
        && before.platform == after.platform
        && before.seed == after.seed
}

/// Ключи, которые на лету не меняются: демон держит старые значения, пока не перезапустят
/// raycat. Это единственное место, где такие ключи перечислены.
pub(crate) fn restart_keys(old: &Config, new: &Config) -> Vec<String> {
    let mut keys = Vec::new();
    if is_proxy(old.mode) != is_proxy(new.mode) {
        keys.push("mode.type".to_owned());
    }
    if let (Mode::Proxy { listen: before }, Mode::Proxy { listen: after }) = (old.mode, new.mode)
        && before != after
    {
        keys.push("mode.listen".to_owned());
    }
    if old.proxy_auth != new.proxy_auth {
        keys.push("mode.auth".to_owned());
    }
    if old.device != new.device {
        keys.push("device".to_owned());
    }
    if old.dns != new.dns {
        keys.push("dns".to_owned());
    }
    if old.xray.path != new.xray.path {
        keys.push("xray.path".to_owned());
    }
    if old.xray.memory_limit != new.xray.memory_limit {
        keys.push("xray.memory_limit".to_owned());
    }
    if old.xray.tcp_congestion != new.xray.tcp_congestion {
        keys.push("xray.tcp_congestion".to_owned());
    }
    if same_names(old, new) {
        for (before, after) in old.subscriptions.iter().zip(&new.subscriptions) {
            if !same_identity(before, after) {
                keys.push(format!(
                    "subscription.{}: url, app, platform, seed",
                    before.name
                ));
            }
        }
    } else {
        keys.push("subscription (состав и порядок)".to_owned());
    }
    keys
}

/// Конфиг, который демон действует после перечитывания: ключи на лету берутся из нового
/// файла, ключи из `restart_keys` остаются прежними.
pub(crate) fn effective(old: &Config, new: &Config) -> Config {
    let mut next = old.clone();
    next.selection = new.selection.clone();
    next.routing = new.routing.clone();
    next.log = new.log;
    next.lan = new.lan.clone();
    next.warnings = new.warnings.clone();
    next.xray.xhttp_connections = new.xray.xhttp_connections;
    next.mode = match (old.mode, new.mode) {
        (Mode::Gateway { .. }, Mode::Gateway { kill_switch, lan }) => {
            Mode::Gateway { kill_switch, lan }
        }
        (Mode::Proxy { listen: before }, Mode::Proxy { listen: after }) if before == after => {
            new.mode
        }
        _ => old.mode,
    };
    if same_names(old, new) {
        next.subscriptions = old
            .subscriptions
            .iter()
            .zip(&new.subscriptions)
            .map(|(before, after)| {
                if same_identity(before, after) {
                    Subscription {
                        allow: after.allow.clone(),
                        deny: after.deny.clone(),
                        priority: after.priority.clone(),
                        update_interval: after.update_interval,
                        ..before.clone()
                    }
                } else {
                    before.clone()
                }
            })
            .collect();
    }
    next
}

fn toml_value(value: &SettingValue) -> Value {
    match value {
        SettingValue::Bool(flag) => Value::from(*flag),
        SettingValue::Number(number) => Value::from(*number),
        SettingValue::Text(text) => Value::from(text.as_str()),
        SettingValue::Texts(texts) => {
            let mut array = toml_edit::Array::new();
            for text in texts {
                array.push(text.as_str());
            }
            Value::Array(array)
        }
    }
}

/// Изменение из запроса: только ключи из списков выше, иначе отказ до записи.
fn change(item: &SettingChange) -> Result<Change, Refusal> {
    let value = toml_value(&item.value);
    if let Some(name) = &item.subscription {
        let key = SUBSCRIPTION_KEYS
            .iter()
            .copied()
            .find(|key| *key == item.key)
            .ok_or_else(|| {
                Refusal::Invalid(format!("«{}» подписки нельзя менять через API", item.key))
            })?;
        return Ok(Change {
            target: Target::Subscription(name.clone()),
            key,
            value,
        });
    }
    let (table, key) = EDITABLE
        .iter()
        .copied()
        .find(|(table, key)| Some(*table) == item.table.as_deref() && *key == item.key)
        .ok_or_else(|| {
            let place = item.table.as_deref().unwrap_or("");
            Refusal::Invalid(format!("«{place}.{}» нельзя менять через API", item.key))
        })?;
    Ok(Change {
        target: Target::Table(table),
        key,
        value,
    })
}

#[cfg(test)]
mod tests {
    use raycat_proto::{SettingChange, SettingValue};

    use super::*;

    fn patch_item(table: Option<&str>, key: &str) -> SettingChange {
        SettingChange {
            table: table.map(str::to_owned),
            subscription: None,
            key: key.to_owned(),
            value: SettingValue::Bool(true),
        }
    }

    #[test]
    fn only_the_listed_keys_can_be_changed() {
        assert!(change(&patch_item(Some("routing"), "ru_direct")).is_ok());
        assert!(change(&patch_item(Some("mode"), "kill_switch")).is_ok());
        assert!(matches!(
            change(&patch_item(Some("dns"), "resolvers")),
            Err(Refusal::Invalid(_))
        ));
        assert!(matches!(
            change(&patch_item(Some("mode"), "listen")),
            Err(Refusal::Invalid(_))
        ));
        let mut masked = patch_item(None, "deny");
        masked.subscription = Some("main".to_owned());
        assert!(change(&masked).is_ok());
        masked.key = "url".to_owned();
        assert!(matches!(change(&masked), Err(Refusal::Invalid(_))));
    }

    #[test]
    fn only_the_mask_keys_of_a_subscription_are_changed_through_the_api() {
        let mut item = patch_item(None, "allow");
        item.subscription = Some("backup".to_owned());
        let change = change(&item).unwrap();
        assert_eq!(change.target, Target::Subscription("backup".to_owned()));
        assert_eq!(change.key, "allow");
    }
}

fn seconds(duration: Duration) -> String {
    format!("{}s", duration.as_secs())
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

fn item(text: &str, target: Target, key: &'static str, value: SettingValue) -> SettingItem {
    let source = if edit::has_key(text, &target, key) {
        "файл"
    } else {
        "по умолчанию"
    };
    let (table, subscription) = match &target {
        Target::Table(table) => (Some((*table).to_owned()), None),
        Target::Subscription(name) => (None, Some(name.clone())),
    };
    SettingItem {
        table,
        subscription,
        key: key.to_owned(),
        value,
        source: source.to_owned(),
    }
}

impl Daemon {
    /// Применяет проверенный конфиг. Ключи на лету — сразу; остальные попадают в ответ.
    pub(super) fn apply_config(&mut self, next: Config) -> Result<Reloaded, Refusal> {
        let restart_required = restart_keys(&self.config, &next);
        let effective = effective(&self.config, &next);
        if effective == self.config {
            return Ok(Reloaded {
                changed: false,
                xray_restart: false,
                restart_required,
            });
        }
        let rules = plan::gateway_rules(&effective)
            .map_err(|error| Refusal::Invalid(format!("{error:#}")))?;
        let mut sources = Vec::new();
        for (index, sub) in self.subs.iter().enumerate() {
            if let Some(subscription) = effective.subscriptions.get(index)
                && sub.source.config() != subscription
            {
                let source = Source::new(&effective, subscription, &self.machine_id)
                    .map_err(|error| Refusal::Invalid(format!("{error:#}")))?;
                sources.push((index, Arc::new(source)));
            }
        }
        if effective.mode != self.config.mode || effective.lan != self.config.lan {
            if let Some(old) = self.gateway.take() {
                gateway::remove(&old);
            }
            self.gateway = rules;
            if let Some(rules) = &self.gateway {
                gateway::install(rules)
                    .map_err(|error| Refusal::Unavailable(format!("правила перехвата: {error:#}")))?;
            }
        }
        for (index, source) in sources {
            self.subs[index].source = source;
        }
        if effective.log.level != self.config.log.level {
            log::init(effective.log.level.into());
        }
        let pin = stored_pin(&self.store, &effective);
        self.selector
            .set_settings(selection::settings(&effective, pin));
        self.next_select = Instant::now();
        self.config = effective;
        let before = self.applied.clone();
        self.reconcile();
        Ok(Reloaded {
            changed: true,
            xray_restart: self.applied != before,
            restart_required,
        })
    }

    /// `POST /v1/reload`: перечитывает файл. Битый файл — ошибка, демон остаётся на старом
    /// конфиге.
    pub(super) fn reload(&mut self) -> Result<Reloaded, Refusal> {
        let next = self.file.load()?;
        self.apply_config(next)
    }

    /// `PATCH /v1/settings`: проверяет новый текст, пишет файл атомарно и применяет его.
    /// Если проверка не прошла, файл не трогается.
    pub(super) fn patch(&mut self, patch: SettingsPatch) -> Result<Reloaded, Refusal> {
        let (path, text) = self.file.text()?;
        let changes = patch
            .changes
            .iter()
            .map(change)
            .collect::<Result<Vec<_>, _>>()?;
        let edited =
            edit::apply(&text, &changes).map_err(|error| Refusal::Invalid(error.to_string()))?;
        let next = self.file.parse(&edited)?;
        edit::write(&path, &edited).map_err(|error| Refusal::Unavailable(error.to_string()))?;
        self.apply_config(next)
    }

    /// `GET /v1/settings`: значения, которые можно менять через API, и откуда они взяты.
    pub(super) fn settings(&self) -> SettingsView {
        let text = self
            .file
            .text()
            .map(|(_, text)| text)
            .unwrap_or_default();
        let config = &self.config;
        let mut items = vec![
            item(
                &text,
                Target::Table("routing"),
                "ru_direct",
                SettingValue::Bool(config.routing.ru_direct),
            ),
            item(
                &text,
                Target::Table("selection"),
                "check_interval",
                SettingValue::Text(seconds(config.selection.check_interval)),
            ),
            item(
                &text,
                Target::Table("selection"),
                "failures",
                SettingValue::Number(i64::from(config.selection.failures)),
            ),
            item(
                &text,
                Target::Table("selection"),
                "switch_gain",
                SettingValue::Text(seconds(config.selection.switch_gain)),
            ),
            item(
                &text,
                Target::Table("selection"),
                "return_delay",
                SettingValue::Text(seconds(config.selection.return_delay)),
            ),
            item(
                &text,
                Target::Table("log"),
                "level",
                SettingValue::Text(level_name(config.log.level).to_owned()),
            ),
        ];
        if let Mode::Gateway { kill_switch, .. } = config.mode {
            items.push(item(
                &text,
                Target::Table("mode"),
                "kill_switch",
                SettingValue::Bool(kill_switch),
            ));
        }
        if let Some(connections) = config.xray.xhttp_connections {
            items.push(item(
                &text,
                Target::Table("xray"),
                "xhttp_connections",
                SettingValue::Number(i64::from(connections)),
            ));
        }
        for subscription in &config.subscriptions {
            let masks = [
                ("allow", &subscription.allow),
                ("deny", &subscription.deny),
                ("priority", &subscription.priority),
            ];
            for (key, patterns) in masks {
                let texts = patterns
                    .iter()
                    .map(|pattern| pattern.as_str().to_owned())
                    .collect();
                items.push(item(
                    &text,
                    Target::Subscription(subscription.name.clone()),
                    key,
                    SettingValue::Texts(texts),
                ));
            }
        }
        SettingsView { items }
    }
}
