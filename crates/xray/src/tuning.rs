use serde_json::{Map, Value, json};

use crate::nodes::with_object;

/// Outbound'ы без собственных сокетов: настраивать в них нечего.
const NO_SOCKET_PROTOCOLS: [&str; 4] = ["blackhole", "dns", "loopback", "wireguard"];

pub(crate) struct Tuning<'a> {
    pub(crate) tcp_congestion: Option<&'a str>,
    pub(crate) xhttp_connections: Option<u8>,
}

enum Transport {
    Tcp,
    Xhttp { h3: bool },
    Udp,
    Unknown,
}

/// Применяет настройки производительности к outbound'у узла. Возвращает `true`,
/// если outbound ходит по UDP (QUIC, mKCP).
///
/// Outbound'ы без `streamSettings` (служебные собираются отдельно) не трогаются.
pub(crate) fn apply(outbound: &mut Map<String, Value>, tuning: &Tuning<'_>) -> bool {
    let protocol = outbound
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if NO_SOCKET_PROTOCOLS.contains(&protocol.as_str()) {
        return false;
    }
    let Some(Value::Object(stream)) = outbound.get_mut("streamSettings") else {
        return false;
    };
    let transport = transport(&protocol, stream);
    if let Some(name) = tuning.tcp_congestion.filter(|name| !name.is_empty())
        && matches!(
            transport,
            Transport::Tcp | Transport::Xhttp { h3: false }
        )
    {
        set_congestion(stream, name);
    }
    if let (Transport::Xhttp { .. }, Some(connections)) = (&transport, tuning.xhttp_connections) {
        set_xmux(stream, connections);
    }
    matches!(transport, Transport::Udp | Transport::Xhttp { h3: true })
}

fn transport(protocol: &str, stream: &Map<String, Value>) -> Transport {
    if protocol == "hysteria" {
        return Transport::Udp;
    }
    // В xray `method` перекрывает `network`.
    let named = ["method", "network"]
        .iter()
        .find_map(|key| stream.get(*key).filter(|value| !value.is_null()));
    let Some(name) = named else {
        return Transport::Tcp;
    };
    match name
        .as_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
        .as_str()
    {
        "tcp" | "raw" | "ws" | "websocket" | "httpupgrade" | "grpc" => Transport::Tcp,
        "xhttp" | "splithttp" => Transport::Xhttp {
            h3: alpn_has_h3(stream),
        },
        "kcp" | "mkcp" | "hysteria" | "quic" => Transport::Udp,
        _ => Transport::Unknown,
    }
}

/// XHTTP идёт по QUIC, если первым в ALPN стоит `h3`; осторожнее считать так
/// любой список с `h3`.
fn alpn_has_h3(stream: &Map<String, Value>) -> bool {
    let is_h3 = |text: &str| text.trim().eq_ignore_ascii_case("h3");
    match stream.get("tlsSettings").and_then(|tls| tls.get("alpn")) {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).any(is_h3),
        Some(Value::String(list)) => list.split(',').any(is_h3),
        _ => false,
    }
}

fn set_congestion(stream: &mut Map<String, Value>, name: &str) {
    with_object(stream, "sockopt", |sockopt| {
        let provider_set = sockopt
            .get("tcpCongestion")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        if !provider_set {
            sockopt.insert("tcpCongestion".to_owned(), Value::String(name.to_owned()));
        }
    });
}

