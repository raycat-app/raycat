//! Ссылки `vless://`, `vmess://`, `trojan://`, `ss://`, `hysteria2://` в outbound'ы xray.
//!
//! Формат `vless`/`vmess` — стандарт Xray (XTLS/Xray-core, обсуждение 716):
//! <https://github.com/XTLS/Xray-core/discussions/716>. Поля outbound'ов
//! соответствуют документации <https://xtls.github.io/>.

use std::collections::HashMap;

use raycat_xray::Node;
use serde_json::{Value, json};

use crate::text::{clean, decode_base64, percent_decode};

const MAX_NAME: usize = 200;
const TAG: &str = "proxy";

type Params = HashMap<String, String>;

pub(crate) struct Link {
    pub(crate) node: Node,
    /// Что не удалось перенести; без содержимого ссылки (там секреты).
    pub(crate) warnings: Vec<String>,
}

struct Built {
    name: String,
    outbound: Value,
}

/// Разобранная ссылка: `userinfo@host:port?query#name`.
struct Url<'a> {
    userinfo: &'a str,
    host: String,
    port_text: &'a str,
    query: Params,
    fragment: String,
}

pub(crate) fn parse(line: &str) -> Result<Link, String> {
    let (scheme, rest) = line
        .split_once("://")
        .ok_or_else(|| "в ссылке нет схемы".to_owned())?;
    let mut warnings = Vec::new();
    let built = match scheme.to_ascii_lowercase().as_str() {
        "vless" => vless(rest, &mut warnings)?,
        "vmess" => vmess(rest, &mut warnings)?,
        "trojan" => trojan(rest, &mut warnings)?,
        "ss" => shadowsocks(rest)?,
        "hysteria2" | "hy2" => hysteria2(rest, &mut warnings)?,
        other => {
            return Err(format!("схема {}:// не поддерживается", clean(other, 32)));
        }
    };
    Ok(Link {
        node: Node {
            name: built.name,
            outbounds: vec![built.outbound],
        },
        warnings,
    })
}

fn split_url(rest: &str) -> Result<Url<'_>, String> {
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    // В userinfo бывают `/` и другие символы (пароли), поэтому `@` ищем с конца.
    let (userinfo, location) = rest.rsplit_once('@').unwrap_or(("", rest));
    let authority = location.split('/').next().unwrap_or("");
    let (host, port_text) = split_authority(authority)?;
    Ok(Url {
        userinfo,
        host,
        port_text,
        query: parse_query(query),
        fragment: percent_decode(fragment),
    })
}

/// `host:port`, `[v6]:port` или просто адрес; адрес v6 возвращается без скобок.
fn split_authority(authority: &str) -> Result<(String, &str), String> {
    let (host, port_text) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, tail) = inner
            .split_once(']')
            .ok_or_else(|| "в адресе нет закрывающей скобки".to_owned())?;
        (host, tail.strip_prefix(':').unwrap_or(""))
    } else {
        authority.rsplit_once(':').unwrap_or((authority, ""))
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("пустой или некорректный адрес сервера".to_owned());
    }
    Ok((host.to_owned(), port_text))
}

fn parse_query(query: &str) -> Params {
    let mut params = Params::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            params
                .entry(percent_decode(key))
                .or_insert_with(|| percent_decode(value));
        }
    }
    params
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("некорректный порт «{}»", clean(text, 16)))
}

fn param<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

fn is_true(value: Option<&str>) -> bool {
    value.is_some_and(|text| text.eq_ignore_ascii_case("true") || text == "1")
}

fn node_name(fragment: &str, host: &str, port: u16) -> String {
    let name = clean(fragment, MAX_NAME);
    if name.is_empty() {
        format!("{}:{port}", clean(host, MAX_NAME))
    } else {
        name
    }
}

fn outbound(protocol: &str, settings: Value, stream: Option<Value>) -> Value {
    let mut outbound = json!({"tag": TAG, "protocol": protocol});
    outbound["settings"] = settings;
    if let Some(stream) = stream {
        outbound["streamSettings"] = stream;
    }
    outbound
}

fn vless(rest: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let url = split_url(rest)?;
    let id = percent_decode(url.userinfo);
    if id.is_empty() {
        return Err("в ссылке нет идентификатора пользователя".to_owned());
    }
    let port = parse_port(url.port_text)?;
    let stream = stream_settings(&url.query, &url.host, "none", warnings)?;
    let mut settings = json!({
        "address": url.host,
        "port": port,
        "id": id,
        "encryption": param(&url.query, "encryption").unwrap_or("none"),
    });
    if let Some(flow) = param(&url.query, "flow") {
        settings["flow"] = json!(flow);
    }
    Ok(Built {
        name: node_name(&url.fragment, &url.host, port),
        outbound: outbound("vless", settings, Some(stream)),
    })
}

