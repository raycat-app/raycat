//! Профиль клиента: данные из `profiles/<приложение>/<платформа>.toml`.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::device::HwidAlgorithm;
use crate::locale::LocaleRule;
use crate::marker;
use crate::template::{Template, Var};

/// Операционная система эмулируемого приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Android,
}

impl Platform {
    fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Android => "android",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Platform {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "windows" => Ok(Self::Windows),
            "android" => Ok(Self::Android),
            _ => bail!("неизвестная платформа {s:?}: доступны windows, android"),
        }
    }
}

/// Архитектура процессора эмулируемого приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    fn as_str(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Arch {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "x64" | "x86_64" | "amd64" => Ok(Self::X64),
            "arm64" | "aarch64" => Ok(Self::Arm64),
            _ => bail!("неизвестная архитектура {s:?}: доступны x64, arm64"),
        }
    }
}

/// Алгоритм дневного маркера User-Agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    MoscowDayParity,
}

impl Marker {
    fn from_name(name: &str) -> Result<Self> {
        match name {
            "moscow-day-parity" => Ok(Self::MoscowDayParity),
            _ => bail!("неизвестный алгоритм маркера {name:?}: доступен moscow-day-parity"),
        }
    }

    pub(crate) fn at(self, unix: u64) -> char {
        match self {
            Self::MoscowDayParity => marker::moscow_day_parity(unix),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Build {
    pub(crate) build: String,
    pub(crate) tail: String,
    pub(crate) cpu: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Profile {
    pub(crate) app: String,
    pub(crate) platform: Platform,
    pub(crate) version: String,
    pub(crate) marker: Option<Marker>,
    pub(crate) user_agent: Template,
    pub(crate) os: String,
    pub(crate) os_version: String,
    pub(crate) model: Template,
    pub(crate) hwid: HwidAlgorithm,
    pub(crate) locale: LocaleRule,
    builds: BTreeMap<String, Build>,
    pub(crate) headers: Vec<(String, Template)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    app: String,
    platform: String,
    version: String,
    marker: Option<String>,
    user_agent: String,
    device: RawDevice,
    builds: BTreeMap<String, RawBuild>,
    headers: Vec<RawHeader>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    os: String,
    os_version: String,
    model: String,
    hwid: String,
    locale: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    build: String,
    #[serde(default)]
    tail: String,
    cpu: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeader {
    name: String,
    value: String,
}

/// Ключ таблицы `[builds.*]` для профиля, не зависящего от архитектуры.
const ANY_ARCH: &str = "any";

impl Profile {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let raw: RawProfile = toml::from_str(text).context("не разобрать TOML")?;
        Self::validate(raw)
    }

    /// Сборка для архитектуры; профиль без разбивки по архитектурам подходит всем.
    pub(crate) fn build_for(&self, arch: Arch) -> Result<&Build> {
        self.builds
            .get(arch.as_str())
            .or_else(|| self.builds.get(ANY_ARCH))
            .ok_or_else(|| {
                anyhow!(
                    "профиль {}/{} не поддерживает архитектуру {arch}",
                    self.app,
                    self.platform
                )
            })
    }

    fn validate(raw: RawProfile) -> Result<Self> {
        let platform: Platform = raw.platform.parse()?;
        if raw.app.is_empty()
            || !raw
                .app
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            bail!("app должен состоять из строчных латинских букв, цифр и «-»");
        }
        non_empty("version", &raw.version)?;
        non_empty("device.os", &raw.device.os)?;
        non_empty("device.os_version", &raw.device.os_version)?;
        non_empty("device.model", &raw.device.model)?;

        let marker = raw.marker.as_deref().map(Marker::from_name).transpose()?;
        let hwid = HwidAlgorithm::from_name(&raw.device.hwid).ok_or_else(|| {
            anyhow!(
                "неизвестный алгоритм HWID {:?}: доступны {}",
                raw.device.hwid,
                HwidAlgorithm::NAMES
            )
        })?;
        let locale = LocaleRule::from_name(&raw.device.locale).ok_or_else(|| {
            anyhow!(
                "неизвестное правило локали {:?}: доступны {}",
                raw.device.locale,
                LocaleRule::NAMES
            )
        })?;

        let builds = validate_builds(raw.builds)?;
        let user_agent = Template::parse(&raw.user_agent).context("user_agent")?;
        if user_agent.vars().any(|var| var == Var::UserAgent) {
            bail!("user_agent не может содержать {{user_agent}}");
        }
        let model = Template::parse(&raw.device.model).context("device.model")?;
        let headers = validate_headers(raw.headers)?;

        let profile = Self {
            app: raw.app,
            platform,
            version: raw.version,
            marker,
            user_agent,
            os: raw.device.os,
            os_version: raw.device.os_version,
            model,
            hwid,
            locale,
            builds,
            headers,
        };
        profile.check_substitutions()?;
        Ok(profile)
    }

    /// Подстановка, которой профиль не может обеспечить, — ошибка профиля, а не
    /// пустое значение в запросе.
    fn check_substitutions(&self) -> Result<()> {
        // Модель вычисляется до запроса: значений, зависящих от запроса, у неё нет.
        for var in self.model.vars() {
            if matches!(var, Var::Host | Var::UserAgent | Var::Marker | Var::Model) {
                bail!("device.model: подстановка {{{}}} недоступна", var.name());
            }
        }
        let templates = std::iter::once(("user_agent", &self.user_agent))
            .chain(std::iter::once(("device.model", &self.model)))
            .chain(self.headers.iter().map(|(name, t)| (name.as_str(), t)));
        for (place, template) in templates {
            for var in template.vars() {
                let available = match var {
                    Var::Marker => self.marker.is_some(),
                    Var::AcceptLanguage => self.locale.provides_accept_language(),
                    Var::Cpu => self.builds.values().all(|b| b.cpu.is_some()),
                    _ => true,
                };
                if !available {
                    bail!("{place}: подстановка {{{}}} недоступна профилю", var.name());
                }
            }
        }
        Ok(())
    }
}

fn validate_builds(raw: BTreeMap<String, RawBuild>) -> Result<BTreeMap<String, Build>> {
    if raw.is_empty() {
        bail!("не задана ни одна сборка [builds.*]");
    }
    let mut builds = BTreeMap::new();
    for (key, build) in raw {
        if !matches!(key.as_str(), "x64" | "arm64" | ANY_ARCH) {
            bail!("неизвестная архитектура [builds.{key}]: доступны x64, arm64, any");
        }
        non_empty(&format!("builds.{key}.build"), &build.build)?;
        if build.cpu.as_deref() == Some("") {
            bail!("builds.{key}.cpu пуст");
        }
        builds.insert(
            key,
            Build {
                build: build.build,
                tail: build.tail,
                cpu: build.cpu,
            },
        );
    }
    Ok(builds)
}

fn validate_headers(raw: Vec<RawHeader>) -> Result<Vec<(String, Template)>> {
    if raw.is_empty() {
        bail!("не задан ни один заголовок [[headers]]");
    }
    let mut headers: Vec<(String, Template)> = Vec::with_capacity(raw.len());
    for header in raw {
        if !is_token(&header.name) {
            bail!("недопустимое имя заголовка {:?}", header.name);
        }
        if headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&header.name))
        {
            bail!("заголовок {} задан дважды", header.name);
        }
        let value = Template::parse(&header.value)
            .with_context(|| format!("заголовок {}", header.name))?;
        headers.push((header.name, value));
    }
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        bail!("нет заголовка Host");
    }
    Ok(headers)
}

