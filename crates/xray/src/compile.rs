use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::net::IpAddr;

use serde_json::{Value, json};

use crate::nodes::compile_node;
use crate::tuning::{self, Tuning};
use crate::{Action, Domain, DomainKind, Mode, Node, Rule, Settings, SpeedtestInbound, Subnet};

const PRIVATE_NETWORKS: [&str; 7] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "fc00::/7",
    "fe80::/10",
    "127.0.0.0/8",
];

const API_SERVICES: [&str; 4] = [
    "HandlerService",
    "RoutingService",
    "ObservatoryService",
    "StatsService",
];

const BALANCER_TAG: &str = "auto";
const SPEEDTEST_TAG: &str = "speedtest-in";
const FAKE_IP_POOL_SIZE: u32 = 65_535;

/// Подписка: идентификатор и узлы. Порядок подписок в списке — их приоритет.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeEntry {
    /// Тег выхода узла в конфиге, `node-NNN-main`.
    pub tag: String,
    pub subscription: String,
    pub name: String,
}

/// Соответствие тегов конфига узлам подписок, в порядке приоритета.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagTable {
    entries: Vec<NodeEntry>,
}

impl TagTable {
    pub fn entries(&self) -> &[NodeEntry] {
        &self.entries
    }

    pub fn get(&self, tag: &str) -> Option<&NodeEntry> {
        self.entries.iter().find(|entry| entry.tag == tag)
    }
}

/// Узел, который не попал в конфиг.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedNode {
    pub subscription: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub config: Value,
    pub tags: TagTable,
    pub skipped: Vec<SkippedNode>,
    /// В конфиге есть узлы на UDP-транспортах (hysteria, XHTTP поверх h3, mKCP).
    pub quic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileError {
    /// Ни одного пригодного узла: конфигу нечем выходить в сеть.
    NoNodes,
    NoResolvers,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNodes => f.write_str("в подписках нет ни одного пригодного узла"),
            Self::NoResolvers => f.write_str("не задан ни один DNS-резолвер"),
        }
    }
}

impl Error for CompileError {}

/// Собирает один конфиг xray из всех подписок; одинаковый вход даёт одинаковый JSON.
pub fn compile(
    subscriptions: &[Subscription],
    settings: &Settings,
) -> Result<Compiled, CompileError> {
    let Some(primary_resolver) = settings.dns.resolvers.first() else {
        return Err(CompileError::NoResolvers);
    };
    let mark = match settings.mode {
        Mode::Gateway { mark, .. } => Some(mark),
        Mode::Proxy { .. } => None,
    };

    let mut outbounds = Vec::new();
    let mut hosts: Vec<String> = Vec::new();
    let mut seen_hosts = HashSet::new();
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut quic = false;
    let tuning = Tuning {
        tcp_congestion: settings.tcp_congestion.as_deref(),
        xhttp_connections: settings.xhttp_connections,
    };
    for subscription in subscriptions {
        for node in &subscription.nodes {
            let Some(mut compiled) = compile_node(entries.len() + 1, node, mark) else {
                skipped.push(SkippedNode {
                    subscription: subscription.id.clone(),
                    name: node.name.clone(),
                });
                continue;
            };
            for outbound in &mut compiled.outbounds {
                if let Some(outbound) = outbound.as_object_mut() {
                    quic |= tuning::apply(outbound, &tuning);
                }
            }
            for host in compiled.hosts {
                if seen_hosts.insert(host.clone()) {
                    hosts.push(host);
                }
            }
            entries.push(NodeEntry {
                tag: compiled.main_tag,
                subscription: subscription.id.clone(),
                name: node.name.clone(),
            });
            outbounds.extend(compiled.outbounds);
        }
    }

    let mains: Vec<&str> = entries.iter().map(|entry| entry.tag.as_str()).collect();
    let Some(&fallback) = mains.first() else {
        return Err(CompileError::NoNodes);
    };

    // Узлы идут первыми: outbound по умолчанию в xray — первый в списке.
    outbounds.extend(service_outbounds(mark));

    let api_listen = format!("127.0.0.1:{}", settings.api_port);
    let direct = direct_domains(&settings.rules);
    let config = json!({
        "log": {"loglevel": "warning", "access": "none"},
        "api": {"tag": "api", "listen": api_listen, "services": API_SERVICES},
        "stats": {},
        "policy": {"system": {"statsOutboundUplink": true, "statsOutboundDownlink": true}},
        "dns": dns(&settings.dns.resolvers, *primary_resolver, &hosts, &direct),
        "fakedns": [{"ipPool": settings.dns.fake_ip_pool, "poolSize": FAKE_IP_POOL_SIZE}],
        "observatory": {
            "subjectSelector": mains,
            "probeUrl": settings.probe.url,
            "probeInterval": format!("{}s", settings.probe.interval.as_secs().max(1)),
            "enableConcurrency": true,
        },
        "routing": {
            "balancers": [{
                "tag": BALANCER_TAG,
                "selector": mains,
                "strategy": {"type": "leastPing"},
                "fallbackTag": fallback,
            }],
            "rules": rules(&settings.mode, &settings.rules),
        },
        "inbounds": inbound_list(settings),
        "outbounds": outbounds,
    });

    Ok(Compiled {
        config,
        tags: TagTable { entries },
        skipped,
        quic,
    })
}

