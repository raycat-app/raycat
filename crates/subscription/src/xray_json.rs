//! Формат xray-json: массив полных конфигов xray (по одному на сервер) или один
//! конфиг. Так отвечают Remnawave (`serveJsonAtBaseSubscription`) и Marzban
//! клиенту Happ.

use raycat_xray::Node;
use serde_json::Value;

use crate::body::{Content, MAX_NODES};
use crate::stub::{Endpoint, endpoint_of};
use crate::text::{clean, push_warning};

/// Служебные outbound'ы маршрутизации: узлом они не бывают.
const HELPER_PROTOCOLS: [&str; 4] = ["freedom", "blackhole", "dns", "loopback"];
const MAX_OUTBOUNDS: usize = 256;
const MAX_NAME: usize = 200;

/// Похож ли JSON на конфиг xray (есть outbound'ы с полем `protocol`).
pub(crate) fn is_xray(value: &Value) -> bool {
    let config = match value {
        Value::Array(items) => items.first(),
        other => Some(other),
    };
    config
        .and_then(|config| config.get("outbounds"))
        .and_then(Value::as_array)
        .is_some_and(|outbounds| {
            outbounds
                .iter()
                .any(|outbound| outbound.get("protocol").is_some())
        })
}

pub(crate) fn parse(value: &Value) -> Content {
    let configs: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut content = Content::default();
    for (index, config) in configs.into_iter().enumerate() {
        if content.nodes.len() >= MAX_NODES {
            push_warning(
                &mut content.warnings,
                format!("лишние конфиги отброшены: узлов больше {MAX_NODES}"),
            );
            break;
        }
        match build_node(config, index + 1) {
            Ok(node) => content.nodes.push(node),
            Err(reason) => push_warning(&mut content.warnings, reason),
        }
    }
    content
}

fn tag_of(outbound: &Value) -> Option<&str> {
    outbound.get("tag").and_then(Value::as_str)
}

fn is_proxy(outbound: &Value) -> bool {
    outbound
        .get("protocol")
        .and_then(Value::as_str)
        .is_some_and(|protocol| !HELPER_PROTOCOLS.contains(&protocol))
}

/// Теги, на которые outbound ссылается как на цепочку (`dialerProxy`, `proxySettings`).
fn referenced_tags(outbound: &Value) -> impl Iterator<Item = &str> {
    [
        outbound.pointer("/streamSettings/sockopt/dialerProxy"),
        outbound.pointer("/proxySettings/tag"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .filter(|tag| !tag.is_empty())
}

fn build_node(config: &Value, position: usize) -> Result<Node, String> {
    let remarks = config
        .get("remarks")
        .and_then(Value::as_str)
        .map(|text| clean(text, MAX_NAME))
        .filter(|text| !text.is_empty());
    let label = remarks.clone().unwrap_or_else(|| format!("№{position}"));
    let outbounds = config
        .get("outbounds")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("конфиг «{label}» пропущен: нет списка outbounds"))?;
    if outbounds.len() > MAX_OUTBOUNDS {
        return Err(format!(
            "конфиг «{label}» пропущен: слишком много outbound'ов"
        ));
    }
    let proxies: Vec<usize> = (0..outbounds.len())
        .filter(|&index| is_proxy(&outbounds[index]))
        .collect();
    let exit = proxies
        .iter()
        .copied()
        .find(|&index| tag_of(&outbounds[index]) == Some("proxy"))
        .or_else(|| proxies.first().copied());
    let Some(exit) = exit else {
        return Err(format!("конфиг «{label}» пропущен: нет прокси-outbound'ов"));
    };
    let Some(endpoint) = endpoint_of(&outbounds[exit]) else {
        return Err(format!(
            "конфиг «{label}» пропущен: у выхода нет адреса сервера"
        ));
    };

    let mut order = vec![exit];
    include_chain(outbounds, &mut order);

    let name = remarks
        .or_else(|| tag_of(&outbounds[exit]).map(|tag| clean(tag, MAX_NAME)))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| default_name(&endpoint));
    Ok(Node {
        name,
        outbounds: order
            .iter()
            .map(|&index| outbounds[index].clone())
            .collect(),
    })
}

