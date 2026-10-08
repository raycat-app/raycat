//! Узлы подписок и настройки → конфиг xray.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result};
use raycat_config::{Config, Mode, ProxyAuth, Subscription};
use raycat_netfilter::{DEFAULT_OWN_MARK, Lan, Rules};
use raycat_subscription::{Routing, RoutingProfile};
use raycat_xray::{Credentials, Node, Settings, TagTable, compile};

use crate::util::fnv1a;

/// Правила перехвата для режима шлюза; `None` в режиме прокси. Для шлюза локальной
/// сети здесь определяются интерфейс и подсети, которых нет в настройках.
pub(crate) fn gateway_rules(config: &Config) -> Result<Option<Rules>> {
    match config.mode {
        Mode::Proxy { .. } => Ok(None),
        Mode::Gateway { kill_switch, lan } => {
            let lan = if lan {
                let lan = raycat_netfilter::resolve_lan(
                    config.lan.interface.as_deref(),
                    &config.lan.subnets,
                )
                .context("не удалось определить сеть устройств шлюза")?;
                Some(lan)
            } else {
                None
            };
            let rules = Rules {
                kill_switch,
                lan,
                ..Rules::default()
            };
            rules.validate()?;
            Ok(Some(rules))
        }
    }
}

/// Режим для строки лога и вывода `check`.
pub(crate) fn describe_mode(config: &Config) -> String {
    match config.mode {
        Mode::Proxy { listen } => format!(
            "режим прокси, адрес {listen}{}",
            proxy_auth_note(&config.proxy_auth)
        ),
        Mode::Gateway { kill_switch, lan } => format!(
            "режим шлюза, kill switch {}{}",
            if kill_switch {
                "включён"
            } else {
                "выключен"
            },
            if lan {
                ", для устройств локальной сети"
            } else {
                ""
            }
        ),
    }
}

/// Что перехватывается в локальной сети: для лога и вывода `check`.
pub(crate) fn describe_lan(lan: &Lan) -> String {
    let subnets: Vec<String> = lan.subnets.iter().map(ToString::to_string).collect();
    format!(
        "локальная сеть: интерфейс {}, перехватываются устройства из подсетей {}",
        lan.interface,
        subnets.join(", ")
    )
}

/// Метка собственных сокетов демона: в режиме шлюза kill switch выпускает наружу
/// только помеченные пакеты.
pub(crate) fn own_mark(config: &Config) -> Option<u32> {
    match config.mode {
        Mode::Proxy { .. } => None,
        Mode::Gateway { .. } => Some(DEFAULT_OWN_MARK),
    }
}

fn proxy_auth_note(auth: &ProxyAuth) -> &'static str {
    match auth {
        ProxyAuth::NotSet => "",
        ProxyAuth::Off => ", без пароля",
        ProxyAuth::Password { .. } => ", вход по логину и паролю",
    }
}

fn credentials(auth: &ProxyAuth) -> Option<Credentials> {
    match auth {
        ProxyAuth::Password { user, password } => Some(Credentials {
            user: user.clone(),
            password: password.expose().to_owned(),
        }),
        ProxyAuth::NotSet | ProxyAuth::Off => None,
    }
}

fn xray_mode(config: &Config) -> raycat_xray::Mode {
    match config.mode {
        Mode::Proxy { listen } => raycat_xray::Mode::Proxy {
            listen,
            auth: credentials(&config.proxy_auth),
        },
        Mode::Gateway { .. } => {
            let rules = Rules::default();
            raycat_xray::Mode::Gateway {
                tproxy_port: rules.tproxy_port,
                mark: rules.own_mark,
            }
        }
    }
}

/// Свои правила по порядку, затем пресет «Россия напрямую», затем правила провайдера.
fn routing_rules(
    routing: &raycat_config::Routing,
    provider: &[raycat_xray::Rule],
) -> Vec<raycat_xray::Rule> {
    let mut rules: Vec<raycat_xray::Rule> = routing.rules.iter().map(xray_rule).collect();
    if routing.ru_direct {
        rules.extend(ru_direct());
    }
    rules.extend_from_slice(provider);
    rules
}

fn xray_rule(rule: &raycat_config::Rule) -> raycat_xray::Rule {
    raycat_xray::Rule {
        domains: rule
            .domains
            .iter()
            .map(|domain| raycat_xray::Domain {
                name: domain.name.clone(),
                kind: if domain.subdomains {
                    raycat_xray::DomainKind::Subdomains
                } else {
                    raycat_xray::DomainKind::Full
                },
            })
            .collect(),
        subnets: rule
            .ips
            .iter()
            .map(|cidr| raycat_xray::Subnet {
                addr: cidr.addr(),
                prefix: cidr.prefix(),
            })
            .collect(),
        action: match rule.action {
            raycat_config::Action::Direct => raycat_xray::Action::Direct,
            raycat_config::Action::Proxy => raycat_xray::Action::Proxy,
            raycat_config::Action::Block => raycat_xray::Action::Block,
        },
    }
}

