//! Определение формата тела ответа и разбор его в узлы.

use raycat_xray::Node;
use serde_json::Value;

use crate::text::{clean, decode_base64, push_warning, strip_prefix_ci};
use crate::{link, xray_json};

pub(crate) const MAX_BODY: usize = 32 * 1024 * 1024;
pub(crate) const MAX_NODES: usize = 5_000;
const MAX_LINE: usize = 64 * 1024;
const MAX_BODY_HEADERS: usize = 128;
const ROUTING_PREFIX: &str = "happ://routing/";

#[derive(Default)]
pub(crate) struct Content {
    pub(crate) nodes: Vec<Node>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) enum Reject {
    Unrecognized(String),
    Encrypted(String),
}

pub(crate) struct Body {
    /// Строки `#имя: значение` из начала тела; имена в нижнем регистре.
    pub(crate) headers: Vec<(String, String)>,
    /// Первая строка `happ://routing/…` в теле.
    pub(crate) routing: Option<String>,
    pub(crate) content: Result<Content, Reject>,
}

impl Body {
    pub(crate) fn rejected(reject: Reject) -> Self {
        Self {
            headers: Vec::new(),
            routing: None,
            content: Err(reject),
        }
    }
}

pub(crate) fn parse(body: &[u8]) -> Body {
    if body.len() > MAX_BODY {
        return Body::rejected(Reject::Unrecognized("тело ответа больше 32 МиБ".to_owned()));
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Body::rejected(Reject::Unrecognized("пустое тело ответа".to_owned()));
    }
    if is_html(text) {
        return Body::rejected(Reject::Unrecognized(
            "получена HTML-страница: панель не узнала клиента".to_owned(),
        ));
    }
    let (mut headers, mut routing) = leading_meta(text);
    let mut text = text.to_owned();
    if !looks_like_json(&text)
        && !has_link_lines(&text)
        && let Some(decoded) = decode_whole(&text)
    {
        let (more_headers, more_routing) = leading_meta(&decoded);
        headers.extend(more_headers);
        routing = routing.or(more_routing);
        text = decoded;
    }
    let content = if looks_like_json(&text) {
        parse_json(&text)
    } else {
        parse_links_or_reject(&text)
    };
    Body {
        headers,
        routing,
        content,
    }
}

fn is_html(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    head.starts_with("<!doctype") || head.starts_with("<html")
}

fn looks_like_json(text: &str) -> bool {
    text.starts_with('{') || text.starts_with('[')
}

/// Если тело целиком в base64 — расшифровывает его, только когда внутри найден
/// список ссылок или JSON: случайный текст тоже может оказаться «валидным» base64.
fn decode_whole(text: &str) -> Option<String> {
    let payload: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    let decoded = String::from_utf8_lossy(&decode_base64(&payload)?).into_owned();
    let decoded = decoded.trim_start_matches('\u{feff}').trim().to_owned();
    (looks_like_json(&decoded) || has_link_lines(&decoded)).then_some(decoded)
}

/// Заголовки в начале тела (`#имя: значение`) и строка маршрутизации Happ.
fn leading_meta(text: &str) -> (Vec<(String, String)>, Option<String>) {
    let mut headers = Vec::new();
    let mut routing = None;
    let mut leading = true;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if leading && let Some(comment) = line.strip_prefix('#') {
            if let Some((name, value)) = comment.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                if is_header_name(&name) && headers.len() < MAX_BODY_HEADERS {
                    headers.push((name, value.trim().to_owned()));
                }
            }
            continue;
        }
        leading = false;
        if routing.is_none() && strip_prefix_ci(line, ROUTING_PREFIX).is_some() {
            routing = Some(line.to_owned());
        }
    }
    (headers, routing)
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Схема ссылки в начале строки: `vless://…`.
fn scheme_of(line: &str) -> Option<&str> {
    let (scheme, _) = line.split_once("://")?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
    .then_some(scheme)
}

fn has_link_lines(text: &str) -> bool {
    text.lines().any(|line| scheme_of(line.trim()).is_some())
}

fn parse_json(text: &str) -> Result<Content, Reject> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Reject::Unrecognized(format!("некорректный JSON: {error}")))?;
    if xray_json::is_xray(&value) {
        return Ok(xray_json::parse(&value));
    }
    let has_outbounds = |config: &Value| config.get("outbounds").is_some();
    let sing_box = match &value {
        Value::Array(items) => items.first().is_some_and(has_outbounds),
        other => has_outbounds(other),
    };
    let kind = if sing_box {
        "конфиг sing-box"
    } else {
        "JSON неизвестного формата"
    };
    Err(Reject::Unrecognized(format!(
        "получен {kind}; поддерживаются xray-json и списки ссылок"
    )))
}

