//! Разбор ответа целиком через публичный интерфейс. Все данные вымышленные.

// Вспомогательная функция шифрования не `#[test]`, и разрешение из `clippy.toml` на неё не действует.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use raycat_subscription::{Problem, Routing, analyze, analyze_with_key, redact, redact_in};
use serde_json::json;

const UUID: &str = "00000000-0000-0000-0000-000000000000";

fn headers(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn link_list() -> String {
    format!(
        "vless://{UUID}@nl.example.com:443?security=reality&pbk=KEY&sid=ab&sni=www.example.org#NL\n\
         vmess://{UUID}@de.example.com:443?security=tls&type=ws&path=%2Fws#DE\n\
         trojan://secret@fi.example.com:443#FI\n\
         hy2://secret@hy.example.com:443#HY\n"
    )
}

fn stub_links() -> String {
    format!(
        "vless://{UUID}@0.0.0.0:1?security=none#%D0%9F%D0%BE%D0%B4%D0%BF%D0%B8%D1%81%D0%BA%D0%B0%20%D0%B8%D1%81%D1%82%D0%B5%D0%BA%D0%BB%D0%B0\n"
    )
}

#[test]
fn good_links_response() {
    let response = headers(&[
        (
            "Subscription-Userinfo",
            "upload=1; download=2; total=10; expire=1767225600",
        ),
        ("Profile-Title", "base64:0JzQvtC5IFZQTg=="),
        ("Profile-Update-Interval", "12"),
        ("Routing", "happ://routing/off"),
    ]);
    let result = analyze(200, &response, link_list().as_bytes());
    assert!(result.problem.is_none(), "{:?}", result.problem);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    let names: Vec<&str> = result.nodes.iter().map(|node| node.name.as_str()).collect();
    assert_eq!(names, ["NL", "DE", "FI", "HY"]);
    assert_eq!(result.info.title.as_deref(), Some("Мой VPN"));
    assert_eq!(
        result.info.usage.as_ref().map(|usage| usage.total),
        Some(10)
    );
    assert_eq!(
        result.info.update_interval,
        Some(Duration::from_secs(12 * 3600))
    );
    assert_eq!(result.info.routing, Some(Routing::Off));
}

#[test]
fn base64_body_with_headers_inside() {
    let text = format!(
        "#profile-title: Из тела\n#support-url: https://example.com/support\n{}",
        link_list()
    );
    let result = analyze(200, &[], STANDARD.encode(text).as_bytes());
    assert!(result.problem.is_none(), "{:?}", result.problem);
    assert_eq!(result.nodes.len(), 4);
    assert_eq!(result.info.title.as_deref(), Some("Из тела"));
    assert_eq!(
        result.info.support_url.as_deref(),
        Some("https://example.com/support")
    );
}

#[test]
fn xray_json_response() {
    let body = json!([
        {"remarks": "🇳🇱 NL", "outbounds": [
            {"tag": "proxy", "protocol": "vless",
             "settings": {"address": "nl.example.com", "port": 443, "id": UUID},
             "streamSettings": {"sockopt": {"dialerProxy": "fragment"}}},
            {"tag": "fragment", "protocol": "freedom",
             "settings": {"fragment": {"packets": "tlshello", "length": "100-200"}}},
            {"tag": "direct", "protocol": "freedom"}]},
        {"remarks": "🇩🇪 DE", "outbounds": [
            {"tag": "proxy", "protocol": "trojan",
             "settings": {"address": "de.example.com", "port": 443, "password": "p"}}]},
    ]);
    let result = analyze(200, &[], body.to_string().as_bytes());
    assert!(result.problem.is_none(), "{:?}", result.problem);
    assert_eq!(result.nodes.len(), 2);
    assert_eq!(result.nodes[0].outbounds.len(), 2);
    assert_eq!(result.nodes[0].outbounds[0]["tag"], "proxy");
    assert_eq!(result.nodes[1].outbounds.len(), 1);
}

#[test]
fn provider_headers_reach_the_info() {
    let response = headers(&[
        ("announce", "base64:0J/RgNC40LLQtdGC"),
        ("fallback-url", "https://reserve.example.com/sub"),
        ("new-domain", "moved.example.com"),
        ("change-user-agent", "Example/1.0"),
    ]);
    let info = analyze(200, &response, link_list().as_bytes()).info;
    assert_eq!(info.announce.as_deref(), Some("Привет"));
    assert_eq!(
        info.fallback_url.as_deref(),
        Some("https://reserve.example.com/sub")
    );
    assert_eq!(info.new_domain.as_deref(), Some("moved.example.com"));
    assert_eq!(info.change_user_agent.as_deref(), Some("Example/1.0"));
}

#[test]
fn stub_response_is_refused() {
    let result = analyze(200, &[], stub_links().as_bytes());
    let Some(Problem::Stub(message)) = &result.problem else {
        panic!("ожидалась заглушка: {:?}", result.problem);
    };
    assert!(message.contains("Подписка истекла"), "{message}");
    assert_eq!(result.nodes.len(), 1);
}

#[test]
fn stubs_next_to_real_nodes_are_dropped() {
    let text = format!("{}{}", stub_links(), link_list());
    let result = analyze(200, &[], text.as_bytes());
    assert!(result.problem.is_none(), "{:?}", result.problem);
    assert_eq!(result.nodes.len(), 4);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("заглушки"))
    );
}

