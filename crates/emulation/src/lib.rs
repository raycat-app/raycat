//! Эмуляция официальных приложений подписок (Happ для Windows и Android, INCY для
//! Android).
//!
//! Панели провайдеров выбирают формат ответа и проверяют клиента по заголовкам
//! запроса, поэтому запрос строится побайтно как у приложения: порядок и регистр
//! заголовков, User-Agent, идентификатор устройства.
//!
//! # Профили
//!
//! Профиль — файл `profiles/<приложение>/<платформа>.toml`, встроенный в бинарник.
//! Новая версия приложения меняет только данные профиля. В коде остаются алгоритмы:
//! вывод идентификаторов из seed, дневной маркер User-Agent, правила локали.
//!
//! ```toml
//! app = "happ"                    # строчные латинские буквы, цифры и «-»
//! platform = "windows"            # windows | android
//! version = "4.3.0"
//! marker = "moscow-day-parity"    # необязательно, для {marker}: moscow-day-parity
//!                                 # (по Москве, Windows) | device-local-day-parity
//!                                 # (по местной дате устройства, Android)
//! user_agent = "Happ/{app_version}/{os}/{build}{marker}{tail}"
//!
//! [device]
//! os = "Windows"
//! os_version = "11_10.0.26100"    # значение по умолчанию
//! model = "{hostname}_{cpu}"      # шаблон; готовое значение для Android
//! hwid = "windows-machine-guid"   # windows-machine-guid | android-id | incy-uuid
//! locale = "qt-windows"           # qt-windows | android | android-region
//!
//! [builds.x64]                    # x64, arm64 или any (для всех архитектур)
//! build = "2609151455"
//! tail = "03"                     # необязательно
//! cpu = "x86_64"                  # необязательно, для {cpu}
//!
//! [[headers]]                     # порядок и регистр имён — как на проводе
//! name = "Host"
//! value = "{host}"
//! ```
//!
//! Подстановки: `{host}`, `{user_agent}` (только в заголовках), `{app_version}`,
//! `{build}`, `{tail}`, `{marker}`, `{cpu}`, `{os}`, `{os_version}`, `{model}`,
//! `{hostname}`, `{hwid}`, `{device_locale}`, `{accept_language}`. Профиль с
//! неизвестной или недоступной подстановкой, пустым обязательным полем, дублем
//! заголовка или без `Host` не загружается: это ловит тест `profile::tests`.

mod device;
mod locale;
mod marker;
mod profile;
mod template;

pub use device::{Device, DeviceInfo, is_valid_hwid, machine_id_from_seed};
pub use profile::{Arch, Platform};
pub use raycat_http::Url;

use anyhow::{Result, bail};

use device::windows_computer_name;
use locale::Locale;
use profile::{Profile, Release};
use template::Values;

/// Профиль клиента вместе с данными устройства: строит запросы подписки.
#[derive(Debug, Clone)]
pub struct Emulation {
    profile: Profile,
    build: Release,
    hwid: String,
    os_version: String,
    model: String,
    hostname: String,
    locale: Locale,
}

impl Emulation {
    /// Профиль `app`/`platform` для архитектуры `arch` на устройстве `device`.
    /// Архитектуру игнорируют профили с единой сборкой (`[builds.any]`).
    pub fn new(app: &str, platform: Platform, arch: Arch, device: &Device) -> Result<Self> {
        let profile = profile::find(app, platform)?;
        let build = profile.build_for(arch)?.clone();
        if device.machine_id.trim().is_empty() {
            bail!("machine_id пуст: идентификатор устройства не из чего вывести");
        }
        let hwid = overridden("hwid", device.hwid.as_deref())?
            .unwrap_or_else(|| profile.hwid.derive(&device.machine_id));
        let os_version = overridden("os_version", device.os_version.as_deref())?
            .unwrap_or_else(|| profile.os_version.clone());
        let hostname = overridden("hostname", device.hostname.as_deref())?
            .unwrap_or_else(|| windows_computer_name(&device.machine_id));
        let locale = profile.locale.apply(&device.locale);
        let model = overridden("model", device.model.as_deref())?;
        let mut emulation = Self {
            profile,
            build,
            hwid,
            os_version,
            model: model.clone().unwrap_or_default(),
            hostname,
            locale,
        };
        if model.is_none() {
            let rendered = emulation
                .profile
                .model
                .render(&emulation.values("", "", ""));
            emulation.model = rendered;
        }
        Ok(emulation)
    }

