use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

const LENIENT: GeneralPurposeConfig = GeneralPurposeConfig::new()
    .with_decode_allow_trailing_bits(true)
    .with_decode_padding_mode(DecodePaddingMode::Indifferent);
const STANDARD: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);
const URL_SAFE: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);

/// Base64 любого алфавита (обычного или URL-safe), с паддингом или без, пробелы
/// и переводы строк игнорируются.
pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    STANDARD
        .decode(&compact)
        .or_else(|_| URL_SAFE.decode(&compact))
        .ok()
}

pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let high = bytes.get(index + 1).copied().and_then(hex_value);
        let low = bytes.get(index + 2).copied().and_then(hex_value);
        if byte == b'%'
            && let (Some(high), Some(low)) = (high, low)
        {
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    // Битая последовательность не должна прятать остаток строки.
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Убирает управляющие символы и символы смены направления текста: значения из
/// ответа панели попадают в терминал и логи.
pub(crate) fn sanitize(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() && !is_bidi(*c))
        .collect()
}

fn is_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Очищает значение и обрезает его до `max` символов.
pub(crate) fn clean(input: &str, max: usize) -> String {
    sanitize(input).trim().chars().take(max).collect()
}

/// Значение с необязательным префиксом `base64:`.
pub(crate) fn decode_header_text(value: &str) -> String {
    let Some(encoded) = value.strip_prefix("base64:") else {
        return value.to_owned();
    };
    decode_base64(encoded)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| value.to_owned())
}

pub(crate) fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// Добавляет предупреждение, но не даёт списку расти без предела.
pub(crate) fn push_warning(list: &mut Vec<String>, message: String) {
    const MAX: usize = 100;
    if list.len() < MAX {
        list.push(message);
    } else if list.len() == MAX {
        list.push("остальные предупреждения не показаны".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_variants() {
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_base64(" aG\nk= \n").unwrap(), b"hi");
        assert_eq!(decode_base64("-_-_").unwrap(), decode_base64("+/+/").unwrap());
        assert!(decode_base64("").is_none());
        assert!(decode_base64("!!!").is_none());
    }

    #[test]
    fn percent_decoding_is_lenient() {
        assert_eq!(percent_decode("%D0%9C%20x"), "М x");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn sanitize_strips_control_and_bidi() {
        assert_eq!(sanitize("a\u{1b}[31mb\r\nc\u{202e}d\u{0}"), "a[31mbcd");
        assert_eq!(clean("  привет мир  ", 6), "привет");
    }

    #[test]
    fn header_text_prefix() {
        assert_eq!(decode_header_text("base64:0J/RgNC40LLQtdGC"), "Привет");
        assert_eq!(decode_header_text("base64:!!!"), "base64:!!!");
        assert_eq!(decode_header_text("plain"), "plain");
    }

    #[test]
    fn prefix_is_case_insensitive() {
        assert_eq!(strip_prefix_ci("HAPP://Routing/off", "happ://routing/"), Some("off"));
        assert_eq!(strip_prefix_ci("hap", "happ://"), None);
        assert_eq!(strip_prefix_ci("привет", "happ://"), None);
    }

    #[test]
    fn warnings_are_capped() {
        let mut list = Vec::new();
        for index in 0..500 {
            push_warning(&mut list, index.to_string());
        }
        assert_eq!(list.len(), 101);
    }
}
