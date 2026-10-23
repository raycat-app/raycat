//! Имена узлов для терминала и короткие причины сбоев узла. Нормализация флагов и эмодзи
//! общая с масками настроек подписки и живёт в `raycat_config::names`.

use raycat_config::names::safe_name;

use crate::util::sanitize;

pub(crate) use raycat_config::names::country_code;

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

/// Имя для показа: управляющие символы становятся пробелами. В режиме `Country` пары
/// флагов заменяются кодом страны, эмодзи убираются, пробелы схлопываются. Режим
/// `AsIs` меняет только управляющие символы.
pub(crate) fn display_name(name: &str, style: NameStyle) -> String {
    match style {
        NameStyle::AsIs => sanitize(name),
        NameStyle::Country => safe_name(name),
    }
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
}