fn parse_links_or_reject(text: &str) -> Result<Content, Reject> {
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let crypted = text
        .lines()
        .map(str::trim)
        .any(|line| strip_prefix_ci(line, "happ://crypt").is_some());
    if crypted && !has_supported_scheme(text) {
        return Err(Reject::Encrypted(
            "получена зашифрованная ссылка happ://crypt…: расшифровать её может только Happ"
                .to_owned(),
        ));
    }
    if !has_link_lines(text) {
        let yaml = text.lines().any(|line| {
            ["proxies:", "proxy-groups:", "proxy-providers:"]
                .iter()
                .any(|key| line.starts_with(key))
        });
        let reason = if yaml {
            "получен конфиг в формате clash/mihomo (YAML): он не поддерживается, нужен xray-json или список ссылок"
        } else if first_line.is_some_and(|line| line.starts_with('#')) {
            "в теле ответа только заголовки провайдера, узлов нет"
        } else {
            "формат ответа не распознан"
        };
        return Err(Reject::Unrecognized(reason.to_owned()));
    }
    Ok(parse_links(text))
}

fn has_supported_scheme(text: &str) -> bool {
    text.lines().any(|line| {
        scheme_of(line.trim()).is_some_and(|scheme| {
            matches!(
                scheme.to_ascii_lowercase().as_str(),
                "vless" | "vmess" | "trojan" | "ss" | "hysteria2" | "hy2"
            )
        })
    })
}

