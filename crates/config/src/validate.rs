use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Error, Problems};
use crate::link::{self, Scheme};
use crate::model::{
    App, Config, Device, Dns, LogLevel, Logging, Mode, Pin, Platform, Routing, Secret, Selection,
    Subscription, Xray,
};
use crate::pattern::Pattern;
use crate::raw::{Raw, RawDevice, RawDns, RawLog, RawMode, RawSelection, RawSubscription, RawXray};
use crate::units::{format_duration, format_size, parse_duration, parse_size};

const MAX_NAME_CHARS: usize = 64;
const MAX_HEADER_CHARS: usize = 128;
const MAX_SEED_CHARS: usize = 256;
const MAX_PATTERNS: usize = 256;
const MAX_PATTERN_CHARS: usize = 256;
const MAX_RESOLVERS: usize = 8;
const MAX_PATH_BYTES: usize = 4_096;

const UPDATE_INTERVAL_RANGE: RangeInclusive<Duration> =
    Duration::from_secs(10 * 60)..=Duration::from_secs(30 * 86_400);
const CHECK_INTERVAL_RANGE: RangeInclusive<Duration> =
    Duration::from_secs(5)..=Duration::from_secs(10 * 60);
const SWITCH_GAIN_RANGE: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(60);
const RETURN_DELAY_RANGE: RangeInclusive<Duration> =
    Duration::ZERO..=Duration::from_secs(24 * 3_600);
const FAILURES_RANGE: RangeInclusive<u32> = 1..=20;
const MEMORY_RANGE: RangeInclusive<u64> = (16 << 20)..=(16 << 30);

const DEFAULT_CHECK_URL: &str = "https://www.gstatic.com/generate_204";
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_FAILURES: u32 = 3;
const DEFAULT_SWITCH_GAIN: Duration = Duration::from_millis(150);
const DEFAULT_RETURN_DELAY: Duration = Duration::from_secs(5 * 60);
const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7890);
const DEFAULT_XRAY_PATH: &str = "/usr/libexec/raycat/xray";
const DEFAULT_MEMORY_LIMIT: u64 = 48 << 20;

pub(crate) const APP_HINT: &str = "допустимо: happ или incy";
pub(crate) const PLATFORM_HINT: &str = "допустимо: windows или android";
pub(crate) const MODE_HINT: &str = "допустимо: proxy или gateway";
pub(crate) const LISTEN_HINT: &str = "ожидается адрес вида 127.0.0.1:7890 или [::1]:7890";
pub(crate) const LEVEL_HINT: &str = "допустимо: error, warn, info или debug";
pub(crate) const BOOL_HINT: &str = "ожидается true или false (также 1/0, yes/no, on/off)";
const IP_HINT: &str = "ожидается IP-адрес, например 1.1.1.1";
const DURATION_HINT: &str = "ожидается длительность вроде 500ms, 30s, 5m, 6h или 1d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeKind {
    Proxy,
    Gateway,
}

pub(crate) fn parse_app(value: &str) -> Option<App> {
    match value.trim().to_ascii_lowercase().as_str() {
        "happ" => Some(App::Happ),
        "incy" => Some(App::Incy),
        _ => None,
    }
}

pub(crate) fn parse_platform(value: &str) -> Option<Platform> {
    match value.trim().to_ascii_lowercase().as_str() {
        "windows" => Some(Platform::Windows),
        "android" => Some(Platform::Android),
        _ => None,
    }
}

pub(crate) fn parse_mode_kind(value: &str) -> Option<ModeKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "proxy" => Some(ModeKind::Proxy),
        "gateway" => Some(ModeKind::Gateway),
        _ => None,
    }
}

pub(crate) fn parse_listen(value: &str) -> Option<SocketAddr> {
    value
        .trim()
        .parse::<SocketAddr>()
        .ok()
        .filter(|addr| addr.port() != 0)
}

pub(crate) fn parse_level(value: &str) -> Option<LogLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Some(LogLevel::Error),
        "warn" => Some(LogLevel::Warn),
        "info" => Some(LogLevel::Info),
        "debug" => Some(LogLevel::Debug),
        _ => None,
    }
}

pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse().ok()
}

