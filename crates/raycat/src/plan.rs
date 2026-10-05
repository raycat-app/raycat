//! Узлы подписок и настройки → конфиг xray.

use anyhow::{Context, Result, bail};
use raycat_config::{Config, Mode, Subscription};
use raycat_netfilter::{DEFAULT_OWN_MARK, Rules};
use raycat_xray::{Node, Settings, TagTable, compile};

const LAN_UNAVAILABLE: &str = "шлюз для локальной сети (lan) появится в следующей версии";

/// Правила перехвата для режима шлюза; `None` в режиме прокси.
pub(crate) fn gateway_rules(config: &Config) -> Result<Option<Rules>> {
    match config.mode {
        Mode::Proxy { .. } => Ok(None),
        Mode::Gateway { kill_switch, lan } => {
            if lan {
                bail!(LAN_UNAVAILABLE);
            }
            let rules = Rules {
                kill_switch,
                ..Rules::default()
            };
            rules.validate()?;
            Ok(Some(rules))
        }
    }
}

/// Режим для строки лога и вывода `check`.
pub(crate) fn describe_mode(config: &Config) -> String {
    match config.mode {
        Mode::Proxy { listen } => format!("режим прокси, адрес {listen}"),
        Mode::Gateway { kill_switch, .. } => format!(
            "режим шлюза, kill switch {}",
            if kill_switch {
                "включён"
            } else {
                "выключен"
            }
        ),
    }
}

/// Метка собственных сокетов демона: в режиме шлюза kill switch выпускает наружу
/// только помеченные пакеты.
pub(crate) fn own_mark(config: &Config) -> Option<u32> {
    match config.mode {
        Mode::Proxy { .. } => None,
        Mode::Gateway { .. } => Some(DEFAULT_OWN_MARK),
    }
}

fn xray_mode(config: &Config) -> Result<raycat_xray::Mode> {
    Ok(match (config.mode, gateway_rules(config)?) {
        (Mode::Proxy { listen }, _) => raycat_xray::Mode::Proxy { listen },
        (Mode::Gateway { .. }, Some(rules)) => raycat_xray::Mode::Gateway {
            tproxy_port: rules.tproxy_port,
            mark: rules.own_mark,
        },
        (Mode::Gateway { .. }, None) => bail!("режим шлюза без правил перехвата"),
    })
}

/// Тег балансировщика в конфиге xray: в нём демон закрепляет выбранный узел.
pub(crate) const BALANCER: &str = "auto";

pub(crate) struct Plan {
    pub(crate) json: Vec<u8>,
    /// Узлов в конфиге и узлов, которые xray не поддерживает.
    pub(crate) nodes: usize,
    pub(crate) skipped: usize,
    pub(crate) tags: TagTable,
    /// В конфиге есть узлы на UDP-транспортах (QUIC и подобные).
    pub(crate) quic: bool,
}

/// Собирает конфиг из узлов подписок в порядке настроек (это их приоритет), оставив
/// узлы, которые пропускают `allow` и `deny` своей подписки. `tcp_congestion` —
/// уже выбранный алгоритм (см. `tuning`), а не настройка из файла.
pub(crate) fn compile_config(
    config: &Config,
    inputs: &[(&Subscription, &[Node])],
    api_port: u16,
    tcp_congestion: Option<&str>,
) -> Result<Plan> {
    let mode = xray_mode(config)?;
    let subscriptions: Vec<raycat_xray::Subscription> = inputs
        .iter()
        .map(|(subscription, nodes)| raycat_xray::Subscription {
            id: subscription.name.clone(),
            nodes: nodes
                .iter()
                .filter(|node| subscription.allows(&node.name))
                .cloned()
                .collect(),
        })
        .collect();
    let mut settings = Settings::new(mode, api_port);
    settings.dns.resolvers.clone_from(&config.dns.resolvers);
    settings.probe.url.clone_from(&config.selection.check_url);
    settings.probe.interval = config.selection.check_interval;
    settings.tcp_congestion = tcp_congestion.map(str::to_owned);
    settings.xhttp_connections = config.xray.xhttp_connections;
    let compiled = compile(&subscriptions, &settings)?;
    let json = serde_json::to_vec(&compiled.config).context("не удалось записать конфиг xray")?;
    Ok(Plan {
        json,
        nodes: compiled.tags.entries().len(),
        skipped: compiled.skipped.len(),
        tags: compiled.tags,
        quic: compiled.quic,
    })
}