    /// User-Agent в момент `unix` (секунды Unix): маркер Happ меняется по суткам.
    pub fn user_agent(&self, unix: u64) -> String {
        let marker = self.marker(unix);
        self.profile
            .user_agent
            .render(&self.values("", "", &marker))
    }

    /// Заголовки запроса подписки в порядке и регистре приложения.
    pub fn headers(&self, url: &Url, unix: u64) -> Vec<(String, String)> {
        let host = url.host_header();
        let marker = self.marker(unix);
        let user_agent = self
            .profile
            .user_agent
            .render(&self.values("", "", &marker));
        let values = self.values(&host, &user_agent, &marker);
        self.profile
            .headers
            .iter()
            .map(|(name, template)| (name.clone(), clean(&template.render(&values))))
            .collect()
    }

    /// Как устройство представляется провайдеру.
    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            hwid: self.hwid.clone(),
            os: self.profile.os.clone(),
            os_version: self.os_version.clone(),
            model: self.model.clone(),
        }
    }

    pub fn app(&self) -> &str {
        &self.profile.app
    }

    pub fn app_version(&self) -> &str {
        &self.profile.version
    }

    pub fn build(&self) -> &str {
        &self.build.build
    }

    fn marker(&self, unix: u64) -> String {
        self.profile
            .marker
            .map(|marker| marker.at(unix).to_string())
            .unwrap_or_default()
    }

    fn values<'a>(&'a self, host: &'a str, user_agent: &'a str, marker: &'a str) -> Values<'a> {
        Values {
            host,
            user_agent,
            app_version: &self.profile.version,
            build: &self.build.build,
            tail: &self.build.tail,
            marker,
            cpu: self.build.cpu.as_deref().unwrap_or_default(),
            os: &self.profile.os,
            os_version: &self.os_version,
            model: &self.model,
            hostname: &self.hostname,
            hwid: &self.hwid,
            device_locale: &self.locale.device_locale,
            accept_language: self.locale.accept_language.as_deref().unwrap_or_default(),
        }
    }
}

/// Переопределение из настроек без управляющих символов: они испортили бы или
/// подменили заголовки запроса.
fn overridden(name: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = clean(value);
    if value.trim().is_empty() {
        bail!("device.{name} пуст");
    }
    Ok(Some(value))
}

fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE_ID: &str = "0d0af05ee8fd4dc29275718f2ce4dff1";
    // 2026-09-28 12:00:00 UTC: чётное число по Москве, маркер «6».
    const EVEN_DAY: u64 = 1_790_596_800;
    const ODD_DAY: u64 = EVEN_DAY + 86_400;

    fn emulation(platform: Platform, arch: Arch, device: &Device) -> Emulation {
        Emulation::new("happ", platform, arch, device).unwrap()
    }

    fn url() -> Url {
        Url::parse("https://sub.example.com/path/token").unwrap()
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }

    #[test]
    fn windows_user_agent_follows_build_and_moscow_day() {
        let device = Device::from_machine_id(MACHINE_ID);
        let x64 = emulation(Platform::Windows, Arch::X64, &device);
        assert_eq!(x64.user_agent(EVEN_DAY), "Happ/4.3.0/Windows/2609151455603");
        assert_eq!(x64.user_agent(ODD_DAY), "Happ/4.3.0/Windows/2609151455503");
        let arm = emulation(Platform::Windows, Arch::Arm64, &device);
        assert_eq!(arm.user_agent(EVEN_DAY), "Happ/4.3.0/Windows/2609151502603");
        assert_eq!(x64.build(), "2609151455");
        assert_eq!(arm.build(), "2609151502");
        assert_eq!((x64.app(), x64.app_version()), ("happ", "4.3.0"));
    }

    #[test]
    fn android_user_agent_ignores_architecture() {
        let device = Device::from_machine_id(MACHINE_ID);
        let x64 = emulation(Platform::Android, Arch::X64, &device);
        let arm = emulation(Platform::Android, Arch::Arm64, &device);
        assert_eq!(
            x64.user_agent(EVEN_DAY),
            "Happ/4.6.0/Android/17903218884031681667"
        );
        assert_eq!(
            x64.user_agent(ODD_DAY),
            "Happ/4.6.0/Android/17903218884031681567"
        );
        assert_eq!(x64.headers(&url(), EVEN_DAY), arm.headers(&url(), EVEN_DAY));
    }

    #[test]
    fn windows_defaults_come_from_the_machine_id() {
        let device = Device::from_machine_id(MACHINE_ID);
        let info = emulation(Platform::Windows, Arch::X64, &device).device_info();
        assert_eq!(info.hwid, device::windows_machine_guid(MACHINE_ID));
        assert_eq!(info.os, "Windows");
        assert_eq!(info.os_version, "11_10.0.26100");
        assert_eq!(
            info.model,
            format!("{}_x86_64", windows_computer_name(MACHINE_ID))
        );
        let arm = emulation(Platform::Windows, Arch::Arm64, &device).device_info();
        assert_eq!(
            arm.model,
            format!("{}_arm64", windows_computer_name(MACHINE_ID))
        );
        assert_eq!(arm.hwid, info.hwid);
    }

    #[test]
    fn android_defaults_come_from_the_machine_id() {
        let device = Device::from_machine_id(MACHINE_ID);
        let info = emulation(Platform::Android, Arch::X64, &device).device_info();
        assert_eq!(info.hwid, device::android_id(MACHINE_ID));
        assert_eq!(info.os, "Android");
        assert_eq!(info.os_version, "14");
        assert_eq!(info.model, "SM-S921B");
    }

    fn incy(device: &Device) -> Emulation {
        Emulation::new("incy", Platform::Android, Arch::X64, device).unwrap()
    }

    #[test]
    fn incy_defaults_come_from_the_machine_id() {
        let device = Device::from_machine_id(MACHINE_ID);
        let incy = incy(&device);
        let info = incy.device_info();
        assert_eq!(info.hwid, device::incy_uuid(MACHINE_ID));
        assert!(is_valid_hwid(&info.hwid));
        assert_eq!(info.os, "Android");
        assert_eq!(info.os_version, "14");
        assert_eq!(info.model, "samsung SM-S921B");
        assert_eq!(incy.user_agent(EVEN_DAY), "INCY/3.7.0/android Dalvik/2.1.0");
        assert_eq!(incy.user_agent(EVEN_DAY), incy.user_agent(ODD_DAY));
    }

    #[test]
    fn incy_headers_have_the_captured_order_and_locale_forms() {
        let device = Device {
            locale: "ru_RU.UTF-8".into(),
            model: Some("Google Pixel 8".into()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let headers = incy(&device).headers(&url(), EVEN_DAY);
        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "User-Agent",
                "Accept",
                "Accept-Language",
                "x-hwid",
                "x-device-os",
                "x-ver-os",
                "x-device-model",
                "x-app-version",
                "x-device-locale",
                "x-client",
                "Host",
                "Connection",
                "Accept-Encoding"
            ]
        );
        assert_eq!(header(&headers, "Accept-Language"), "ru-RU");
        assert_eq!(header(&headers, "x-device-locale"), "ru_RU");
        assert_eq!(header(&headers, "x-device-model"), "Google Pixel 8");
        assert_eq!(header(&headers, "x-app-version"), "3.7.0");
        assert_eq!(header(&headers, "x-client"), "INCY");
    }

    #[test]
    fn overrides_replace_derived_values() {
        let device = Device {
            hostname: Some("MY-PC".into()),
            os_version: Some("10_10.0.19045".into()),
            hwid: Some("custom-hwid-123".into()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let info = emulation(Platform::Windows, Arch::X64, &device).device_info();
        assert_eq!(info.hwid, "custom-hwid-123");
        assert_eq!(info.os_version, "10_10.0.19045");
        assert_eq!(info.model, "MY-PC_x86_64");

        let device = Device {
            model: Some("Pixel 8".into()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let info = emulation(Platform::Android, Arch::X64, &device).device_info();
        assert_eq!(info.model, "Pixel 8");
        let windows = emulation(Platform::Windows, Arch::X64, &device).device_info();
        assert_eq!(windows.model, "Pixel 8");
    }

    #[test]
    fn same_seed_gives_the_same_device_on_every_request() {
        let device = Device::from_seed("home server");
        let a = emulation(Platform::Windows, Arch::X64, &device);
        let b = emulation(
            Platform::Windows,
            Arch::X64,
            &Device::from_seed("home server"),
        );
        assert_eq!(a.headers(&url(), EVEN_DAY), b.headers(&url(), EVEN_DAY));
        assert_eq!(a.headers(&url(), EVEN_DAY), a.headers(&url(), EVEN_DAY));
        let other = emulation(Platform::Windows, Arch::X64, &Device::from_seed("another"));
        assert_ne!(a.device_info().hwid, other.device_info().hwid);
        assert!(is_valid_hwid(&a.device_info().hwid));
    }

    #[test]
    fn windows_headers_have_the_captured_order_and_case() {
        let device = Device::from_machine_id(MACHINE_ID);
        let headers = emulation(Platform::Windows, Arch::X64, &device).headers(&url(), EVEN_DAY);
        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "Host",
                "User-Agent",
                "X-App-Version",
                "X-Device-Locale",
                "X-Device-Os",
                "X-Device-Model",
                "X-Hwid",
                "X-Ver-Os",
                "Connection",
                "Accept-Encoding",
                "Accept-Language"
            ]
        );
        assert_eq!(header(&headers, "Host"), "sub.example.com");
    }

    #[test]
    fn android_headers_have_the_captured_order_and_case() {
        let device = Device::from_machine_id(MACHINE_ID);
        let headers = emulation(Platform::Android, Arch::X64, &device).headers(&url(), EVEN_DAY);
        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "Connection",
                "User-agent",
                "X-Device-Locale",
                "X-HWID",
                "X-Device-OS",
                "X-Ver-OS",
                "X-Device-model",
                "Host",
                "Accept-Encoding"
            ]
        );
    }

    #[test]
    fn locale_reaches_the_headers() {
        let device = Device {
            locale: "ru_RU.UTF-8".into(),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let windows = emulation(Platform::Windows, Arch::X64, &device).headers(&url(), EVEN_DAY);
        assert_eq!(header(&windows, "X-Device-Locale"), "RU");
        assert_eq!(header(&windows, "Accept-Language"), "ru-RU,en,*");
        let android = emulation(Platform::Android, Arch::X64, &device).headers(&url(), EVEN_DAY);
        assert_eq!(header(&android, "X-Device-Locale"), "ru");
    }

    #[test]
    fn host_header_keeps_a_non_default_port() {
        let device = Device::from_machine_id(MACHINE_ID);
        let url = Url::parse("http://127.0.0.1:18080/sub").unwrap();
        let headers = emulation(Platform::Windows, Arch::X64, &device).headers(&url, EVEN_DAY);
        assert_eq!(header(&headers, "Host"), "127.0.0.1:18080");
    }

    #[test]
    fn control_characters_never_reach_the_headers() {
        let device = Device {
            hostname: Some("PC\r\nX-Evil: 1".into()),
            hwid: Some("abc\r\ndef-1234567".into()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let emulation = emulation(Platform::Windows, Arch::X64, &device);
        for (name, value) in emulation.headers(&url(), EVEN_DAY) {
            assert!(!value.chars().any(char::is_control), "{name}");
        }
        assert_eq!(emulation.device_info().hwid, "abcdef-1234567");
    }

    #[test]
    fn errors_are_understandable() {
        let device = Device::from_machine_id(MACHINE_ID);
        let unknown = Emulation::new("nope", Platform::Android, Arch::X64, &device);
        assert!(
            unknown
                .unwrap_err()
                .to_string()
                .contains("нет профиля nope/android")
        );
        let empty = Emulation::new("happ", Platform::Windows, Arch::X64, &Device::default());
        assert!(empty.unwrap_err().to_string().contains("machine_id"));
        let blank = Device {
            hwid: Some("  ".into()),
            ..Device::from_machine_id(MACHINE_ID)
        };
        let error = Emulation::new("happ", Platform::Windows, Arch::X64, &blank).unwrap_err();
        assert!(error.to_string().contains("device.hwid"));
    }
}