pub(crate) fn build(raw: &Raw, mut problems: Problems) -> Result<Config, Error> {
    let p = &mut problems;
    let device = device(&raw.device, p);
    let mut names = HashSet::new();
    let subscriptions = subscriptions(&raw.subscription, &mut names, p);
    let selection = selection(&raw.selection, &names, p);
    let mode = mode(&raw.mode, p);
    let dns = dns(&raw.dns, p);
    let routing = Routing {
        provider: raw.routing.provider.unwrap_or(false),
    };
    let xray = xray(&raw.xray, p);
    let log = log(&raw.log, p);
    if problems.is_empty() {
        Ok(Config {
            device,
            subscriptions,
            selection,
            mode,
            dns,
            routing,
            xray,
            log,
        })
    } else {
        Err(problems.into_error())
    }
}

fn device(raw: &RawDevice, p: &mut Problems) -> Device {
    if raw.seed.is_some() && raw.machine_id.is_some() {
        p.add("device", "seed и machine_id нельзя задавать вместе");
    }
    Device {
        seed: secret_text("device.seed", raw.seed.as_deref(), p),
        machine_id: raw
            .machine_id
            .as_deref()
            .and_then(|value| machine_id(value, p)),
        hostname: header_text("device.hostname", raw.hostname.as_deref(), p),
        model: header_text("device.model", raw.model.as_deref(), p),
        os_version: header_text("device.os_version", raw.os_version.as_deref(), p),
        locale: header_text("device.locale", raw.locale.as_deref(), p),
    }
}

fn machine_id(value: &str, p: &mut Problems) -> Option<Secret> {
    let value = value.trim();
    if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(Secret::new(value.to_ascii_lowercase()))
    } else {
        p.add(
            "device.machine_id",
            "ожидается 32 шестнадцатеричных символа (0-9, a-f)",
        );
        None
    }
}

fn secret_text(field: &str, value: Option<&str>, p: &mut Problems) -> Option<Secret> {
    let value = value?;
    if value.trim().is_empty() {
        p.add(field, "не может быть пустым");
        return None;
    }
    if value.chars().count() > MAX_SEED_CHARS {
        p.add(field, format!("длиннее {MAX_SEED_CHARS} символов"));
        return None;
    }
    Some(Secret::new(value.to_owned()))
}

fn header_text(field: &str, value: Option<&str>, p: &mut Problems) -> Option<String> {
    let value = value?;
    if value.trim().is_empty() {
        p.add(field, "не может быть пустым");
        return None;
    }
    if value.chars().any(char::is_control) {
        p.add(
            field,
            "управляющие символы (переводы строк, табуляция) недопустимы: значение уходит в заголовки запроса",
        );
        return None;
    }
    if value.chars().count() > MAX_HEADER_CHARS {
        p.add(field, format!("длиннее {MAX_HEADER_CHARS} символов"));
        return None;
    }
    Some(value.to_owned())
}

fn subscriptions(
    list: &[RawSubscription],
    names: &mut HashSet<String>,
    p: &mut Problems,
) -> Vec<Subscription> {
    if list.is_empty() {
        p.add(
            "subscription",
            "нужна хотя бы одна подписка ([[subscription]] в файле или RAYCAT_SUBSCRIPTION)",
        );
    }
    list.iter()
        .enumerate()
        .filter_map(|(index, raw)| subscription(index, raw, names, p))
        .collect()
}

fn subscription(
    index: usize,
    raw: &RawSubscription,
    names: &mut HashSet<String>,
    p: &mut Problems,
) -> Option<Subscription> {
    let at = |key: &str| format!("subscription[{index}].{key}");
    let name = name(&at("name"), raw.name.as_deref(), names, p);
    let url = url_field(
        &at("url"),
        raw.url.as_deref(),
        raw.allow_http == Some(true),
        p,
    );
    let app = required(&at("app"), raw.app.as_deref(), parse_app, APP_HINT, p);
    let platform = required(
        &at("platform"),
        raw.platform.as_deref(),
        parse_platform,
        PLATFORM_HINT,
        p,
    );
    if let (Some(App::Incy), Some(Platform::Windows)) = (app, platform) {
        p.add(at("platform"), "для incy доступна только платформа android");
    }
    let seed = secret_text(&at("seed"), raw.seed.as_deref(), p);
    let update_interval = raw
        .update_interval
        .as_deref()
        .and_then(|value| duration_in(&at("update_interval"), value, &UPDATE_INTERVAL_RANGE, p));
    let allow = patterns(&at("allow"), &raw.allow, p);
    let deny = patterns(&at("deny"), &raw.deny, p);
    let priority = patterns(&at("priority"), &raw.priority, p);
    Some(Subscription {
        name: name?,
        url: url?,
        app: app?,
        platform: platform?,
        seed,
        update_interval,
        allow,
        deny,
        priority,
    })
}

