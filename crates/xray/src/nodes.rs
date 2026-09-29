use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use serde_json::{Map, Value};

use crate::Node;

const SERVICE_PROTOCOLS: [&str; 4] = ["freedom", "blackhole", "dns", "loopback"];

pub(crate) struct CompiledNode {
    pub(crate) main_tag: String,
    pub(crate) outbounds: Vec<Value>,
    pub(crate) hosts: Vec<String>,
}

/// Переименовывает теги узла, правит ссылки цепочки и добавляет `sockopt`.
///
/// Служебный outbound (`freedom`, `blackhole`, `dns`, `loopback`) остаётся в узле,
/// только если на него ссылается цепочка (так провайдеры делают фрагментацию), иначе
/// выбрасывается.
///
/// `None` — узел непригоден: первый outbound служебный или не объект либо ссылка
/// цепочки указывает на отсутствующий тег. Половина цепочки хуже её отсутствия:
/// трафик пошёл бы в обход промежуточного сервера.
pub(crate) fn compile_node(number: usize, node: &Node, mark: Option<u32>) -> Option<CompiledNode> {
    let prefix = format!("node-{number:03}");
    let main_tag = format!("{prefix}-main");

    let first = node.outbounds.first()?.as_object()?;
    if protocol(first).is_none() || is_service(first) {
        return None;
    }
    let objects: Vec<&Map<String, Value>> =
        node.outbounds.iter().filter_map(Value::as_object).collect();
    let referenced = referenced_tags(&objects);
    let wanted = |outbound: &Map<String, Value>| {
        protocol(outbound).is_some()
            && (!is_service(outbound)
                || outbound
                    .get("tag")
                    .and_then(Value::as_str)
                    .is_some_and(|tag| referenced.contains(tag)))
    };

    let mut renames: HashMap<&str, String> = HashMap::new();
    let mut kept: Vec<(String, &Map<String, Value>)> = Vec::new();
    for (index, value) in node.outbounds.iter().enumerate() {
        let Some(outbound) = value.as_object().filter(|o| wanted(o)) else {
            continue;
        };
        let old_tag = outbound.get("tag").and_then(Value::as_str);
        let new_tag = if index == 0 {
            main_tag.clone()
        } else {
            match old_tag {
                Some(tag) if !tag.is_empty() && !renames.contains_key(tag) => {
                    format!("{prefix}-x-{tag}")
                }
                _ => continue,
            }
        };
        if let Some(tag) = old_tag {
            renames.insert(tag, new_tag.clone());
        }
        kept.push((new_tag, outbound));
    }

    let mut outbounds = Vec::with_capacity(kept.len());
    let mut hosts = Vec::new();
    for (tag, outbound) in kept {
        collect_hosts(outbound, &mut hosts);
        outbounds.push(rewrite(outbound, tag, &renames, mark)?);
    }
    Some(CompiledNode {
        main_tag,
        outbounds,
        hosts,
    })
}

fn protocol(outbound: &Map<String, Value>) -> Option<&str> {
    outbound.get("protocol").and_then(Value::as_str)
}

fn is_service(outbound: &Map<String, Value>) -> bool {
    protocol(outbound).is_some_and(|name| SERVICE_PROTOCOLS.contains(&name))
}

/// Тег, на который outbound ссылается как на предыдущий шаг цепочки.
fn chain_ref(outbound: &Map<String, Value>) -> Option<&str> {
    outbound
        .get("streamSettings")
        .and_then(|stream| stream.get("sockopt"))
        .and_then(|sockopt| sockopt.get("dialerProxy"))
        .or_else(|| outbound.get("proxySettings").and_then(|p| p.get("tag")))
        .and_then(Value::as_str)
}

