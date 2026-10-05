//! Из захватов — новый профиль и образцы для golden-тестов.
//!
//! Инструмент ничего не угадывает: значения читаются из запроса по шаблонам профиля,
//! а результат проверяется настоящим построением запроса. Если профиль не может
//! выразить запрос, строится отчёт о различиях.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};

use super::capture::{Artifact, Request};
use super::clock::Instant;
use super::edit;
use super::report;
use super::unrender::{Piece, pieces, push_text, unrender, value_of};
use crate::profile::{Marker, Profile};
use crate::template::{Part, Var};
use crate::{Arch, Device, DeviceInfo, Emulation, Platform, Url};

/// Идентификатор устройства в образцах: все остальные значения берутся из захвата.
const MACHINE_ID: &str = "0d0af05ee8fd4dc29275718f2ce4dff1";
const CAPTURE_PORT: &str = ":18080";
const MOSCOW_OFFSET_HOURS: i64 = 3;

pub(super) struct Inputs<'a> {
    pub(super) app: &'a str,
    pub(super) platform: Platform,
    pub(super) release: &'a str,
    pub(super) profile_text: &'a str,
    pub(super) artifacts: &'a [Artifact],
}

pub(super) struct CaptureFile {
    pub(super) stem: String,
    pub(super) http: String,
    pub(super) toml: String,
}

pub(super) struct Output {
    pub(super) profile_text: String,
    pub(super) captures: Vec<CaptureFile>,
    pub(super) summary: String,
}

pub(super) fn build(inputs: &Inputs<'_>) -> Result<Output> {
    let base = Profile::parse(inputs.profile_text).context("профиль не разобран")?;
    check_header_names(&base, inputs.artifacts)?;
    let (group, parsed) = read_user_agents(&base, inputs.artifacts)?;
    let version = common_version(&parsed)?;
    let built = compute_builds(&group, &parsed)?;
    let text = edit_profile(inputs.profile_text, &base, &version, &built)?;
    let profile = Profile::parse(&text).context("обновлённый профиль не разобран")?;

    let mut captures = Vec::new();
    let mut failures = Vec::new();
    for artifact in inputs.artifacts {
        match capture_file(inputs, &profile, artifact, &version) {
            Ok(file) => captures.push(file),
            Err(error) => failures.push(format!("{error:#}")),
        }
    }
    if !failures.is_empty() {
        bail!(
            "Профиль {}/{} не воспроизводит захваты версии {version}, файлы не изменены.\n\n{}",
            inputs.app,
            inputs.platform,
            failures.join("\n\n")
        );
    }
    captures.sort_by(|a, b| a.stem.cmp(&b.stem));
    if let Some(pair) = captures
        .windows(2)
        .find(|pair| pair[0].stem == pair[1].stem)
    {
        bail!("два захвата с одним именем {}", pair[0].stem);
    }
    let summary = format!(
        "Профиль {}/{}: версия {} -> {version}, релиз {}.\nЗахваты: {}.",
        inputs.app,
        inputs.platform,
        base.version,
        inputs.release,
        captures
            .iter()
            .map(|file| file.stem.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(Output {
        profile_text: text,
        captures,
        summary,
    })
}

/// Профиль выражает только запросы с теми же именами заголовков в том же порядке и
/// регистре: иначе нужен человек.
fn check_header_names(profile: &Profile, artifacts: &[Artifact]) -> Result<()> {
    let expected: Vec<String> = profile
        .headers
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let mut problems = String::new();
    for artifact in artifacts {
        let actual = artifact.request.names();
        if actual != expected {
            writeln!(problems, "Захват {}:", artifact.dir_name)?;
            for line in report::diff_names(&expected, &actual) {
                writeln!(problems, "{line}")?;
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "Заголовки запроса изменились, профиль {}/{} их не выражает. «-» есть в профиле, \
         но нет в захвате, «+» есть в захвате, но нет в профиле.\n{problems}\
         Профиль нужно доработать вручную.",
        profile.app,
        profile.platform
    )
}

/// Имя заголовка, значение которого — ровно одна подстановка.
fn header_name(profile: &Profile, var: Var) -> Option<String> {
    profile
        .headers
        .iter()
        .find(|(_, template)| template.0 == [Part::Var(var)])
        .map(|(name, _)| name.clone())
}

struct UaShape {
    pieces: Vec<Piece>,
    group: Vec<Var>,
}

/// `user_agent` профиля как куски; `{build}{marker}{tail}` — один неразбираемый
/// кусок, границы которого определяются по паре захватов.
fn ua_shape(profile: &Profile, cpu: Option<&str>) -> Result<UaShape> {
    let parts = &profile.user_agent.0;
    let is_group = |part: &Part| matches!(part, Part::Var(Var::Build | Var::Marker | Var::Tail));
    let positions: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| is_group(part))
        .map(|(index, _)| index)
        .collect();
    if positions.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        bail!("{{build}}, {{marker}} и {{tail}} в user_agent должны идти подряд");
    }
    let group: Vec<Var> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Var(var) if is_group(part) => Some(*var),
            _ => None,
        })
        .collect();
    let supported = matches!(
        group.as_slice(),
        [] | [Var::Build] | [Var::Build, Var::Marker] | [Var::Build, Var::Marker, Var::Tail]
    );
    if !supported {
        bail!("состав {{build}}{{marker}}{{tail}} в user_agent не поддерживается инструментом");
    }
    let mut pieces = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        match part {
            Part::Text(text) => push_text(&mut pieces, text),
            Part::Var(Var::AppVersion) => pieces.push(Piece::Free(Var::AppVersion)),
            Part::Var(Var::Os) => push_text(&mut pieces, &profile.os),
            Part::Var(Var::Cpu) => {
                let cpu = cpu.ok_or_else(|| anyhow!("в сборке профиля нет cpu для user_agent"))?;
                push_text(&mut pieces, cpu);
            }
            Part::Var(Var::Build | Var::Marker | Var::Tail) => {
                if positions.first() == Some(&index) {
                    pieces.push(Piece::Free(Var::Build));
                }
            }
            Part::Var(other) => {
                bail!(
                    "подстановка {{{}}} в user_agent инструментом не поддерживается",
                    other.name()
                )
            }
        }
    }
    Ok(UaShape { pieces, group })
}

