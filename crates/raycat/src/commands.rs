//! Команды для человека: `check`, `fetch`, `identity`.

use std::fs;
use std::io::{self, Write as _};

use anyhow::{Context, Result, anyhow, bail};
use raycat_config::{Config, Subscription};
use raycat_http::{Response, Url, redact};
use raycat_subscription::{ProviderInfo, analyze, redact as redact_text, redact_in};
use raycat_xray::Node;
use serde_json::Value;

use crate::daemon::free_port;
use crate::plan;
use crate::store::Store;
use crate::updater::{self, Source};
use crate::util::{format_bytes, format_date, format_duration, format_time, now_unix, sanitize};
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
    let listen = plan::proxy_listen(config)?;
    let machine_id = store.machine_id()?;
    say!(
        "Настройки в порядке: подписок {}, режим прокси, адрес {listen}",
        config.subscriptions.len()
    );
    let mut cached: Vec<Vec<Node>> = Vec::new();
    for subscription in &config.subscriptions {
        let source = Source::new(config, subscription, &machine_id)?;
        let name = &subscription.name;
        if let Some(found) = source.cached(store) {
            say!(
                "Подписка «{name}»: кэш от {}, узлов: {}",
                format_time(found.fetched_at),
                found.nodes.len()
            );
            cached.push(found.nodes);
        } else {
            say!("Подписка «{name}»: кэша нет");
            cached.push(Vec::new());
        }
    }
    if cached.iter().all(Vec::is_empty) {
        bail!("кэша подписок нет: запустите демон (raycat daemon), он получит подписки");
    }
    let inputs: Vec<(&Subscription, &[Node])> = config
        .subscriptions
        .iter()
        .zip(&cached)
        .map(|(subscription, nodes)| (subscription, nodes.as_slice()))
        .collect();
    let plan = plan::compile_config(config, &inputs, free_port()?)?;
    say!(
        "Конфиг xray собран: узлов {}, пропущено (xray не поддерживает): {}",
        plan.nodes,
        plan.skipped
    );
    store.write_file(CHECK_CONFIG, &plan.json)?;
    let path = store.path(CHECK_CONFIG);
    let tested = xray::test_config(&config.xray.path, &path);
    let _ = fs::remove_file(&path);
    tested?;
    say!("xray run -test: конфиг принят");
    Ok(())
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
    say!("Подписка «{name}»: {}", subscription.masked_url());
    let mut send = |url: &Url| -> Result<Response> {
        let headers = source.headers(url);
        say!("Запрос GET {}", redact(url));
        for (key, value) in &headers {
            say!("  {key}: {value}");
        }
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
            say!("  срок подписки: до {}", format_date(usage.expire));
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
