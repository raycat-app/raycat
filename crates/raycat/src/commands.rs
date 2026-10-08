//! Команды для человека: `check`, `fetch`, `identity`.

use std::fs;
use std::io::{self, Write as _};

use anyhow::{Context, Result, anyhow, bail};
use raycat_config::{Config, Routing, Subscription};
use raycat_http::{Response, Url, redact};
use raycat_subscription::{ProviderInfo, analyze, redact as redact_text, redact_in};
use raycat_xray::Node;
use serde_json::Value;

use crate::daemon::free_port;
use crate::plan;
use crate::store::Store;
use crate::tuning;
use crate::updater::{self, Source};
use crate::util::{
    format_bytes, format_date, format_duration, local_zone, moment, now_unix, sanitize,
};
use crate::xray;

const MAX_LISTED_NODES: usize = 50;
const CHECK_CONFIG: &str = "xray-check.json";

/// Строка в stdout; закрытый канал (`| head`) не повод падать.
macro_rules! say {
    ($($arg:tt)*) => {{
        let _ = writeln!(io::stdout().lock(), $($arg)*);
    }};
}

/// `raycat check`: настройки, кэш подписок, конфиг xray и `xray run -test`.
pub(crate) fn check(config: &Config, store: &Store) -> Result<()> {
    let rules = plan::gateway_rules(config)?;
    let machine_id = store.machine_id()?;
    for warning in &config.warnings {
        say!("Предупреждение: {warning}");
    }
    say!(
        "Настройки в порядке: подписок {}, {}",
        config.subscriptions.len(),
        plan::describe_mode(config)
    );
    if let Some(lan) = rules.as_ref().and_then(|rules| rules.lan.as_ref()) {
        say!("{}", plan::describe_lan(lan));
    }
    let mut cached: Vec<Vec<Node>> = Vec::new();
    let mut answers: Vec<Option<raycat_subscription::Routing>> = Vec::new();
    for subscription in &config.subscriptions {
        let source = Source::new(config, subscription, &machine_id)?;
        let name = &subscription.name;
        if let Some(found) = source.cached(store) {
            say!(
                "Подписка «{name}»: кэш от {}, узлов: {}",
                moment(found.fetched_at),
                found.nodes.len()
            );
            cached.push(found.nodes);
            answers.push(found.info.routing);
        } else {
            say!("Подписка «{name}»: кэша нет");
            cached.push(Vec::new());
            answers.push(None);
        }
    }
    let provider = plan::provider_profile(config, answers.iter().map(Option::as_ref))
        .map(plan::translate);
    let provider_text = config.routing.provider.then(|| match &provider {
        Some((provider, _)) => plan::provider_summary(provider),
        None => "провайдер: профиля нет".to_owned(),
    });
    say!(
        "{}",
        routing_line(
            &config.routing,
            raycat_routing::ru_zones().len(),
            raycat_routing::ru_ipv4().count(),
            raycat_routing::data_date(),
            provider_text.as_deref(),
        )
    );
    if cached.iter().all(Vec::is_empty) {
        bail!("кэша подписок нет: запустите демон (raycat daemon), он получит подписки");
    }
    let inputs: Vec<(&Subscription, &[Node], Option<&raycat_subscription::Routing>)> = config
        .subscriptions
        .iter()
        .zip(&cached)
        .zip(&answers)
        .map(|((subscription, nodes), routing)| {
            (subscription, nodes.as_slice(), routing.as_ref())
        })
        .collect();
    let congestion = tuning::congestion(&config.xray.tcp_congestion);
    let plan = plan::compile_config(config, &inputs, free_port()?, congestion.algorithm())?;
    say!(
        "Конфиг xray собран: узлов {}, пропущено (xray не поддерживает): {}",
        plan.nodes,
        plan.skipped
    );
    if let Some(line) = congestion.describe() {
        say!("{line}");
    }
    store.write_file(CHECK_CONFIG, &plan.json)?;
    let path = store.path(CHECK_CONFIG);
    let tested = xray::test_config(&config.xray.path, &path);
    let _ = fs::remove_file(&path);
    tested?;
    say!("xray run -test: конфиг принят");
    Ok(())
}