fn describe(shape: &UaShape) -> String {
    let mut out = String::new();
    for piece in &shape.pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Free(Var::Build) => {
                for var in &shape.group {
                    push_substitution(&mut out, *var);
                }
            }
            Piece::Free(var) => push_substitution(&mut out, *var),
        }
    }
    out
}

fn push_substitution(out: &mut String, var: Var) {
    out.push('{');
    out.push_str(var.name());
    out.push('}');
}

struct Parsed<'a> {
    artifact: &'a Artifact,
    version: String,
    token: Option<String>,
}

fn read_user_agents<'a>(
    profile: &Profile,
    artifacts: &'a [Artifact],
) -> Result<(Vec<Var>, Vec<Parsed<'a>>)> {
    let name = header_name(profile, Var::UserAgent)
        .ok_or_else(|| anyhow!("в профиле нет заголовка со значением {{user_agent}}"))?;
    let mut group = Vec::new();
    let mut parsed = Vec::new();
    for artifact in artifacts {
        let cpu = profile.build_for(artifact.arch)?.cpu.clone();
        let shape = ua_shape(profile, cpu.as_deref())?;
        let value = artifact
            .request
            .header(&name)
            .ok_or_else(|| anyhow!("в захвате {} нет заголовка {name}", artifact.dir_name))?;
        let Some(found) = unrender(&shape.pieces, value) else {
            bail!(
                "User-Agent в захвате {} не соответствует шаблону профиля.\n  захват: {value}\n  шаблон: {}\n\
                 Профиль нужно доработать вручную.",
                artifact.dir_name,
                describe(&shape)
            );
        };
        let version = value_of(&found, Var::AppVersion)
            .ok_or_else(|| anyhow!("в user_agent профиля нет {{app_version}}"))?
            .to_owned();
        let token = value_of(&found, Var::Build).map(str::to_owned);
        group = shape.group;
        parsed.push(Parsed {
            artifact,
            version,
            token,
        });
    }
    Ok((group, parsed))
}