/// Теги, достижимые по цепочке от неслужебных outbound'ов (и от служебных,
/// которые сами оказались в цепочке).
fn referenced_tags<'a>(outbounds: &[&'a Map<String, Value>]) -> HashSet<&'a str> {
    let mut referenced: HashSet<&str> = HashSet::new();
    loop {
        let before = referenced.len();
        for &outbound in outbounds {
            let in_chain = outbound
                .get("tag")
                .and_then(Value::as_str)
                .is_some_and(|tag| referenced.contains(tag));
            if !is_service(outbound) || in_chain {
                referenced.extend(chain_ref(outbound));
            }
        }
        if referenced.len() == before {
            return referenced;
        }
    }
}

fn rewrite(
    source: &Map<String, Value>,
    tag: String,
    renames: &HashMap<&str, String>,
    mark: Option<u32>,
) -> Option<Value> {
    let mut outbound = source.clone();
    // `proxySettings` удалён из xray: конфиг с ним не собирается, поэтому старую
    // ссылку переносим в `dialerProxy`.
    let legacy = outbound.remove("proxySettings");
    outbound.insert("tag".to_owned(), Value::String(tag));

    with_object(&mut outbound, "streamSettings", |stream| {
        with_object(stream, "sockopt", |sockopt| {
            if let Some(hop) = legacy.as_ref().and_then(|l| l.get("tag")) {
                sockopt.entry("dialerProxy").or_insert_with(|| hop.clone());
            }
            let dialer = match sockopt.get("dialerProxy") {
                None => None,
                Some(Value::String(old)) => Some(old.clone()),
                Some(_) => return None,
            };
            if let Some(old) = dialer.filter(|old| !old.is_empty()) {
                let new = renames.get(old.as_str())?.clone();
                sockopt.insert("dialerProxy".to_owned(), Value::String(new));
            }
            // Без этого xray резолвит адреса серверов системным резолвером,
            // а при перехвате DNS тот отвечает fake-IP.
            sockopt.insert(
                "domainStrategy".to_owned(),
                Value::String("UseIPv4".to_owned()),
            );
            if let Some(mark) = mark {
                sockopt.insert("mark".to_owned(), Value::from(mark));
            }
            Some(())
        })
    })?;
    Some(Value::Object(outbound))
}

/// Выполняет `f` над объектом `map[key]`; отсутствующее или не объект значение
/// заменяется пустым объектом (данные подписки недоверенные).
fn with_object<R>(
    map: &mut Map<String, Value>,
    key: &str,
    f: impl FnOnce(&mut Map<String, Value>) -> R,
) -> R {
    let slot = map.entry(key).or_insert(Value::Null);
    if let Value::Object(inner) = slot {
        return f(inner);
    }
    let mut inner = Map::new();
    let result = f(&mut inner);
    *slot = Value::Object(inner);
    result
}

fn collect_hosts(outbound: &Map<String, Value>, hosts: &mut Vec<String>) {
    let Some(settings) = outbound.get("settings").and_then(Value::as_object) else {
        return;
    };
    hosts.extend(
        settings
            .get("address")
            .and_then(Value::as_str)
            .and_then(domain_host),
    );
    for key in ["vnext", "servers"] {
        let Some(items) = settings.get(key).and_then(Value::as_array) else {
            continue;
        };
        hosts.extend(
            items
                .iter()
                .filter_map(|item| item.get("address")?.as_str())
                .filter_map(domain_host),
        );
    }
}