fn parse_links(text: &str) -> Content {
    let mut content = Content::default();
    for line in text.lines() {
        let line = line.trim();
        let Some(scheme) = scheme_of(line) else {
            continue;
        };
        // Ссылки на сайты — часть сообщений провайдера, `happ://` обрабатывается отдельно.
        if matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "happ"
        ) {
            continue;
        }
        if content.nodes.len() >= MAX_NODES {
            push_warning(
                &mut content.warnings,
                format!("лишние ссылки отброшены: узлов больше {MAX_NODES}"),
            );
            break;
        }
        let scheme = clean(scheme, 32);
        if line.len() > MAX_LINE {
            push_warning(
                &mut content.warnings,
                format!("ссылка {scheme}:// пропущена: слишком длинная"),
            );
            continue;
        }
        match link::parse(line) {
            Ok(parsed) => {
                for warning in &parsed.warnings {
                    push_warning(
                        &mut content.warnings,
                        format!("«{}»: {warning}", parsed.node.name),
                    );
                }
                content.nodes.push(parsed.node);
            }
            Err(reason) => push_warning(
                &mut content.warnings,
                format!("ссылка {scheme}:// пропущена: {reason}"),
            ),
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};

    use super::*;

    const UUID: &str = "00000000-0000-0000-0000-000000000000";

    fn links() -> String {
        format!(
            "vless://{UUID}@nl.example.com:443?security=tls#Первый\n\
             trojan://secret@de.example.com:443#Второй\n\
             ss://{}@fi.example.com:8388#Третий\n",
            STANDARD.encode("aes-128-gcm:pw")
        )
    }

    fn names(body: &Body) -> Vec<String> {
        let content = body.content.as_ref().ok().unwrap();
        content.nodes.iter().map(|node| node.name.clone()).collect()
    }

    #[test]
    fn plain_links() {
        let body = parse(links().as_bytes());
        assert_eq!(names(&body), ["Первый", "Второй", "Третий"]);
        assert!(body.content.as_ref().ok().unwrap().warnings.is_empty());
    }

    #[test]
    fn base64_variants() {
        let text = links();
        let variants = [
            STANDARD.encode(&text),
            URL_SAFE.encode(&text),
            URL_SAFE_NO_PAD.encode(&text),
            // Переносы строк внутри base64.
            STANDARD
                .encode(&text)
                .as_bytes()
                .chunks(40)
                .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                .collect::<Vec<_>>()
                .join("\r\n"),
        ];
        for variant in variants {
            let body = parse(variant.as_bytes());
            assert_eq!(names(&body), ["Первый", "Второй", "Третий"], "{variant}");
        }
    }

    #[test]
    fn crlf_and_bom() {
        let text = format!("\u{feff}{}", links().replace('\n', "\r\n"));
        assert_eq!(names(&parse(text.as_bytes())).len(), 3);
    }

    #[test]
    fn bad_links_are_skipped_with_warnings() {
        let text = format!(
            "vless://{UUID}@ok.example.com:443#Рабочий\n\
             wireguard://x@a.example.com:51820#WG\n\
             vless://broken\n\
             https://example.com/renew\n\
             hy2://secret@hy.example.com:443?obfs=unknown\n"
        );
        let body = parse(text.as_bytes());
        assert_eq!(names(&body), ["Рабочий"]);
        let warnings = &body.content.as_ref().ok().unwrap().warnings;
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().all(|warning| !warning.contains("secret")));
    }

    #[test]
    fn headers_in_the_body() {
        let text = format!(
            "#profile-title: base64:0JzQvtC5\n\
             #Profile-Update-Interval: 6\n\
             # просто комментарий\n\
             #routing: happ://routing/off\n\
             {}",
            links()
        );
        let body = parse(text.as_bytes());
        assert_eq!(names(&body).len(), 3);
        let headers: Vec<(&str, &str)> = body
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            headers,
            [
                ("profile-title", "base64:0JzQvtC5"),
                ("profile-update-interval", "6"),
                ("routing", "happ://routing/off"),
            ]
        );
    }

    #[test]
    fn headers_inside_base64_and_before_it() {
        let inner = format!("#profile-title: Внутри\n{}", links());
        let body = parse(STANDARD.encode(inner).as_bytes());
        assert_eq!(names(&body).len(), 3);
        assert_eq!(
            body.headers,
            [("profile-title".to_owned(), "Внутри".to_owned())]
        );

        let outer = format!("#profile-title: Снаружи\n{}", STANDARD.encode(links()));
        let body = parse(outer.as_bytes());
        assert_eq!(names(&body).len(), 3);
        assert_eq!(
            body.headers,
            [("profile-title".to_owned(), "Снаружи".to_owned())]
        );
    }

    #[test]
    fn headers_only_count_at_the_start() {
        let text = format!("{}#support-url: https://example.com\n", links());
        assert!(parse(text.as_bytes()).headers.is_empty());
    }

    #[test]
    fn routing_line_in_the_body() {
        let text = format!("{}happ://routing/onadd/e30=\n", links());
        let body = parse(text.as_bytes());
        assert_eq!(body.routing.as_deref(), Some("happ://routing/onadd/e30="));
        assert_eq!(names(&body).len(), 3);
        assert!(body.content.as_ref().ok().unwrap().warnings.is_empty());
    }

    #[test]
    fn xray_json_body() {
        let text = format!(
            r#"[{{"remarks": "Узел", "outbounds": [{{"tag": "proxy", "protocol": "vless",
                "settings": {{"address": "j.example.com", "port": 443, "id": "{UUID}"}}}}]}}]"#
        );
        assert_eq!(names(&parse(text.as_bytes())), ["Узел"]);
        assert_eq!(names(&parse(STANDARD.encode(&text).as_bytes())), ["Узел"]);
    }

    fn rejection(body: &[u8]) -> String {
        match parse(body).content {
            Err(Reject::Unrecognized(reason) | Reject::Encrypted(reason)) => reason,
            Ok(_) => panic!("тело должно быть отвергнуто"),
        }
    }

    #[test]
    fn unrecognized_bodies() {
        assert!(rejection(b"").contains("пустое"));
        assert!(rejection(b"   \n ").contains("пустое"));
        assert!(rejection(b"<!DOCTYPE html><html></html>").contains("HTML"));
        assert!(rejection(b"<html>").contains("HTML"));
        assert!(rejection(b"just some text").contains("не распознан"));
        assert!(rejection(b"key: value").contains("не распознан"));
        assert!(rejection(b"proxies:\n  - {name: a}\n").contains("YAML"));
        assert!(rejection(br#"{"outbounds":[{"type":"vless"}]}"#).contains("sing-box"));
        assert!(rejection(br#"{"a": 1}"#).contains("неизвестного"));
        assert!(rejection(b"{not json").contains("JSON"));
        assert!(rejection(b"#profile-title: x\n").contains("только заголовки"));
        assert!(rejection(&[0xff, 0xfe, 0x00, 0x01]).contains("не распознан"));
    }

    #[test]
    fn encrypted_link_is_reported() {
        let body = parse(b"happ://crypt5/AAAA");
        assert!(matches!(body.content, Err(Reject::Encrypted(_))));
    }

    #[test]
    fn oversized_body_is_rejected() {
        let huge = vec![b'a'; MAX_BODY + 1];
        assert!(rejection(&huge).contains("32 МиБ"));
    }

    #[test]
    fn node_limit() {
        let line = format!("vless://{UUID}@a.example.com:443\n");
        let text = line.repeat(MAX_NODES + 10);
        let body = parse(text.as_bytes());
        let content = body.content.ok().unwrap();
        assert_eq!(content.nodes.len(), MAX_NODES);
        assert_eq!(content.warnings.len(), 1);
    }

    #[test]
    fn very_long_line_is_skipped() {
        let long = format!(
            "vless://{UUID}@a.example.com:443?path={}\n",
            "x".repeat(MAX_LINE)
        );
        let text = format!("{long}vless://{UUID}@b.example.com:443\n");
        let body = parse(text.as_bytes());
        assert_eq!(names(&body), ["b.example.com:443"]);
    }
}
