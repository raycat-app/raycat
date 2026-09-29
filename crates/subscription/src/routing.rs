use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::text::{clean, decode_base64, strip_prefix_ci};

const PREFIX: &str = "happ://routing/";
const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
const MAX_LIST: usize = 20_000;

/// Профиль маршрутизации, который провайдер передаёт клиенту Happ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Routing {
    /// `happ://routing/off`: маршрутизация выключена.
    Off,
    Profile(Box<RoutingProfile>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RoutingProfile {
    /// Ссылка `onadd` просит сразу включить профиль, `add` — только добавить.
    pub activate: bool,
    pub name: String,
    pub global_proxy: bool,
    pub fake_dns: bool,
    pub domain_strategy: String,
    pub remote_dns: DnsServer,
    pub domestic_dns: DnsServer,
    pub geoip_url: String,
    pub geosite_url: String,
    pub last_updated: Option<u64>,
    pub dns_hosts: BTreeMap<String, String>,
    pub direct_sites: Vec<String>,
    pub direct_ip: Vec<String>,
    pub proxy_sites: Vec<String>,
    pub proxy_ip: Vec<String>,
    pub block_sites: Vec<String>,
    pub block_ip: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DnsServer {
    /// Протокол: `DoH`, `DoU` и т. п.
    pub kind: String,
    pub domain: String,
    pub ip: String,
}

/// Разбирает значение заголовка `routing` (или строку в теле ответа).
pub(crate) fn parse(value: &str) -> Result<Routing, String> {
    let value = value.trim();
    let rest = strip_prefix_ci(value, PREFIX)
        .ok_or_else(|| "значение не начинается с happ://routing/".to_owned())?;
    if rest.trim_end_matches('/').eq_ignore_ascii_case("off") {
        return Ok(Routing::Off);
    }
    let (action, payload) = rest
        .split_once('/')
        .ok_or_else(|| "в ссылке нет профиля".to_owned())?;
    let activate = match action.to_ascii_lowercase().as_str() {
        "add" => false,
        "onadd" => true,
        other => {
            return Err(format!(
                "неизвестное действие «{}»",
                clean(other, 32)
            ));
        }
    };
    if payload.len() > MAX_PAYLOAD {
        return Err("профиль слишком большой".to_owned());
    }
    let bytes = decode_base64(payload).ok_or_else(|| "профиль не в формате base64".to_owned())?;
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "профиль не является корректным JSON".to_owned())?;
    let object = json
        .as_object()
        .ok_or_else(|| "профиль не является JSON-объектом".to_owned())?;
    // Регистр имён полей у провайдеров разный.
    let fields: BTreeMap<String, &Value> = object
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value))
        .collect();
    Ok(Routing::Profile(Box::new(profile(&fields, activate))))
}

fn profile(fields: &BTreeMap<String, &Value>, activate: bool) -> RoutingProfile {
    RoutingProfile {
        activate,
        name: text(fields, "name"),
        global_proxy: flag(fields, "globalproxy"),
        fake_dns: flag(fields, "fakedns"),
        domain_strategy: text(fields, "domainstrategy"),
        remote_dns: DnsServer {
            kind: text(fields, "remotednstype"),
            domain: text(fields, "remotednsdomain"),
            ip: text(fields, "remotednsip"),
        },
        domestic_dns: DnsServer {
            kind: text(fields, "domesticdnstype"),
            domain: text(fields, "domesticdnsdomain"),
            ip: text(fields, "domesticdnsip"),
        },
        geoip_url: text(fields, "geoipurl"),
        geosite_url: text(fields, "geositeurl"),
        last_updated: text(fields, "lastupdated").parse().ok(),
        dns_hosts: hosts(fields),
        direct_sites: list(fields, "directsites"),
        direct_ip: list(fields, "directip"),
        proxy_sites: list(fields, "proxysites"),
        proxy_ip: list(fields, "proxyip"),
        block_sites: list(fields, "blocksites"),
        block_ip: list(fields, "blockip"),
    }
}