/// Пресет «Россия напрямую»: домены зон РФ и российские подсети IPv4 мимо VPN.
fn ru_direct() -> Vec<raycat_xray::Rule> {
    vec![
        raycat_xray::Rule {
            domains: ru_zone_domains().collect(),
            subnets: Vec::new(),
            action: raycat_xray::Action::Direct,
        },
        raycat_xray::Rule {
            domains: Vec::new(),
            subnets: ru_subnets().collect(),
            action: raycat_xray::Action::Direct,
        },
    ]
}

fn ru_zone_domains() -> impl Iterator<Item = raycat_xray::Domain> {
    raycat_routing::ru_zones()
        .iter()
        .map(|zone| raycat_xray::Domain {
            name: (*zone).to_owned(),
            kind: raycat_xray::DomainKind::Subdomains,
        })
}

fn ru_subnets() -> impl Iterator<Item = raycat_xray::Subnet> {
    raycat_routing::ru_ipv4().map(|(addr, prefix)| raycat_xray::Subnet {
        addr: IpAddr::V4(addr),
        prefix,
    })
}

/// Профиль провайдера, который применяется: первый в порядке подписок, у которой он
/// есть. `None`, если `routing.provider` выключен или профиля ни у кого нет.
pub(crate) fn provider_profile<'a>(
    config: &Config,
    routings: impl IntoIterator<Item = Option<&'a Routing>>,
) -> Option<&'a RoutingProfile> {
    if !config.routing.provider {
        return None;
    }
    routings
        .into_iter()
        .flatten()
        .find_map(|routing| match routing {
            Routing::Profile(profile) => Some(&**profile),
            Routing::Off => None,
        })
}

/// Что из профиля провайдера применено и что пропущено; для журнала и `raycat check`.
pub(crate) struct ProviderRouting {
    pub(crate) name: String,
    /// Записей списков, которые перевели в правила.
    pub(crate) applied: usize,
    pub(crate) skipped: Vec<Skipped>,
    /// Отпечаток правил и отчёта: по нему демон замечает, что профиль изменился.
    pub(crate) digest: u64,
}

/// Запись профиля, которую не удалось перевести в правила.
pub(crate) struct Skipped {
    pub(crate) entry: String,
    pub(crate) reason: Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Skip {
    /// Категории geosite и geoip, кроме ru и private, и файлы `ext:`: геоданных нет.
    Geo,
    /// Регулярные выражения: xray проверяет их только при запуске, и ошибка уронила бы
    /// весь конфиг.
    Regexp,
    /// `geoip:private` вне списка напрямую: частные сети всегда идут напрямую.
    Private,
    /// Запись не похожа ни на домен, ни на адрес, или слишком длинная.
    Invalid,
}

impl Skip {
    fn describe(self) -> &'static str {
        match self {
            Self::Geo => "категории geosite/geoip, кроме ru и private, и ext: не поддерживаются",
            Self::Regexp => "regexp: не поддерживаются",
            Self::Private => {
                "geoip:private только в списке напрямую, частные сети и так идут напрямую"
            }
            Self::Invalid => "некорректные записи",
        }
    }
}

const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;
/// Сколько пропущенных записей называть в предупреждении и сколько символов в каждой.
const NOTE_EXAMPLES: usize = 5;
const EXAMPLE_CHARS: usize = 64;

/// Переводит профиль провайдера в правила xray: сначала блокировка, затем напрямую,
/// затем через VPN. Глобальный прокси из профиля правил не даёт.
pub(crate) fn translate(profile: &RoutingProfile) -> (ProviderRouting, Vec<raycat_xray::Rule>) {
    let (mut report, rules) = translate_lists(profile);
    report.digest = fnv1a(format!("{}\n{rules:?}", provider_note(&report)).as_bytes());
    (report, rules)
}

fn translate_lists(profile: &RoutingProfile) -> (ProviderRouting, Vec<raycat_xray::Rule>) {
    let mut report = ProviderRouting {
        name: profile.name.clone(),
        applied: 0,
        skipped: Vec::new(),
        digest: 0,
    };
    let mut rules = Vec::new();
    if profile.global_proxy {
        return (report, rules);
    }
    let lists = [
        (
            raycat_xray::Action::Block,
            &profile.block_sites,
            &profile.block_ip,
        ),
        (
            raycat_xray::Action::Direct,
            &profile.direct_sites,
            &profile.direct_ip,
        ),
        (
            raycat_xray::Action::Proxy,
            &profile.proxy_sites,
            &profile.proxy_ip,
        ),
    ];
    for (action, sites, ips) in lists {
        let mut pieces = Pieces::default();
        for entry in sites.iter().chain(ips) {
            match translate_entry(entry, action, &mut pieces) {
                Ok(()) => report.applied += 1,
                Err(reason) => report.skipped.push(Skipped {
                    entry: entry.clone(),
                    reason,
                }),
            }
        }
        if !pieces.domains.is_empty() || !pieces.subnets.is_empty() {
            rules.push(raycat_xray::Rule {
                domains: pieces.domains,
                subnets: pieces.subnets,
                action,
            });
        }
    }
    (report, rules)
}

