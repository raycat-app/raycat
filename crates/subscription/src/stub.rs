//! Заглушки: ответы, которые выглядят как конфиг, но несут только сообщение
//! провайдера («устройство не поддерживается», «лимит устройств», «подписка
//! истекла»). Remnawave рисует их как узлы с адресом `0.0.0.0` и портом 1.

use std::net::IpAddr;

use raycat_xray::Node;
use serde_json::Value;

use crate::info::HwidFlags;

pub(crate) struct Endpoint {
    pub(crate) server: String,
    pub(crate) port: u16,
}

/// Адрес и порт сервера outbound'а: плоские настройки, `vnext`, `servers`
/// или `peers[].endpoint` у wireguard.
pub(crate) fn endpoint_of(outbound: &Value) -> Option<Endpoint> {
    let settings = outbound.get("settings")?;
    let entry = ["vnext", "servers", "peers"]
        .iter()
        .find_map(|key| settings.get(*key)?.as_array()?.first())
        .unwrap_or(settings);
    if let Some(endpoint) = entry.get("endpoint").and_then(Value::as_str) {
        return split_host_port(endpoint);
    }
    Some(Endpoint {
        server: entry.get("address")?.as_str()?.to_owned(),
        port: port_of(entry.get("port")?)?,
    })
}

fn port_of(value: &Value) -> Option<u16> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|port| u16::try_from(port).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn split_host_port(endpoint: &str) -> Option<Endpoint> {
    let (host, port) = endpoint.rsplit_once(':')?;
    Some(Endpoint {
        server: host.trim_matches(['[', ']']).to_owned(),
        port: port.trim().parse().ok()?,
    })
}

pub(crate) fn is_placeholder(endpoint: &Endpoint) -> bool {
    if endpoint.port <= 1 {
        return true;
    }
    let host = endpoint.server.trim().trim_matches(['[', ']']);
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified() || ip.is_loopback())
}

/// Узел-заглушка: сервер выхода (первый outbound) ненастоящий.
pub(crate) fn is_stub(node: &Node) -> bool {
    node.outbounds
        .first()
        .and_then(endpoint_of)
        .is_some_and(|endpoint| is_placeholder(&endpoint))
}

/// Признаки отказа панели, которые она ставит даже при пустом теле.
pub(crate) fn hwid_refusal(flags: HwidFlags) -> Option<String> {
    if flags.max_devices_reached {
        Some("достигнут лимит устройств: этот HWID новый, а все места заняты (x-hwid-max-devices-reached)".to_owned())
    } else if flags.not_supported {
        Some("панель требует корректный заголовок x-hwid (x-hwid-not-supported)".to_owned())
    } else if flags.limit {
        Some("панель отказала этому устройству (x-hwid-limit)".to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(outbound: Value) -> Node {
        Node {
            name: "тест".to_owned(),
            outbounds: vec![outbound],
        }
    }

    fn flat(address: &str, port: u64) -> Value {
        json!({"protocol": "vless", "settings": {"address": address, "port": port, "id": "x"}})
    }

    #[test]
    fn placeholders() {
        for (address, port) in [
            ("0.0.0.0", 1),
            ("0.0.0.0", 443),
            ("127.0.0.1", 443),
            ("127.8.8.8", 443),
            ("::", 443),
            ("[::1]", 443),
            ("localhost", 443),
            ("", 443),
            ("nl.example.com", 1),
            ("nl.example.com", 0),
        ] {
            assert!(is_stub(&node(flat(address, port))), "{address}:{port}");
        }
        for (address, port) in [
            ("nl.example.com", 443),
            ("203.0.113.5", 2),
            ("2001:db8::1", 443),
        ] {
            assert!(!is_stub(&node(flat(address, port))), "{address}:{port}");
        }
    }

    #[test]
    fn endpoint_shapes() {
        let vnext = json!({"protocol": "vless", "settings": {"vnext": [
            {"address": "0.0.0.0", "port": "1", "users": [{"id": "x"}]}]}});
        assert!(is_stub(&node(vnext)));
        let servers = json!({"protocol": "trojan", "settings": {"servers": [
            {"address": "t.example.com", "port": 443, "password": "p"}]}});
        assert!(!is_stub(&node(servers)));
        let wireguard = json!({"protocol": "wireguard", "settings": {"peers": [
            {"endpoint": "[::]:51820"}]}});
        assert!(is_stub(&node(wireguard)));
        let unknown = json!({"protocol": "vless", "settings": {}});
        assert!(!is_stub(&node(unknown)));
    }

    #[test]
    fn refusal_priority() {
        assert!(hwid_refusal(HwidFlags::default()).is_none());
        let active_only = HwidFlags {
            active: true,
            ..HwidFlags::default()
        };
        assert!(hwid_refusal(active_only).is_none());
        let all = HwidFlags {
            active: true,
            not_supported: true,
            max_devices_reached: true,
            limit: true,
        };
        assert!(hwid_refusal(all).unwrap().contains("лимит устройств"));
        let limit = HwidFlags {
            limit: true,
            ..HwidFlags::default()
        };
        assert!(hwid_refusal(limit).unwrap().contains("x-hwid-limit"));
    }
}