/// Добавляет к выходу его цепочку: outbound'ы, на которые выход (прямо или через
/// другие звенья) ссылается по `dialerProxy` или `proxySettings`. Так в узле
/// остаётся `freedom` с фрагментацией; прочие outbound'ы конфига (запасные
/// прокси для балансировщика провайдера, `direct`, `block`) в узел не попадают.
fn include_chain(outbounds: &[Value], order: &mut Vec<usize>) {
    loop {
        let wanted: Vec<&str> = order
            .iter()
            .flat_map(|&index| referenced_tags(&outbounds[index]))
            .collect();
        let missing: Vec<usize> = (0..outbounds.len())
            .filter(|index| !order.contains(index))
            .filter(|&index| tag_of(&outbounds[index]).is_some_and(|tag| wanted.contains(&tag)))
            .collect();
        if missing.is_empty() {
            return;
        }
        order.extend(missing);
    }
}

fn default_name(endpoint: &Endpoint) -> String {
    format!("{}:{}", clean(&endpoint.server, MAX_NAME), endpoint.port)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn vless(tag: &str, address: &str) -> Value {
        json!({
            "tag": tag,
            "protocol": "vless",
            "settings": {"vnext": [{"address": address, "port": 443,
                "users": [{"id": "00000000-0000-0000-0000-000000000000", "encryption": "none"}]}]},
            "streamSettings": {"network": "raw", "security": "none"},
        })
    }

    #[test]
    fn detection() {
        assert!(is_xray(&json!([{"outbounds": [{"protocol": "vless"}]}])));
        assert!(is_xray(&json!({"outbounds": [{"protocol": "freedom"}]})));
        assert!(!is_xray(&json!({"outbounds": [{"type": "vless"}]})));
        assert!(!is_xray(&json!({"proxies": []})));
        assert!(!is_xray(&json!([])));
    }

    #[test]
    fn array_of_configs() {
        let doc = json!([
            {"remarks": "🇳🇱 Нидерланды", "outbounds": [
                vless("proxy", "nl.example.com"),
                {"tag": "direct", "protocol": "freedom"},
                {"tag": "block", "protocol": "blackhole"},
                {"tag": "dns-out", "protocol": "dns"},
                {"tag": "lo", "protocol": "loopback"},
            ], "routing": {"rules": []}},
            {"remarks": "Второй", "outbounds": [vless("proxy", "de.example.com")]},
        ]);
        let content = parse(&doc);
        assert!(content.warnings.is_empty(), "{:?}", content.warnings);
        assert_eq!(content.nodes.len(), 2);
        assert_eq!(content.nodes[0].name, "🇳🇱 Нидерланды");
        assert_eq!(content.nodes[0].outbounds.len(), 1);
        assert_eq!(content.nodes[1].name, "Второй");
    }

    #[test]
    fn single_config() {
        let doc = json!({"remarks": "Один", "outbounds": [
            {"tag": "direct", "protocol": "freedom"},
            vless("proxy", "one.example.com"),
        ]});
        let content = parse(&doc);
        assert_eq!(content.nodes.len(), 1);
        assert_eq!(content.nodes[0].name, "Один");
        assert_eq!(content.nodes[0].outbounds[0]["tag"], "proxy");
    }

    fn tags(node: &Node) -> Vec<String> {
        node.outbounds
            .iter()
            .map(|outbound| outbound["tag"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn exit_is_the_proxy_tag_else_the_first() {
        let doc = json!([
            {"remarks": "по тегу", "outbounds": [
                vless("first", "a.example.com"), vless("proxy", "b.example.com")]},
            {"remarks": "без тега", "outbounds": [
                {"tag": "direct", "protocol": "freedom"},
                vless("x", "c.example.com"), vless("y", "d.example.com")]},
        ]);
        let content = parse(&doc);
        assert_eq!(tags(&content.nodes[0]), ["proxy"]);
        assert_eq!(tags(&content.nodes[1]), ["x"]);
        assert_eq!(
            content.nodes[0].outbounds[0]["settings"]["vnext"][0]["address"],
            "b.example.com"
        );
    }

    #[test]
    fn unreferenced_outbounds_stay_out_of_the_node() {
        let doc = json!({"remarks": "Балансировщик", "outbounds": [
            vless("proxy", "a.example.com"),
            vless("spare-1", "b.example.com"),
            vless("spare-2", "c.example.com"),
            {"tag": "direct", "protocol": "freedom"},
            {"tag": "block", "protocol": "blackhole"},
        ]});
        assert_eq!(tags(&parse(&doc).nodes[0]), ["proxy"]);
    }

    #[test]
    fn fragment_freedom_is_part_of_the_chain() {
        for fragment in [
            json!({"packets": "tlshello", "length": "100-200", "interval": "10-20"}),
            json!({"packets": "tlshello"}),
        ] {
            let mut exit = vless("proxy", "exit.example.com");
            exit["streamSettings"]["sockopt"] = json!({"dialerProxy": "fragment"});
            let helper = json!({"tag": "fragment", "protocol": "freedom",
                "settings": {"fragment": fragment}});
            let doc = json!([{"remarks": "Фрагментация", "outbounds": [
                helper.clone(), exit.clone(), {"tag": "block", "protocol": "blackhole"}]}]);
            let content = parse(&doc);
            let node = &content.nodes[0];
            assert_eq!(tags(node), ["proxy", "fragment"]);
            assert_eq!(node.outbounds[1], helper);
            assert_eq!(node.outbounds[0], exit);
        }
        let mut exit = vless("proxy", "exit.example.com");
        exit["streamSettings"]["sockopt"] = json!({"dialerProxy": "noise"});
        let noises = json!({"tag": "noise", "protocol": "freedom",
            "settings": {"noises": [{"type": "rand", "packet": "10-20", "delay": "10-16"}]}});
        let doc = json!({"outbounds": [exit, noises]});
        assert_eq!(tags(&parse(&doc).nodes[0]), ["proxy", "noise"]);
    }

    #[test]
    fn dialer_proxy_chain_is_kept() {
        let mut exit = vless("proxy", "exit.example.com");
        exit["streamSettings"]["sockopt"] = json!({"dialerProxy": "hop"});
        let mut hop = vless("hop", "hop.example.com");
        hop["streamSettings"]["sockopt"] = json!({"dialerProxy": "fragment"});
        let fragment = json!({"tag": "fragment", "protocol": "freedom",
            "settings": {"fragment": {"packets": "tlshello", "length": "100-200", "interval": "10-20"}}});
        let unrelated = json!({"tag": "direct", "protocol": "freedom"});
        let doc = json!({"remarks": "Цепочка", "outbounds": [unrelated, fragment, hop, exit]});
        let content = parse(&doc);
        assert_eq!(content.nodes.len(), 1, "{:?}", content.warnings);
        let tags: Vec<&str> = content.nodes[0]
            .outbounds
            .iter()
            .map(|outbound| outbound["tag"].as_str().unwrap())
            .collect();
        assert_eq!(tags, ["proxy", "hop", "fragment"]);
    }

    #[test]
    fn proxy_settings_reference_counts_as_chain() {
        let mut exit = vless("proxy", "exit.example.com");
        exit["proxySettings"] = json!({"tag": "front"});
        let doc = json!({"outbounds": [exit, {"tag": "front", "protocol": "freedom"}]});
        let content = parse(&doc);
        assert_eq!(content.nodes[0].outbounds.len(), 2);
    }

    #[test]
    fn config_without_proxies_is_skipped_with_a_warning() {
        let doc = json!([
            {"remarks": "Пустой", "outbounds": [{"tag": "direct", "protocol": "freedom"}]},
            {"remarks": "Без списка"},
            {"remarks": "Без адреса", "outbounds": [{"tag": "proxy", "protocol": "vless", "settings": {}}]},
            {"remarks": "Рабочий", "outbounds": [vless("proxy", "ok.example.com")]},
            "не объект",
        ]);
        let content = parse(&doc);
        assert_eq!(content.nodes.len(), 1);
        assert_eq!(content.nodes[0].name, "Рабочий");
        assert_eq!(content.warnings.len(), 4);
        assert!(content.warnings[0].contains("Пустой"));
    }

    #[test]
    fn name_falls_back_to_tag_then_address() {
        let doc = json!([
            {"outbounds": [vless("proxy", "a.example.com")]},
            {"remarks": "  ", "outbounds": [{"protocol": "vless", "settings": {"address": "b.example.com", "port": 8443, "id": "x"}}]},
        ]);
        let content = parse(&doc);
        assert_eq!(content.nodes[0].name, "proxy");
        assert_eq!(content.nodes[1].name, "b.example.com:8443");
    }
}
