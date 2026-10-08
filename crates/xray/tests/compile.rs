// Хелперы тестов не покрыты allow-unwrap-in-tests: там ошибка и есть падение теста.
#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use raycat_xray::{
    CompileError, Compiled, Credentials, Mode, Node, Settings, SkippedNode, Subscription, compile,
};
use serde_json::{Value, json};

const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";
const ZERO_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn vless_reality(tag: &str, address: &str) -> Value {
    json!({
        "tag": tag,
        "protocol": "vless",
        "settings": {"vnext": [{
            "address": address,
            "port": 443,
            "users": [{"id": ZERO_UUID, "encryption": "none", "flow": "xtls-rprx-vision"}]
        }]},
        "streamSettings": {
            "network": "tcp",
            "security": "reality",
            "realitySettings": {
                "serverName": "www.example.com",
                "fingerprint": "chrome",
                "publicKey": ZERO_KEY,
                "shortId": "0000000000000000",
                "spiderX": "/"
            }
        }
    })
}

fn vless_xhttp_through_hop() -> Vec<Value> {
    vec![
        json!({
            "tag": "proxy",
            "protocol": "vless",
            "settings": {"vnext": [{
                "address": "xhttp.example.com",
                "port": 443,
                "users": [{"id": ZERO_UUID, "encryption": "none"}]
            }]},
            "streamSettings": {
                "network": "xhttp",
                "security": "tls",
                "tlsSettings": {
                    "serverName": "xhttp.example.com",
                    "fingerprint": "chrome",
                    "alpn": ["h2"]
                },
                "xhttpSettings": {"path": "/", "host": "xhttp.example.com", "mode": "auto"},
                "sockopt": {"dialerProxy": "hop"}
            }
        }),
        vless_reality("hop", "203.0.113.10"),
        json!({"tag": "direct", "protocol": "freedom", "settings": {}}),
        json!({"tag": "block", "protocol": "blackhole", "settings": {}}),
    ]
}

fn hysteria2() -> Value {
    json!({
        "tag": "proxy",
        "protocol": "hysteria",
        "settings": {"version": 2, "address": "hy2.example.com", "port": 443},
        "streamSettings": {
            "network": "hysteria",
            "security": "tls",
            "tlsSettings": {"serverName": "hy2.example.com", "alpn": ["h3"]},
            "hysteriaSettings": {"version": 2, "auth": ZERO_UUID}
        }
    })
}

fn trojan_via_legacy_proxy_settings() -> Vec<Value> {
    vec![
        json!({
            "tag": "proxy",
            "protocol": "trojan",
            "settings": {"servers": [{
                "address": "203.0.113.20",
                "port": 443,
                "password": ZERO_UUID
            }]},
            "streamSettings": {
                "network": "tcp",
                "security": "tls",
                "tlsSettings": {"serverName": "example.com", "fingerprint": "chrome"}
            },
            "proxySettings": {"tag": "relay"}
        }),
        vless_reality("relay", "203.0.113.21"),
    ]
}

// Фрагментация как у провайдеров: выход ссылается на freedom-outbound с `fragment`,
// а лишний freedom рядом никому не нужен.
fn reality_with_fragment() -> Vec<Value> {
    let mut main = vless_reality("proxy", "reality.example.com");
    main["streamSettings"]["sockopt"] = json!({"dialerProxy": "fragment"});
    vec![
        main,
        json!({
            "tag": "fragment",
            "protocol": "freedom",
            "settings": {"fragment": {"packets": "tlshello", "length": "100-200", "interval": "10-20"}}
        }),
        json!({"tag": "direct", "protocol": "freedom", "settings": {}}),
    ]
}

fn node(name: &str, outbounds: Vec<Value>) -> Node {
    Node {
        name: name.to_owned(),
        outbounds,
    }
}