#[derive(Default)]
struct Pieces {
    domains: Vec<raycat_xray::Domain>,
    subnets: Vec<raycat_xray::Subnet>,
}

/// Одна запись списка провайдера. Геоданные ru переводятся в зоны и подсети, private
/// в списке напрямую ничего не добавляет: компилятор и так пускает частные сети мимо VPN.
fn translate_entry(
    entry: &str,
    action: raycat_xray::Action,
    pieces: &mut Pieces,
) -> Result<(), Skip> {
    let entry = entry.trim().to_ascii_lowercase();
    if let Some(tag) = entry.strip_prefix("geosite:") {
        return match tag {
            "ru" | "category-ru" => {
                pieces.domains.extend(ru_zone_domains());
                Ok(())
            }
            _ => Err(Skip::Geo),
        };
    }
    if let Some(tag) = entry.strip_prefix("geoip:") {
        return match tag {
            "ru" => {
                pieces.subnets.extend(ru_subnets());
                Ok(())
            }
            "private" if action == raycat_xray::Action::Direct => Ok(()),
            "private" => Err(Skip::Private),
            _ => Err(Skip::Geo),
        };
    }
    if entry.starts_with("ext:") {
        return Err(Skip::Geo);
    }
    if entry.starts_with("regexp:") {
        return Err(Skip::Regexp);
    }
    if let Some(subnet) = subnet(&entry) {
        pieces.subnets.push(subnet);
        return Ok(());
    }
    let (kind, name) = if let Some(name) = entry.strip_prefix("keyword:") {
        (raycat_xray::DomainKind::Keyword, name)
    } else if let Some(name) = entry.strip_prefix("full:") {
        (raycat_xray::DomainKind::Full, name)
    } else if let Some(name) = entry.strip_prefix("domain:") {
        (raycat_xray::DomainKind::Subdomains, name)
    } else {
        (raycat_xray::DomainKind::Subdomains, entry.as_str())
    };
    let name = if kind == raycat_xray::DomainKind::Keyword {
        name
    } else {
        name.strip_prefix("*.").unwrap_or(name)
    };
    if !valid_name(name) {
        return Err(Skip::Invalid);
    }
    pieces.domains.push(raycat_xray::Domain {
        name: name.to_owned(),
        kind,
    });
    Ok(())
}

/// Имя из латинских букв, цифр, дефисов и подчёркиваний, разделённых точками. Кириллица
/// не проходит: такие домены провайдер должен присылать в punycode.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && name.split('.').all(valid_label)
}

fn valid_label(label: &str) -> bool {
    (1..=MAX_LABEL).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// IP-адрес или подсеть. Без длины префикса адрес одиночный; биты справа от префикса
/// обнуляются.
fn subnet(text: &str) -> Option<raycat_xray::Subnet> {
    let (addr, prefix) = match text.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (text, None),
    };
    let addr: IpAddr = addr.parse().ok()?;
    let width: u8 = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix.parse::<u8>().ok().filter(|len| *len <= width)?,
        None => width,
    };
    Some(raycat_xray::Subnet {
        addr: network(addr, prefix),
        prefix,
    })
}

fn network(addr: IpAddr, prefix: u8) -> IpAddr {
    match addr {
        IpAddr::V4(addr) => {
            let mask = u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0);
            IpAddr::V4(Ipv4Addr::from(u32::from(addr) & mask))
        }
        IpAddr::V6(addr) => {
            let mask = u128::MAX.checked_shl(u32::from(128 - prefix)).unwrap_or(0);
            IpAddr::V6(Ipv6Addr::from(u128::from(addr) & mask))
        }
    }
}

/// Строка для журнала демона: что применено и что пропущено, с примерами пропущенного.
pub(crate) fn provider_note(provider: &ProviderRouting) -> String {
    let head = format!(
        "маршрутизация провайдера «{}»: применено правил {}, пропущено {}",
        title(&provider.name),
        provider.applied,
        provider.skipped.len()
    );
    if provider.skipped.is_empty() {
        return head;
    }
    let reasons: Vec<&str> = [Skip::Geo, Skip::Regexp, Skip::Private, Skip::Invalid]
        .into_iter()
        .filter(|reason| {
            provider
                .skipped
                .iter()
                .any(|skipped| skipped.reason == *reason)
        })
        .map(Skip::describe)
        .collect();
    let examples: Vec<String> = provider
        .skipped
        .iter()
        .take(NOTE_EXAMPLES)
        .map(|skipped| short(&skipped.entry))
        .collect();
    let more = provider.skipped.len().saturating_sub(NOTE_EXAMPLES);
    let tail = if more > 0 {
        format!(" и ещё {more}")
    } else {
        String::new()
    };
    format!(
        "{head} ({}: {}{tail})",
        reasons.join("; "),
        examples.join(", ")
    )
}