fn service_outbounds(mark: Option<u32>) -> [Value; 3] {
    let mut direct = json!({"tag": "direct", "protocol": "freedom", "settings": {}});
    if let Some(mark) = mark {
        direct["streamSettings"] = json!({"sockopt": {"mark": mark}});
    }
    [
        direct,
        json!({"tag": "block", "protocol": "blackhole", "settings": {}}),
        json!({"tag": "dns-out", "protocol": "dns", "settings": {}}),
    ]
}

fn dns(resolvers: &[IpAddr], primary: IpAddr, hosts: &[String], direct: &[String]) -> Value {
    let mut servers = Vec::new();
    // Без этой записи fakedns подменяет адреса самих серверов, и узлы с доменом
    // не подключаются. Прямые домены тоже нужны настоящие адреса: их соединения идут мимо узлов.
    let mut domains: Vec<String> = hosts.iter().map(|host| format!("full:{host}")).collect();
    domains.extend_from_slice(direct);
    if !domains.is_empty() {
        servers.push(json!({
            "address": primary.to_string(),
            "domains": domains,
            "skipFallback": true,
        }));
    }
    servers.push(json!("fakedns"));
    servers.extend(resolvers.iter().map(|ip| json!(ip.to_string())));
    json!({"tag": "dns-internal", "queryStrategy": "UseIPv4", "servers": servers})
}

/// Домены правил с действием `direct`: их DNS-запросы идут к настоящему резолверу.
fn direct_domains(rules: &[Rule]) -> Vec<String> {
    rules
        .iter()
        .filter(|rule| rule.action == Action::Direct)
        .flat_map(|rule| rule.domains.iter().map(domain_text))
        .collect()
}

/// Доменные правила требуют, чтобы xray видел домен в соединении.
fn route_only(rules: &[Rule]) -> bool {
    rules.iter().any(|rule| !rule.domains.is_empty())
}

fn domain_text(domain: &Domain) -> String {
    let kind = match domain.kind {
        DomainKind::Full => "full",
        DomainKind::Subdomains => "domain",
        DomainKind::Keyword => "keyword",
    };
    format!("{kind}:{}", domain.name)
}

fn subnet_text(subnet: &Subnet) -> String {
    format!("{}/{}", subnet.addr, subnet.prefix)
}