fn subscriptions() -> Vec<Subscription> {
    vec![
        Subscription {
            id: "main".to_owned(),
            nodes: vec![
                node("Узел 1", reality_with_fragment()),
                node("Узел 2", vless_xhttp_through_hop()),
            ],
        },
        Subscription {
            id: "backup".to_owned(),
            nodes: vec![
                node("Узел 3", vec![hysteria2()]),
                node("Узел 4", trojan_via_legacy_proxy_settings()),
            ],
        },
    ]
}

fn proxy_mode() -> Mode {
    Mode::Proxy {
        listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1080),
        auth: None,
    }
}

fn proxy_with_password() -> Mode {
    Mode::Proxy {
        listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1080),
        auth: Some(Credentials {
            user: "golden-user".to_owned(),
            password: "golden-password".to_owned(),
        }),
    }
}

fn gateway_mode() -> Mode {
    Mode::Gateway {
        tproxy_port: 12345,
        mark: 255,
    }
}

fn build(mode: Mode) -> Compiled {
    compile(&subscriptions(), &Settings::new(mode, 10085)).unwrap()
}

fn outbound<'a>(config: &'a Value, tag: &str) -> &'a Value {
    config["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["tag"] == tag)
        .unwrap_or_else(|| panic!("нет outbound {tag}"))
}

fn outbound_tags(config: &Value) -> Vec<&str> {
    config["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["tag"].as_str().unwrap())
        .collect()
}

const MAIN_TAGS: [&str; 4] = [
    "node-001-main",
    "node-002-main",
    "node-003-main",
    "node-004-main",
];

#[test]
fn tags_are_numbered_and_chains_are_rewritten() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        outbound_tags(&config),
        [
            "node-001-main",
            "node-001-x-fragment",
            "node-002-main",
            "node-002-x-hop",
            "node-003-main",
            "node-004-main",
            "node-004-x-relay",
            "direct",
            "block",
            "dns-out",
        ]
    );
    assert_eq!(
        outbound(&config, "node-001-main")["streamSettings"]["sockopt"]["dialerProxy"],
        "node-001-x-fragment"
    );
    let fragment = outbound(&config, "node-001-x-fragment");
    assert_eq!(fragment["protocol"], "freedom");
    assert_eq!(fragment["settings"]["fragment"]["packets"], "tlshello");
    assert_eq!(
        outbound(&config, "node-002-main")["streamSettings"]["sockopt"]["dialerProxy"],
        "node-002-x-hop"
    );
    let legacy = outbound(&config, "node-004-main");
    assert_eq!(
        legacy["streamSettings"]["sockopt"]["dialerProxy"],
        "node-004-x-relay"
    );
    assert!(legacy.get("proxySettings").is_none());
}

#[test]
fn node_outbounds_resolve_names_with_the_builtin_dns_only() {
    let config = build(proxy_mode()).config;

    for tag in
        MAIN_TAGS
            .iter()
            .chain(&["node-001-x-fragment", "node-002-x-hop", "node-004-x-relay"])
    {
        let sockopt = &outbound(&config, tag)["streamSettings"]["sockopt"];
        assert_eq!(sockopt["domainStrategy"], "UseIPv4", "{tag}");
        assert!(sockopt.get("mark").is_none(), "{tag}");
    }
    // Остальные части streamSettings не теряются.
    assert_eq!(
        outbound(&config, "node-001-main")["streamSettings"]["security"],
        "reality"
    );
}

#[test]
fn gateway_marks_own_sockets() {
    let config = build(gateway_mode()).config;

    for tag in MAIN_TAGS.iter().chain(&[
        "node-001-x-fragment",
        "node-002-x-hop",
        "node-004-x-relay",
        "direct",
    ]) {
        let sockopt = &outbound(&config, tag)["streamSettings"]["sockopt"];
        assert_eq!(sockopt["mark"], 255, "{tag}");
    }
    assert_eq!(
        outbound(&config, "node-001-main")["streamSettings"]["sockopt"]["domainStrategy"],
        "UseIPv4"
    );
}

