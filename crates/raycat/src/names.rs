//! Имена узлов для терминала и короткие причины сбоев узла. Флаги стран (пары
//! региональных индикаторов) и эмодзи терминалы рисуют по-разному, поэтому по
//! умолчанию они заменяются кодом страны и не попадают в вёрстку.

use crate::util::sanitize;

/// Как показывать имя узла: код страны вместо флага или имя как есть.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NameStyle {
    #[default]
    Country,
    AsIs,
}

impl NameStyle {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Country => "country",
            Self::AsIs => "as-is",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "country" => Some(Self::Country),
            "as-is" => Some(Self::AsIs),
            _ => None,
        }
    }
}

const REGIONAL_FIRST: u32 = 0x1F1E6;
const REGIONAL_LAST: u32 = 0x1F1FF;

/// Буква кода страны у регионального индикатора: `🇩` — `D`.
fn letter(c: char) -> Option<char> {
    let code = u32::from(c);
    if (REGIONAL_FIRST..=REGIONAL_LAST).contains(&code) {
        char::from_u32(u32::from(b'A') + code - REGIONAL_FIRST)
    } else {
        None
    }
}

/// Код страны по первой паре региональных индикаторов (флагу) в имени.
pub(crate) fn country_code(name: &str) -> Option<String> {
    let chars: Vec<char> = name.chars().collect();
    chars.windows(2).find_map(|pair| {
        let first = letter(pair[0])?;
        let second = letter(pair[1])?;
        Some(format!("{first}{second}"))
    })
}

/// Эмодзи-пиктограммы, вариационные селекторы, соединитель и теги: терминал рисует
/// их шириной, которую нельзя предсказать, либо не рисует вовсе.
fn decoration(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x200D
            | 0x20E3
            | 0xFE0E
            | 0xFE0F
            | 0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x1F000..=0x1FAFF
            | 0xE0020..=0xE007F
    )
}

fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(first) = letter(c)
            && let Some(second) = chars.peek().copied().and_then(letter)
        {
            chars.next();
            out.push(first);
            out.push(second);
            continue;
        }
        if !decoration(c) {
            out.push(c);
        }
    }
    out
}

/// Имя для показа: управляющие символы становятся пробелами. В режиме `Country` пары
/// флагов заменяются кодом страны, эмодзи убираются, пробелы схлопываются. Режим
/// `AsIs` меняет только управляющие символы.
pub(crate) fn display_name(name: &str, style: NameStyle) -> String {
    let clean = sanitize(name);
    match style {
        NameStyle::AsIs => clean,
        NameStyle::Country => {
            let stripped = strip(&clean);
            stripped.split_whitespace().collect::<Vec<_>>().join(" ")
        }
    }
}

/// Маска скрытия узла: `*` — любая последовательность символов, без учёта регистра.
/// Без `*` имя должно совпасть целиком; `Обход*` — все имена, начинающиеся с «Обход».
pub(crate) fn mask_matches(mask: &str, name: &str) -> bool {
    let mask = mask.trim().to_lowercase();
    let name = name.to_lowercase();
    let parts: Vec<&str> = mask.split('*').collect();
    let [only] = parts.as_slice() else {
        let Some((head, tail)) = parts.split_first() else {
            return false;
        };
        let Some((last, middle)) = tail.split_last() else {
            return false;
        };
        let Some(mut rest) = name.strip_prefix(*head) else {
            return false;
        };
        for part in middle {
            let Some(at) = rest.find(*part) else {
                return false;
            };
            rest = &rest[at + part.len()..];
        }
        return rest.ends_with(*last);
    };
    name == *only
}