#[test]
fn hwid_refusal_wins_over_the_body() {
    for header in ["x-hwid-max-devices-reached", "x-hwid-not-supported"] {
        let result = analyze(200, &headers(&[(header, "true")]), link_list().as_bytes());
        assert!(
            matches!(result.problem, Some(Problem::Refused(_))),
            "{header}"
        );
    }
    let empty = analyze(200, &headers(&[("x-hwid-not-supported", "true")]), b"");
    assert!(matches!(empty.problem, Some(Problem::Refused(_))));
    let active_only = analyze(
        200,
        &headers(&[("x-hwid-active", "true")]),
        link_list().as_bytes(),
    );
    assert!(active_only.problem.is_none());
    assert!(active_only.info.hwid.active);
}

#[test]
fn http_errors() {
    for status in [301, 403, 404, 429, 451, 500] {
        let result = analyze(status, &[], link_list().as_bytes());
        let Some(Problem::Http(message)) = &result.problem else {
            panic!("статус {status}: {:?}", result.problem);
        };
        assert!(message.contains(&status.to_string()));
        assert!(result.nodes.is_empty());
    }
    let with_headers = analyze(403, &headers(&[("profile-title", "VPN")]), b"Forbidden");
    assert_eq!(with_headers.info.title.as_deref(), Some("VPN"));
}

#[test]
fn unrecognized_bodies() {
    for body in [
        &b""[..],
        b"<html><body>404</body></html>",
        b"proxies:\n  - {name: a, type: vless}\n",
        b"just words",
        br#"{"log": {}}"#,
    ] {
        let result = analyze(200, &[], body);
        assert!(
            matches!(result.problem, Some(Problem::Unrecognized(_))),
            "{:?}",
            result.problem
        );
        assert!(result.nodes.is_empty());
    }
}

#[test]
fn only_unusable_links_is_unrecognized() {
    let result = analyze(200, &[], b"wireguard://x@a.example.com:51820#WG\n");
    assert!(matches!(result.problem, Some(Problem::Unrecognized(_))));
    assert_eq!(result.warnings.len(), 1);
}

#[test]
fn garbage_never_panics() {
    let mut noise = Vec::new();
    for round in 0u8..=255 {
        noise.push(round);
        noise.push(round.wrapping_mul(31));
    }
    let bodies: Vec<Vec<u8>> = vec![
        noise,
        b"[".to_vec(),
        b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[".to_vec(),
        b"vless://".to_vec(),
        b"vless://@:".to_vec(),
        b"vmess://e30=".to_vec(),
        b"ss://@".to_vec(),
        b"hy2://@[".to_vec(),
        "#\n#:\n#:x\n\u{feff}".as_bytes().to_vec(),
        format!(
            "{{\"outbounds\":[{{\"protocol\":\"vless\",\"settings\":{{\"port\":{}}}}}]}}",
            u64::MAX
        )
        .into_bytes(),
    ];
    for body in bodies {
        let result = analyze(
            200,
            &headers(&[("routing", "happ://routing/add/\u{0}")]),
            &body,
        );
        assert!(result.problem.is_some());
    }
}

fn encrypt(plain: &[u8], key: &[u8; 16]) -> (String, String) {
    let cipher = Aes128Gcm::new_from_slice(key).unwrap();
    let sealed = cipher
        .encrypt(
            &Nonce::<U12>::from(std::array::from_fn::<u8, 12, _>(|_| b'k')),
            plain,
        )
        .unwrap();
    let (data, tag) = sealed.split_at(sealed.len() - 16);
    (STANDARD.encode(data), STANDARD.encode(tag))
}

#[test]
fn encrypted_response() {
    let key: [u8; 16] = std::array::from_fn(|index| u8::try_from(index).unwrap_or(0) * 3 + 1);
    let (body, tag) = encrypt(link_list().as_bytes(), &key);
    let response = headers(&[("Encrypt-Tag", tag.as_str()), ("profile-title", "VPN")]);

    let without_key = analyze(200, &response, body.as_bytes());
    assert!(matches!(without_key.problem, Some(Problem::Encrypted(_))));
    assert_eq!(without_key.info.title.as_deref(), Some("VPN"));

    let decrypted = analyze_with_key(200, &response, body.as_bytes(), &key);
    assert!(decrypted.problem.is_none(), "{:?}", decrypted.problem);
    assert_eq!(decrypted.nodes.len(), 4);

    let wrong = analyze_with_key(200, &response, body.as_bytes(), &key.map(|byte| byte ^ 1));
    assert!(matches!(wrong.problem, Some(Problem::Encrypted(_))));
}

#[test]
fn redaction() {
    assert_eq!(
        redact("https://sub.example.com/api/sub/AbCdEfGh1234"),
        "https://sub.example.com/…1234"
    );
    let urls = ["https://sub.example.com/api/sub/AbCdEfGh1234"];
    assert_eq!(
        redact_in("Продлить: https://mirror.example.com/AbCdEfGh1234", &urls),
        "Продлить: https://mirror.example.com/…1234"
    );
}

#[test]
fn problem_messages_are_readable() {
    let result = analyze(403, &[], b"");
    let problem = result.problem.unwrap();
    assert!(problem.message().starts_with("HTTP 403"));
    assert_eq!(problem.to_string(), problem.message());
}