#[test]
fn server_domains_get_a_real_resolver_before_fakedns() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        config["dns"],
        json!({
            "tag": "dns-internal",
            "queryStrategy": "UseIPv4",
            "servers": [
                {
                    "address": "1.1.1.1",
                    "domains": [
                        "full:reality.example.com",
                        "full:xhttp.example.com",
                        "full:hy2.example.com"
                    ],
                    "skipFallback": true
                },
                "fakedns",
                "1.1.1.1",
                "8.8.8.8"
            ]
        })
    );
    assert_eq!(
        config["fakedns"],
        json!([{"ipPool": "198.18.0.0/15", "poolSize": 65_535}])
    );
}

#[test]
fn dns_without_server_domains_has_no_special_entry() {
    let only_ip = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("IP", vec![vless_reality("proxy", "203.0.113.30")])],
    }];
    let config = compile(&only_ip, &Settings::new(proxy_mode(), 10085))
        .unwrap()
        .config;

    assert_eq!(
        config["dns"]["servers"],
        json!(["fakedns", "1.1.1.1", "8.8.8.8"])
    );
}

#[test]
fn custom_dns_settings_are_used() {
    let mut settings = Settings::new(proxy_mode(), 10085);
    settings.dns.resolvers = vec![
        IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
        IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)),
    ];
    settings.dns.fake_ip_pool = "198.18.0.0/16".to_owned();
    let config = compile(&subscriptions(), &settings).unwrap().config;

    let servers = config["dns"]["servers"].as_array().unwrap();
    assert_eq!(servers[0]["address"], "2606:4700:4700::1111");
    assert_eq!(servers[1], "fakedns");
    assert_eq!(servers[2], "2606:4700:4700::1111");
    assert_eq!(servers[3], "9.9.9.9");
    assert_eq!(config["fakedns"][0]["ipPool"], "198.18.0.0/16");
}

#[test]
fn observatory_probes_every_main_outbound() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        config["observatory"],
        json!({
            "subjectSelector": MAIN_TAGS,
            "probeUrl": "https://www.gstatic.com/generate_204",
            "probeInterval": "30s",
            "enableConcurrency": true
        })
    );
}

#[test]
fn custom_probe_settings_are_used() {
    let mut settings = Settings::new(proxy_mode(), 10085);
    settings.probe.url = "https://example.com/health".to_owned();
    settings.probe.interval = Duration::from_millis(400);
    let config = compile(&subscriptions(), &settings).unwrap().config;

    assert_eq!(
        config["observatory"]["probeUrl"],
        "https://example.com/health"
    );
    assert_eq!(config["observatory"]["probeInterval"], "1s");
}

#[test]
fn balancer_prefers_lowest_ping_and_falls_back_to_the_first_node() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        config["routing"]["balancers"],
        json!([{
            "tag": "auto",
            "selector": MAIN_TAGS,
            "strategy": {"type": "leastPing"},
            "fallbackTag": "node-001-main"
        }])
    );
}

