//! Подробности узла для API и история его проверок. Наружу уходят протокол, транспорт,
//! безопасность и последние проверки: ни адресов, ни портов, ни ключей. Текст ошибки
//! очищается от адреса сервера и IPv4-адресов до того, как попадёт в ответ.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv6Addr;
use std::time::Duration;

use raycat_proto::NodeCheck;
use raycat_xray::Node;
use serde_json::Value;

/// Сколько последних проверок хранит каждый узел.
pub(super) const HISTORY: usize = 8;

/// Короче этого значения секрет не прячем: иначе очистка испортит обычный текст.
const MIN_SECRET: usize = 4;

/// Ключи JSON, чьи строковые значения — адреса, идентификаторы и ключи узла.
const SECRET_KEYS: [&str; 7] = [
    "address",
    "id",
    "password",
    "publicKey",
    "shortId",
    "serverName",
    "host",
];

/// Что известно об узле снаружи и что надо скрыть в тексте его ошибок.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Details {
    pub(super) protocol: Option<String>,
    pub(super) transport: Option<String>,
    pub(super) security: Option<String>,
    /// Адреса, пары «адрес:порт», идентификаторы и ключи узла, длинные первыми.
    pub(super) secrets: Vec<String>,
}

/// Сведения об узле из первого outbound'а, то есть точки выхода.
pub(super) fn details(node: &Node) -> Details {
    let Some(outbound) = node.outbounds.first() else {
        return Details::default();
    };
    let stream = outbound.get("streamSettings");
    let mut secrets = Vec::new();
    collect(outbound, &mut secrets);
    secrets.retain(|secret| secret.len() >= MIN_SECRET);
    secrets.sort_unstable();
    secrets.dedup();
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    Details {
        protocol: text_of(Some(outbound), "protocol"),
        transport: Some(text_of(stream, "network").unwrap_or_else(|| "tcp".to_owned())),
        security: Some(text_of(stream, "security").unwrap_or_else(|| "none".to_owned())),
        secrets,
    }
}

fn text_of(value: Option<&Value>, key: &str) -> Option<String> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Собирает адреса, пары «адрес:порт» и ключи из всего JSON узла.
fn collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if let Some(text) = child.as_str()
                    && SECRET_KEYS.contains(&key.as_str())
                {
                    out.push(text.to_owned());
                }
                collect(child, out);
            }
            let address = map.get("address").and_then(Value::as_str);
            let port = map.get("port").and_then(Value::as_u64);
            if let (Some(address), Some(port)) = (address, port) {
                out.push(format!("{address}:{port}"));
                if address.contains(':') {
                    out.push(format!("[{address}]:{port}"));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, out);
            }
        }
        _ => {}
    }
}

/// Заменяет секреты узла, IPv6- и IPv4-адреса (с портом) в тексте ошибки.
pub(super) fn mask(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        out = out.replace(secret.as_str(), "…");
    }
    mask_ipv4(&mask_ipv6(&out))
}