fn trojan(rest: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let url = split_url(rest)?;
    let password = percent_decode(url.userinfo);
    if password.is_empty() {
        return Err("в ссылке нет пароля".to_owned());
    }
    let port = parse_port(url.port_text)?;
    let stream = stream_settings(&url.query, &url.host, "tls", warnings)?;
    let settings = json!({"address": url.host, "port": port, "password": password});
    Ok(Built {
        name: node_name(&url.fragment, &url.host, port),
        outbound: outbound("trojan", settings, Some(stream)),
    })
}

fn vmess(rest: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let head = rest.split(['?', '#']).next().unwrap_or("");
    if head.contains('@') {
        vmess_standard(rest, warnings)
    } else {
        vmess_legacy(head, warnings)
    }
}

/// `vmess://uuid@host:port?…` — формат стандарта Xray.
fn vmess_standard(rest: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let url = split_url(rest)?;
    let id = percent_decode(url.userinfo);
    if id.is_empty() {
        return Err("в ссылке нет идентификатора пользователя".to_owned());
    }
    let port = parse_port(url.port_text)?;
    let stream = stream_settings(&url.query, &url.host, "none", warnings)?;
    let settings = json!({
        "address": url.host,
        "port": port,
        "id": id,
        "security": param(&url.query, "encryption").unwrap_or("auto"),
    });
    Ok(Built {
        name: node_name(&url.fragment, &url.host, port),
        outbound: outbound("vmess", settings, Some(stream)),
    })
}

/// `vmess://base64(JSON)` — формат v2rayN.
fn vmess_legacy(head: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let bytes = decode_base64(head).ok_or_else(|| "данные vmess не в формате base64".to_owned())?;
    let data: Value =
        serde_json::from_slice(&bytes).map_err(|_| "данные vmess не являются JSON".to_owned())?;
    let field = |key: &str| -> Option<String> {
        let text = match data.get(key)? {
            Value::String(text) => text.trim().to_owned(),
            Value::Number(number) => number.to_string(),
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    };
    let host = field("add").ok_or_else(|| "в ссылке нет адреса сервера".to_owned())?;
    if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("пустой или некорректный адрес сервера".to_owned());
    }
    let port = parse_port(&field("port").unwrap_or_default())?;
    let id = field("id").ok_or_else(|| "в ссылке нет идентификатора пользователя".to_owned())?;

    let network = field("net").unwrap_or_else(|| "tcp".to_owned());
    let header_type = field("type");
    let path = field("path");
    let mut params = Params::new();
    params.insert("type".to_owned(), network.clone());
    params.insert(
        "security".to_owned(),
        if field("tls").is_some_and(|tls| tls.eq_ignore_ascii_case("tls")) {
            "tls".to_owned()
        } else {
            "none".to_owned()
        },
    );
    // В формате v2rayN `type` для grpc — режим, а `path` — имя сервиса.
    if network == "grpc" {
        params.extend(path.map(|path| ("serviceName".to_owned(), path)));
        params.extend(header_type.map(|mode| ("mode".to_owned(), mode)));
    } else {
        params.extend(path.map(|path| ("path".to_owned(), path)));
        params.extend(header_type.map(|kind| ("headerType".to_owned(), kind)));
    }
    for (source, target) in [
        ("host", "host"),
        ("sni", "sni"),
        ("alpn", "alpn"),
        ("fp", "fp"),
    ] {
        params.extend(field(source).map(|value| (target.to_owned(), value)));
    }

    let stream = stream_settings(&params, &host, "none", warnings)?;
    let settings = json!({
        "address": host,
        "port": port,
        "id": id,
        "security": field("scy").unwrap_or_else(|| "auto".to_owned()),
    });
    let ps = field("ps").unwrap_or_default();
    Ok(Built {
        name: node_name(&ps, &host, port),
        outbound: outbound("vmess", settings, Some(stream)),
    })
}