fn name(
    field: &str,
    value: Option<&str>,
    names: &mut HashSet<String>,
    p: &mut Problems,
) -> Option<String> {
    let Some(value) = value else {
        p.add(field, "не указано");
        return None;
    };
    if value.trim().is_empty() {
        p.add(field, "имя не может быть пустым");
        return None;
    }
    if value.chars().count() > MAX_NAME_CHARS {
        p.add(field, format!("имя длиннее {MAX_NAME_CHARS} символов"));
        return None;
    }
    if value.chars().any(char::is_control) || value.contains('/') {
        p.add(
            field,
            "в имени нельзя использовать управляющие символы и «/»",
        );
        return None;
    }
    if !names.insert(value.to_owned()) {
        p.add(field, format!("имя «{value}» уже используется"));
        return None;
    }
    Some(value.to_owned())
}

fn url_field(
    field: &str,
    value: Option<&str>,
    allow_http: bool,
    p: &mut Problems,
) -> Option<Secret> {
    let Some(value) = value.map(str::trim) else {
        p.add(field, "не указано");
        return None;
    };
    let scheme = match link::check(value) {
        Ok(scheme) => scheme,
        Err(message) => {
            p.add(field, format!("{message} ({})", link::mask(value)));
            return None;
        }
    };
    if scheme == Scheme::Http && !allow_http {
        p.add(
            field,
            format!(
                "http небезопасен: используйте https или добавьте allow_http = true ({})",
                link::mask(value)
            ),
        );
        return None;
    }
    Some(Secret::new(value.to_owned()))
}

fn required<T>(
    field: &str,
    value: Option<&str>,
    parse: fn(&str) -> Option<T>,
    hint: &str,
    p: &mut Problems,
) -> Option<T> {
    let Some(value) = value else {
        p.add(field, format!("не указано ({hint})"));
        return None;
    };
    parsed(field, value, parse, hint, p)
}

fn parsed<T>(
    field: &str,
    value: &str,
    parse: fn(&str) -> Option<T>,
    hint: &str,
    p: &mut Problems,
) -> Option<T> {
    let result = parse(value);
    if result.is_none() {
        p.add(field, hint);
    }
    result
}

fn patterns(field: &str, values: &[String], p: &mut Problems) -> Vec<Pattern> {
    if values.len() > MAX_PATTERNS {
        p.add(field, format!("не больше {MAX_PATTERNS} масок"));
        return Vec::new();
    }
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let at = format!("{field}[{index}]");
            if value.trim().is_empty() {
                p.add(at, "маска не может быть пустой");
                None
            } else if value.chars().count() > MAX_PATTERN_CHARS {
                p.add(at, format!("маска длиннее {MAX_PATTERN_CHARS} символов"));
                None
            } else {
                Some(Pattern::new(value))
            }
        })
        .collect()
}

fn duration_in(
    field: &str,
    value: &str,
    range: &RangeInclusive<Duration>,
    p: &mut Problems,
) -> Option<Duration> {
    let Some(duration) = parse_duration(value) else {
        p.add(field, DURATION_HINT);
        return None;
    };
    if !range.contains(&duration) {
        p.add(
            field,
            format!(
                "должно быть от {} до {}",
                format_duration(*range.start()),
                format_duration(*range.end())
            ),
        );
        return None;
    }
    Some(duration)
}

fn duration_or(
    field: &str,
    value: Option<&str>,
    default: Duration,
    range: &RangeInclusive<Duration>,
    p: &mut Problems,
) -> Duration {
    value
        .and_then(|value| duration_in(field, value, range, p))
        .unwrap_or(default)
}

fn selection(raw: &RawSelection, names: &HashSet<String>, p: &mut Problems) -> Selection {
    let check_url = match raw.check_url.as_deref().map(str::trim) {
        None => DEFAULT_CHECK_URL.to_owned(),
        Some(value) => {
            if let Err(message) = link::check(value) {
                p.add("selection.check_url", message);
            }
            value.to_owned()
        }
    };
    let failures = match raw.failures {
        None => DEFAULT_FAILURES,
        Some(value) => u32::try_from(value)
            .ok()
            .filter(|value| FAILURES_RANGE.contains(value))
            .unwrap_or_else(|| {
                p.add(
                    "selection.failures",
                    format!(
                        "должно быть целым числом от {} до {}",
                        FAILURES_RANGE.start(),
                        FAILURES_RANGE.end()
                    ),
                );
                DEFAULT_FAILURES
            }),
    };
    Selection {
        check_url,
        check_interval: duration_or(
            "selection.check_interval",
            raw.check_interval.as_deref(),
            DEFAULT_CHECK_INTERVAL,
            &CHECK_INTERVAL_RANGE,
            p,
        ),
        failures,
        switch_gain: duration_or(
            "selection.switch_gain",
            raw.switch_gain.as_deref(),
            DEFAULT_SWITCH_GAIN,
            &SWITCH_GAIN_RANGE,
            p,
        ),
        return_delay: duration_or(
            "selection.return_delay",
            raw.return_delay.as_deref(),
            DEFAULT_RETURN_DELAY,
            &RETURN_DELAY_RANGE,
            p,
        ),
        pin: raw.pin.as_deref().and_then(|value| pin(value, names, p)),
    }
}