/// IPv6-адрес в скобках с портом (`[2a01:4f8::1]:443`) или без скобок. Адрес — это
/// последовательность шестнадцатеричных цифр, двоеточий и точек, которую разбирает
/// `Ipv6Addr`. Время вроде `12:34:56` и одиночное `::` под это не подходят.
fn mask_ipv6(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close) = chars[i + 1..].iter().position(|c| *c == ']')
            && chars[i + 1..i + 1 + close]
                .iter()
                .collect::<String>()
                .parse::<Ipv6Addr>()
                .is_ok()
        {
            out.push('…');
            i += close + 2;
            if chars.get(i) == Some(&':') && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                i += 1;
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            continue;
        }
        if is_address_char(chars[i]) {
            let start = i;
            while i < chars.len() && is_address_char(chars[i]) {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            // Двоеточие или точка сразу за адресом — знаки препинания, не часть адреса.
            let address = run.trim_end_matches([':', '.']);
            if is_ipv6(address) {
                out.push('…');
                out.push_str(&run[address.len()..]);
            } else {
                out.push_str(&run);
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_address_char(c: char) -> bool {
    c.is_ascii_hexdigit() || c == ':' || c == '.'
}

fn is_ipv6(text: &str) -> bool {
    text.matches(':').count() >= 2
        && text.chars().any(|c| c.is_ascii_hexdigit())
        && text.parse::<Ipv6Addr>().is_ok()
}

/// Четыре числа через точку — адрес; двоеточие и число после него — порт. Всё
/// заменяется одним знаком.
fn mask_ipv4(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if !(c.is_ascii_digit() || c == '.') {
            out.push(c);
            continue;
        }
        let mut run = String::from(c);
        while let Some(&next) = chars.peek() {
            if !(next.is_ascii_digit() || next == '.') {
                break;
            }
            run.push(next);
            chars.next();
        }
        if !is_ipv4(&run) {
            out.push_str(&run);
            continue;
        }
        out.push('…');
        let mut lookahead = chars.clone();
        if lookahead.next() == Some(':') && lookahead.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
    }
    out
}

fn is_ipv4(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.len() <= 3 && part.parse::<u8>().is_ok())
}

/// Кольцевой буфер проверок по имени узла: не больше `HISTORY` записей на узел.
#[derive(Debug, Default)]
pub(super) struct Checks {
    rings: HashMap<String, VecDeque<NodeCheck>>,
    last: HashMap<String, Duration>,
}

impl Checks {
    /// Учитывает проверку, если она новее предыдущей проверки этого узла.
    pub(super) fn record(&mut self, id: &str, checked_at: Duration, alive: bool, latency_ms: u64) {
        if self.last.get(id).is_some_and(|last| checked_at <= *last) {
            return;
        }
        self.last.insert(id.to_owned(), checked_at);
        let ring = self.rings.entry(id.to_owned()).or_default();
        if ring.len() == HISTORY {
            ring.pop_front();
        }
        ring.push_back(NodeCheck {
            at: checked_at.as_secs(),
            alive,
            latency_ms: alive.then_some(latency_ms),
        });
    }

    /// Последние проверки узла, от старой к новой.
    pub(super) fn of(&self, id: &str) -> Vec<NodeCheck> {
        self.rings
            .get(id)
            .map(|ring| ring.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Забывает узлы, которых больше нет в настройках; остальные сохраняют историю.
    pub(super) fn keep(&mut self, live: &HashSet<String>) {
        self.rings.retain(|id, _| live.contains(id));
        self.last.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(outbound: Value) -> Node {
        Node {
            name: "n".to_owned(),
            outbounds: vec![outbound],
        }
    }

    #[test]
    fn details_name_the_protocol_and_keep_the_address_and_keys_inside() {
        let outbound = json!({
            "protocol": "vless",
            "settings": {"vnext": [{
                "address": "203.0.113.5",
                "port": 443,
                "users": [{"id": "00000000-0000-4000-8000-000000000001"}]
            }]},
            "streamSettings": {
                "network": "xhttp",
                "security": "reality",
                "realitySettings": {"serverName": "cdn.example.com", "publicKey": "AbCdEf0123456789"}
            }
        });
        let info = details(&node(outbound));
        assert_eq!(info.protocol.as_deref(), Some("vless"));
        assert_eq!(info.transport.as_deref(), Some("xhttp"));
        assert_eq!(info.security.as_deref(), Some("reality"));
        for secret in [
            "203.0.113.5",
            "203.0.113.5:443",
            "00000000-0000-4000-8000-000000000001",
            "AbCdEf0123456789",
        ] {
            assert!(info.secrets.iter().any(|known| known == secret));
        }
    }

    #[test]
    fn a_node_without_stream_settings_is_plain_tcp_without_security() {
        let info = details(&node(json!({"protocol": "trojan", "settings": {}})));
        assert_eq!(info.transport.as_deref(), Some("tcp"));
        assert_eq!(info.security.as_deref(), Some("none"));
    }

    #[test]
    fn an_error_loses_the_server_host_its_port_and_other_addresses() {
        let secrets = vec!["203.0.113.5:443".to_owned(), "203.0.113.5".to_owned()];
        let text = "dial tcp 203.0.113.5:443: i/o timeout; резерв 198.51.100.7:80";
        let masked = mask(text, &secrets);
        assert!(!masked.contains("203.0.113.5"));
        assert!(!masked.contains("443"));
        assert!(!masked.contains("198.51.100.7"));
        assert!(!masked.contains(":80"));
        assert!(masked.contains("i/o timeout"));
    }

    #[test]
    fn an_ipv6_address_is_masked_in_brackets_with_its_port() {
        assert_eq!(
            mask("dial tcp [2a01:4f8::1]:443: i/o timeout", &[]),
            "dial tcp …: i/o timeout"
        );
    }

    #[test]
    fn a_bare_ipv6_address_is_masked_and_the_colon_after_it_stays() {
        assert_eq!(
            mask("dial tcp 2a01:4f8::1: i/o timeout", &[]),
            "dial tcp …: i/o timeout"
        );
        assert_eq!(mask("адрес 2a01:4f8::1.", &[]), "адрес ….");
    }

    #[test]
    fn times_and_plain_text_with_colons_stay_readable() {
        assert_eq!(
            mask("в 12:34:56 ошибка, 2 из 10: готово", &[]),
            "в 12:34:56 ошибка, 2 из 10: готово"
        );
        assert_eq!(mask("разделитель :: здесь", &[]), "разделитель :: здесь");
        assert_eq!(
            mask("обычный текст без адреса", &[]),
            "обычный текст без адреса"
        );
    }

    #[test]
    fn plain_numbers_in_an_error_stay_readable() {
        let masked = mask("ошибка 12 из 40, за 3.5 с", &[]);
        assert_eq!(masked, "ошибка 12 из 40, за 3.5 с");
    }

    #[test]
    fn the_history_keeps_eight_checks_and_drops_the_oldest() {
        let mut checks = Checks::default();
        for second in 1..=12_u64 {
            checks.record("a/One", Duration::from_secs(second), true, second);
        }
        let kept = checks.of("a/One");
        assert_eq!(kept.len(), HISTORY);
        assert_eq!(kept[0].at, 5);
        assert_eq!(kept[HISTORY - 1].at, 12);
    }

    #[test]
    fn a_repeated_check_counts_once_and_a_failure_has_no_latency() {
        let mut checks = Checks::default();
        checks.record("a/One", Duration::from_secs(10), true, 31);
        checks.record("a/One", Duration::from_secs(10), true, 31);
        checks.record("a/One", Duration::from_secs(11), false, 0);
        let kept = checks.of("a/One");
        assert_eq!(kept.len(), 2);
        assert_eq!(
            kept[1],
            NodeCheck {
                at: 11,
                alive: false,
                latency_ms: None
            }
        );
    }

    #[test]
    fn history_follows_the_name_and_goes_with_a_vanished_node() {
        let mut checks = Checks::default();
        checks.record("a/One", Duration::from_secs(1), true, 30);
        checks.record("a/Two", Duration::from_secs(1), true, 40);
        let live: HashSet<String> = ["a/One".to_owned()].into_iter().collect();
        checks.keep(&live);
        assert_eq!(checks.of("a/One").len(), 1);
        assert_eq!(checks.of("a/Two").len(), 0);
    }
}