/// Итог маршрутизации: пресет «Россия напрямую», свои правила и профиль провайдера.
fn routing_line(
    routing: &Routing,
    zones: usize,
    subnets: usize,
    date: Option<&str>,
    provider: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if routing.ru_direct {
        let stamp = date.map_or_else(String::new, |date| format!(", данные от {}", ru_date(date)));
        parts.push(format!(
            "Россия напрямую (зон {zones}, подсетей {subnets}{stamp})"
        ));
    }
    if !routing.rules.is_empty() {
        parts.push(format!("своих правил {}", routing.rules.len()));
    }
    if let Some(provider) = provider {
        parts.push(provider.to_owned());
    }
    if parts.is_empty() {
        "Маршрутизация: всё через VPN".to_owned()
    } else {
        format!("Маршрутизация: {}", parts.join("; "))
    }
}

/// `2026-10-07` → `07.10.2026`.
fn ru_date(iso: &str) -> String {
    match iso.split('-').collect::<Vec<_>>().as_slice() {
        [year, month, day] => format!("{day}.{month}.{year}"),
        _ => iso.to_owned(),
    }
}

/// `raycat fetch`: разовый запрос без применения и без записи в кэш.
pub(crate) fn fetch(config: &Config, store: &Store, name: &str) -> Result<()> {
    let subscription = config
        .subscriptions
        .iter()
        .find(|subscription| subscription.name == name)
        .with_context(|| unknown_subscription(config, name))?;
    let source = Source::new(config, subscription, &store.machine_id()?)?;
    let start = Url::parse(subscription.url.expose()).context("некорректная ссылка подписки")?;
    let hwid = source.emulation().device_info().hwid;
    say!("Подписка «{name}»: {}", subscription.masked_url());
    let mut send = |url: &Url| -> Result<Response> {
        let headers = source.headers(url);
        say!("Запрос GET {}", redact(url));
        for (key, value) in shown_headers(&headers, &hwid) {
            say!("  {key}: {value}");
        }
        say!("  (идентификаторы устройства скрыты; полностью — raycat identity)");
        source.send_with(url, &headers)
    };
    let (response, served) = updater::follow(&start, &mut send)?;
    say!("Ответ: HTTP {} от {}", response.status, redact(&served));

    let analysis = analyze(response.status, &response.headers, &response.body);
    let secret = subscription.url.expose();
    print_provider(&analysis.info, secret);
    print_nodes(&analysis.nodes);
    if !analysis.warnings.is_empty() {
        say!("Предупреждения ({}):", analysis.warnings.len());
        for warning in &analysis.warnings {
            say!("  - {}", sanitize(warning));
        }
    }
    if let Some(problem) = &analysis.problem {
        say!("Проблема: {}", redact_in(problem.message(), &[secret]));
        return Err(anyhow!("ответ подписки нельзя применить"));
    }
    say!("Проблем нет: ответ можно применять");
    Ok(())
}

/// Заголовки для вывода: `X-HWID`, значения с HWID и имя хоста в модели скрыты.
fn shown_headers(headers: &[(String, String)], hwid: &str) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let shown = if name.eq_ignore_ascii_case("x-hwid") || has_hwid(value, hwid) {
                hide_value(value)
            } else if name.eq_ignore_ascii_case("x-device-model") {
                hide_model(value)
            } else {
                value.clone()
            };
            (name.clone(), shown)
        })
        .collect()
}

fn has_hwid(value: &str, hwid: &str) -> bool {
    !hwid.is_empty()
        && value
            .to_ascii_lowercase()
            .contains(&hwid.to_ascii_lowercase())
}

/// Модель Windows — имя хоста и процессор через `_`: скрывается только имя хоста.
fn hide_model(model: &str) -> String {
    for cpu in ["_x86_64", "_arm64"] {
        if let Some(host) = model.strip_suffix(cpu) {
            return format!("{}{cpu}", hide_value(host));
        }
    }
    model.to_owned()
}