fn common_version(parsed: &[Parsed<'_>]) -> Result<String> {
    let Some(first) = parsed.first() else {
        bail!("нет ни одного захвата");
    };
    if let Some(other) = parsed.iter().find(|item| item.version != first.version) {
        bail!(
            "захваты разных версий: {} в {}, {} в {}",
            first.version,
            first.artifact.dir_name,
            other.version,
            other.artifact.dir_name
        );
    }
    let version = &first.version;
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        bail!("версия в User-Agent содержит недопустимые символы");
    }
    Ok(version.clone())
}

#[derive(Debug, PartialEq, Eq)]
struct Built {
    build: Option<String>,
    tail: Option<String>,
}

/// Позиция маркера в двух токенах, отличающихся ровно одним символом.
fn single_difference(a: &str, b: &str) -> Option<usize> {
    if a.len() != b.len() {
        return None;
    }
    let mut differing = a
        .bytes()
        .zip(b.bytes())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(index, _)| index);
    let first = differing.next()?;
    differing.next().is_none().then_some(first)
}

fn pair_position(items: &[&Parsed<'_>]) -> Option<usize> {
    items.iter().enumerate().find_map(|(index, first)| {
        items[index + 1..]
            .iter()
            .find_map(|second| single_difference(first.token.as_deref()?, second.token.as_deref()?))
    })
}

fn split_token(
    token: &str,
    group: &[Var],
    marker_at: Option<usize>,
    tail_len: Option<usize>,
) -> Result<Built> {
    if !token.is_ascii() {
        bail!("сборка в User-Agent содержит не ASCII");
    }
    let too_short = || anyhow!("сборка в User-Agent «{token}» слишком короткая");
    let at = match group {
        [Var::Build] => {
            return Ok(Built {
                build: Some(token.to_owned()),
                tail: None,
            });
        }
        [Var::Build, Var::Marker] => token.len().checked_sub(1).ok_or_else(too_short)?,
        _ => match (marker_at, tail_len) {
            (Some(at), _) => at,
            (None, Some(tail_len)) => token
                .len()
                .checked_sub(tail_len + 1)
                .ok_or_else(too_short)?,
            (None, None) => bail!(
                "нет пары захватов чётного и нечётного дня: без неё не определить, \
                 где в User-Agent кончается сборка и начинается хвост"
            ),
        },
    };
    if at == 0 || at >= token.len() {
        return Err(too_short());
    }
    let tail = (group.len() == 3).then(|| token[at + 1..].to_owned());
    Ok(Built {
        build: Some(token[..at].to_owned()),
        tail,
    })
}

/// Сборка и хвост по архитектурам.
fn compute_builds(group: &[Var], parsed: &[Parsed<'_>]) -> Result<BTreeMap<String, Built>> {
    let mut by_arch: BTreeMap<String, Vec<&Parsed<'_>>> = BTreeMap::new();
    for item in parsed {
        by_arch
            .entry(item.artifact.arch.to_string())
            .or_default()
            .push(item);
    }
    let marker_at: BTreeMap<&str, usize> = by_arch
        .iter()
        .filter_map(|(arch, items)| Some((arch.as_str(), pair_position(items)?)))
        .collect();
    let tail_len = by_arch.iter().find_map(|(arch, items)| {
        let at = *marker_at.get(arch.as_str())?;
        Some(items.first()?.token.as_deref()?.len() - at - 1)
    });
    let mut result = BTreeMap::new();
    for (arch, items) in &by_arch {
        let mut built: Option<Built> = None;
        for item in items {
            let current = match &item.token {
                Some(token) => split_token(
                    token,
                    group,
                    marker_at.get(arch.as_str()).copied(),
                    tail_len,
                )?,
                None => Built {
                    build: item.artifact.version_code.clone(),
                    tail: None,
                },
            };
            if built.as_ref().is_some_and(|previous| *previous != current) {
                bail!("захваты {arch} разных сборок: {built:?} и {current:?}");
            }
            built = Some(current);
        }
        if let Some(built) = built {
            result.insert(arch.clone(), built);
        }
    }
    Ok(result)
}

fn built_for_key<'a>(key: &str, built: &'a BTreeMap<String, Built>) -> Result<&'a Built> {
    match key {
        "x64" | "arm64" => built.get(key).ok_or_else(|| {
            anyhow!("нет захвата для [builds.{key}]: профиль объявляет эту архитектуру")
        }),
        _ => built
            .get("x64")
            .or_else(|| built.values().next())
            .ok_or_else(|| anyhow!("нет захватов для [builds.{key}]")),
    }
}