#[test]
fn routing_rules_come_in_order() {
    let private = json!([
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "169.254.0.0/16",
        "fc00::/7",
        "fe80::/10",
        "127.0.0.0/8"
    ]);

    let proxy = build(proxy_mode()).config;
    assert_eq!(
        proxy["routing"]["rules"],
        json!([
            {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
            {"type": "field", "inboundTag": ["dns-internal"], "outboundTag": "direct"},
            {"type": "field", "ip": private, "outboundTag": "direct"},
            {"type": "field", "network": "tcp,udp", "balancerTag": "auto"}
        ])
    );

    let gateway = build(gateway_mode()).config;
    assert_eq!(
        gateway["routing"]["rules"],
        json!([
            {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
            {"type": "field", "inboundTag": ["dns-internal"], "outboundTag": "direct"},
            {"type": "field", "port": "53", "outboundTag": "dns-out"},
            {"type": "field", "ip": private, "outboundTag": "direct"},
            {"type": "field", "network": "tcp,udp", "balancerTag": "auto"}
        ])
    );
}

#[test]
fn service_outbounds_follow_the_nodes() {
    let proxy = build(proxy_mode()).config;
    assert_eq!(
        outbound(&proxy, "direct"),
        &json!({"tag": "direct", "protocol": "freedom", "settings": {}})
    );
    assert_eq!(outbound(&proxy, "block")["protocol"], "blackhole");
    assert_eq!(outbound(&proxy, "dns-out")["protocol"], "dns");

    let freedom = proxy["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["tag"] == "direct")
        .count();
    assert_eq!(
        freedom, 1,
        "неиспользуемый freedom из узла не попадает в конфиг"
    );
}

#[test]
fn api_listens_on_loopback_only() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        config["api"],
        json!({
            "tag": "api",
            "listen": "127.0.0.1:10085",
            "services": [
                "HandlerService",
                "RoutingService",
                "ObservatoryService",
                "StatsService"
            ]
        })
    );
}

#[test]
fn outbound_traffic_counters_are_enabled() {
    for mode in [proxy_mode(), gateway_mode()] {
        let config = build(mode).config;

        assert_eq!(config["stats"], json!({}));
        assert_eq!(
            config["policy"],
            json!({"system": {"statsOutboundUplink": true, "statsOutboundDownlink": true}})
        );
    }
}

#[test]
fn gateway_inbound_is_tproxy() {
    let config = build(gateway_mode()).config;

    assert_eq!(
        config["inbounds"],
        json!([{
            "tag": "tproxy-in",
            "protocol": "dokodemo-door",
            "port": 12345,
            "settings": {"network": "tcp,udp", "followRedirect": true},
            "streamSettings": {"sockopt": {"tproxy": "tproxy"}},
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic", "fakedns"]
            }
        }])
    );
}

#[test]
fn proxy_inbound_serves_http_and_socks_on_one_port() {
    let config = build(proxy_mode()).config;

    assert_eq!(
        config["inbounds"],
        json!([{
            "tag": "proxy-in",
            "protocol": "mixed",
            "listen": "0.0.0.0",
            "port": 1080,
            "settings": {"auth": "noauth", "udp": true},
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic", "fakedns"]
            }
        }])
    );
}

#[test]
fn proxy_on_a_concrete_address_tells_it_to_udp_clients() {
    let mode = Mode::Proxy {
        listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 3128),
        auth: None,
    };
    let config = build(mode).config;

    assert_eq!(config["inbounds"][0]["listen"], "192.0.2.1");
    assert_eq!(config["inbounds"][0]["port"], 3128);
    assert_eq!(config["inbounds"][0]["settings"]["ip"], "192.0.2.1");
}

#[test]
fn proxy_with_a_password_asks_for_it_on_the_mixed_inbound() {
    let config = build(proxy_with_password()).config;

    assert_eq!(
        config["inbounds"][0]["settings"],
        json!({
            "auth": "password",
            "accounts": [{"user": "golden-user", "pass": "golden-password"}],
            "udp": true
        })
    );
}

#[test]
fn password_stays_out_of_debug_output() {
    let text = format!("{:?}", proxy_with_password());

    assert!(!text.contains("golden-password"), "{text}");
    assert!(text.contains("golden-user"), "{text}");
}

#[test]
fn log_is_quiet_and_has_no_access_log() {
    for mode in [proxy_mode(), gateway_mode()] {
        assert_eq!(
            build(mode).config["log"],
            json!({"loglevel": "warning", "access": "none"})
        );
    }
}

fn tuned_settings(mode: Mode) -> Settings {
    let mut settings = Settings::new(mode, 10085);
    settings.tcp_congestion = Some("bbr".to_owned());
    settings.xhttp_connections = Some(4);
    settings
}