/// Два вида ссылок: SIP002 (`ss://userinfo@host:port#name`, userinfo — base64 или
/// открытый текст у шифров 2022) и старый `ss://base64(method:password@host:port)`.
fn shadowsocks(rest: &str) -> Result<Built, String> {
    let (body, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (location, query) = body.split_once('?').unwrap_or((body, ""));
    let params = parse_query(query);
    if param(&params, "plugin").is_some() {
        return Err("плагины shadowsocks xray не поддерживает".to_owned());
    }
    let location = location.trim_end_matches('/');
    let (method, password, authority) =
        if let Some((userinfo, authority)) = location.rsplit_once('@') {
            let (method, password) = ss_credentials(userinfo)?;
            let authority = authority.split('/').next().unwrap_or("");
            (method, password, authority.to_owned())
        } else {
            ss_legacy(location)?
        };
    ss_built(&method, &password, &authority, fragment)
}

/// `base64(method:password@host:port)`: и учётные данные, и адрес внутри base64.
fn ss_legacy(location: &str) -> Result<(String, String, String), String> {
    let bytes =
        decode_base64(location).ok_or_else(|| "данные ss не в формате base64".to_owned())?;
    let decoded = String::from_utf8(bytes).map_err(|_| "данные ss не текст".to_owned())?;
    let (credentials, authority) = decoded
        .rsplit_once('@')
        .ok_or_else(|| "в ссылке ss нет адреса сервера".to_owned())?;
    let (method, password) = credentials
        .split_once(':')
        .ok_or_else(|| "в ссылке ss нет пароля".to_owned())?;
    Ok((method.to_owned(), password.to_owned(), authority.to_owned()))
}

fn ss_credentials(userinfo: &str) -> Result<(String, String), String> {
    let decoded = percent_decode(userinfo);
    let plain = if decoded.contains(':') {
        decoded
    } else {
        let bytes =
            decode_base64(&decoded).ok_or_else(|| "учётные данные ss не в base64".to_owned())?;
        String::from_utf8(bytes).map_err(|_| "учётные данные ss не текст".to_owned())?
    };
    plain
        .split_once(':')
        .map(|(method, password)| (method.to_owned(), password.to_owned()))
        .ok_or_else(|| "в ссылке ss нет пароля".to_owned())
}

fn ss_built(
    method: &str,
    password: &str,
    authority: &str,
    fragment: &str,
) -> Result<Built, String> {
    if method.is_empty() {
        return Err("в ссылке ss нет метода шифрования".to_owned());
    }
    let (host, port_text) = split_authority(authority)?;
    let port = parse_port(port_text)?;
    let settings = json!({
        "address": host,
        "port": port,
        "method": method,
        "password": password,
    });
    Ok(Built {
        name: node_name(&percent_decode(fragment), &host, port),
        outbound: outbound("shadowsocks", settings, None),
    })
}

fn hysteria2(rest: &str, warnings: &mut Vec<String>) -> Result<Built, String> {
    let url = split_url(rest)?;
    // Если пароль записан как `имя:пароль`, сервер ждёт его целиком.
    let auth = percent_decode(url.userinfo);
    if auth.is_empty() {
        return Err("в ссылке нет пароля".to_owned());
    }
    let mport = param(&url.query, "mport");
    let main_spec = if url.port_text.is_empty() {
        mport.unwrap_or("")
    } else {
        url.port_text
    };
    let port = if main_spec.is_empty() {
        443
    } else {
        parse_port(main_spec.split([',', '-']).next().unwrap_or(""))?
    };
    let hop = mport.or_else(|| main_spec.contains([',', '-']).then_some(main_spec));
    if hop.is_some_and(|ports| {
        !ports
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '-')
    }) {
        return Err("некорректный список портов".to_owned());
    }

    let mut tls = json!({"alpn": alpn_list(param(&url.query, "alpn").unwrap_or("h3"))});
    if let Some(sni) = param(&url.query, "sni") {
        tls["serverName"] = json!(sni);
    }
    if let Some(pin) = param(&url.query, "pinSHA256") {
        tls["pinnedPeerCertSha256"] = json!(pin.replace(':', "").to_ascii_lowercase());
    }
    if is_true(param(&url.query, "insecure")) {
        tls["allowInsecure"] = json!(true);
        warnings.push("проверка сертификата отключена ссылкой (insecure)".to_owned());
    }

    let mut stream = json!({
        "network": "hysteria",
        "security": "tls",
        "tlsSettings": tls,
        "hysteriaSettings": {"version": 2, "auth": auth},
    });
    let mut finalmask = json!({});
    match param(&url.query, "obfs") {
        None => {}
        Some("salamander") => {
            let password = param(&url.query, "obfs-password")
                .ok_or_else(|| "у obfs salamander нет obfs-password".to_owned())?;
            finalmask["udp"] = json!([{"type": "salamander", "settings": {"password": password}}]);
        }
        Some(other) => {
            return Err(format!("obfs «{}» не поддерживается", clean(other, 32)));
        }
    }
    if let Some(ports) = hop {
        finalmask["quicParams"] = json!({"udpHop": {"ports": ports}});
    }
    if finalmask.as_object().is_some_and(|map| !map.is_empty()) {
        stream["finalmask"] = finalmask;
    }
    let settings = json!({"version": 2, "address": url.host, "port": port});
    Ok(Built {
        name: node_name(&url.fragment, &url.host, port),
        outbound: outbound("hysteria", settings, Some(stream)),
    })
}