fn edit_profile(
    text: &str,
    base: &Profile,
    version: &str,
    built: &BTreeMap<String, Built>,
) -> Result<String> {
    let mut text = edit::set_value(text, None, "version", version)?;
    for key in base.builds.keys() {
        let section = format!("builds.{key}");
        let found = built_for_key(key, built)?;
        if let Some(build) = &found.build {
            text = edit::set_value(&text, Some(&section), "build", build)?;
        }
        if let Some(tail) = &found.tail {
            text = edit::set_value(&text, Some(&section), "tail", tail)?;
        }
    }
    Ok(text)
}

fn capture_locale(profile: &Profile, request: &Request) -> Result<String> {
    let value = |var: Var| header_name(profile, var).and_then(|name| request.header(&name));
    let Some(device_locale) = value(Var::DeviceLocale) else {
        return Ok(String::new());
    };
    let accept = value(Var::AcceptLanguage);
    let lower = device_locale.to_ascii_lowercase();
    let candidates = if device_locale.bytes().any(|b| b.is_ascii_lowercase()) {
        [device_locale, lower.as_str()]
    } else {
        [lower.as_str(), device_locale]
    };
    candidates
        .into_iter()
        .find(|candidate| {
            let applied = profile.locale.apply(candidate);
            applied.device_locale == device_locale
                && accept
                    .is_none_or(|expected| applied.accept_language.as_deref() == Some(expected))
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("локаль «{device_locale}» не воспроизводится правилом локали профиля")
        })
}

/// Устройство, которое при этом профиле даёт ровно такой запрос.
fn derive_device(profile: &Profile, arch: Arch, request: &Request) -> Result<Device> {
    let exact = |var: Var| header_name(profile, var).and_then(|name| request.header(&name));
    let mut device = Device::from_machine_id(MACHINE_ID);
    device.hwid = exact(Var::Hwid).map(str::to_owned);
    device.os_version = exact(Var::OsVersion).map(str::to_owned);
    device.locale = capture_locale(profile, request)?;

    let model_header = profile
        .headers
        .iter()
        .find(|(_, template)| template.0.contains(&Part::Var(Var::Model)));
    if let Some((name, template)) = model_header {
        let value = request
            .header(name)
            .ok_or_else(|| anyhow!("в захвате нет заголовка {name}"))?;
        let found = unrender(&pieces(template, |_| None), value).ok_or_else(|| {
            anyhow!("значение заголовка {name} «{value}» не соответствует шаблону профиля")
        })?;
        let model = value_of(&found, Var::Model).unwrap_or_default();
        if profile.model.0.contains(&Part::Var(Var::Hostname)) {
            let cpu = profile.build_for(arch)?.cpu.clone();
            let known = |var: Var| if var == Var::Cpu { cpu.clone() } else { None };
            let from_model = unrender(&pieces(&profile.model, known), model).ok_or_else(|| {
                anyhow!("модель «{model}» не соответствует шаблону device.model профиля")
            })?;
            device.hostname = value_of(&from_model, Var::Hostname).map(str::to_owned);
        } else {
            device.model = Some(model.to_owned());
        }
        if profile.manufacturer.is_some() {
            device.manufacturer = value_of(&found, Var::Manufacturer).map(str::to_owned);
        }
    }
    Ok(device)
}

/// Часы Android-приложения идут по местной дате эмулятора (UTC), остальные профили
/// считают по Москве: так же устроен тест захватов.
fn utc_offset(profile: &Profile) -> Option<i64> {
    (profile.marker == Some(Marker::DeviceLocalDayParity)).then_some(0)
}

fn shifted(unix: u64, offset: Option<i64>) -> Result<u64> {
    let seconds = (offset.unwrap_or(MOSCOW_OFFSET_HOURS) - MOSCOW_OFFSET_HOURS) * 3600;
    unix.checked_add_signed(seconds)
        .ok_or_else(|| anyhow!("момент захвата вне допустимого диапазона"))
}