/// Итог для строки маршрутизации в `raycat check`.
pub(crate) fn provider_summary(provider: &ProviderRouting) -> String {
    format!(
        "провайдер: «{}», правил {}, пропущено {}",
        title(&provider.name),
        provider.applied,
        provider.skipped.len()
    )
}

fn title(name: &str) -> String {
    if name.trim().is_empty() {
        "без названия".to_owned()
    } else {
        short(name)
    }
}

fn short(text: &str) -> String {
    text.chars().take(EXAMPLE_CHARS).collect()
}

/// Тег балансировщика в конфиге xray: в нём демон закрепляет выбранный узел.
pub(crate) const BALANCER: &str = "auto";

pub(crate) struct Plan {
    pub(crate) json: Vec<u8>,
    /// Узлов в конфиге и узлов, которые xray не поддерживает.
    pub(crate) nodes: usize,
    pub(crate) skipped: usize,
    pub(crate) tags: TagTable,
    /// В конфиге есть узлы на UDP-транспортах (QUIC и подобные).
    pub(crate) quic: bool,
    /// Применённый профиль провайдера; `None`, если профиля нет или он выключен.
    pub(crate) provider: Option<ProviderRouting>,
}

/// Собирает конфиг из узлов подписок в порядке настроек (это их приоритет), оставив
/// узлы, которые пропускают `allow` и `deny` своей подписки. `tcp_congestion` —
/// уже выбранный алгоритм (см. `tuning`), а не настройка из файла. Третий элемент входа —
/// профиль маршрутизации из последнего ответа подписки.
pub(crate) fn compile_config(
    config: &Config,
    inputs: &[(&Subscription, &[Node], Option<&Routing>)],
    api_port: u16,
    tcp_congestion: Option<&str>,
) -> Result<Plan> {
    let mode = xray_mode(config);
    let subscriptions: Vec<raycat_xray::Subscription> = inputs
        .iter()
        .map(|(subscription, nodes, _)| raycat_xray::Subscription {
            id: subscription.name.clone(),
            nodes: nodes
                .iter()
                .filter(|node| subscription.allows(&node.name))
                .cloned()
                .collect(),
        })
        .collect();
    let (provider, provider_rules) =
        match provider_profile(config, inputs.iter().map(|input| input.2)) {
            Some(profile) => {
                let (provider, rules) = translate(profile);
                (Some(provider), rules)
            }
            None => (None, Vec::new()),
        };
    let mut settings = Settings::new(mode, api_port);
    settings.dns.resolvers.clone_from(&config.dns.resolvers);
    settings.probe.url.clone_from(&config.selection.check_url);
    settings.probe.interval = config.selection.check_interval;
    settings.tcp_congestion = tcp_congestion.map(str::to_owned);
    settings.xhttp_connections = config.xray.xhttp_connections;
    settings.rules = routing_rules(&config.routing, &provider_rules);
    let compiled = compile(&subscriptions, &settings)?;
    let json = serde_json::to_vec(&compiled.config).context("не удалось записать конфиг xray")?;
    Ok(Plan {
        json,
        nodes: compiled.tags.entries().len(),
        skipped: compiled.skipped.len(),
        tags: compiled.tags,
        quic: compiled.quic,
        provider,
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
        compile_config(
            config,
            &[(&config.subscriptions[0], nodes, None)],
            10_085,
            None,
        )
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
    fn proxy_mode_needs_no_rules_and_no_mark() {
        let config = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        assert_eq!(gateway_rules(&config).unwrap(), None);
        assert_eq!(own_mark(&config), None);
    }

    #[test]
    fn gateway_mode_takes_the_kill_switch_from_the_settings() {
        let on = config("type = \"gateway\"", "");
        let rules = gateway_rules(&on).unwrap().unwrap();
        assert!(rules.kill_switch);
        assert!(!rules.intercept_ipv6);
        assert_eq!(own_mark(&on), Some(rules.own_mark));

        let off = config("type = \"gateway\"\nkill_switch = false", "");
        assert!(!gateway_rules(&off).unwrap().unwrap().kill_switch);
    }

    #[test]
    fn the_mode_is_described_for_people() {
        let proxy = config("type = \"proxy\"\nlisten = \"127.0.0.1:7891\"", "");
        assert_eq!(describe_mode(&proxy), "режим прокси, адрес 127.0.0.1:7891");
        let gateway = config("type = \"gateway\"", "");
        assert_eq!(describe_mode(&gateway), "режим шлюза, kill switch включён");
        let open = config("type = \"gateway\"\nkill_switch = false", "");
        assert_eq!(describe_mode(&open), "режим шлюза, kill switch выключен");
    }

    #[test]
    fn the_proxy_password_reaches_xray_but_not_the_description() {
        let config = config(
            "type = \"proxy\"\nlisten = \"127.0.0.1:7891\"\nauth = \"alice:s3cret-pass\"",
            "",
        );
        let description = describe_mode(&config);
        assert_eq!(
            description,
            "режим прокси, адрес 127.0.0.1:7891, вход по логину и паролю"
        );
        let plan = plan(&config, &nodes()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["inbounds"][0]["settings"]["auth"], "password");
        assert_eq!(
            json["inbounds"][0]["settings"]["accounts"],
            serde_json::json!([{"user": "alice", "pass": "s3cret-pass"}])
        );
    }

    #[test]
    fn a_lan_gateway_takes_the_interface_and_subnets_from_the_settings() {
        let config = config(
            "type = \"gateway\"\nlan = true\nlan_interface = \"lo\"\nlan_subnets = [\"10.77.0.0/24\"]",
            "",
        );
        let rules = gateway_rules(&config).unwrap().unwrap();
        assert!(rules.kill_switch);
        let lan = rules.lan.unwrap();
        assert_eq!(lan.interface, "lo");
        assert_eq!(lan.subnets.len(), 1);
        assert_eq!(
            describe_mode(&config),
            "режим шлюза, kill switch включён, для устройств локальной сети"
        );
        assert_eq!(
            describe_lan(&lan),
            "локальная сеть: интерфейс lo, перехватываются устройства из подсетей 10.77.0.0/24"
        );
        assert!(plan(&config, &nodes()).is_ok());
    }

    #[test]
    fn a_missing_lan_interface_stops_the_gateway() {
        let config = config(
            "type = \"gateway\"\nlan = true\nlan_interface = \"raycat-nope0\"\nlan_subnets = [\"10.77.0.0/24\"]",
            "",
        );
        let error = format!("{:#}", gateway_rules(&config).unwrap_err());
        assert!(error.contains("raycat-nope0"), "{error}");
        assert!(error.contains("нет в системе"), "{error}");
    }

    #[test]
    fn a_gateway_without_lan_has_no_lan_rules() {
        let config = config("type = \"gateway\"", "");
        assert_eq!(gateway_rules(&config).unwrap().unwrap().lan, None);
    }

    #[test]
    fn the_gateway_config_has_a_tproxy_inbound_and_marked_sockets() {
        let config = config("type = \"gateway\"", "");
        let plan = plan(&config, &nodes()).unwrap();
        let rules = gateway_rules(&config).unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["inbounds"][0]["tag"], "tproxy-in");
        assert_eq!(json["inbounds"][0]["port"], rules.tproxy_port);
        let text = String::from_utf8(plan.json).unwrap();
        assert!(text.contains(&format!("\"mark\":{}", rules.own_mark)));
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
    fn the_balancer_tag_matches_the_constant() {
        let config = config("type = \"proxy\"", "");
        let plan = plan(&config, &nodes()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&plan.json).unwrap();
        assert_eq!(json["routing"]["balancers"][0]["tag"], BALANCER);
        assert_eq!(plan.tags.entries().len(), 2);
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
            (&config.subscriptions[0], &all[1..], None),
            (&config.subscriptions[1], &all[..1], None),
        ];
        let plan = compile_config(&config, &inputs, 10_085, None).unwrap();
        assert_eq!(plan.nodes, 2);
    }

    fn xhttp_node() -> Node {
        Node {
            name: "xhttp".to_owned(),
            outbounds: vec![serde_json::json!({
                "tag": "proxy",
                "protocol": "vless",
                "settings": {"vnext": [{
                    "address": "203.0.113.9",
                    "port": 443,
                    "users": [{"id": "00000000-0000-0000-0000-000000000000", "encryption": "none"}]
                }]},
                "streamSettings": {"network": "xhttp", "security": "tls"}
            })],
        }
    }

    fn json_of(config: &Config, tcp_congestion: Option<&str>) -> serde_json::Value {
        let nodes = [xhttp_node()];
        let plan = compile_config(
            config,
            &[(&config.subscriptions[0], nodes.as_slice(), None)],
            10_085,
            tcp_congestion,
        )
        .unwrap();
        serde_json::from_slice(&plan.json).unwrap()
    }

    #[test]
    fn performance_settings_reach_the_compiler() {
        let config = config("type = \"proxy\"\n[xray]\nxhttp_connections = 6", "");
        let json = json_of(&config, Some("bbr"));
        let stream = &json["outbounds"][0]["streamSettings"];

        assert_eq!(stream["sockopt"]["tcpCongestion"], "bbr");
        assert_eq!(stream["xhttpSettings"]["xmux"]["maxConnections"], 6);
        assert_eq!(json["log"]["access"], "none");
    }

    #[test]
    fn without_settings_nothing_is_tuned() {
        let config = config("type = \"proxy\"", "");
        let json = json_of(&config, None);
        let text = serde_json::to_string(&json).unwrap();

        assert!(!text.contains("tcpCongestion"));
        assert!(!text.contains("xmux"));
    }

    #[test]
    fn plain_nodes_are_not_quic() {
        let config = config("type = \"proxy\"", "");
        assert!(!plan(&config, &nodes()).unwrap().quic);
    }

    fn with_routing(routing: &str) -> Config {
        let text = format!(
            "[[subscription]]\nname = \"a\"\nurl = \"https://a.example.com/x/abcd\"\napp = \"happ\"\nplatform = \"windows\"\n[mode]\ntype = \"proxy\"\nlisten = \"127.0.0.1:7891\"\n{routing}"
        );
        Config::from_toml_str(&text, &Env::new()).unwrap()
    }

    fn routing_json(config: &Config) -> serde_json::Value {
        let plan = plan(config, &nodes()).unwrap();
        serde_json::from_slice(&plan.json).unwrap()
    }

    #[test]
    fn own_rules_reach_the_compiler_in_order() {
        let config = with_routing(
            "[[routing.rule]]\ndomains = [\"example.ru\", \"*.bank.example\"]\naction = \"direct\"\n[[routing.rule]]\nips = [\"203.0.113.0/24\"]\naction = \"block\"\n",
        );
        let json = routing_json(&config);
        let rules = json["routing"]["rules"].as_array().unwrap();

        assert_eq!(rules.len(), 6);
        assert_eq!(
            rules[3],
            serde_json::json!({"type": "field", "domain": ["full:example.ru", "domain:bank.example"], "outboundTag": "direct"})
        );
        assert_eq!(
            rules[4],
            serde_json::json!({"type": "field", "ip": ["203.0.113.0/24"], "outboundTag": "block"})
        );
        assert_eq!(rules[5]["balancerTag"], BALANCER);
        assert_eq!(json["inbounds"][0]["sniffing"]["routeOnly"], true);
    }

    #[test]
    fn proxy_rules_point_at_the_balancer() {
        let config =
            with_routing("[[routing.rule]]\nips = [\"203.0.113.0/24\"]\naction = \"proxy\"\n");
        let json = routing_json(&config);

        assert_eq!(
            json["routing"]["rules"][3],
            serde_json::json!({"type": "field", "ip": ["203.0.113.0/24"], "balancerTag": BALANCER})
        );
    }

    #[test]
    fn without_rules_nothing_changes_for_routing_or_sniffing() {
        let config = config("type = \"proxy\"", "");
        let json = routing_json(&config);

        assert_eq!(json["routing"]["rules"].as_array().unwrap().len(), 4);
        assert!(json["inbounds"][0]["sniffing"].get("routeOnly").is_none());
    }

    #[test]
    fn ru_direct_adds_the_zones_and_the_subnets_after_own_rules() {
        let config = with_routing(
            "[routing]\nru_direct = true\n[[routing.rule]]\ndomains = [\"example.net\"]\naction = \"block\"\n",
        );
        let json = routing_json(&config);
        let rules = json["routing"]["rules"].as_array().unwrap();

        assert_eq!(rules.len(), 7);
        assert_eq!(rules[3]["outboundTag"], "block");
        let zones = rules[4]["domain"].as_array().unwrap();
        assert_eq!(zones.len(), raycat_routing::ru_zones().len());
        assert!(zones.contains(&serde_json::json!("domain:ru")));
        assert_eq!(rules[4]["outboundTag"], "direct");
        assert_eq!(
            rules[5]["ip"].as_array().unwrap().len(),
            raycat_routing::ru_ipv4().count()
        );
        assert_eq!(rules[5]["outboundTag"], "direct");
    }

    #[test]
    fn ru_direct_zones_go_to_the_real_resolver() {
        let config = with_routing("[routing]\nru_direct = true\n");
        let json = routing_json(&config);
        let server = &json["dns"]["servers"][0];

        assert_eq!(server["address"], "1.1.1.1");
        assert!(
            server["domains"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("domain:ru"))
        );
        assert_eq!(json["dns"]["servers"][1], "fakedns");
    }

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn dom(name: &str, kind: raycat_xray::DomainKind) -> raycat_xray::Domain {
        raycat_xray::Domain {
            name: name.to_owned(),
            kind,
        }
    }

    fn provider_config(extra: &str) -> Config {
        with_routing(&format!("[routing]\nprovider = true\n{extra}"))
    }

    fn provider_json(config: &Config, profile: &RoutingProfile) -> serde_json::Value {
        let nodes = nodes();
        let routing = Routing::Profile(Box::new(profile.clone()));
        let plan = compile_config(
            config,
            &[(&config.subscriptions[0], nodes.as_slice(), Some(&routing))],
            10_085,
            None,
        )
        .unwrap();
        serde_json::from_slice(&plan.json).unwrap()
    }

    #[test]
    fn provider_names_and_addresses_become_rules() {
        let profile = RoutingProfile {
            direct_sites: list(&[
                "domain:Example.RU",
                "full:exact.example.org",
                "bank.example",
                "*.shop.example",
                "keyword:shop-words",
            ]),
            direct_ip: list(&["10.20.5.7/16", "2001:db8::1/32", "192.0.2.9"]),
            ..RoutingProfile::default()
        };
        let (provider, rules) = translate(&profile);

        assert_eq!(provider.applied, 8);
        assert_eq!(provider.skipped.len(), 0);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, raycat_xray::Action::Direct);
        assert_eq!(
            rules[0].domains,
            vec![
                dom("example.ru", raycat_xray::DomainKind::Subdomains),
                dom("exact.example.org", raycat_xray::DomainKind::Full),
                dom("bank.example", raycat_xray::DomainKind::Subdomains),
                dom("shop.example", raycat_xray::DomainKind::Subdomains),
                dom("shop-words", raycat_xray::DomainKind::Keyword),
            ]
        );
        assert_eq!(
            rules[0].subnets,
            vec![
                raycat_xray::Subnet {
                    addr: "10.20.0.0".parse().unwrap(),
                    prefix: 16,
                },
                raycat_xray::Subnet {
                    addr: "2001:db8::".parse().unwrap(),
                    prefix: 32,
                },
                raycat_xray::Subnet {
                    addr: "192.0.2.9".parse().unwrap(),
                    prefix: 32,
                },
            ]
        );
    }

    #[test]
    fn russian_geodata_comes_from_the_bundled_zones_and_subnets() {
        let profile = RoutingProfile {
            proxy_sites: list(&["geosite:category-ru"]),
            proxy_ip: list(&["geoip:ru"]),
            direct_ip: list(&["geoip:private"]),
            ..RoutingProfile::default()
        };
        let (provider, rules) = translate(&profile);

        assert_eq!(provider.applied, 3);
        assert_eq!(provider.skipped.len(), 0);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, raycat_xray::Action::Proxy);
        assert_eq!(rules[0].domains.len(), raycat_routing::ru_zones().len());
        assert_eq!(rules[0].subnets.len(), raycat_routing::ru_ipv4().count());
    }

    #[test]
    fn unsupported_and_broken_entries_are_skipped_with_a_reason() {
        let long = format!("{}.example", "a".repeat(64));
        let profile = RoutingProfile {
            block_sites: list(&[
                "geosite:category-ads",
                "ext:geo.dat:tag",
                "regexp:^a.*",
                "plain:x",
                "bad host!",
                "example..ru",
                "пример.рф",
                long.as_str(),
            ]),
            block_ip: list(&[
                "geoip:us",
                "geoip:private",
                "203.0.113.0/33",
                "203.0.113.5/24",
            ]),
            ..RoutingProfile::default()
        };
        let (provider, rules) = translate(&profile);

        assert_eq!(provider.applied, 1);
        let skipped: Vec<(&str, Skip)> = provider
            .skipped
            .iter()
            .map(|skipped| (skipped.entry.as_str(), skipped.reason))
            .collect();
        assert_eq!(
            skipped,
            vec![
                ("geosite:category-ads", Skip::Geo),
                ("ext:geo.dat:tag", Skip::Geo),
                ("regexp:^a.*", Skip::Regexp),
                ("plain:x", Skip::Invalid),
                ("bad host!", Skip::Invalid),
                ("example..ru", Skip::Invalid),
                ("пример.рф", Skip::Invalid),
                (long.as_str(), Skip::Invalid),
                ("geoip:us", Skip::Geo),
                ("geoip:private", Skip::Private),
                ("203.0.113.0/33", Skip::Invalid),
            ]
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].domains, Vec::<raycat_xray::Domain>::new());
        assert_eq!(rules[0].subnets.len(), 1);
    }

    #[test]
    fn the_note_names_the_profile_and_the_reasons() {
        let profile = RoutingProfile {
            name: "Тест".to_owned(),
            block_sites: list(&["geosite:category-ads", "regexp:^a"]),
            direct_sites: list(&["example.ru"]),
            ..RoutingProfile::default()
        };
        let (provider, _) = translate(&profile);

        assert_eq!(
            provider_note(&provider),
            "маршрутизация провайдера «Тест»: применено правил 1, пропущено 2 (категории geosite/geoip, кроме ru и private, и ext: не поддерживаются; regexp: не поддерживаются: geosite:category-ads, regexp:^a)"
        );
        assert_eq!(
            provider_summary(&provider),
            "провайдер: «Тест», правил 1, пропущено 2"
        );
    }

    #[test]
    fn the_note_lists_five_examples_and_counts_the_rest() {
        let names: Vec<String> = (0..7).map(|i| format!("geosite:c{i}")).collect();
        let profile = RoutingProfile {
            block_sites: names,
            ..RoutingProfile::default()
        };
        let (provider, _) = translate(&profile);

        assert_eq!(
            provider_note(&provider),
            "маршрутизация провайдера «без названия»: применено правил 0, пропущено 7 (категории geosite/geoip, кроме ru и private, и ext: не поддерживаются: geosite:c0, geosite:c1, geosite:c2, geosite:c3, geosite:c4 и ещё 2)"
        );
    }

    #[test]
    fn a_profile_without_skips_has_a_note_without_reasons() {
        let profile = RoutingProfile {
            name: "Тест".to_owned(),
            direct_sites: list(&["example.ru"]),
            ..RoutingProfile::default()
        };
        let (provider, _) = translate(&profile);

        assert_eq!(
            provider_note(&provider),
            "маршрутизация провайдера «Тест»: применено правил 1, пропущено 0"
        );
    }

    #[test]
    fn global_proxy_and_switched_off_routing_give_no_rules() {
        let config = provider_config("");
        let profile = RoutingProfile {
            global_proxy: true,
            direct_sites: list(&["example.ru"]),
            ..RoutingProfile::default()
        };
        let (provider, rules) = translate(&profile);

        assert_eq!(provider.applied, 0);
        assert_eq!(rules, Vec::<raycat_xray::Rule>::new());
        let json = provider_json(&config, &profile);
        assert_eq!(json["routing"]["rules"].as_array().unwrap().len(), 4);

        let off = Routing::Off;
        assert!(provider_profile(&config, [Some(&off)]).is_none());
    }

    #[test]
    fn without_the_switch_the_profile_is_not_applied() {
        let config = with_routing("");
        let profile = RoutingProfile {
            direct_sites: list(&["example.ru"]),
            ..RoutingProfile::default()
        };
        let routing = Routing::Profile(Box::new(profile.clone()));
        assert!(provider_profile(&config, [Some(&routing)]).is_none());
        let json = provider_json(&config, &profile);
        assert_eq!(json["routing"]["rules"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn the_first_subscription_with_a_profile_wins() {
        let config = provider_config("");
        let named = |name: &str| {
            Routing::Profile(Box::new(RoutingProfile {
                name: name.to_owned(),
                ..RoutingProfile::default()
            }))
        };
        let second = named("второй");
        let off = Routing::Off;
        let found = provider_profile(&config, [None, Some(&off), Some(&second)]).unwrap();
        assert_eq!(found.name, "второй");
        let first = named("первый");
        let found = provider_profile(&config, [Some(&first), Some(&second)]).unwrap();
        assert_eq!(found.name, "первый");
    }

    #[test]
    fn provider_rules_come_after_own_rules_and_the_preset() {
        let config = provider_config(
            "ru_direct = true\n[[routing.rule]]\ndomains = [\"own.example\"]\naction = \"block\"\n",
        );
        let profile = RoutingProfile {
            block_sites: list(&["prov-block.example"]),
            direct_sites: list(&["prov-direct.example"]),
            proxy_sites: list(&["prov-proxy.example"]),
            ..RoutingProfile::default()
        };
        let json = provider_json(&config, &profile);
        let rules = json["routing"]["rules"].as_array().unwrap();

        assert_eq!(rules.len(), 10);
        assert_eq!(
            rules[3],
            serde_json::json!({"type": "field", "domain": ["full:own.example"], "outboundTag": "block"})
        );
        assert_eq!(rules[4]["outboundTag"], "direct");
        assert_eq!(rules[5]["outboundTag"], "direct");
        assert_eq!(
            rules[6],
            serde_json::json!({"type": "field", "domain": ["domain:prov-block.example"], "outboundTag": "block"})
        );
        assert_eq!(
            rules[7],
            serde_json::json!({"type": "field", "domain": ["domain:prov-direct.example"], "outboundTag": "direct"})
        );
        assert_eq!(
            rules[8],
            serde_json::json!({"type": "field", "domain": ["domain:prov-proxy.example"], "balancerTag": BALANCER})
        );
        assert_eq!(rules[9]["network"], "tcp,udp");
    }

    #[test]
    fn provider_direct_domains_resolve_through_the_real_resolver() {
        let config = provider_config("");
        let profile = RoutingProfile {
            block_sites: list(&["blocked.example"]),
            direct_sites: list(&["direct.example", "keyword:shop"]),
            ..RoutingProfile::default()
        };
        let json = provider_json(&config, &profile);
        let domains = json["dns"]["servers"][0]["domains"].as_array().unwrap();

        assert!(domains.contains(&serde_json::json!("domain:direct.example")));
        assert!(domains.contains(&serde_json::json!("keyword:shop")));
        assert!(!domains.contains(&serde_json::json!("domain:blocked.example")));
    }
}