#[cfg(test)]
mod tests {
    use raycat_config::Env;
    use raycat_subscription::analyze;

    use super::*;

    const LINKS: &[u8] = b"ss://aes-128-gcm:secret@203.0.113.5:8388#One\nss://aes-128-gcm:secret@203.0.113.6:8388#Two\n";

    fn config(mode: &str, subscription_extra: &str) -> Config {
        let text = format!(
            "[[subscription]]\nname = \"a\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n{subscription_extra}\n[mode]\n{mode}\n"
        );
        Config::from_toml_str(&text, &Env::new()).unwrap()
    }

    fn nodes() -> Vec<Node> {
        let analysis = analyze(200, &[], LINKS);
        assert!(analysis.problem.is_none());
        analysis.nodes
    }

    fn plan(config: &Config, nodes: &[Node]) -> Result<Plan> {
        compile_config(config, &[(&config.subscriptions[0], nodes)], 10_085, None)
    }

    fn outbound_tags(plan: &Plan) -> Vec<String> {
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|outbound| outbound["tag"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn proxy_mode_needs_no_rules_and_no_mark() {
        let config = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        assert_eq!(gateway_rules(&config).unwrap(), None);
        assert_eq!(own_mark(&config), None);
    }

    #[test]
    fn gateway_mode_takes_the_kill_switch_from_the_settings() {
        let on = config("type = \"gateway\"", "");
        let rules = gateway_rules(&on).unwrap().unwrap();
        assert!(rules.kill_switch);
        assert!(!rules.intercept_ipv6);
        assert_eq!(own_mark(&on), Some(rules.own_mark));

        let off = config("type = \"gateway\"\nkill_switch = false", "");
        assert!(!gateway_rules(&off).unwrap().unwrap().kill_switch);
    }

    #[test]
    fn the_mode_is_described_for_people() {
        let proxy = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        assert_eq!(describe_mode(&proxy), "режим прокси, адрес 127.0.0.1:7891");
        let gateway = config("type = \"gateway\"", "");
        assert_eq!(describe_mode(&gateway), "режим шлюза, kill switch включён");
        let open = config("type = \"gateway\"\nkill_switch = false", "");
        assert_eq!(describe_mode(&open), "режим шлюза, kill switch выключен");
    }

    #[test]
    fn a_lan_gateway_is_not_available_yet() {
        let config = config("type = \"gateway\"\nlan = true", "");
        let error = gateway_rules(&config).unwrap_err();
        assert!(error.to_string().contains("lan"), "{error}");
        assert!(plan(&config, &nodes()).is_err());
    }

    #[test]
    fn the_gateway_config_has_a_tproxy_inbound_and_marked_sockets() {
        let config = config("type = \"gateway\"", "");
        let plan = plan(&config, &nodes()).unwrap();
        let rules = gateway_rules(&config).unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["inbounds"][0]["tag"], "tproxy-in");
        assert_eq!(json["inbounds"][0]["port"], rules.tproxy_port);
        let text = String::from_utf8(plan.json).unwrap();
        assert!(text.contains(&format!("\"mark\":{}", rules.own_mark)));
    }

    #[test]
    fn the_config_carries_settings_and_every_node() {
        let config = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        let plan = plan(&config, &nodes()).unwrap();
        assert_eq!((plan.nodes, plan.skipped), (2, 0));
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["api"]["listen"], "127.0.0.1:10085");
        assert_eq!(json["inbounds"][0]["port"], 7891);
        assert_eq!(json["inbounds"][0]["listen"], "127.0.0.1");
        let tags = outbound_tags(&plan);
        assert!(tags.iter().any(|tag| tag == "node-001-main"));
        assert!(tags.iter().any(|tag| tag == "node-002-main"));
    }