fn alpn_list(text: &str) -> Vec<&str> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect()
}

/// `streamSettings`: транспорт, затем защита (TLS/REALITY), затем `finalmask`.
fn stream_settings(
    query: &Params,
    host: &str,
    default_security: &str,
    warnings: &mut Vec<String>,
) -> Result<Value, String> {
    let network = match param(query, "type").unwrap_or("tcp") {
        "tcp" | "raw" => "raw",
        "ws" => "ws",
        "grpc" => "grpc",
        "xhttp" | "splithttp" => "xhttp",
        "httpupgrade" => "httpupgrade",
        other => {
            return Err(format!(
                "транспорт «{}» не поддерживается",
                clean(other, 32)
            ));
        }
    };
    let security = param(query, "security").unwrap_or(default_security);
    let mut stream = json!({"network": network, "security": security});
    if let Some((key, settings)) = transport_settings(network, query, warnings) {
        stream[key] = settings;
    }
    match security {
        "none" => {}
        "tls" => stream["tlsSettings"] = tls_settings(query, warnings),
        "reality" => stream["realitySettings"] = reality_settings(query, host)?,
        other => {
            return Err(format!("защита «{}» не поддерживается", clean(other, 32)));
        }
    }
    if let Some(mask) = param(query, "fm") {
        match serde_json::from_str::<Value>(mask) {
            Ok(mask) if mask.is_object() => stream["finalmask"] = mask,
            _ => warnings.push("параметр fm не JSON-объект, пропущен".to_owned()),
        }
    }
    Ok(stream)
}

fn transport_settings(
    network: &str,
    query: &Params,
    warnings: &mut Vec<String>,
) -> Option<(&'static str, Value)> {
    let path = param(query, "path").unwrap_or("/");
    let host = param(query, "host");
    match network {
        "ws" | "httpupgrade" => {
            let mut settings = json!({"path": path});
            if let Some(host) = host {
                settings["host"] = json!(host);
            }
            let key = if network == "ws" {
                "wsSettings"
            } else {
                "httpupgradeSettings"
            };
            Some((key, settings))
        }
        "grpc" => {
            let mut settings = json!({
                "serviceName": param(query, "serviceName").unwrap_or(""),
                "multiMode": param(query, "mode") == Some("multi"),
            });
            if let Some(authority) = param(query, "authority") {
                settings["authority"] = json!(authority);
            }
            Some(("grpcSettings", settings))
        }
        "xhttp" => {
            let mut settings = json!({"path": path});
            if let Some(host) = host {
                settings["host"] = json!(host);
            }
            if let Some(mode) = param(query, "mode") {
                settings["mode"] = json!(mode);
            }
            if let Some(extra) = param(query, "extra") {
                match serde_json::from_str::<Value>(extra) {
                    Ok(extra) if extra.is_object() => settings["extra"] = extra,
                    _ => warnings.push("параметр extra не JSON-объект, пропущен".to_owned()),
                }
            }
            Some(("xhttpSettings", settings))
        }
        _ => raw_settings(query, path, host, warnings),
    }
}