/// Первые и последние четыре символа; короткое значение (до 10 символов) скрыто целиком.
fn hide_value(value: &str) -> String {
    let len = value.chars().count();
    if len <= 10 {
        return "…".to_owned();
    }
    let head: String = value.chars().take(4).collect();
    let tail: String = value.chars().skip(len - 4).collect();
    format!("{head}…{tail}")
}

fn unknown_subscription(config: &Config, name: &str) -> String {
    let names: Vec<&str> = config
        .subscriptions
        .iter()
        .map(|subscription| subscription.name.as_str())
        .collect();
    format!(
        "подписки «{name}» нет в настройках; есть: {}",
        names.join(", ")
    )
}

fn print_provider(info: &ProviderInfo, secret: &str) {
    let hide = |text: &str| redact_in(text, &[secret]);
    say!("Провайдер:");
    if let Some(title) = &info.title {
        say!("  название: {}", hide(title));
    }
    if let Some(usage) = &info.usage {
        let total = if usage.total == 0 {
            "без лимита".to_owned()
        } else {
            format_bytes(usage.total)
        };
        say!(
            "  трафик: использовано {} из {total}",
            format_bytes(usage.used())
        );
        if usage.expire != 0 {
            say!(
                "  срок подписки: до {}",
                format_date(usage.expire, local_zone())
            );
        }
    }
    if let Some(interval) = info.update_interval {
        say!("  интервал обновления: {}", format_duration(interval));
    }
    if let Some(announce) = &info.announce {
        say!("  объявление: {}", hide(announce));
    }
    if let Some(url) = &info.support_url {
        say!("  поддержка: {}", hide(url));
    }
    if let Some(url) = &info.web_page_url {
        say!("  страница: {}", hide(url));
    }
    if let Some(url) = &info.fallback_url {
        say!("  запасной адрес: {}", redact_text(url));
    }
    if let Some(domain) = &info.new_domain {
        say!("  провайдер переехал на домен: {}", hide(domain));
    }
    if let Some(url) = &info.new_url {
        say!("  провайдер переехал на адрес: {}", redact_text(url));
    }
    if info.hwid.active {
        say!("  лимит устройств: панель его проверяет");
    }
    if info.routing.is_some() {
        say!("  маршрутизация провайдера: есть (применяется, только если включена в настройках)");
    }
}

