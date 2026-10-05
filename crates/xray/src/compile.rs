use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::net::IpAddr;

use serde_json::{Value, json};

use crate::nodes::compile_node;
use crate::tuning::{self, Tuning};
use crate::{Mode, Node, Settings};

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
    let config = json!({
        "log": {"loglevel": "warning", "access": "none"},
        "api": {"tag": "api", "listen": api_listen, "services": API_SERVICES},
        "stats": {},
        "policy": {"system": {"statsOutboundUplink": true, "statsOutboundDownlink": true}},
        "dns": dns(&settings.dns.resolvers, *primary_resolver, &hosts),
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
            "rules": rules(&settings.mode),
        },
        "inbounds": inbounds(&settings.mode),
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

fn dns(resolvers: &[IpAddr], primary: IpAddr, hosts: &[String]) -> Value {
    let mut servers = Vec::new();
    // Без этой записи fakedns подменяет адреса самих серверов, и узлы с доменом
    // не подключаются.
    if !hosts.is_empty() {
        let domains: Vec<String> = hosts.iter().map(|host| format!("full:{host}")).collect();
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

fn rules(mode: &Mode) -> Vec<Value> {
    let mut rules = vec![
        json!({"type": "field", "inboundTag": ["api"], "outboundTag": "api"}),
        json!({"type": "field", "inboundTag": ["dns-internal"], "outboundTag": "direct"}),
    ];
    if matches!(mode, Mode::Gateway { .. }) {
        rules.push(json!({"type": "field", "port": "53", "outboundTag": "dns-out"}));
    }
    // geoip:private не используем: geo-файлов в поставке нет.
    rules.push(json!({"type": "field", "ip": PRIVATE_NETWORKS, "outboundTag": "direct"}));
    rules.push(json!({"type": "field", "network": "tcp,udp", "balancerTag": BALANCER_TAG}));
    rules
}

fn inbounds(mode: &Mode) -> Vec<Value> {
    let sniffing = json!({"enabled": true, "destOverride": ["http", "tls", "quic", "fakedns"]});
    match mode {
        Mode::Gateway { tproxy_port, .. } => vec![json!({
            "tag": "tproxy-in",
            "protocol": "dokodemo-door",
            "port": tproxy_port,
            "settings": {"network": "tcp,udp", "followRedirect": true},
            "streamSettings": {"sockopt": {"tproxy": "tproxy"}},
            "sniffing": sniffing,
        })],
        Mode::Proxy { listen } => {
            let mut settings = json!({"auth": "noauth", "udp": true});
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