/// Доменное имя в нижнем регистре; IP-адреса и всё, что на имя не похоже, отбрасывается.
fn domain_host(address: &str) -> Option<String> {
    let host = address.trim().to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    (valid && host.parse::<IpAddr>().is_err()).then_some(host)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(outbounds: Vec<Value>) -> Node {
        Node {
            name: "тест".to_owned(),
            outbounds,
        }
    }

    fn tags(compiled: &CompiledNode) -> Vec<&str> {
        compiled
            .outbounds
            .iter()
            .filter_map(|o| o["tag"].as_str())
            .collect()
    }

    #[test]
    fn renames_tags_and_dialer_proxy() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless",
                   "streamSettings": {"sockopt": {"dialerProxy": "hop"}}}),
            json!({"tag": "hop", "protocol": "vless"}),
            json!({"tag": "direct", "protocol": "freedom"}),
            json!({"tag": "block", "protocol": "blackhole"}),
            json!({"tag": "dns-out", "protocol": "dns"}),
            json!({"tag": "lo", "protocol": "loopback"}),
        ]);
        let compiled = compile_node(7, &node, None).unwrap();

        assert_eq!(compiled.main_tag, "node-007-main");
        assert_eq!(tags(&compiled), ["node-007-main", "node-007-x-hop"]);
        assert_eq!(
            compiled.outbounds[0]["streamSettings"]["sockopt"]["dialerProxy"],
            "node-007-x-hop"
        );
    }

    #[test]
    fn legacy_proxy_settings_become_dialer_proxy() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "trojan", "proxySettings": {"tag": "relay"}}),
            json!({"tag": "relay", "protocol": "vless"}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        let main = &compiled.outbounds[0];
        assert!(main.get("proxySettings").is_none());
        assert_eq!(
            main["streamSettings"]["sockopt"]["dialerProxy"],
            "node-001-x-relay"
        );
    }

    #[test]
    fn dialer_proxy_wins_over_legacy_proxy_settings() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless",
                   "proxySettings": {"tag": "b"},
                   "streamSettings": {"sockopt": {"dialerProxy": "a"}}}),
            json!({"tag": "a", "protocol": "vless"}),
            json!({"tag": "b", "protocol": "vless"}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        assert_eq!(
            compiled.outbounds[0]["streamSettings"]["sockopt"]["dialerProxy"],
            "node-001-x-a"
        );
    }

    #[test]
    fn dangling_chain_reference_drops_the_node() {
        let node = node(vec![json!({"tag": "proxy", "protocol": "vless",
                   "streamSettings": {"sockopt": {"dialerProxy": "gone"}}})]);
        assert!(compile_node(1, &node, None).is_none());
    }

    #[test]
    fn non_string_dialer_proxy_drops_the_node() {
        let node = node(vec![json!({"tag": "proxy", "protocol": "vless",
                   "streamSettings": {"sockopt": {"dialerProxy": 5}}})]);
        assert!(compile_node(1, &node, None).is_none());
    }

    #[test]
    fn service_or_invalid_first_outbound_drops_the_node() {
        for first in [
            json!({"tag": "direct", "protocol": "freedom"}),
            json!({"tag": "x"}),
            json!("garbage"),
        ] {
            let node = node(vec![first, json!({"tag": "b", "protocol": "vless"})]);
            assert!(compile_node(1, &node, None).is_none());
        }
        assert!(compile_node(1, &node(Vec::new()), None).is_none());
    }

    #[test]
    fn duplicate_and_untagged_chain_outbounds_are_skipped() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless"}),
            json!({"tag": "proxy", "protocol": "vless"}),
            json!({"protocol": "vless"}),
            json!({"tag": "", "protocol": "vless"}),
            json!(42),
            json!({"tag": "ok", "protocol": "vless"}),
        ]);
        let compiled = compile_node(3, &node, None).unwrap();

        assert_eq!(tags(&compiled), ["node-003-main", "node-003-x-ok"]);
    }

    #[test]
    fn sockopt_gets_domain_strategy_and_optional_mark() {
        let node = node(vec![json!({
            "tag": "proxy",
            "protocol": "vless",
            "streamSettings": {"network": "tcp", "sockopt": {"tcpFastOpen": true, "domainStrategy": "AsIs"}}
        })]);

        let plain = compile_node(1, &node, None).unwrap();
        let sockopt = &plain.outbounds[0]["streamSettings"]["sockopt"];
        assert_eq!(sockopt["domainStrategy"], "UseIPv4");
        assert_eq!(sockopt["tcpFastOpen"], true);
        assert!(sockopt.get("mark").is_none());
        assert_eq!(plain.outbounds[0]["streamSettings"]["network"], "tcp");

        let marked = compile_node(1, &node, Some(255)).unwrap();
        assert_eq!(
            marked.outbounds[0]["streamSettings"]["sockopt"]["mark"],
            255
        );
    }

    #[test]
    fn garbage_stream_settings_are_replaced() {
        let node = node(vec![
            json!({"tag": "a", "protocol": "vless", "streamSettings": "x"}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        assert_eq!(
            compiled.outbounds[0]["streamSettings"]["sockopt"]["domainStrategy"],
            "UseIPv4"
        );
    }

    #[test]
    fn referenced_freedom_is_part_of_the_chain_and_unused_one_is_dropped() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless",
                   "streamSettings": {"sockopt": {"dialerProxy": "fragment"}}}),
            json!({"tag": "fragment", "protocol": "freedom",
                   "settings": {"fragment": {"packets": "tlshello", "length": "100-200"}}}),
            json!({"tag": "direct", "protocol": "freedom"}),
        ]);
        let compiled = compile_node(2, &node, Some(255)).unwrap();

        assert_eq!(tags(&compiled), ["node-002-main", "node-002-x-fragment"]);
        assert_eq!(
            compiled.outbounds[0]["streamSettings"]["sockopt"]["dialerProxy"],
            "node-002-x-fragment"
        );
        let fragment = &compiled.outbounds[1];
        assert_eq!(fragment["protocol"], "freedom");
        assert_eq!(fragment["settings"]["fragment"]["packets"], "tlshello");
        assert_eq!(fragment["streamSettings"]["sockopt"]["mark"], 255);
        assert_eq!(
            fragment["streamSettings"]["sockopt"]["domainStrategy"],
            "UseIPv4"
        );
    }

    #[test]
    fn service_outbound_referenced_through_a_chain_hop_is_kept() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless",
                   "streamSettings": {"sockopt": {"dialerProxy": "hop"}}}),
            json!({"tag": "hop", "protocol": "vless",
                   "proxySettings": {"tag": "noise"}}),
            json!({"tag": "noise", "protocol": "freedom"}),
            json!({"tag": "unused", "protocol": "blackhole"}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        assert_eq!(
            tags(&compiled),
            ["node-001-main", "node-001-x-hop", "node-001-x-noise"]
        );
    }

    #[test]
    fn service_outbound_referenced_only_by_an_unused_one_is_dropped() {
        let node = node(vec![
            json!({"tag": "proxy", "protocol": "vless"}),
            json!({"tag": "a", "protocol": "freedom",
                   "streamSettings": {"sockopt": {"dialerProxy": "b"}}}),
            json!({"tag": "b", "protocol": "freedom"}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        assert_eq!(tags(&compiled), ["node-001-main"]);
    }

    #[test]
    fn collects_only_domain_hosts_from_all_settings_shapes() {
        let node = node(vec![
            json!({"tag": "a", "protocol": "vless", "settings": {"vnext": [
                {"address": "Vnext.Example.com"},
                {"address": "203.0.113.5"},
                {"address": ""},
                {"address": "2001:db8::1"},
                {"port": 1}
            ]}}),
            json!({"tag": "b", "protocol": "trojan", "settings": {"servers": [
                {"address": "servers.example.com"}
            ]}}),
            json!({"tag": "c", "protocol": "hysteria", "settings": {"address": "flat.example.com"}}),
            json!({"tag": "d", "protocol": "vless", "settings": {"address": "bad host\"", "vnext": 5}}),
        ]);
        let compiled = compile_node(1, &node, None).unwrap();

        assert_eq!(
            compiled.hosts,
            [
                "vnext.example.com",
                "servers.example.com",
                "flat.example.com"
            ]
        );
    }
}
