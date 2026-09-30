//! Узлы подписок и настройки → конфиг xray.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};
use raycat_config::{Config, Mode, Subscription};
use raycat_xray::{Node, Settings, compile};

const GATEWAY_UNAVAILABLE: &str = "режим шлюза появится в следующей версии";

/// Адрес прокси из настроек; режим шлюза пока не поддерживается.
pub(crate) fn proxy_listen(config: &Config) -> Result<SocketAddr> {
    match config.mode {
        Mode::Proxy { listen } => Ok(listen),
        Mode::Gateway { .. } => bail!(GATEWAY_UNAVAILABLE),
    }
}

pub(crate) struct Plan {
    pub(crate) json: Vec<u8>,
    /// Узлов в конфиге и узлов, которые xray не поддерживает.
    pub(crate) nodes: usize,
    pub(crate) skipped: usize,
}

/// Собирает конфиг из узлов подписок в порядке настроек (это их приоритет), оставив
/// узлы, которые пропускают `allow` и `deny` своей подписки.
pub(crate) fn compile_config(
    config: &Config,
    inputs: &[(&Subscription, &[Node])],
    api_port: u16,
) -> Result<Plan> {
    let listen = proxy_listen(config)?;
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
    let mut settings = Settings::new(raycat_xray::Mode::Proxy { listen }, api_port);
    settings.dns.resolvers.clone_from(&config.dns.resolvers);
    settings.probe.url.clone_from(&config.selection.check_url);
    settings.probe.interval = config.selection.check_interval;
    let compiled = compile(&subscriptions, &settings)?;
    let json = serde_json::to_vec(&compiled.config).context("не удалось записать конфиг xray")?;
    Ok(Plan {
        json,
        nodes: compiled.tags.entries().len(),
        skipped: compiled.skipped.len(),
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
        compile_config(config, &[(&config.subscriptions[0], nodes)], 10_085)
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
    fn proxy_mode_gives_the_listen_address() {
        let config = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        assert_eq!(proxy_listen(&config).unwrap(), "127.0.0.1:7891".parse().unwrap());
    }

    #[test]
    fn gateway_mode_is_not_available_yet() {
        let config = config("type = \"gateway\"", "");
        let error = proxy_listen(&config).unwrap_err();
        assert_eq!(error.to_string(), "режим шлюза появится в следующей версии");
        assert!(plan(&config, &nodes()).is_err());
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
        let plan = compile_config(&config, &inputs, 10_085).unwrap();
        assert_eq!(plan.nodes, 2);
    }
}