fn pin(value: &str, names: &HashSet<String>, p: &mut Problems) -> Option<Pin> {
    let field = "selection.pin";
    let Some((subscription, node)) = value.split_once('/') else {
        p.add(field, "ожидается «имя подписки/имя узла»");
        return None;
    };
    if node.trim().is_empty() {
        p.add(field, "не указано имя узла после «/»");
        return None;
    }
    if !names.contains(subscription) {
        p.add(field, format!("подписки «{subscription}» нет в настройках"));
        return None;
    }
    Some(Pin {
        subscription: subscription.to_owned(),
        node: node.to_owned(),
    })
}

fn mode(raw: &RawMode, p: &mut Problems) -> Mode {
    let kind = match raw.kind.as_deref() {
        None => Some(ModeKind::Proxy),
        Some(value) => parsed("mode.type", value, parse_mode_kind, MODE_HINT, p),
    };
    let listen = match raw.listen.as_deref() {
        None => DEFAULT_LISTEN,
        Some(value) => {
            parsed("mode.listen", value, parse_listen, LISTEN_HINT, p).unwrap_or(DEFAULT_LISTEN)
        }
    };
    match kind {
        Some(ModeKind::Gateway) => Mode::Gateway {
            kill_switch: raw.kill_switch.unwrap_or(true),
            lan: raw.lan.unwrap_or(false),
        },
        Some(ModeKind::Proxy) | None => Mode::Proxy { listen },
    }
}

fn dns(raw: &RawDns, p: &mut Problems) -> Dns {
    let Some(list) = &raw.resolvers else {
        return Dns {
            resolvers: vec![
                IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
                IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            ],
        };
    };
    if list.is_empty() || list.len() > MAX_RESOLVERS {
        p.add(
            "dns.resolvers",
            format!("нужно от 1 до {MAX_RESOLVERS} адресов"),
        );
    }
    let resolvers = list
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            parsed(
                &format!("dns.resolvers[{index}]"),
                value,
                parse_ip,
                IP_HINT,
                p,
            )
        })
        .collect();
    Dns { resolvers }
}

fn xray(raw: &RawXray, p: &mut Problems) -> Xray {
    let path = match raw.path.as_deref() {
        None => DEFAULT_XRAY_PATH,
        Some(value) => {
            if value.trim().is_empty()
                || value.len() > MAX_PATH_BYTES
                || value.chars().any(char::is_control)
            {
                p.add(
                    "xray.path",
                    "ожидается путь к исполняемому файлу без управляющих символов",
                );
            }
            value
        }
    };
    let memory_limit = match raw.memory_limit.as_deref() {
        None => DEFAULT_MEMORY_LIMIT,
        Some(value) => match parse_size(value) {
            None => {
                p.add(
                    "xray.memory_limit",
                    "ожидается размер вроде 48MiB (единицы: B, KiB, MiB, GiB)",
                );
                DEFAULT_MEMORY_LIMIT
            }
            Some(bytes) if MEMORY_RANGE.contains(&bytes) => bytes,
            Some(_) => {
                p.add(
                    "xray.memory_limit",
                    format!(
                        "должно быть от {} до {}",
                        format_size(*MEMORY_RANGE.start()),
                        format_size(*MEMORY_RANGE.end())
                    ),
                );
                DEFAULT_MEMORY_LIMIT
            }
        },
    };
    Xray {
        path: PathBuf::from(path),
        memory_limit,
    }
}

fn log(raw: &RawLog, p: &mut Problems) -> Logging {
    let level = match raw.level.as_deref() {
        None => LogLevel::Info,
        Some(value) => {
            parsed("log.level", value, parse_level, LEVEL_HINT, p).unwrap_or(LogLevel::Info)
        }
    };
    Logging { level }
}