fn build_tuned(mode: Mode) -> Compiled {
    compile(&subscriptions(), &tuned_settings(mode)).unwrap()
}

fn sockopt<'a>(config: &'a Value, tag: &str) -> &'a Value {
    &outbound(config, tag)["streamSettings"]["sockopt"]
}

#[test]
fn nothing_is_tuned_by_default() {
    let compiled = build(proxy_mode());
    let text = serde_json::to_string(&compiled.config).unwrap();

    assert!(!text.contains("tcpCongestion"));
    assert!(!text.contains("xmux"));
    assert!(compiled.quic);
}

#[test]
fn bbr_goes_to_tcp_node_outbounds_only() {
    let config = build_tuned(proxy_mode()).config;

    for tag in MAIN_TAGS
        .iter()
        .filter(|tag| **tag != "node-003-main")
        .chain(&["node-001-x-fragment", "node-002-x-hop", "node-004-x-relay"])
    {
        assert_eq!(sockopt(&config, tag)["tcpCongestion"], "bbr", "{tag}");
    }
    assert!(sockopt(&config, "node-003-main")["tcpCongestion"].is_null());
    for tag in ["direct", "block", "dns-out"] {
        let text = serde_json::to_string(outbound(&config, tag)).unwrap();
        assert!(!text.contains("tcpCongestion"), "{tag}");
    }
}

#[test]
fn providers_congestion_is_not_overwritten() {
    let mut vless = vless_reality("proxy", "reality.example.com");
    vless["streamSettings"]["sockopt"] = json!({"tcpCongestion": "cubic"});
    let subs = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("свой", vec![vless])],
    }];
    let config = compile(&subs, &tuned_settings(proxy_mode()))
        .unwrap()
        .config;

    assert_eq!(sockopt(&config, "node-001-main")["tcpCongestion"], "cubic");
}

#[test]
fn xmux_is_added_to_xhttp_nodes_without_one() {
    let config = build_tuned(proxy_mode()).config;

    assert_eq!(
        outbound(&config, "node-002-main")["streamSettings"]["xhttpSettings"],
        json!({
            "path": "/",
            "host": "xhttp.example.com",
            "mode": "auto",
            "xmux": {"maxConnections": 4, "hMaxRequestTimes": "600-900", "hMaxReusableSecs": "1800-3000"}
        })
    );
    for tag in ["node-001-main", "node-003-main", "node-004-main"] {
        let stream = &outbound(&config, tag)["streamSettings"];
        assert!(stream.get("xhttpSettings").is_none(), "{tag}");
    }
}

#[test]
fn providers_xmux_is_kept() {
    let mut hop = vless_xhttp_through_hop();
    hop[0]["streamSettings"]["xhttpSettings"]["xmux"] = json!({"maxConcurrency": "16-32"});
    let subs = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("свой xmux", hop)],
    }];
    let config = compile(&subs, &tuned_settings(proxy_mode()))
        .unwrap()
        .config;

    assert_eq!(
        outbound(&config, "node-001-main")["streamSettings"]["xhttpSettings"]["xmux"],
        json!({"maxConcurrency": "16-32"})
    );
}

#[test]
fn xhttp_over_h3_gets_xmux_but_no_bbr() {
    let mut h3 = vless_xhttp_through_hop();
    h3[0]["streamSettings"]["tlsSettings"]["alpn"] = json!(["h3"]);
    let subs = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("h3", h3)],
    }];
    let compiled = compile(&subs, &tuned_settings(proxy_mode())).unwrap();

    let main = outbound(&compiled.config, "node-001-main");
    assert!(main["streamSettings"]["sockopt"]["tcpCongestion"].is_null());
    assert_eq!(
        main["streamSettings"]["xhttpSettings"]["xmux"]["maxConnections"],
        4
    );
    assert!(compiled.quic);
}