fn rules(mode: &Mode, custom: &[Rule]) -> Vec<Value> {
    let mut rules = vec![
        json!({"type": "field", "inboundTag": ["api"], "outboundTag": "api"}),
        json!({"type": "field", "inboundTag": ["dns-internal"], "outboundTag": "direct"}),
    ];
    if matches!(mode, Mode::Gateway { .. }) {
        rules.push(json!({"type": "field", "port": "53", "outboundTag": "dns-out"}));
    }
    // geoip:private не используем: geo-файлов в поставке нет.
    rules.push(json!({"type": "field", "ip": PRIVATE_NETWORKS, "outboundTag": "direct"}));
    rules.extend(custom.iter().flat_map(custom_rules));
    rules.push(json!({"type": "field", "network": "tcp,udp", "balancerTag": BALANCER_TAG}));
    rules
}

/// Доменные и IP-условия — два правила xray, чтобы совпадение любым из них сохранилось.
fn custom_rules(rule: &Rule) -> Vec<Value> {
    let mut rules = Vec::new();
    if !rule.domains.is_empty() {
        let domains: Vec<String> = rule.domains.iter().map(domain_text).collect();
        rules.push(route("domain", json!(domains), rule.action));
    }
    if !rule.subnets.is_empty() {
        let subnets: Vec<String> = rule.subnets.iter().map(subnet_text).collect();
        rules.push(route("ip", json!(subnets), rule.action));
    }
    rules
}

fn route(field: &str, value: Value, action: Action) -> Value {
    let mut rule = json!({"type": "field"});
    rule[field] = value;
    match action {
        Action::Direct => rule["outboundTag"] = json!("direct"),
        Action::Block => rule["outboundTag"] = json!("block"),
        Action::Proxy => rule["balancerTag"] = json!(BALANCER_TAG),
    }
    rule
}

fn inbound_list(settings: &Settings) -> Vec<Value> {
    let mut list = inbounds(&settings.mode, route_only(&settings.rules));
    list.extend(settings.speedtest.as_ref().map(speedtest_inbound));
    list
}

/// Служебный вход теста скорости: только петля и логин с паролем. Трафик от него уходит по тому
/// же правилу по умолчанию, что и весь остальной, то есть на выбранный узел.
fn speedtest_inbound(inbound: &SpeedtestInbound) -> Value {
    json!({
        "tag": SPEEDTEST_TAG,
        "protocol": "mixed",
        "listen": "127.0.0.1",
        "port": inbound.port,
        "settings": {
            "auth": "password",
            "accounts": [{"user": inbound.credentials.user, "pass": inbound.credentials.password}],
        },
    })
}

fn inbounds(mode: &Mode, by_domain: bool) -> Vec<Value> {
    let mut sniffing = json!({"enabled": true, "destOverride": ["http", "tls", "quic", "fakedns"]});
    // Адрес соединения остаётся настоящим: fake-IP xray подменяет сам, поэтому
    // routeOnly безопасен рядом с fakedns.
    if by_domain {
        sniffing["routeOnly"] = json!(true);
    }
    match mode {
        Mode::Gateway { tproxy_port, .. } => vec![json!({
            "tag": "tproxy-in",
            "protocol": "dokodemo-door",
            "port": tproxy_port,
            "settings": {"network": "tcp,udp", "followRedirect": true},
            "streamSettings": {"sockopt": {"tproxy": "tproxy"}},
            "sniffing": sniffing,
        })],
        Mode::Proxy { listen, auth } => {
            let mut settings = match auth {
                Some(credentials) => json!({
                    "auth": "password",
                    "accounts": [{"user": credentials.user, "pass": credentials.password}],
                    "udp": true,
                }),
                None => json!({"auth": "noauth", "udp": true}),
            };
            // Адрес для UDP ASSOCIATE: по неопределённому адресу клиенту не подключиться.
            if !listen.ip().is_unspecified() {
                settings["ip"] = json!(listen.ip().to_string());
            }
            vec![json!({
                "tag": "proxy-in",
                "protocol": "mixed",
                "listen": listen.ip().to_string(),
                "port": listen.port(),
                "settings": settings,
                "sniffing": sniffing,
            })]
        }
    }
}