fn render(emulation: &Emulation, url: &Url, unix: u64) -> Result<String> {
    let mut request = format!("GET {} HTTP/1.1\r\n", url.target);
    for (name, value) in emulation.headers(url, unix) {
        write!(request, "{name}: {value}\r\n")?;
    }
    request.push_str("\r\n");
    Ok(request)
}

fn quoted(value: &str) -> Result<String> {
    if value.chars().any(char::is_control) {
        bail!("значение содержит управляющие символы");
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// `1790726584` → `1_790_726_584`, как в остальных образцах.
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut groups = Vec::new();
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(3);
        groups.push(&digits[start..end]);
        end = start;
    }
    groups.reverse();
    groups.join("_")
}

fn capture_file(
    inputs: &Inputs<'_>,
    profile: &Profile,
    artifact: &Artifact,
    version: &str,
) -> Result<CaptureFile> {
    let name = &artifact.dir_name;
    let device = derive_device(profile, artifact.arch, &artifact.request)
        .with_context(|| format!("захват {name}"))?;
    let emulation = Emulation::with_profile(profile.clone(), artifact.arch, &device)?;
    let host = artifact
        .request
        .header("Host")
        .ok_or_else(|| anyhow!("в захвате {name} нет заголовка Host"))?;
    let Some(authority) = host.strip_suffix(CAPTURE_PORT) else {
        bail!("захват {name}: порт сервера захвата не 18080, тест подставляет именно его");
    };
    let url = Url::parse(&format!("http://{host}{}", artifact.request.target))?;
    let offset = utc_offset(profile);

    let mut first_try = None;
    let mut matched = None;
    for instant in &artifact.instants {
        let rendered = render(&emulation, &url, shifted(instant.unix, offset)?)?;
        if rendered == artifact.request.text {
            matched = Some(instant);
            break;
        }
        first_try.get_or_insert(rendered);
    }
    let Some(instant) = matched else {
        let rendered = first_try.unwrap_or_default();
        bail!(
            "Профиль {}/{} не воспроизводит захват {name}. Строки запроса, которые отличаются:\n{}",
            inputs.app,
            inputs.platform,
            report::diff_lines(&artifact.request.text, &rendered).join("\n")
        );
    };

    let defaults = Emulation::with_profile(
        profile.clone(),
        artifact.arch,
        &Device::from_machine_id(MACHINE_ID),
    )?
    .device_info();
    let arch_part = if profile.builds.contains_key(&artifact.arch.to_string()) {
        format!("-{}", artifact.arch)
    } else {
        String::new()
    };
    let odd_part = if artifact.odd_day { "-odd-day" } else { "" };
    let stem = format!(
        "{}-{version}-{}{arch_part}{odd_part}",
        inputs.app, inputs.platform
    );

    let sample = Sample {
        inputs,
        artifact,
        authority,
        instant,
        offset,
        device: &device,
        defaults: &defaults,
    };
    Ok(CaptureFile {
        stem,
        http: artifact.request.text.clone(),
        toml: sample.toml()?,
    })
}

/// Всё, что нужно для файла условий захвата.
struct Sample<'a> {
    inputs: &'a Inputs<'a>,
    artifact: &'a Artifact,
    authority: &'a str,
    instant: &'a Instant,
    offset: Option<i64>,
    device: &'a Device,
    defaults: &'a DeviceInfo,
}