fn text(fields: &BTreeMap<String, &Value>, key: &str) -> String {
    match fields.get(key) {
        Some(Value::String(value)) => clean(value, 2048),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

/// Булевы поля Happ — строки `"true"`/`"false"`, но встречаются и настоящие.
fn flag(fields: &BTreeMap<String, &Value>, key: &str) -> bool {
    match fields.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => {
            value.eq_ignore_ascii_case("true") || value == "1"
        }
        _ => false,
    }
}

fn list(fields: &BTreeMap<String, &Value>, key: &str) -> Vec<String> {
    let Some(Value::Array(items)) = fields.get(key) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(|item| clean(item, 512))
        .filter(|item| !item.is_empty())
        .take(MAX_LIST)
        .collect()
}

fn hosts(fields: &BTreeMap<String, &Value>) -> BTreeMap<String, String> {
    let Some(Value::Object(entries)) = fields.get("dnshosts") else {
        return BTreeMap::new();
    };
    entries
        .iter()
        .filter_map(|(domain, target)| {
            let target = match target {
                Value::String(value) => value.as_str(),
                Value::Array(values) => values.first()?.as_str()?,
                _ => return None,
            };
            Some((clean(domain, 512), clean(target, 512)))
        })
        .take(MAX_LIST)
        .collect()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    use super::*;

    fn link(action: &str, profile: &Value) -> String {
        format!("{PREFIX}{action}/{}", STANDARD.encode(profile.to_string()))
    }

    #[test]
    fn off() {
        assert_eq!(parse("happ://routing/off").unwrap(), Routing::Off);
        assert_eq!(parse(" HAPP://routing/OFF/ ").unwrap(), Routing::Off);
    }

    #[test]
    fn profile_from_link() {
        let value = json!({
            "Name": "Тест",
            "GlobalProxy": "true",
            "FakeDNS": "false",
            "RemoteDNSType": "DoH",
            "RemoteDNSDomain": "https://dns.example.com/dns-query",
            "RemoteDNSIP": "203.0.113.53",
            "DomesticDNSType": "DoU",
            "DomesticDNSIP": "203.0.113.54",
            "Geoipurl": "https://example.com/geoip.dat",
            "LastUpdated": "1767225600",
            "DomainStrategy": "IPIfNonMatch",
            "DnsHosts": {"example.com": "203.0.113.7", "multi.example.com": ["203.0.113.8"]},
            "DirectSites": ["geosite:category-ru", "", "domain:example.org"],
            "DirectIp": ["geoip:ru", "geoip:private"],
            "BlockSites": ["geosite:category-ads"],
        });
        let Routing::Profile(profile) = parse(&link("onadd", &value)).unwrap() else {
            panic!("ожидался профиль");
        };
        assert!(profile.activate && profile.global_proxy && !profile.fake_dns);
        assert_eq!(profile.name, "Тест");
        assert_eq!(profile.remote_dns.kind, "DoH");
        assert_eq!(profile.remote_dns.ip, "203.0.113.53");
        assert_eq!(profile.domestic_dns.domain, "");
        assert_eq!(profile.last_updated, Some(1_767_225_600));
        assert_eq!(profile.dns_hosts["example.com"], "203.0.113.7");
        assert_eq!(profile.dns_hosts["multi.example.com"], "203.0.113.8");
        assert_eq!(profile.direct_sites, ["geosite:category-ru", "domain:example.org"]);
        assert_eq!(profile.direct_ip.len(), 2);
        assert!(profile.proxy_sites.is_empty());

        let Routing::Profile(added) = parse(&link("add", &json!({"globalproxy": true}))).unwrap()
        else {
            panic!("ожидался профиль");
        };
        assert!(!added.activate && added.global_proxy);
    }

    #[test]
    fn bad_values() {
        assert!(parse("https://example.com").is_err());
        assert!(parse("happ://routing/add").is_err());
        assert!(parse("happ://routing/remove/e30=").is_err());
        assert!(parse("happ://routing/add/!!!").is_err());
        assert!(parse(&link("add", &json!([1, 2]))).is_err());
        let not_json = format!("{PREFIX}add/{}", STANDARD.encode("не json"));
        assert!(parse(&not_json).is_err());
    }
}
