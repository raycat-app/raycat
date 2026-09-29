//! Разбор ответов панелей подписок.
//!
//! Крейт не ходит в сеть: на вход — статус, заголовки и тело ответа, на выход —
//! сведения провайдера, узлы (`raycat_xray::Node`) и, если ответ применять нельзя,
//! причина ([`Problem`]).

mod body;
mod encrypt;
mod info;
mod link;
mod redact;
mod routing;
mod stub;
mod text;
mod xray_json;

use std::borrow::Cow;

use raycat_xray::Node;

use body::{Body, Reject};
use text::{clean, push_warning};

pub use encrypt::{DecryptError, decrypt_body};
pub use info::{HwidFlags, ProviderInfo, Usage};
pub use redact::{redact, redact_in};
pub use routing::{DnsServer, Routing, RoutingProfile};

/// Почему ответ нельзя применять: он не заменяет последний рабочий конфиг.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Панель отказала устройству (признаки HWID).
    Refused(String),
    /// Статус HTTP не 2xx.
    Http(String),
    /// Корректный конфиг, в котором только сообщение провайдера.
    Stub(String),
    /// Тело не удалось разобрать в узлы.
    Unrecognized(String),
    /// Тело зашифровано, и расшифровать его не получилось.
    Encrypted(String),
}

impl Problem {
    pub fn message(&self) -> &str {
        match self {
            Self::Refused(text)
            | Self::Http(text)
            | Self::Stub(text)
            | Self::Unrecognized(text)
            | Self::Encrypted(text) => text,
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub info: ProviderInfo,
    /// Разобранные узлы. При `problem` их применять нельзя; у заглушки это сами
    /// узлы-заглушки, их названия несут сообщение провайдера.
    pub nodes: Vec<Node>,
    /// Что не удалось перенести или пришлось пропустить; без секретов.
    pub warnings: Vec<String>,
    pub problem: Option<Problem>,
}

/// Разбирает ответ панели. Ответ с `Encrypt-Tag` без ключа считается
/// зашифрованным ([`Problem::Encrypted`]); ключ задаёт [`analyze_with_key`].
pub fn analyze(status: u16, headers: &[(String, String)], body: &[u8]) -> Analysis {
    analyze_inner(status, headers, body, None)
}

/// То же, что [`analyze`], но тело с `Encrypt-Tag` расшифровывается этим ключом.
pub fn analyze_with_key(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    key: &[u8; 16],
) -> Analysis {
    analyze_inner(status, headers, body, Some(key))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, value)| key.trim().eq_ignore_ascii_case(name) && !value.trim().is_empty())
        .map(|(_, value)| value.trim())
}

/// Тело после снятия шифрования или причина, по которой его читать нельзя.
fn plain_body<'a>(
    headers: &[(String, String)],
    body: &'a [u8],
    key: Option<&[u8; 16]>,
) -> Result<Cow<'a, [u8]>, String> {
    let Some(tag) = header(headers, "encrypt-tag") else {
        return Ok(Cow::Borrowed(body));
    };
    let Some(key) = key else {
        return Err("ответ зашифрован (заголовок Encrypt-Tag), ключ не задан".to_owned());
    };
    decrypt_body(body, tag, key)
        .map(Cow::Owned)
        .map_err(|error| error.to_string())
}

fn analyze_inner(
    status: u16,
    headers: &[(String, String)],
    raw_body: &[u8],
    key: Option<&[u8; 16]>,
) -> Analysis {
    let http_ok = (200..300).contains(&status);
    let parsed = if http_ok {
        match plain_body(headers, raw_body, key) {
            Ok(plain) => body::parse(&plain),
            Err(reason) => Body::rejected(Reject::Encrypted(reason)),
        }
    } else {
        Body::rejected(Reject::Unrecognized(String::new()))
    };
    let (info, mut warnings) = info::parse(headers, &parsed.headers, parsed.routing.as_deref());

    let (mut nodes, rejection) = match parsed.content {
        Ok(content) => {
            for warning in content.warnings {
                push_warning(&mut warnings, warning);
            }
            (content.nodes, None)
        }
        Err(reject) => (Vec::new(), Some(reject)),
    };
    let stub_message = drop_stubs(&mut nodes, &mut warnings);

    let problem = if let Some(reason) = stub::hwid_refusal(info.hwid) {
        Some(Problem::Refused(reason))
    } else if !http_ok {
        Some(Problem::Http(http_message(status)))
    } else if let Some(reject) = rejection {
        Some(match reject {
            Reject::Unrecognized(reason) => Problem::Unrecognized(reason),
            Reject::Encrypted(reason) => Problem::Encrypted(reason),
        })
    } else if nodes.is_empty() {
        Some(Problem::Unrecognized(no_nodes_message(warnings.len())))
    } else {
        stub_message.map(Problem::Stub)
    };
    Analysis {
        info,
        nodes,
        warnings,
        problem,
    }
}

/// Если рядом с настоящими узлами есть заглушки (Remnawave добавляет узлы-сообщения
/// вроде «Подписка истекает»), убирает их из `nodes` и оставляет предупреждение.
/// Если настоящих узлов нет, узлы остаются как есть, а возвращается сообщение
/// о заглушке.
fn drop_stubs(nodes: &mut Vec<Node>, warnings: &mut Vec<String>) -> Option<String> {
    let names: Vec<String> = nodes
        .iter()
        .filter(|node| stub::is_stub(node))
        .map(|node| clean(&node.name, 100))
        .collect();
    if names.is_empty() {
        return None;
    }
    if names.len() == nodes.len() {
        return Some(format!(
            "все узлы — заглушки (адрес 0.0.0.0 или локальный): {}",
            names.join(" | ")
        ));
    }
    nodes.retain(|node| !stub::is_stub(node));
    push_warning(
        warnings,
        format!("пропущены узлы-заглушки: {}", names.join(" | ")),
    );
    None
}

fn http_message(status: u16) -> String {
    let hint = match status {
        403 => ": панель блокирует этот клиент (правило по User-Agent) или требует HWID",
        404 => ": подписка не найдена, либо панель требует HWID",
        429 => ": панель ограничила частоту запросов",
        451 => ": правила ответа панели не пропускают этот клиент",
        _ => "",
    };
    format!("HTTP {status}{hint}")
}

fn no_nodes_message(skipped: usize) -> String {
    if skipped == 0 {
        "в ответе нет ни одного узла".to_owned()
    } else {
        format!("в ответе нет ни одного пригодного узла (предупреждений: {skipped})")
    }
}