/// У raw-транспорта настройки нужны только для http-маскировки заголовков.
fn raw_settings(
    query: &Params,
    path: &str,
    host: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<(&'static str, Value)> {
    match param(query, "headerType") {
        None | Some("none") => None,
        Some("http") => {
            let hosts: Vec<&str> = host
                .map(|list| list.split(',').map(str::trim).collect())
                .unwrap_or_default();
            let header = json!({
                "type": "http",
                "request": {"path": [path], "headers": {"Host": hosts}},
            });
            Some(("rawSettings", json!({"header": header})))
        }
        Some(other) => {
            warnings.push(format!(
                "headerType «{}» не поддерживается",
                clean(other, 32)
            ));
            None
        }
    }
}

fn fingerprint(query: &Params) -> &str {
    param(query, "fp").unwrap_or("chrome")
}

fn tls_settings(query: &Params, warnings: &mut Vec<String>) -> Value {
    let mut tls = json!({"fingerprint": fingerprint(query)});
    if let Some(sni) = param(query, "sni").or_else(|| param(query, "peer")) {
        tls["serverName"] = json!(sni);
    }
    if let Some(alpn) = param(query, "alpn") {
        tls["alpn"] = json!(alpn_list(alpn));
    }
    for (source, target) in [
        ("pcs", "pinnedPeerCertSha256"),
        ("vcn", "verifyPeerCertByName"),
        ("ech", "echConfigList"),
    ] {
        if let Some(value) = param(query, source) {
            tls[target] = json!(value);
        }
    }
    if is_true(param(query, "allowInsecure").or_else(|| param(query, "insecure"))) {
        tls["allowInsecure"] = json!(true);
        warnings.push("проверка сертификата отключена ссылкой (allowInsecure)".to_owned());
    }
    tls
}

fn reality_settings(query: &Params, host: &str) -> Result<Value, String> {
    let password = param(query, "pbk").ok_or_else(|| "у reality нет параметра pbk".to_owned())?;
    let mut reality = json!({
        "serverName": param(query, "sni").unwrap_or(host),
        "fingerprint": fingerprint(query),
        "password": password,
        "shortId": param(query, "sid").unwrap_or(""),
    });
    for (source, target) in [("spx", "spiderX"), ("pqv", "mldsa65Verify")] {
        if let Some(value) = param(query, source) {
            reality[target] = json!(value);
        }
    }
    Ok(reality)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

    use super::*;

    const UUID: &str = "00000000-0000-0000-0000-000000000000";

    fn parsed(line: &str) -> Link {
        parse(line).unwrap_or_else(|error| panic!("{line}: {error}"))
    }

    fn outbound_of(line: &str) -> Value {
        parsed(line).node.outbounds.remove(0)
    }

    #[test]
    fn vless_reality_tcp() {
        let link = format!(
            "vless://{UUID}@nl.example.com:443?type=tcp&security=reality&encryption=none\
             &flow=xtls-rprx-vision&sni=www.example.org&fp=firefox&pbk=PUBLICKEY&sid=ab12\
             &spx=%2F&pqv=VERIFYKEY#%F0%9F%87%B3%F0%9F%87%B1%20NL"
        );
        let parsed = parsed(&link);
        assert_eq!(parsed.node.name, "🇳🇱 NL");
        assert!(parsed.warnings.is_empty());
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["tag"], "proxy");
        assert_eq!(ob["protocol"], "vless");
        assert_eq!(ob["settings"]["address"], "nl.example.com");
        assert_eq!(ob["settings"]["port"], 443);
        assert_eq!(ob["settings"]["id"], UUID);
        assert_eq!(ob["settings"]["flow"], "xtls-rprx-vision");
        assert_eq!(ob["settings"]["encryption"], "none");
        let stream = &ob["streamSettings"];
        assert_eq!(stream["network"], "raw");
        assert_eq!(stream["security"], "reality");
        let reality = &stream["realitySettings"];
        assert_eq!(reality["serverName"], "www.example.org");
        assert_eq!(reality["fingerprint"], "firefox");
        assert_eq!(reality["password"], "PUBLICKEY");
        assert_eq!(reality["shortId"], "ab12");
        assert_eq!(reality["spiderX"], "/");
        assert_eq!(reality["mldsa65Verify"], "VERIFYKEY");
        assert!(stream.get("rawSettings").is_none());
    }

    #[test]
    fn vless_reality_needs_a_key() {
        let error = parse(&format!(
            "vless://{UUID}@a.example.com:443?security=reality"
        ))
        .err();
        assert!(error.unwrap().contains("pbk"));
    }

    #[test]
    fn vless_ws_tls() {
        let ob = outbound_of(&format!(
            "vless://{UUID}@ws.example.com:8443?type=ws&security=tls&sni=cdn.example.com\
             &host=cdn.example.com&path=%2Fsocket%3Fed%3D2048&alpn=h2,http/1.1\
             &pcs=AA:BB&vcn=other.example.com"
        ));
        let stream = &ob["streamSettings"];
        assert_eq!(stream["network"], "ws");
        assert_eq!(stream["wsSettings"]["path"], "/socket?ed=2048");
        assert_eq!(stream["wsSettings"]["host"], "cdn.example.com");
        let tls = &stream["tlsSettings"];
        assert_eq!(tls["serverName"], "cdn.example.com");
        assert_eq!(tls["fingerprint"], "chrome");
        assert_eq!(tls["alpn"], json!(["h2", "http/1.1"]));
        assert_eq!(tls["pinnedPeerCertSha256"], "AA:BB");
        assert_eq!(tls["verifyPeerCertByName"], "other.example.com");
        assert!(tls.get("allowInsecure").is_none());
    }

    #[test]
    fn vless_grpc() {
        let ob = outbound_of(&format!(
            "vless://{UUID}@g.example.com:443?type=grpc&security=tls&serviceName=svc%2Fone\
             &mode=multi&authority=auth.example.com"
        ));
        let grpc = &ob["streamSettings"]["grpcSettings"];
        assert_eq!(ob["streamSettings"]["network"], "grpc");
        assert_eq!(grpc["serviceName"], "svc/one");
        assert_eq!(grpc["multiMode"], true);
        assert_eq!(grpc["authority"], "auth.example.com");
    }

    #[test]
    fn vless_xhttp_with_extra() {
        let extra = "%7B%22xPaddingBytes%22%3A%22100-1000%22%7D";
        let ob = outbound_of(&format!(
            "vless://{UUID}@x.example.com:443?type=xhttp&security=tls&path=%2Fx&host=h.example.com\
             &mode=packet-up&extra={extra}"
        ));
        let xhttp = &ob["streamSettings"]["xhttpSettings"];
        assert_eq!(xhttp["path"], "/x");
        assert_eq!(xhttp["host"], "h.example.com");
        assert_eq!(xhttp["mode"], "packet-up");
        assert_eq!(xhttp["extra"]["xPaddingBytes"], "100-1000");
    }

    #[test]
    fn broken_xhttp_extra_is_a_warning() {
        let parsed = parsed(&format!(
            "vless://{UUID}@x.example.com:443?type=xhttp&extra=nope"
        ));
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.node.outbounds[0]["streamSettings"]["xhttpSettings"]
                .get("extra")
                .is_none()
        );
    }

    #[test]
    fn vless_httpupgrade_and_raw_http_header() {
        let ob = outbound_of(&format!(
            "vless://{UUID}@h.example.com:80?type=httpupgrade&path=%2Fup&host=up.example.com"
        ));
        assert_eq!(ob["streamSettings"]["network"], "httpupgrade");
        assert_eq!(ob["streamSettings"]["httpupgradeSettings"]["path"], "/up");
        assert_eq!(ob["streamSettings"]["security"], "none");

        let ob = outbound_of(&format!(
            "vless://{UUID}@r.example.com:80?type=tcp&headerType=http&host=a.example.com,b.example.com&path=%2Fp"
        ));
        let header = &ob["streamSettings"]["rawSettings"]["header"];
        assert_eq!(header["type"], "http");
        assert_eq!(header["request"]["path"], json!(["/p"]));
        assert_eq!(
            header["request"]["headers"]["Host"],
            json!(["a.example.com", "b.example.com"])
        );
    }

    #[test]
    fn finalmask_parameter() {
        let mask = "%7B%22tcp%22%3A%5B%7B%22type%22%3A%22header-custom%22%7D%5D%7D";
        let ob = outbound_of(&format!("vless://{UUID}@m.example.com:443?fm={mask}"));
        assert_eq!(
            ob["streamSettings"]["finalmask"]["tcp"][0]["type"],
            "header-custom"
        );
    }

    #[test]
    fn vless_ipv6_and_default_name() {
        let parsed = parsed(&format!("vless://{UUID}@[2001:db8::1]:8443?security=none"));
        assert_eq!(parsed.node.name, "2001:db8::1:8443");
        assert_eq!(
            parsed.node.outbounds[0]["settings"]["address"],
            "2001:db8::1"
        );
    }

    #[test]
    fn insecure_tls_is_reported() {
        let parsed = parsed(&format!(
            "vless://{UUID}@i.example.com:443?security=tls&allowInsecure=1"
        ));
        assert_eq!(
            parsed.node.outbounds[0]["streamSettings"]["tlsSettings"]["allowInsecure"],
            true
        );
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn unsupported_pieces_are_errors_without_secrets() {
        for suffix in ["type=kcp", "type=http", "security=xtls", "type=quic"] {
            let line = format!("vless://{UUID}@k.example.com:443?{suffix}");
            let error = parse(&line).err().unwrap();
            assert!(error.contains("не поддерживается"), "{error}");
            assert!(!error.contains(UUID));
        }
        for line in [
            "vless://@a.example.com:443",
            "vless://x@:443",
            "vless://x@a.example.com",
            "vless://x@a.example.com:99999",
            "vless://x@[::1:443",
            "vless://x@a b.example.com:443",
            "wireguard://x@a.example.com:443",
            "vless",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn vmess_v2rayn_json() {
        let json = json!({
            "v": "2", "ps": "VM ws", "add": "vm.example.com", "port": "443", "id": UUID,
            "aid": "0", "scy": "chacha20-poly1305", "net": "ws", "type": "none",
            "host": "cdn.example.com", "path": "/vm", "tls": "tls", "sni": "cdn.example.com",
            "alpn": "h2", "fp": "safari",
        });
        for encoded in [
            STANDARD.encode(json.to_string()),
            URL_SAFE_NO_PAD.encode(json.to_string()),
        ] {
            let parsed = parsed(&format!("vmess://{encoded}"));
            assert_eq!(parsed.node.name, "VM ws");
            let ob = &parsed.node.outbounds[0];
            assert_eq!(ob["protocol"], "vmess");
            assert_eq!(ob["settings"]["address"], "vm.example.com");
            assert_eq!(ob["settings"]["port"], 443);
            assert_eq!(ob["settings"]["id"], UUID);
            assert_eq!(ob["settings"]["security"], "chacha20-poly1305");
            let stream = &ob["streamSettings"];
            assert_eq!(stream["network"], "ws");
            assert_eq!(stream["wsSettings"]["path"], "/vm");
            assert_eq!(stream["wsSettings"]["host"], "cdn.example.com");
            assert_eq!(stream["tlsSettings"]["fingerprint"], "safari");
            assert_eq!(stream["tlsSettings"]["alpn"], json!(["h2"]));
        }
    }

    #[test]
    fn vmess_v2rayn_grpc_and_numeric_port() {
        let json = json!({
            "ps": "", "add": "g.example.com", "port": 8443, "id": UUID,
            "net": "grpc", "type": "multi", "path": "service", "tls": "",
        });
        let parsed = parsed(&format!("vmess://{}", STANDARD.encode(json.to_string())));
        assert_eq!(parsed.node.name, "g.example.com:8443");
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["settings"]["security"], "auto");
        assert_eq!(ob["streamSettings"]["security"], "none");
        assert_eq!(
            ob["streamSettings"]["grpcSettings"]["serviceName"],
            "service"
        );
        assert_eq!(ob["streamSettings"]["grpcSettings"]["multiMode"], true);
    }

    #[test]
    fn vmess_standard_form() {
        let ob = outbound_of(&format!(
            "vmess://{UUID}@v.example.com:443?encryption=aes-128-gcm&security=tls&type=ws&path=%2Fv#Std"
        ));
        assert_eq!(ob["settings"]["security"], "aes-128-gcm");
        assert_eq!(ob["streamSettings"]["security"], "tls");
        assert_eq!(ob["streamSettings"]["wsSettings"]["path"], "/v");
        assert_eq!(
            parsed(&format!("vmess://{UUID}@v.example.com:443#Std"))
                .node
                .name,
            "Std"
        );
    }

    #[test]
    fn vmess_garbage() {
        assert!(parse("vmess://!!!").is_err());
        assert!(parse(&format!("vmess://{}", STANDARD.encode("не json"))).is_err());
        let no_host = json!({"port": 1, "id": UUID});
        assert!(parse(&format!("vmess://{}", STANDARD.encode(no_host.to_string()))).is_err());
    }

    #[test]
    fn trojan_defaults_to_tls() {
        let parsed = parsed(
            "trojan://p%40ss%2Fword@t.example.com:8443?sni=front.example.com&type=grpc&serviceName=g#TR",
        );
        assert_eq!(parsed.node.name, "TR");
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["protocol"], "trojan");
        assert_eq!(ob["settings"]["password"], "p@ss/word");
        assert_eq!(ob["settings"]["port"], 8443);
        assert_eq!(ob["streamSettings"]["security"], "tls");
        assert_eq!(
            ob["streamSettings"]["tlsSettings"]["serverName"],
            "front.example.com"
        );
        assert_eq!(ob["streamSettings"]["grpcSettings"]["serviceName"], "g");
    }

    #[test]
    fn trojan_password_with_raw_slash_and_security_none() {
        let ob = outbound_of("trojan://pa/ss@t.example.com:443?security=none");
        assert_eq!(ob["settings"]["password"], "pa/ss");
        assert_eq!(ob["streamSettings"]["security"], "none");
        assert!(parse("trojan://@t.example.com:443").is_err());
    }

    #[test]
    fn shadowsocks_sip002_base64_userinfo() {
        for userinfo in [
            STANDARD.encode("aes-128-gcm:s3cret"),
            URL_SAFE_NO_PAD.encode("aes-128-gcm:s3cret"),
        ] {
            let parsed = parsed(&format!("ss://{userinfo}@ss.example.com:8388/?foo=bar#SS"));
            assert_eq!(parsed.node.name, "SS");
            let ob = &parsed.node.outbounds[0];
            assert_eq!(ob["protocol"], "shadowsocks");
            assert_eq!(ob["settings"]["method"], "aes-128-gcm");
            assert_eq!(ob["settings"]["password"], "s3cret");
            assert_eq!(ob["settings"]["address"], "ss.example.com");
            assert_eq!(ob["settings"]["port"], 8388);
            assert!(ob.get("streamSettings").is_none());
        }
    }

    #[test]
    fn shadowsocks_2022_plain_userinfo() {
        let ob = outbound_of("ss://2022-blake3-aes-128-gcm:YWJj%2BZGVm%3D@[2001:db8::2]:443#Plain");
        assert_eq!(ob["settings"]["method"], "2022-blake3-aes-128-gcm");
        assert_eq!(ob["settings"]["password"], "YWJj+ZGVm=");
        assert_eq!(ob["settings"]["address"], "2001:db8::2");
    }

    #[test]
    fn shadowsocks_legacy_whole_base64() {
        let encoded = STANDARD.encode("aes-256-gcm:pw@203.0.113.9:8388");
        let parsed = parsed(&format!("ss://{encoded}#Legacy"));
        assert_eq!(parsed.node.name, "Legacy");
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["settings"]["method"], "aes-256-gcm");
        assert_eq!(ob["settings"]["address"], "203.0.113.9");
    }

    #[test]
    fn shadowsocks_errors() {
        let userinfo = STANDARD.encode("aes-128-gcm:pw");
        assert!(
            parse(&format!(
                "ss://{userinfo}@a.example.com:1?plugin=obfs-local"
            ))
            .is_err()
        );
        assert!(parse("ss://!!!").is_err());
        assert!(
            parse(&format!(
                "ss://{}@a.example.com:1",
                STANDARD.encode("nopassword")
            ))
            .is_err()
        );
        assert!(parse(&format!("ss://{userinfo}@a.example.com")).is_err());
    }

    #[test]
    fn hysteria2_full() {
        let parsed = parsed(
            "hysteria2://auth%3Apass@hy.example.com:443/?sni=front.example.com&obfs=salamander\
             &obfs-password=mask&pinSHA256=AB:CD:EF&mport=20000-30000#HY",
        );
        assert_eq!(parsed.node.name, "HY");
        assert!(parsed.warnings.is_empty());
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["protocol"], "hysteria");
        assert_eq!(
            ob["settings"],
            json!({"version": 2, "address": "hy.example.com", "port": 443})
        );
        let stream = &ob["streamSettings"];
        assert_eq!(stream["network"], "hysteria");
        assert_eq!(stream["security"], "tls");
        assert_eq!(
            stream["hysteriaSettings"],
            json!({"version": 2, "auth": "auth:pass"})
        );
        assert_eq!(stream["tlsSettings"]["serverName"], "front.example.com");
        assert_eq!(stream["tlsSettings"]["alpn"], json!(["h3"]));
        assert_eq!(stream["tlsSettings"]["pinnedPeerCertSha256"], "abcdef");
        assert_eq!(stream["finalmask"]["udp"][0]["type"], "salamander");
        assert_eq!(
            stream["finalmask"]["udp"][0]["settings"]["password"],
            "mask"
        );
        assert_eq!(
            stream["finalmask"]["quicParams"]["udpHop"]["ports"],
            "20000-30000"
        );
    }

    #[test]
    fn hysteria2_defaults_and_hy2_alias() {
        let parsed = parsed("hy2://secret@hy.example.com?insecure=1");
        let ob = &parsed.node.outbounds[0];
        assert_eq!(ob["settings"]["port"], 443);
        assert_eq!(parsed.node.name, "hy.example.com:443");
        assert_eq!(ob["streamSettings"]["tlsSettings"]["allowInsecure"], true);
        assert!(ob["streamSettings"].get("finalmask").is_none());
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn hysteria2_port_lists() {
        let ob = outbound_of("hy2://s@hy.example.com:443,20000-30000");
        assert_eq!(ob["settings"]["port"], 443);
        assert_eq!(
            ob["streamSettings"]["finalmask"]["quicParams"]["udpHop"]["ports"],
            "443,20000-30000"
        );
        let ob = outbound_of("hy2://s@hy.example.com:20000-30000");
        assert_eq!(ob["settings"]["port"], 20000);
    }

    #[test]
    fn hysteria2_errors() {
        assert!(parse("hy2://@hy.example.com:443").is_err());
        assert!(parse("hy2://s@hy.example.com:443?obfs=other").is_err());
        assert!(parse("hy2://s@hy.example.com:443?obfs=salamander").is_err());
        assert!(parse("hy2://s@hy.example.com:443?mport=1;2").is_err());
    }

    #[test]
    fn duplicate_parameters_keep_the_first() {
        let ob = outbound_of(&format!(
            "vless://{UUID}@a.example.com:443?security=tls&sni=one&sni=two"
        ));
        assert_eq!(ob["streamSettings"]["tlsSettings"]["serverName"], "one");
    }
}
