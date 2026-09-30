//! Как Happ превращает локаль системы в `X-Device-Locale` и `Accept-Language`.

/// Заголовки, зависящие от локали. `accept_language` есть не у всех правил.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Locale {
    pub(crate) device_locale: String,
    pub(crate) accept_language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocaleRule {
    /// Qt в Windows: локаль системы всегда с регионом.
    QtWindows,
    /// Android: только язык в нижнем регистре, `Accept-Language` не отправляется.
    Android,
    /// Android с регионом: `en_US` в `X-Device-Locale` и `en-US` в `Accept-Language`.
    AndroidRegion,
}

impl LocaleRule {
    pub(crate) const NAMES: &'static str = "qt-windows, android, android-region";

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "qt-windows" => Some(Self::QtWindows),
            "android" => Some(Self::Android),
            "android-region" => Some(Self::AndroidRegion),
            _ => None,
        }
    }

    pub(crate) fn provides_accept_language(self) -> bool {
        matches!(self, Self::QtWindows | Self::AndroidRegion)
    }

    /// `locale` — локаль в стиле POSIX (`ru_RU.UTF-8`, `ru`, `en`, `C`).
    pub(crate) fn apply(self, locale: &str) -> Locale {
        match self {
            Self::QtWindows => {
                let (device_locale, accept_language) = qt_windows(locale);
                Locale {
                    device_locale,
                    accept_language: Some(accept_language),
                }
            }
            Self::Android => Locale {
                device_locale: android_language(locale),
                accept_language: None,
            },
            Self::AndroidRegion => {
                let (device_locale, accept_language) = android_region(locale);
                Locale {
                    device_locale,
                    accept_language: Some(accept_language),
                }
            }
        }
    }
}

/// (`en_US`, `en-US`); язык без региона получает вероятный регион по CLDR.
fn android_region(locale: &str) -> (String, String) {
    let locale = locale.split(['.', '@']).next().unwrap_or_default();
    let (language, region) = match locale.split_once(['_', '-']) {
        Some((language, region)) => (
            language.to_ascii_lowercase(),
            Some(region.to_ascii_uppercase()),
        ),
        None => (locale.to_ascii_lowercase(), None),
    };
    if region.is_none() && matches!(language.as_str(), "" | "c" | "posix") {
        return ("en_US".to_owned(), "en-US".to_owned());
    }
    match region.or_else(|| likely_region(&language).map(str::to_owned)) {
        Some(region) => (
            format!("{language}_{region}"),
            format!("{language}-{region}"),
        ),
        None => (language.clone(), language),
    }
}

fn android_language(locale: &str) -> String {
    let language = locale
        .split(['_', '-', '.', '@'])
        .next()
        .unwrap_or_default();
    match language.to_ascii_lowercase().as_str() {
        "" | "c" | "posix" => "en".to_owned(),
        other => other.to_owned(),
    }
}

/// Windows всегда сообщает регион: `en` → (`EN`, `en-US,*`) (захват),
/// `ru` → (`RU`, `ru-RU,en,*`).
fn qt_windows(locale: &str) -> (String, String) {
    let locale = locale.split(['.', '@']).next().unwrap_or_default();
    let has_region = locale.contains(['_', '-']);
    match locale.to_ascii_lowercase().as_str() {
        "" | "c" | "posix" | "en" => qt_locale("en_US"),
        _ if has_region => qt_locale(locale),
        language => match likely_region(language) {
            Some(region) => qt_locale(&format!("{language}_{region}")),
            None => qt_locale(language),
        },
    }
}

/// Qt отправляет `QLocale::system().name()` через `-`, затем `,*` для английского
/// и `,en,*` для остальных языков.
fn qt_locale(locale: &str) -> (String, String) {
    let (language, region) = match locale.split_once(['_', '-']) {
        Some((language, region)) => (
            language.to_ascii_lowercase(),
            Some(region.to_ascii_uppercase()),
        ),
        None => (locale.to_ascii_lowercase(), None),
    };
    let name = match region {
        Some(region) => format!("{language}-{region}"),
        None => language.clone(),
    };
    let tail = if language == "en" { ",*" } else { ",en,*" };
    (language.to_ascii_uppercase(), format!("{name}{tail}"))
}