#[test]
fn quic_flag_follows_udp_transports() {
    let tcp_only = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node(
            "tcp",
            vec![vless_reality("proxy", "reality.example.com")],
        )],
    }];
    let settings = Settings::new(proxy_mode(), 10085);
    assert!(!compile(&tcp_only, &settings).unwrap().quic);

    let with_hysteria = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("hy2", vec![hysteria2()])],
    }];
    assert!(compile(&with_hysteria, &settings).unwrap().quic);
}

#[test]
fn golden_tuned_config() {
    assert_golden("tuned.json", &build_tuned(gateway_mode()).config);
}

#[test]
fn same_input_gives_byte_identical_json() {
    for mode in [proxy_mode(), gateway_mode()] {
        let first = serde_json::to_string(&build(mode.clone()).config).unwrap();
        let second = serde_json::to_string(&build(mode).config).unwrap();
        assert_eq!(first, second);
    }
}

#[test]
fn tag_table_maps_tags_back_to_subscription_and_node() {
    let compiled = build(proxy_mode());

    let entries: Vec<_> = compiled
        .tags
        .entries()
        .iter()
        .map(|e| (e.tag.as_str(), e.subscription.as_str(), e.name.as_str()))
        .collect();
    assert_eq!(
        entries,
        [
            ("node-001-main", "main", "Узел 1"),
            ("node-002-main", "main", "Узел 2"),
            ("node-003-main", "backup", "Узел 3"),
            ("node-004-main", "backup", "Узел 4"),
        ]
    );
    assert_eq!(compiled.tags.get("node-003-main").unwrap().name, "Узел 3");
    assert!(compiled.tags.get("node-002-x-hop").is_none());
    assert_eq!(compiled.skipped, Vec::<SkippedNode>::new());
}

#[test]
fn unusable_nodes_are_skipped_and_numbering_stays_contiguous() {
    let subs = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![
            node("пустой", Vec::new()),
            node(
                "служебный",
                vec![json!({"tag": "direct", "protocol": "freedom"})],
            ),
            node(
                "хороший",
                vec![vless_reality("proxy", "reality.example.com")],
            ),
        ],
    }];
    let compiled = compile(&subs, &Settings::new(proxy_mode(), 10085)).unwrap();

    assert_eq!(compiled.tags.entries().len(), 1);
    assert_eq!(compiled.tags.entries()[0].tag, "node-001-main");
    let skipped: Vec<_> = compiled.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(skipped, ["пустой", "служебный"]);
    assert_eq!(compiled.skipped[0].subscription, "s");
}

#[test]
fn no_usable_nodes_is_an_error() {
    let settings = Settings::new(proxy_mode(), 10085);

    assert_eq!(compile(&[], &settings).unwrap_err(), CompileError::NoNodes);
    let subs = vec![Subscription {
        id: "s".to_owned(),
        nodes: vec![node("пустой", Vec::new())],
    }];
    assert_eq!(
        compile(&subs, &settings).unwrap_err(),
        CompileError::NoNodes
    );
}

#[test]
fn no_resolvers_is_an_error() {
    let mut settings = Settings::new(proxy_mode(), 10085);
    settings.dns.resolvers.clear();

    assert_eq!(
        compile(&subscriptions(), &settings).unwrap_err(),
        CompileError::NoResolvers
    );
}

fn assert_golden(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let expected: Value = serde_json::from_str(&text).unwrap();
    assert!(
        *actual == expected,
        "конфиг не совпал с {name}; актуальный:\n{}",
        serde_json::to_string_pretty(actual).unwrap()
    );
}

#[test]
fn golden_proxy_config() {
    assert_golden("proxy.json", &build(proxy_mode()).config);
}

#[test]
fn golden_proxy_with_password_config() {
    assert_golden("proxy-auth.json", &build(proxy_with_password()).config);
}

#[test]
fn golden_gateway_config() {
    assert_golden("gateway.json", &build(gateway_mode()).config);
}