impl Sample<'_> {
    fn toml(&self) -> Result<String> {
        let (inputs, artifact, device) = (self.inputs, self.artifact, self.device);
        let mut toml = String::new();
        writeln!(toml, "app = {}", quoted(inputs.app)?)?;
        writeln!(toml, "platform = {}", quoted(&inputs.platform.to_string())?)?;
        writeln!(toml, "arch = {}", quoted(&artifact.arch.to_string())?)?;
        writeln!(toml, "release = {}", quoted(inputs.release)?)?;
        let url = format!(
            "http://{}:{{PORT}}{}",
            self.authority, artifact.request.target
        );
        writeln!(toml, "url = {}", quoted(&url)?)?;
        writeln!(toml, "# Часы захвата: {}.", self.instant.label)?;
        writeln!(toml, "unix = {}", grouped(self.instant.unix))?;
        if let Some(hours) = self.offset {
            writeln!(toml, "utc_offset_hours = {hours}")?;
        }
        writeln!(toml, "machine_id = {}", quoted(MACHINE_ID)?)?;
        let defaults = self.defaults;
        let overrides = [
            ("hwid", device.hwid.as_deref()),
            ("hostname", device.hostname.as_deref()),
            (
                "model",
                device.model.as_deref().filter(|m| *m != defaults.model),
            ),
            (
                "manufacturer",
                device
                    .manufacturer
                    .as_deref()
                    .filter(|m| Some(*m) != defaults.manufacturer.as_deref()),
            ),
            (
                "os_version",
                device
                    .os_version
                    .as_deref()
                    .filter(|v| *v != defaults.os_version),
            ),
        ];
        for (key, value) in overrides {
            if let Some(value) = value {
                writeln!(toml, "{key} = {}", quoted(value)?)?;
            }
        }
        writeln!(toml, "locale = {}", quoted(&device.locale)?)?;
        Ok(toml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: &str = include_str!("../../profiles/happ/windows.toml");
    const ANDROID: &str = include_str!("../../profiles/happ/android.toml");
    const INCY: &str = include_str!("../../profiles/incy/android.toml");

    // 2026-09-28 12:00:00 UTC: чётное число и по Москве, и в UTC.
    const EVEN_DAY: u64 = 1_790_596_800;
    const ODD_DAY: u64 = EVEN_DAY + 86_400;

    fn set(text: &str, section: Option<&str>, key: &str, value: &str) -> String {
        edit::set_value(text, section, key, value).unwrap()
    }

    fn windows_updated() -> String {
        let mut text = set(WINDOWS, None, "version", "9.9.9");
        text = set(&text, Some("builds.x64"), "build", "1111111111");
        text = set(&text, Some("builds.x64"), "tail", "07");
        text = set(&text, Some("builds.arm64"), "build", "2222222222");
        set(&text, Some("builds.arm64"), "tail", "07")
    }

    fn android_updated() -> String {
        let text = set(ANDROID, None, "version", "9.9.9");
        let text = set(&text, Some("builds.any"), "build", "12345678901234567");
        set(&text, Some("builds.any"), "tail", "99")
    }

    fn incy_updated() -> String {
        let text = set(INCY, None, "version", "9.9.9");
        set(&text, Some("builds.any"), "build", "999")
    }

    fn shoot(
        profile_text: &str,
        arch: Arch,
        device: &Device,
        instant: u64,
        name: &str,
    ) -> Artifact {
        let profile = Profile::parse(profile_text).unwrap();
        let emulation = Emulation::with_profile(profile.clone(), arch, device).unwrap();
        let url = Url::parse("http://127.0.0.1:18080/sub/capture-test").unwrap();
        let unix = shifted(instant, utc_offset(&profile)).unwrap();
        let text = render(&emulation, &url, unix).unwrap();
        Artifact {
            dir_name: name.to_owned(),
            arch,
            odd_day: name.ends_with("-odd-day"),
            request: Request::parse(text.as_bytes()).unwrap(),
            instants: vec![Instant {
                unix: instant,
                label: "fixture clock".to_owned(),
            }],
            version_code: None,
        }
    }

    fn run(app: &str, platform: Platform, old: &str, artifacts: &[Artifact]) -> Result<Output> {
        build(&Inputs {
            app,
            platform,
            release: "9.9.9",
            profile_text: old,
            artifacts,
        })
    }

    fn windows_device(hostname: &str, hwid: &str) -> Device {
        Device {
            hostname: Some(hostname.to_owned()),
            hwid: Some(hwid.to_owned()),
            locale: "en".to_owned(),
            ..Device::from_machine_id(MACHINE_ID)
        }
    }

    fn windows_artifacts(updated: &str) -> Vec<Artifact> {
        let x64 = windows_device("runnervmaaaaa", "00000000-0000-4000-8000-000000000001");
        let arm = Device {
            os_version: Some("11_10.0.26200".to_owned()),
            ..windows_device("runnervmbbbbb", "00000000-0000-4000-8000-000000000002")
        };
        vec![
            shoot(
                updated,
                Arch::X64,
                &x64,
                EVEN_DAY,
                "capture-happ-windows-x64-9.9.9",
            ),
            shoot(
                updated,
                Arch::X64,
                &x64,
                ODD_DAY,
                "capture-happ-windows-x64-9.9.9-odd-day",
            ),
            shoot(
                updated,
                Arch::Arm64,
                &arm,
                EVEN_DAY,
                "capture-happ-windows-arm64-9.9.9",
            ),
        ]
    }

    #[test]
    fn windows_profile_follows_the_captures() {
        let updated = windows_updated();
        let output = run(
            "happ",
            Platform::Windows,
            WINDOWS,
            &windows_artifacts(&updated),
        )
        .unwrap();
        assert_eq!(output.profile_text, updated);
        let stems: Vec<&str> = output.captures.iter().map(|c| c.stem.as_str()).collect();
        assert_eq!(
            stems,
            [
                "happ-9.9.9-windows-arm64",
                "happ-9.9.9-windows-x64",
                "happ-9.9.9-windows-x64-odd-day"
            ]
        );
        let (arm, x64) = (&output.captures[0], &output.captures[1]);
        assert!(
            arm.toml.contains("os_version = \"11_10.0.26200\"\n"),
            "{}",
            arm.toml
        );
        assert!(!x64.toml.contains("os_version"), "{}", x64.toml);
        assert!(x64.toml.contains("hostname = \"runnervmaaaaa\"\n"));
        assert!(
            x64.toml
                .contains("hwid = \"00000000-0000-4000-8000-000000000001\"\n")
        );
        assert!(x64.toml.contains("release = \"9.9.9\"\n"));
        assert!(
            x64.toml
                .contains("url = \"http://127.0.0.1:{PORT}/sub/capture-test\"\n")
        );
        assert!(x64.toml.contains("unix = 1_790_596_800\n"));
        assert!(x64.toml.contains("locale = \"en\"\n"));
        assert!(!x64.toml.contains("utc_offset_hours"));
        assert!(
            x64.http
                .starts_with("GET /sub/capture-test HTTP/1.1\r\nHost: 127.0.0.1:18080\r\n")
        );
        assert!(output.summary.contains("версия"), "{}", output.summary);
    }

    #[test]
    fn android_profile_follows_the_captures() {
        let updated = android_updated();
        let device = Device {
            model: Some("sdk_gphone64_x86_64".to_owned()),
            hwid: Some("0123456789abcdef".to_owned()),
            locale: "en".to_owned(),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let artifacts = [
            shoot(
                &updated,
                Arch::X64,
                &device,
                EVEN_DAY,
                "capture-happ-android-x86_64-9.9.9",
            ),
            shoot(
                &updated,
                Arch::X64,
                &device,
                ODD_DAY,
                "capture-happ-android-x86_64-9.9.9-odd-day",
            ),
        ];
        let output = run("happ", Platform::Android, ANDROID, &artifacts).unwrap();
        assert_eq!(output.profile_text, updated);
        let stems: Vec<&str> = output.captures.iter().map(|c| c.stem.as_str()).collect();
        assert_eq!(stems, ["happ-9.9.9-android", "happ-9.9.9-android-odd-day"]);
        let toml = &output.captures[0].toml;
        assert!(toml.contains("utc_offset_hours = 0\n"), "{toml}");
        assert!(toml.contains("model = \"sdk_gphone64_x86_64\"\n"), "{toml}");
        assert!(toml.contains("hwid = \"0123456789abcdef\"\n"), "{toml}");
        assert!(!toml.contains("hostname"), "{toml}");
    }

    #[test]
    fn incy_profile_takes_the_build_from_the_apk() {
        let updated = incy_updated();
        let device = Device {
            model: Some("sdk_gphone64_x86_64".to_owned()),
            manufacturer: Some("Google".to_owned()),
            hwid: Some("60C76286-FA42-CA7E-66EC-E0B059F47E23".to_owned()),
            locale: "en_US".to_owned(),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let mut artifact = shoot(
            &updated,
            Arch::X64,
            &device,
            EVEN_DAY,
            "capture-incy-android-x86_64-9.9.9",
        );
        artifact.version_code = Some("999".to_owned());
        let output = run("incy", Platform::Android, INCY, &[artifact]).unwrap();
        assert_eq!(output.profile_text, updated);
        let toml = &output.captures[0].toml;
        assert_eq!(output.captures[0].stem, "incy-9.9.9-android");
        assert!(toml.contains("manufacturer = \"Google\"\n"), "{toml}");
        assert!(toml.contains("model = \"sdk_gphone64_x86_64\"\n"), "{toml}");
        assert!(toml.contains("locale = \"en_US\"\n"), "{toml}");
        assert!(!toml.contains("utc_offset_hours"), "{toml}");
    }

    #[test]
    fn a_new_header_is_reported_with_a_diff() {
        let changed = format!(
            "{}\n[[headers]]\nname = \"X-New\"\nvalue = \"1\"\n",
            windows_updated()
        );
        let artifacts = windows_artifacts(&changed);
        let error = run("happ", Platform::Windows, WINDOWS, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Заголовки запроса изменились"), "{error}");
        assert!(error.contains("  + X-New"), "{error}");
        assert!(error.contains("    Accept-Language"), "{error}");
    }

    #[test]
    fn a_changed_user_agent_format_is_reported() {
        let changed = windows_updated().replace("user_agent = \"Happ/", "user_agent = \"HappPro/");
        let artifacts = windows_artifacts(&changed);
        let error = run("happ", Platform::Windows, WINDOWS, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("User-Agent в захвате"), "{error}");
        assert!(
            error.contains("Happ/{app_version}/Windows/{build}{marker}{tail}"),
            "{error}"
        );
    }

    #[test]
    fn without_a_day_pair_the_build_boundary_is_unknown() {
        let updated = android_updated();
        let device = Device {
            model: Some("sdk".to_owned()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let artifacts = [shoot(
            &updated,
            Arch::X64,
            &device,
            EVEN_DAY,
            "capture-happ-android-x86_64-9.9.9",
        )];
        let error = run("happ", Platform::Android, ANDROID, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("нет пары захватов"), "{error}");
    }

    #[test]
    fn a_marker_that_does_not_follow_the_clock_is_reported() {
        let updated = windows_updated();
        let mut artifacts = windows_artifacts(&updated);
        artifacts[1].instants[0].unix = EVEN_DAY;
        let error = run("happ", Platform::Windows, WINDOWS, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("не воспроизводит захват capture-happ-windows-x64-9.9.9-odd-day"),
            "{error}"
        );
        assert!(
            error.contains("захват:  User-Agent: Happ/9.9.9/Windows/1111111111507"),
            "{error}"
        );
        assert!(
            error.contains("профиль: User-Agent: Happ/9.9.9/Windows/1111111111607"),
            "{error}"
        );
    }

    #[test]
    fn captures_of_different_versions_are_refused() {
        let updated = windows_updated();
        let mut artifacts = windows_artifacts(&updated);
        let other = windows_updated().replace("9.9.9", "9.9.8");
        artifacts[2] = shoot(
            &other,
            Arch::Arm64,
            &windows_device("runnervmbbbbb", "00000000-0000-4000-8000-000000000002"),
            EVEN_DAY,
            "capture-happ-windows-arm64-9.9.8",
        );
        let error = run("happ", Platform::Windows, WINDOWS, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("захваты разных версий"), "{error}");
    }

    #[test]
    fn a_missing_architecture_is_an_error() {
        let updated = windows_updated();
        let mut artifacts = windows_artifacts(&updated);
        artifacts.remove(2);
        let error = run("happ", Platform::Windows, WINDOWS, &artifacts)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("[builds.arm64]"), "{error}");
    }

    #[test]
    fn dates_are_grouped_like_the_other_captures() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1_000");
        assert_eq!(grouped(1_790_596_800), "1_790_596_800");
    }

    #[test]
    fn single_difference_needs_exactly_one_changed_character() {
        assert_eq!(single_difference("12345", "12945"), Some(2));
        assert_eq!(single_difference("12345", "12345"), None);
        assert_eq!(single_difference("12345", "92945"), None);
        assert_eq!(single_difference("1234", "12345"), None);
    }

    #[test]
    fn toml_strings_are_escaped() {
        assert_eq!(quoted("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quoted("a\nb").is_err());
    }
}