fn non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} пуст");
    }
    Ok(())
}

/// Имя заголовка по RFC 9110: `token`.
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

pub(crate) struct Source {
    pub(crate) app: &'static str,
    pub(crate) platform: Platform,
    pub(crate) text: &'static str,
}

/// Встроенные профили. Новый профиль — новый файл в `profiles/` и строка здесь.
pub(crate) const SOURCES: &[Source] = &[
    Source {
        app: "happ",
        platform: Platform::Windows,
        text: include_str!("../profiles/happ/windows.toml"),
    },
    Source {
        app: "happ",
        platform: Platform::Android,
        text: include_str!("../profiles/happ/android.toml"),
    },
];

pub(crate) fn find(app: &str, platform: Platform) -> Result<Profile> {
    let Some(source) = SOURCES
        .iter()
        .find(|s| s.app.eq_ignore_ascii_case(app) && s.platform == platform)
    else {
        let available: Vec<String> = SOURCES
            .iter()
            .map(|s| format!("{}/{}", s.app, s.platform))
            .collect();
        bail!(
            "нет профиля {app}/{platform}, доступны: {}",
            available.join(", ")
        );
    };
    Profile::parse(source.text).with_context(|| format!("профиль {}/{}", source.app, source.platform))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
app = "test"
platform = "windows"
version = "1.0.0"
marker = "moscow-day-parity"
user_agent = "T/{app_version}/{build}{marker}{tail}"

[device]
os = "Windows"
os_version = "11"
model = "{hostname}_{cpu}"
hwid = "windows-machine-guid"
locale = "qt-windows"

[builds.x64]
build = "1"
tail = "00"
cpu = "x86_64"

[[headers]]
name = "Host"
value = "{host}"

[[headers]]
name = "Accept-Language"
value = "{accept_language}"
"#;

    fn error_of(text: &str) -> String {
        format!("{:#}", Profile::parse(text).unwrap_err())
    }

    #[test]
    fn every_embedded_profile_is_valid() {
        for source in SOURCES {
            let profile = Profile::parse(source.text)
                .unwrap_or_else(|e| panic!("{}/{}: {e:#}", source.app, source.platform));
            assert_eq!(profile.app, source.app);
            assert_eq!(profile.platform, source.platform);
            assert!(!profile.version.is_empty());
        }
    }

    #[test]
    fn embedded_profiles_match_the_files_on_disk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles");
        let mut on_disk = Vec::new();
        for app in std::fs::read_dir(&dir).unwrap() {
            let app = app.unwrap().path();
            for file in std::fs::read_dir(&app).unwrap() {
                let file = file.unwrap().path();
                assert_eq!(file.extension().and_then(|e| e.to_str()), Some("toml"));
                on_disk.push(format!(
                    "{}/{}",
                    app.file_name().unwrap().to_str().unwrap(),
                    file.file_stem().unwrap().to_str().unwrap()
                ));
            }
        }
        let mut embedded: Vec<String> = SOURCES
            .iter()
            .map(|s| format!("{}/{}", s.app, s.platform))
            .collect();
        on_disk.sort();
        embedded.sort();
        assert_eq!(on_disk, embedded, "профиль не зарегистрирован в SOURCES");
    }

    #[test]
    fn valid_profile_parses() {
        let profile = Profile::parse(VALID).unwrap();
        assert_eq!(profile.app, "test");
        assert_eq!(profile.platform, Platform::Windows);
        assert_eq!(profile.build_for(Arch::X64).unwrap().build, "1");
        assert!(profile.build_for(Arch::Arm64).is_err());
    }

    #[test]
    fn any_build_fits_every_architecture() {
        let text = VALID.replace("[builds.x64]", "[builds.any]");
        let profile = Profile::parse(&text).unwrap();
        assert_eq!(profile.build_for(Arch::X64).unwrap().build, "1");
        assert_eq!(profile.build_for(Arch::Arm64).unwrap().build, "1");
    }

    #[test]
    fn unknown_substitution_is_an_error() {
        let text = VALID.replace("{host}", "{hots}");
        assert!(error_of(&text).contains("{hots}"), "{}", error_of(&text));
    }

    #[test]
    fn empty_version_is_an_error() {
        let text = VALID.replace("version = \"1.0.0\"", "version = \"\"");
        assert!(error_of(&text).contains("version"));
        let text = VALID.replace("build = \"1\"", "build = \"\"");
        assert!(error_of(&text).contains("builds.x64.build"));
    }

    #[test]
    fn unavailable_substitutions_are_errors() {
        let no_marker = VALID.replace("marker = \"moscow-day-parity\"\n", "");
        assert!(error_of(&no_marker).contains("{marker}"));
        let no_cpu = VALID.replace("cpu = \"x86_64\"\n", "");
        assert!(error_of(&no_cpu).contains("{cpu}"));
        let android_locale = VALID.replace("qt-windows", "android");
        assert!(error_of(&android_locale).contains("{accept_language}"));
        let request_model = VALID.replace("{hostname}_{cpu}", "{host}");
        assert!(error_of(&request_model).contains("device.model"));
        let recursive = VALID.replace("T/{app_version}", "T/{user_agent}");
        assert!(error_of(&recursive).contains("user_agent"));
    }

    #[test]
    fn unknown_names_and_typos_are_errors() {
        assert!(error_of(&VALID.replace("moscow-day-parity", "x")).contains("маркера"));
        assert!(error_of(&VALID.replace("windows-machine-guid", "x")).contains("HWID"));
        assert!(error_of(&VALID.replace("qt-windows", "x")).contains("локали"));
        assert!(error_of(&VALID.replace("platform = \"windows\"", "platform = \"ios\"")).contains("платформа"));
        assert!(error_of(&VALID.replace("[builds.x64]", "[builds.mips]")).contains("mips"));
        assert!(Profile::parse(&VALID.replace("version =", "versoin =")).is_err());
        assert!(Profile::parse("").is_err());
    }

    #[test]
    fn broken_headers_are_errors() {
        let dup = format!("{VALID}\n[[headers]]\nname = \"host\"\nvalue = \"x\"\n");
        assert!(error_of(&dup).contains("дважды"));
        let bad_name = VALID.replace("Accept-Language", "Accept Language");
        assert!(error_of(&bad_name).contains("имя заголовка"));
        let no_host = VALID.replace("\"Host\"", "\"Hos\"");
        assert!(error_of(&no_host).contains("Host"));
    }

    #[test]
    fn platform_and_arch_parse_from_settings() {
        assert_eq!("Windows".parse::<Platform>().unwrap(), Platform::Windows);
        assert_eq!("android".parse::<Platform>().unwrap(), Platform::Android);
        assert!("ios".parse::<Platform>().is_err());
        assert_eq!("x64".parse::<Arch>().unwrap(), Arch::X64);
        assert_eq!("aarch64".parse::<Arch>().unwrap(), Arch::Arm64);
        assert!("mips".parse::<Arch>().is_err());
        assert_eq!(Platform::Windows.to_string(), "windows");
        assert_eq!(Arch::Arm64.to_string(), "arm64");
    }

    #[test]
    fn unknown_profile_lists_the_available_ones() {
        let error = find("nope", Platform::Windows).unwrap_err().to_string();
        assert!(error.contains("happ/windows"), "{error}");
        assert!(find("HAPP", Platform::Android).is_ok());
    }
}