fn set_xmux(stream: &mut Map<String, Value>, connections: u8) {
    let present = |key: &str| stream.get(key).is_some_and(|value| !value.is_null());
    // В xray `xhttpSettings` перекрывает `splithttpSettings`.
    let key = if !present("xhttpSettings") && present("splithttpSettings") {
        "splithttpSettings"
    } else {
        "xhttpSettings"
    };
    with_object(stream, key, |settings| {
        // `extra` целиком заменяет остальные поля (кроме host, path и mode), поэтому
        // xmux нужен там, где его прочитает xray.
        let target = if settings.contains_key("extra") {
            match settings.get_mut("extra") {
                Some(Value::Object(extra)) => extra,
                _ => return,
            }
        } else {
            settings
        };
        let provider_set = match target.get("xmux") {
            None | Some(Value::Null) => false,
            Some(Value::Object(xmux)) => !xmux.is_empty(),
            Some(_) => true,
        };
        if provider_set {
            return;
        }
        // Любой заданный xmux отключает умолчания xray (3 соединения, смена соединения
        // через 600-900 запросов или 1800-3000 с); остальное оставляем теми же.
        target.insert(
            "xmux".to_owned(),
            json!({
                "maxConnections": connections,
                "hMaxRequestTimes": "600-900",
                "hMaxReusableSecs": "1800-3000"
            }),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuning() -> Tuning<'static> {
        Tuning {
            tcp_congestion: Some("bbr"),
            xhttp_connections: Some(4),
        }
    }

    fn outbound(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("ожидался объект, получено {other}"),
        }
    }

    fn run(value: Value, tuning: &Tuning<'_>) -> (Map<String, Value>, bool) {
        let mut map = outbound(value);
        let udp = apply(&mut map, tuning);
        (map, udp)
    }

    fn stream(network: Option<&str>) -> Value {
        match network {
            Some(network) => json!({"network": network, "sockopt": {"domainStrategy": "UseIPv4"}}),
            None => json!({"sockopt": {"domainStrategy": "UseIPv4"}}),
        }
    }

    fn congestion(map: &Map<String, Value>) -> Option<&str> {
        map["streamSettings"]["sockopt"]["tcpCongestion"].as_str()
    }

    #[test]
    fn bbr_goes_to_tcp_based_transports() {
        for network in [
            None,
            Some("tcp"),
            Some("raw"),
            Some("RAW"),
            Some("ws"),
            Some("websocket"),
            Some("httpupgrade"),
            Some("grpc"),
            Some("xhttp"),
            Some("splithttp"),
        ] {
            let (map, udp) = run(
                json!({"protocol": "vless", "streamSettings": stream(network)}),
                &tuning(),
            );
            assert_eq!(congestion(&map), Some("bbr"), "{network:?}");
            assert!(!udp, "{network:?}");
            assert_eq!(
                map["streamSettings"]["sockopt"]["domainStrategy"], "UseIPv4",
                "{network:?}"
            );
        }
    }

    #[test]
    fn bbr_skips_udp_transports_and_reports_them() {
        for network in ["kcp", "mkcp", "hysteria", "quic"] {
            let (map, udp) = run(
                json!({"protocol": "vless", "streamSettings": stream(Some(network))}),
                &tuning(),
            );
            assert_eq!(congestion(&map), None, "{network}");
            assert!(udp, "{network}");
        }
        let (map, udp) = run(
            json!({"protocol": "hysteria", "streamSettings": stream(None)}),
            &tuning(),
        );
        assert_eq!(congestion(&map), None);
        assert!(udp);
    }

    #[test]
    fn unknown_transport_is_left_alone() {
        let (map, udp) = run(
            json!({"protocol": "vless", "streamSettings": stream(Some("carrier-pigeon"))}),
            &tuning(),
        );
        assert_eq!(congestion(&map), None);
        assert!(!udp);
    }

    #[test]
    fn method_overrides_network_like_in_xray() {
        let (map, udp) = run(
            json!({"protocol": "vless", "streamSettings": {"network": "tcp", "method": "kcp"}}),
            &tuning(),
        );
        assert!(map["streamSettings"].get("sockopt").is_none());
        assert!(udp);
    }

    #[test]
    fn xhttp_over_h3_is_quic() {
        for alpn in [json!(["h3"]), json!(["H3", "h2"]), json!("h3,h2")] {
            let (map, udp) = run(
                json!({"protocol": "vless", "streamSettings": {
                    "network": "xhttp",
                    "tlsSettings": {"alpn": alpn}
                }}),
                &tuning(),
            );
            assert_eq!(congestion(&map), None);
            assert!(udp);
            assert_eq!(
                map["streamSettings"]["xhttpSettings"]["xmux"]["maxConnections"],
                4
            );
        }
        let (map, udp) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "xhttp",
                "tlsSettings": {"alpn": ["h2", "http/1.1"]}
            }}),
            &tuning(),
        );
        assert_eq!(congestion(&map), Some("bbr"));
        assert!(!udp);
    }

    #[test]
    fn providers_own_congestion_is_kept() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "tcp", "sockopt": {"tcpCongestion": "cubic"}
            }}),
            &tuning(),
        );
        assert_eq!(congestion(&map), Some("cubic"));
    }

    #[test]
    fn empty_provider_congestion_is_replaced() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "tcp", "sockopt": {"tcpCongestion": ""}
            }}),
            &tuning(),
        );
        assert_eq!(congestion(&map), Some("bbr"));
    }

    #[test]
    fn nothing_is_set_without_settings() {
        let off = Tuning {
            tcp_congestion: None,
            xhttp_connections: None,
        };
        let source = json!({"protocol": "vless", "streamSettings": {"network": "xhttp"}});
        let (map, _) = run(source.clone(), &off);
        assert_eq!(Value::Object(map), source);

        let empty = Tuning {
            tcp_congestion: Some(""),
            xhttp_connections: None,
        };
        let (map, _) = run(source.clone(), &empty);
        assert_eq!(Value::Object(map), source);
    }

    #[test]
    fn service_protocols_are_not_touched() {
        for protocol in ["blackhole", "dns", "loopback", "wireguard", "DNS"] {
            let source = json!({"protocol": protocol, "streamSettings": {"sockopt": {}}});
            let (map, udp) = run(source.clone(), &tuning());
            assert_eq!(Value::Object(map), source, "{protocol}");
            assert!(!udp);
        }
    }

    #[test]
    fn chain_freedom_gets_bbr_like_any_dialer() {
        let (map, udp) = run(
            json!({"protocol": "freedom", "streamSettings": {"sockopt": {"mark": 255}}}),
            &tuning(),
        );
        assert_eq!(congestion(&map), Some("bbr"));
        assert_eq!(map["streamSettings"]["sockopt"]["mark"], 255);
        assert!(!udp);
    }

    #[test]
    fn outbound_without_stream_settings_is_skipped() {
        let source = json!({"protocol": "vless"});
        let (map, udp) = run(source.clone(), &tuning());
        assert_eq!(Value::Object(map), source);
        assert!(!udp);
    }

    #[test]
    fn xmux_is_added_to_xhttp_without_one() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "xhttp",
                "xhttpSettings": {"path": "/", "host": "example.com", "mode": "auto"}
            }}),
            &tuning(),
        );
        let settings = &map["streamSettings"]["xhttpSettings"];
        assert_eq!(settings["path"], "/");
        assert_eq!(
            settings["xmux"],
            json!({
                "maxConnections": 4,
                "hMaxRequestTimes": "600-900",
                "hMaxReusableSecs": "1800-3000"
            })
        );
        assert!(settings.get("extra").is_none());
    }

    #[test]
    fn xmux_is_added_even_without_xhttp_settings() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {"network": "splithttp"}}),
            &tuning(),
        );
        assert_eq!(
            map["streamSettings"]["xhttpSettings"]["xmux"]["maxConnections"],
            4
        );
    }

    #[test]
    fn legacy_splithttp_settings_are_used_when_xhttp_settings_are_absent() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "splithttp",
                "splithttpSettings": {"path": "/s"}
            }}),
            &tuning(),
        );
        let stream = &map["streamSettings"];
        assert_eq!(stream["splithttpSettings"]["path"], "/s");
        assert_eq!(stream["splithttpSettings"]["xmux"]["maxConnections"], 4);
        assert!(stream.get("xhttpSettings").is_none());
    }

    #[test]
    fn xhttp_settings_win_over_splithttp_settings() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "xhttp",
                "xhttpSettings": {"path": "/x"},
                "splithttpSettings": {"path": "/s"}
            }}),
            &tuning(),
        );
        let stream = &map["streamSettings"];
        assert_eq!(stream["xhttpSettings"]["xmux"]["maxConnections"], 4);
        assert!(stream["splithttpSettings"].get("xmux").is_none());
    }

    #[test]
    fn providers_xmux_is_kept() {
        for xmux in [
            json!({"maxConcurrency": "16-32"}),
            json!({"maxConnections": 2}),
            json!({"hKeepAlivePeriod": 0}),
            json!("garbage"),
        ] {
            let (map, _) = run(
                json!({"protocol": "vless", "streamSettings": {
                    "network": "xhttp",
                    "xhttpSettings": {"xmux": xmux.clone()}
                }}),
                &tuning(),
            );
            assert_eq!(
                map["streamSettings"]["xhttpSettings"]["xmux"], xmux,
                "{xmux}"
            );
        }
    }

    #[test]
    fn empty_provider_xmux_counts_as_absent() {
        for xmux in [json!({}), Value::Null] {
            let (map, _) = run(
                json!({"protocol": "vless", "streamSettings": {
                    "network": "xhttp",
                    "xhttpSettings": {"xmux": xmux}
                }}),
                &tuning(),
            );
            assert_eq!(
                map["streamSettings"]["xhttpSettings"]["xmux"]["maxConnections"],
                4
            );
        }
    }

    #[test]
    fn xmux_goes_into_extra_when_the_provider_uses_it() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "xhttp",
                "xhttpSettings": {"path": "/", "extra": {"xPaddingBytes": "100-1000"}}
            }}),
            &tuning(),
        );
        let settings = &map["streamSettings"]["xhttpSettings"];
        assert!(settings.get("xmux").is_none());
        assert_eq!(settings["extra"]["xPaddingBytes"], "100-1000");
        assert_eq!(settings["extra"]["xmux"]["maxConnections"], 4);
    }

    #[test]
    fn providers_xmux_inside_extra_is_kept() {
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {
                "network": "xhttp",
                "xhttpSettings": {"extra": {"xmux": {"maxConcurrency": "8"}}}
            }}),
            &tuning(),
        );
        assert_eq!(
            map["streamSettings"]["xhttpSettings"]["extra"]["xmux"],
            json!({"maxConcurrency": "8"})
        );
    }

    #[test]
    fn non_object_extra_is_left_alone() {
        let source = json!({"protocol": "vless", "streamSettings": {
            "network": "xhttp",
            "xhttpSettings": {"extra": "text"}
        }});
        let (map, _) = run(source, &tuning());
        assert_eq!(
            map["streamSettings"]["xhttpSettings"],
            json!({"extra": "text"})
        );
    }

    #[test]
    fn xmux_is_only_for_xhttp() {
        for network in ["tcp", "ws", "grpc", "kcp"] {
            let (map, _) = run(
                json!({"protocol": "vless", "streamSettings": stream(Some(network))}),
                &tuning(),
            );
            let stream = &map["streamSettings"];
            assert!(stream.get("xhttpSettings").is_none(), "{network}");
            assert!(stream.get("splithttpSettings").is_none(), "{network}");
        }
    }

    #[test]
    fn xmux_without_a_setting_is_not_added() {
        let only_bbr = Tuning {
            tcp_congestion: Some("bbr"),
            xhttp_connections: None,
        };
        let (map, _) = run(
            json!({"protocol": "vless", "streamSettings": {"network": "xhttp"}}),
            &only_bbr,
        );
        assert!(map["streamSettings"].get("xhttpSettings").is_none());
        assert_eq!(congestion(&map), Some("bbr"));
    }
}