fn print_nodes(nodes: &[Node]) {
    say!("Узлы ({}):", nodes.len());
    for (index, node) in nodes.iter().take(MAX_LISTED_NODES).enumerate() {
        let protocol = node
            .outbounds
            .first()
            .and_then(|outbound| outbound.get("protocol"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        say!("  {}. {} [{protocol}]", index + 1, sanitize(&node.name));
    }
    if nodes.len() > MAX_LISTED_NODES {
        say!("  … и ещё {}", nodes.len() - MAX_LISTED_NODES);
    }
}

/// `raycat identity`: каким устройством raycat представляется каждой подписке.
pub(crate) fn identity(config: &Config, store: &Store) -> Result<()> {
    let machine_id = store.machine_id()?;
    for subscription in &config.subscriptions {
        let source = Source::new(config, subscription, &machine_id)?;
        let emulation = source.emulation();
        let device = emulation.device_info();
        say!("«{}» ({})", subscription.name, subscription.masked_url());
        say!(
            "  приложение: {} {} (сборка {}), платформа: {}",
            emulation.app(),
            emulation.app_version(),
            emulation.build(),
            subscription.platform.as_str()
        );
        say!("  User-Agent: {}", emulation.user_agent(now_unix()));
        say!("  HWID: {}", device.hwid);
        say!("  система: {} {}", device.os, device.os_version);
        say!("  модель: {}", device.model);
        if let Some(manufacturer) = &device.manufacturer {
            say!("  производитель: {manufacturer}");
        }
        say!("  устройство выведено из: {}", source.origin().describe());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use raycat_config::{Action, Rule};

    use super::*;

    const HWID: &str = "a1b2c3d4e5f6a7b8";

    #[test]
    fn hidden_values_keep_only_the_edges() {
        assert_eq!(hide_value(HWID), "a1b2…a7b8");
        assert_eq!(hide_value("0123456789"), "…");
        assert_eq!(hide_value("01234567890"), "0123…7890");
        assert_eq!(hide_value("ЖЖЖЖЖЖЖЖЖЖЖЖ"), "ЖЖЖЖ…ЖЖЖЖ");
        assert_eq!(hide_value(""), "…");
    }

    #[test]
    fn model_hides_only_the_computer_name() {
        assert_eq!(hide_model("EXAMPLE-HOST_x86_64"), "EXAM…HOST_x86_64");
        assert_eq!(hide_model("EXAMPLE-PC_arm64"), "…_arm64");
        assert_eq!(hide_model("SM-S921B"), "SM-S921B");
        assert_eq!(hide_model("Pixel 8"), "Pixel 8");
    }

    #[test]
    fn shown_headers_hide_device_identifiers() {
        let headers: Vec<(String, String)> = [
            ("User-Agent", "Happ/4.3.0/Windows/2609151455"),
            ("X-Device-Model", "EXAMPLE-HOST_x86_64"),
            ("X-Hwid", HWID),
            ("X-HWID", "zq7w-plain-value-0000"),
            ("X-Custom", "A1B2C3D4E5F6A7B8"),
            ("X-Ref", "id-a1b2c3d4e5f6a7b8-end"),
            ("Accept-Language", "ru-RU,en,*"),
            ("Host", "sub.example.com"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let pairs = shown_headers(&headers, HWID);
        let shown: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("User-Agent", "Happ/4.3.0/Windows/2609151455"),
                ("X-Device-Model", "EXAM…HOST_x86_64"),
                ("X-Hwid", "a1b2…a7b8"),
                ("X-HWID", "zq7w…0000"),
                ("X-Custom", "A1B2…A7B8"),
                ("X-Ref", "id-a…-end"),
                ("Accept-Language", "ru-RU,en,*"),
                ("Host", "sub.example.com"),
            ]
        );
    }

    #[test]
    fn empty_hwid_hides_no_value() {
        let headers = [("Accept".to_owned(), "text/html".to_owned())];
        assert_eq!(shown_headers(&headers, "")[0].1, "text/html");
    }

    fn rule() -> Rule {
        Rule {
            domains: Vec::new(),
            ips: Vec::new(),
            action: Action::Direct,
        }
    }

    #[test]
    fn routing_line_says_what_leaves_the_vpn() {
        let mut routing = Routing {
            provider: false,
            ru_direct: false,
            rules: Vec::new(),
        };
        let stamp = Some("2026-10-07");
        assert_eq!(
            routing_line(&routing, 8, 8655, stamp, None),
            "Маршрутизация: всё через VPN"
        );
        routing.rules = vec![rule(), rule(), rule()];
        assert_eq!(
            routing_line(&routing, 8, 8655, stamp, None),
            "Маршрутизация: своих правил 3"
        );
        routing.ru_direct = true;
        assert_eq!(
            routing_line(&routing, 8, 8655, stamp, None),
            "Маршрутизация: Россия напрямую (зон 8, подсетей 8655, данные от 07.10.2026); своих правил 3"
        );
        routing.rules.clear();
        assert_eq!(
            routing_line(&routing, 8, 8655, None, None),
            "Маршрутизация: Россия напрямую (зон 8, подсетей 8655)"
        );
    }

    #[test]
    fn routing_line_names_the_provider_profile() {
        let routing = Routing {
            provider: true,
            ru_direct: false,
            rules: Vec::new(),
        };
        assert_eq!(
            routing_line(
                &routing,
                8,
                8655,
                None,
                Some("провайдер: «Тест», правил 3, пропущено 1")
            ),
            "Маршрутизация: провайдер: «Тест», правил 3, пропущено 1"
        );
        assert_eq!(
            routing_line(&routing, 8, 8655, None, Some("провайдер: профиля нет")),
            "Маршрутизация: провайдер: профиля нет"
        );
    }

    #[test]
    fn snapshot_date_is_written_day_first() {
        assert_eq!(ru_date("2026-10-07"), "07.10.2026");
        assert_eq!(ru_date("2026-10"), "2026-10");
    }
}
