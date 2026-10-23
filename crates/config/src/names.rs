//! Имена узлов для сопоставления масок и для показа. Флаги стран (пары региональных
//! индикаторов) становятся кодами стран, эмодзи и управляющие символы убираются. Этим
//! пользуются и маски настроек подписки, и TUI, поэтому правила у них общие.

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
pub fn country_code(name: &str) -> Option<String> {
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

/// Безопасное имя: управляющие символы — пробелами, флаги — кодами стран, эмодзи
/// убраны, пробелы схлопнуты.
pub fn safe_name(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    strip(&clean)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Имя без ведущего кода страны: `DE Обход 1` → `Обход 1`. Остальное не трогает.
pub fn without_code(label: &str) -> &str {
    match label.as_bytes() {
        [first, second, b' ', ..] if first.is_ascii_uppercase() && second.is_ascii_uppercase() => {
            label.get(3..).unwrap_or(label)
        }
        _ => label,
    }
}

/// Три варианта имени узла, по которым совпадает маска: исходное, безопасное и
/// безопасное без кода страны. Совпадение с любым из них — совпадение маски.
pub fn variants(raw: &str) -> [String; 3] {
    let safe = safe_name(raw);
    let bare = without_code(&safe).to_owned();
    [raw.to_owned(), safe, bare]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_becomes_its_country_code() {
        assert_eq!(safe_name("🇩🇪 Германия"), "DE Германия");
        assert_eq!(country_code("🇩🇪 Германия"), Some("DE".to_owned()));
        assert_eq!(safe_name("  🇫🇷  Франция   1 "), "FR Франция 1");
    }

    #[test]
    fn symbols_and_variation_selectors_are_dropped() {
        assert_eq!(safe_name("⚡️ Быстрый"), "Быстрый");
        assert_eq!(safe_name("🌎 Мир"), "Мир");
        assert_eq!(safe_name("🛡 Щит"), "Щит");
        assert_eq!(safe_name("🇳🇱 ⚡️ Нидерланды"), "NL Нидерланды");
    }

    #[test]
    fn plain_names_keep_their_marks_and_a_lone_indicator_goes() {
        assert_eq!(safe_name("Обход №1.0"), "Обход №1.0");
        assert_eq!(country_code("Обход №1.0"), None);
        assert_eq!(safe_name("🇩 X"), "X");
        assert_eq!(country_code("🇩 X"), None);
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(safe_name("a\x1b b"), "a b");
    }

    #[test]
    fn the_code_is_taken_off_only_when_it_leads() {
        assert_eq!(without_code("DE Обход 1"), "Обход 1");
        assert_eq!(without_code("NL-1"), "NL-1");
        assert_eq!(without_code("Обход 1"), "Обход 1");
    }

    #[test]
    fn a_name_has_three_variants() {
        assert_eq!(
            variants("🇩🇪 Обход 1"),
            [
                "🇩🇪 Обход 1".to_owned(),
                "DE Обход 1".to_owned(),
                "Обход 1".to_owned(),
            ]
        );
    }
}
