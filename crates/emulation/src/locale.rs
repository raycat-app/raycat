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
}

impl LocaleRule {
    pub(crate) const NAMES: &'static str = "qt-windows, android";

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "qt-windows" => Some(Self::QtWindows),
            "android" => Some(Self::Android),
            _ => None,
        }
    }

    pub(crate) fn provides_accept_language(self) -> bool {
        matches!(self, Self::QtWindows)
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
        }
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
    fn rule_names_round_trip() {
        assert_eq!(
            LocaleRule::from_name("qt-windows"),
            Some(LocaleRule::QtWindows)
        );
        assert_eq!(LocaleRule::from_name("android"), Some(LocaleRule::Android));
        assert_eq!(LocaleRule::from_name("linux"), None);
    }
}