/// Вероятные регионы по CLDR для языков, на которых чаще всего сидят пользователи Happ.
fn likely_region(language: &str) -> Option<&'static str> {
    Some(match language {
        "en" => "US",
        "ru" => "RU",
        "uk" => "UA",
        "be" => "BY",
        "kk" => "KZ",
        "uz" => "UZ",
        "ky" => "KG",
        "tg" => "TJ",
        "hy" => "AM",
        "ka" => "GE",
        "az" => "AZ",
        "tk" => "TM",
        "fa" => "IR",
        "tr" => "TR",
        "de" => "DE",
        "fr" => "FR",
        "es" => "ES",
        "it" => "IT",
        "pl" => "PL",
        "pt" => "BR",
        "zh" => "CN",
        "ja" => "JP",
        "ko" => "KR",
        "ar" => "EG",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows(locale: &str) -> (String, String) {
        let l = LocaleRule::QtWindows.apply(locale);
        (l.device_locale, l.accept_language.unwrap())
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_owned(), b.to_owned())
    }

    #[test]
    fn windows_locale_always_has_a_region() {
        assert_eq!(windows("en"), pair("EN", "en-US,*"));
        assert_eq!(windows(""), pair("EN", "en-US,*"));
        assert_eq!(windows("C.UTF-8"), pair("EN", "en-US,*"));
        assert_eq!(windows("ru_RU"), pair("RU", "ru-RU,en,*"));
        assert_eq!(windows("ru"), pair("RU", "ru-RU,en,*"));
        assert_eq!(windows("ru-RU.UTF-8"), pair("RU", "ru-RU,en,*"));
        assert_eq!(windows("uk"), pair("UK", "uk-UA,en,*"));
        assert_eq!(windows("en_GB.UTF-8"), pair("EN", "en-GB,*"));
        assert_eq!(windows("eo"), pair("EO", "eo,en,*"));
    }

    #[test]
    fn android_sends_only_the_lowercase_language() {
        let l = LocaleRule::Android.apply("ru_RU.UTF-8");
        assert_eq!(l.device_locale, "ru");
        assert_eq!(l.accept_language, None);
        assert_eq!(LocaleRule::Android.apply("EN").device_locale, "en");
        assert_eq!(LocaleRule::Android.apply("").device_locale, "en");
        assert_eq!(LocaleRule::Android.apply("C").device_locale, "en");
        assert_eq!(LocaleRule::Android.apply("pt-BR").device_locale, "pt");
    }

    #[test]
    fn android_region_gives_underscore_and_hyphen_forms() {
        let region = |locale: &str| {
            let l = LocaleRule::AndroidRegion.apply(locale);
            (l.device_locale, l.accept_language.unwrap())
        };
        assert_eq!(region("en_US"), pair("en_US", "en-US"));
        assert_eq!(region("en"), pair("en_US", "en-US"));
        assert_eq!(region(""), pair("en_US", "en-US"));
        assert_eq!(region("C.UTF-8"), pair("en_US", "en-US"));
        assert_eq!(region("ru_RU.UTF-8"), pair("ru_RU", "ru-RU"));
        assert_eq!(region("ru"), pair("ru_RU", "ru-RU"));
        assert_eq!(region("pt-br"), pair("pt_BR", "pt-BR"));
        assert_eq!(region("en_GB"), pair("en_GB", "en-GB"));
        assert_eq!(region("eo"), pair("eo", "eo"));
    }

    #[test]
    fn rule_names_round_trip() {
        assert_eq!(
            LocaleRule::from_name("android-region"),
            Some(LocaleRule::AndroidRegion)
        );
        assert_eq!(
            LocaleRule::from_name("qt-windows"),
            Some(LocaleRule::QtWindows)
        );
        assert_eq!(LocaleRule::from_name("android"), Some(LocaleRule::Android));
        assert_eq!(LocaleRule::from_name("linux"), None);
    }
}