    #[test]
    fn the_balancer_tag_matches_the_constant() {
        let config = config("type = \"proxy\"", "");
        let plan = plan(&config, &nodes()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["routing"]["balancers"][0]["tag"], BALANCER);
        assert_eq!(plan.tags.entries().len(), 2);
    }

    #[test]
    fn the_same_input_gives_the_same_bytes() {
        let config = config("type = \"proxy\"", "");
        let first = plan(&config, &nodes()).unwrap();
        let second = plan(&config, &nodes()).unwrap();
        assert_eq!(first.json, second.json);
    }

    #[test]
    fn deny_and_allow_filter_the_nodes() {
        let denied = config("type = \"proxy\"", "deny = [\"Two\"]");
        assert_eq!(plan(&denied, &nodes()).unwrap().nodes, 1);
        let allowed = config("type = \"proxy\"", "allow = [\"Tw*\"]");
        assert_eq!(plan(&allowed, &nodes()).unwrap().nodes, 1);
    }

    #[test]
    fn filtering_everything_out_is_an_error() {
        let config = config("type = \"proxy\"", "deny = [\"*\"]");
        assert!(plan(&config, &nodes()).is_err());
    }

    #[test]
    fn subscriptions_keep_the_order_of_the_settings() {
        let text = "[[subscription]]\nname = \"first\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n[[subscription]]\nname = \"second\"\nurl = \"https://b.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"android\"\n";
        let config = Config::from_toml_str(text, &Env::new()).unwrap();
        let all = nodes();
        let inputs = [
            (&config.subscriptions[0], &all[1..]),
            (&config.subscriptions[1], &all[..1]),
        ];
        let plan = compile_config(&config, &inputs, 10_085, None).unwrap();
        assert_eq!(plan.nodes, 2);
    }

    fn xhttp_node() -> Node {
        Node {
            name: "xhttp".to_owned(),
            outbounds: vec![serde_json::json!({
                "tag": "proxy",
                "protocol": "vless",
                "settings": {"vnext": [{
                    "address": "203.0.113.9",
                    "port": 443,
                    "users": [{"id": "00000000-0000-0000-0000-000000000000", "encryption": "none"}]
                }]},
                "streamSettings": {"network": "xhttp", "security": "tls"}
            })],
        }
    }

    fn json_of(config: &Config, tcp_congestion: Option<&str>) -> serde_json::Value {
        let nodes = [xhttp_node()];
        let plan = compile_config(
            config,
            &[(&config.subscriptions[0], nodes.as_slice())],
            10_085,
            tcp_congestion,
        )
        .unwrap();
        serde_json::from_slice(&plan.json).unwrap()
    }

    #[test]
    fn performance_settings_reach_the_compiler() {
        let config = config("type = \"proxy\"\n[xray]\nxhttp_connections = 6", "");
        let json = json_of(&config, Some("bbr"));
        let stream = &json["outbounds"][0]["streamSettings"];

        assert_eq!(stream["sockopt"]["tcpCongestion"], "bbr");
        assert_eq!(stream["xhttpSettings"]["xmux"]["maxConnections"], 6);
        assert_eq!(json["log"]["access"], "none");
    }

    #[test]
    fn without_settings_nothing_is_tuned() {
        let config = config("type = \"proxy\"", "");
        let json = json_of(&config, None);
        let text = serde_json::to_string(&json).unwrap();

        assert!(!text.contains("tcpCongestion"));
        assert!(!text.contains("xmux"));
    }

    #[test]
    fn plain_nodes_are_not_quic() {
        let config = config("type = \"proxy\"", "");
        assert!(!plan(&config, &nodes()).unwrap().quic);
    }
}
