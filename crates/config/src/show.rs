//! `raycat config`: итоговые настройки так, как их видит демон. Все значения показаны явно,
//! заданные отмечены источником, секреты скрыты.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use crate::envvars;
use crate::error::Error;
use crate::load::{self, Env};
use crate::model::{
    Action, Config, Device, Dns, DomainMatch, Lan, LogLevel, Mode, Pin, ProxyAuth, Routing, Rule,
    Selection, Subscription, TcpCongestion, Xray,
};
use crate::pattern::Pattern;
use crate::units::{format_duration, format_size};

const RAYCAT_CHOOSES: &str = "значение выбирает raycat";

/// Итоговые настройки: текст TOML со всеми значениями. `path` — явный файл, как у `--config`;
/// `root` — процесс работает от root (от этого зависит подсказка при ошибке чтения).
pub fn effective(path: Option<&Path>, env: &Env, root: bool) -> Result<String, Error> {
    let file = load::settings_path(path, env);
    let text = match &file {
        Some(file) => load::read(file, root)?,
        None => String::new(),
    };
    let config = Config::from_toml_str(&text, env)?;
    let mut shown = Shown {
        text: String::new(),
        prefix: String::new(),
        origins: Origins::new(&text, env),
    };
    shown.comment("Итоговые настройки raycat: так их видит демон.");
    shown.comment(
        "Значения по умолчанию показаны явно, заданные отмечены источником, секреты скрыты.",
    );
    match &file {
        Some(file) => shown.comment(&format!("Файл: {}", file.display())),
        None => shown.comment("Файла нет: настройки целиком из переменных окружения."),
    }
    for warning in &config.warnings {
        shown.comment(&format!("Предупреждение: {warning}"));
    }
    device(&mut shown, &config.device);
    for (index, subscription) in config.subscriptions.iter().enumerate() {
        subscription_section(&mut shown, index, subscription);
    }
    selection(&mut shown, &config.selection);
    mode(&mut shown, &config);
    dns(&mut shown, &config.dns);
    routing(&mut shown, &config.routing);
    xray(&mut shown, &config.xray);
    logging(&mut shown, config.log.level);
    Ok(shown.text)
}

/// Текст вывода, таблица, к которой относятся следующие строки, и откуда взяты значения.
struct Shown {
    text: String,
    prefix: String,
    origins: Origins,
}

impl Shown {
    fn comment(&mut self, line: &str) {
        let _ = writeln!(self.text, "# {line}");
    }

    /// Таблица как в файле (`[mode]`, `[[subscription]]`); `prefix` — путь к её ключам.
    fn section(&mut self, header: &str, prefix: &str) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let _ = writeln!(self.text, "{header}");
        prefix.clone_into(&mut self.prefix);
    }

    /// Строка `ключ = значение`; значение уже записано как в TOML.
    fn line(&mut self, key: &str, value: &str) {
        let origin = match self.origins.of(&format!("{}.{key}", self.prefix)) {
            Some(source) => format!("  # {source}"),
            None => String::new(),
        };
        let _ = writeln!(self.text, "{key} = {value}{origin}");
    }

    /// Ключ без значения по умолчанию: строка-пояснение вместо значения.
    fn unset(&mut self, key: &str, note: &str) {
        let _ = writeln!(self.text, "# {key}: не задано, {note}");
    }
}

/// Значение, которое может быть не задано: `None` выводится пояснением.
fn optional(shown: &mut Shown, key: &str, value: Option<String>, note: &str) {
    match value {
        Some(value) => shown.line(key, &value),
        None => shown.unset(key, note),
    }
}

/// Откуда взято значение: из файла или из переменной окружения, которая важнее файла.
struct Origins {
    file: BTreeSet<String>,
    variables: Vec<(&'static str, &'static str)>,
}

impl Origins {
    fn new(text: &str, env: &Env) -> Self {
        let variables = envvars::KEYS
            .iter()
            .filter(|(variable, _)| envvars::value(env, variable).is_some())
            .map(|(variable, key)| (*key, *variable))
            .collect();
        Self {
            file: file_keys(text),
            variables,
        }
    }

    fn of(&self, key: &str) -> Option<String> {
        for (path, variable) in &self.variables {
            if *path == key {
                return Some(format!("из {variable}"));
            }
        }
        self.file.contains(key).then(|| "из файла".to_owned())
    }
}