/// Короткая причина сбоя узла по тексту ошибки xray или observatory; `None`, если
/// ошибки нет.
pub(crate) fn node_reason(error: &str) -> Option<&'static str> {
    let text = error.to_lowercase();
    if text.trim().is_empty() {
        return None;
    }
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    let reason = if has(&["connection refused"]) {
        "порт закрыт"
    } else if has(&["no such host", "lookup"]) {
        "адрес сервера не разрешается"
    } else if has(&["connection reset", "eof"]) {
        "сервер разорвал соединение"
    } else if has(&["tls", "reality", "handshake"]) {
        "рукопожатие не удалось (TLS/REALITY)"
    } else if has(&["deadline", "timeout", "timed out", "тайм"]) {
        "не отвечает (тайм-аут)"
    } else {
        "ошибка проверки"
    };
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_becomes_its_country_code() {
        assert_eq!(
            display_name("🇩🇪 Германия", NameStyle::Country),
            "DE Германия"
        );
        assert_eq!(country_code("🇩🇪 Германия"), Some("DE".to_owned()));
        assert_eq!(
            display_name("  🇫🇷  Франция   1 ", NameStyle::Country),
            "FR Франция 1"
        );
    }

    #[test]
    fn symbols_and_variation_selectors_are_dropped() {
        assert_eq!(display_name("⚡️ Быстрый", NameStyle::Country), "Быстрый");
        assert_eq!(display_name("🌎 Мир", NameStyle::Country), "Мир");
        assert_eq!(display_name("🛡 Щит", NameStyle::Country), "Щит");
        assert_eq!(
            display_name("🇳🇱 ⚡️ Нидерланды", NameStyle::Country),
            "NL Нидерланды"
        );
    }

    #[test]
    fn plain_names_keep_their_marks() {
        assert_eq!(display_name("Обход №1.0", NameStyle::Country), "Обход №1.0");
        assert_eq!(country_code("Обход №1.0"), None);
    }

    #[test]
    fn a_lone_indicator_is_removed() {
        assert_eq!(display_name("🇩 X", NameStyle::Country), "X");
        assert_eq!(country_code("🇩 X"), None);
    }

    #[test]
    fn as_is_only_cleans_control_characters() {
        assert_eq!(display_name("🇩🇪 Германия", NameStyle::AsIs), "🇩🇪 Германия");
        assert_eq!(display_name("a\x1b b", NameStyle::AsIs), "a  b");
    }

    #[test]
    fn errors_become_short_russian_reasons() {
        let timeout = "the outbound node-007-main is dead: GET request failed:app/observatory: context deadline exceeded";
        assert_eq!(node_reason(timeout), Some("не отвечает (тайм-аут)"));
        let eof = "the outbound node-008-main is dead: GET request failed:app/observatory: EOF";
        assert_eq!(node_reason(eof), Some("сервер разорвал соединение"));
        let reset = "the outbound node-009-main is dead: read tcp: connection reset by peer";
        assert_eq!(node_reason(reset), Some("сервер разорвал соединение"));
        let refused = "the outbound node-010-main is dead: dial tcp: connection refused";
        assert_eq!(node_reason(refused), Some("порт закрыт"));
        let tls = "the outbound node-011-main is dead: tls: first record does not look like a TLS handshake";
        assert_eq!(
            node_reason(tls),
            Some("рукопожатие не удалось (TLS/REALITY)")
        );
        let reality = "the outbound node-012-main is dead: reality verification failed";
        assert_eq!(
            node_reason(reality),
            Some("рукопожатие не удалось (TLS/REALITY)")
        );
        let dns = "the outbound node-013-main is dead: lookup example.com: no such host";
        assert_eq!(node_reason(dns), Some("адрес сервера не разрешается"));
        assert_eq!(node_reason("что-то новое"), Some("ошибка проверки"));
        assert_eq!(node_reason("  "), None);
    }

    #[test]
    fn a_mask_with_a_star_takes_any_run_of_characters() {
        assert!(mask_matches("Обход*", "Обход 1"));
        assert!(mask_matches("обход*", "ОБХОД блокировок 2"));
        assert!(!mask_matches("Обход*", "Германия Обход"));
        assert!(mask_matches("*Германия*", "Северная Германия 3"));
        assert!(mask_matches("NL*2", "NL-hub 2"));
        assert!(!mask_matches("NL*2", "NL-hub 3"));
        assert!(mask_matches("*", "что угодно"));
    }

    #[test]
    fn a_mask_without_a_star_must_match_the_whole_name() {
        assert!(mask_matches("NL-1", "nl-1"));
        assert!(!mask_matches("NL", "NL-1"));
        assert!(!mask_matches("", "NL-1"));
    }
}