/// Ключи, которые есть в файле: `mode.type`, `subscription[0].url`. Ссылка из `url_file`
/// и пароль из `auth_file` показываются под именами `url` и `auth`.
fn file_keys(text: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Ok(table) = toml::from_str::<toml::Table>(text) {
        collect_keys(&table, "", &mut keys);
    }
    let aliases: Vec<String> = keys
        .iter()
        .filter_map(|key| {
            key.strip_suffix("url_file")
                .map(|stem| format!("{stem}url"))
                .or_else(|| {
                    key.strip_suffix("auth_file")
                        .map(|stem| format!("{stem}auth"))
                })
        })
        .collect();
    keys.extend(aliases);
    keys
}

fn collect_keys(table: &toml::Table, prefix: &str, keys: &mut BTreeSet<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) => collect_keys(inner, &path, keys),
            toml::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if let toml::Value::Table(inner) = item {
                        collect_keys(inner, &format!("{path}[{index}]"), keys);
                    } else {
                        keys.insert(path.clone());
                    }
                }
            }
            _ => {
                keys.insert(path);
            }
        }
    }
}

/// Строка TOML в кавычках: кавычки, обратная косая черта и управляющие символы экранируются.
fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn secret() -> String {
    quoted("***")
}

fn array(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

fn device(shown: &mut Shown, device: &Device) {
    shown.section("[device]", "device");
    let created = "raycat создаёт машинный идентификатор сам";
    optional(
        shown,
        "seed",
        device.seed.as_ref().map(|_| secret()),
        created,
    );
    optional(
        shown,
        "machine_id",
        device.machine_id.as_ref().map(|_| secret()),
        created,
    );
    optional(
        shown,
        "hostname",
        device.hostname.as_deref().map(quoted),
        RAYCAT_CHOOSES,
    );
    optional(
        shown,
        "model",
        device.model.as_deref().map(quoted),
        RAYCAT_CHOOSES,
    );
    optional(
        shown,
        "manufacturer",
        device.manufacturer.as_deref().map(quoted),
        RAYCAT_CHOOSES,
    );
    optional(
        shown,
        "os_version",
        device.os_version.as_deref().map(quoted),
        RAYCAT_CHOOSES,
    );
    optional(
        shown,
        "locale",
        device.locale.as_deref().map(quoted),
        RAYCAT_CHOOSES,
    );
}

fn subscription_section(shown: &mut Shown, index: usize, subscription: &Subscription) {
    shown.section("[[subscription]]", &format!("subscription[{index}]"));
    shown.line("name", &quoted(&subscription.name));
    shown.line("url", &quoted(&subscription.masked_url()));
    shown.line("app", &quoted(subscription.app.as_str()));
    shown.line("platform", &quoted(subscription.platform.as_str()));
    optional(
        shown,
        "seed",
        subscription.seed.as_ref().map(|_| secret()),
        "общее устройство из [device]",
    );
    optional(
        shown,
        "update_interval",
        subscription
            .update_interval
            .map(|interval| quoted(&format_duration(interval))),
        "интервал из ответа провайдера, иначе 12h",
    );
    shown.line("allow", &array(&patterns(&subscription.allow)));
    shown.line("deny", &array(&patterns(&subscription.deny)));
    shown.line("priority", &array(&patterns(&subscription.priority)));
}

fn patterns(list: &[Pattern]) -> Vec<String> {
    list.iter()
        .map(|pattern| quoted(pattern.as_str()))
        .collect()
}

fn selection(shown: &mut Shown, selection: &Selection) {
    shown.section("[selection]", "selection");
    shown.line("check_url", &quoted(&selection.check_url));
    shown.line(
        "check_interval",
        &quoted(&format_duration(selection.check_interval)),
    );
    shown.line("failures", &selection.failures.to_string());
    shown.line(
        "switch_gain",
        &quoted(&format_duration(selection.switch_gain)),
    );
    shown.line(
        "return_delay",
        &quoted(&format_duration(selection.return_delay)),
    );
    optional(
        shown,
        "pin",
        selection.pin.as_ref().map(pin_text),
        "закрепления нет",
    );
}

fn pin_text(pin: &Pin) -> String {
    quoted(&format!("{}/{}", pin.subscription, pin.node))
}

fn mode(shown: &mut Shown, config: &Config) {
    shown.section("[mode]", "mode");
    match config.mode {
        Mode::Proxy { listen } => {
            shown.line("type", &quoted("proxy"));
            shown.line("listen", &quoted(&listen.to_string()));
            match &config.proxy_auth {
                ProxyAuth::NotSet => {
                    shown.unset("auth", "без пароля: прокси слушает только петлевой адрес");
                }
                ProxyAuth::Off => shown.line("auth", &quoted("off")),
                ProxyAuth::Password { user, .. } => {
                    shown.line("auth", &quoted(&format!("{user}:***")));
                }
            }
        }
        Mode::Gateway { kill_switch, lan } => {
            shown.line("type", &quoted("gateway"));
            shown.line("kill_switch", &kill_switch.to_string());
            shown.line("lan", &lan.to_string());
            if lan {
                lan_keys(shown, &config.lan);
            }
        }
    }
}

fn lan_keys(shown: &mut Shown, lan: &Lan) {
    optional(
        shown,
        "lan_interface",
        lan.interface.as_deref().map(quoted),
        "интерфейс маршрута по умолчанию",
    );
    if lan.subnets.is_empty() {
        shown.unset("lan_subnets", "подсети этого интерфейса");
    } else {
        let subnets: Vec<String> = lan
            .subnets
            .iter()
            .map(|net| quoted(&net.to_string()))
            .collect();
        shown.line("lan_subnets", &array(&subnets));
    }
}

fn dns(shown: &mut Shown, dns: &Dns) {
    shown.section("[dns]", "dns");
    let resolvers: Vec<String> = dns
        .resolvers
        .iter()
        .map(|address| quoted(&address.to_string()))
        .collect();
    shown.line("resolvers", &array(&resolvers));
}

fn routing(shown: &mut Shown, routing: &Routing) {
    shown.section("[routing]", "routing");
    shown.line("provider", &routing.provider.to_string());
    shown.line("ru_direct", &routing.ru_direct.to_string());
    for (index, rule) in routing.rules.iter().enumerate() {
        rule_section(shown, index, rule);
    }
}

fn rule_section(shown: &mut Shown, index: usize, rule: &Rule) {
    shown.section("[[routing.rule]]", &format!("routing.rule[{index}]"));
    let domains: Vec<String> = rule
        .domains
        .iter()
        .map(|entry| quoted(&domain_text(entry)))
        .collect();
    if !domains.is_empty() {
        shown.line("domains", &array(&domains));
    }
    let ips: Vec<String> = rule
        .ips
        .iter()
        .map(|net| quoted(&net.to_string()))
        .collect();
    if !ips.is_empty() {
        shown.line("ips", &array(&ips));
    }
    shown.line("action", &quoted(action_text(rule.action)));
}

fn domain_text(domain: &DomainMatch) -> String {
    if domain.subdomains {
        format!("*.{}", domain.name)
    } else {
        domain.name.clone()
    }
}

fn action_text(action: Action) -> &'static str {
    match action {
        Action::Direct => "direct",
        Action::Proxy => "proxy",
        Action::Block => "block",
    }
}

fn xray(shown: &mut Shown, xray: &Xray) {
    shown.section("[xray]", "xray");
    shown.line("path", &quoted(&xray.path.display().to_string()));
    shown.line("memory_limit", &quoted(&format_size(xray.memory_limit)));
    shown.line(
        "tcp_congestion",
        &quoted(&congestion_text(&xray.tcp_congestion)),
    );
    optional(
        shown,
        "xhttp_connections",
        xray.xhttp_connections.map(|count| count.to_string()),
        "как у провайдера",
    );
}

fn congestion_text(congestion: &TcpCongestion) -> String {
    match congestion {
        TcpCongestion::Auto => "auto".to_owned(),
        TcpCongestion::Off => "off".to_owned(),
        TcpCongestion::Algorithm(name) => name.clone(),
    }
}

fn logging(shown: &mut Shown, level: LogLevel) {
    shown.section("[log]", "log");
    let text = match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    };
    shown.line("level", &quoted(text));
}
