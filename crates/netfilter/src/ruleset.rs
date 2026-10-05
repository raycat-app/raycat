use anyhow::Result;

use crate::cidr::{Cidr, normalize};
use crate::rules::{Lan, Rules};

pub(crate) const FAMILY: &str = "inet";
pub(crate) const TABLE: &str = "raycat";

/// Назначения, которые не уходят в xray при любых настройках: multicast и broadcast
/// (иначе не работают DHCP, mDNS, IGMP) и link-local IPv6 (соседи, NDP).
const ALWAYS_DIRECT: [Cidr; 4] = [
    Cidr::v4(224, 0, 0, 0, 4),
    Cidr::v4(255, 255, 255, 255, 32),
    Cidr::v6(0xfe80, 10),
    Cidr::v6(0xff00, 8),
];

/// Текст для `nft -f`: таблица `inet raycat`, которая целиком заменяет прежнюю.
///
/// `add table` перед `delete table` нужен, чтобы удаление не падало, когда таблицы
/// ещё нет, и работало на nft без `destroy`; вся загрузка проходит одной транзакцией.
///
/// Цепочки:
/// * `output` (route) — метит исходящие tcp и udp меткой перехвата, после чего ядро
///   заново выбирает маршрут и отправляет пакет на loopback;
/// * `prerouting` — отдаёт помеченные пакеты прозрачному сокету xray (TPROXY); для
///   шлюза локальной сети сама метит пакеты устройств;
/// * `input` и `forward` (только для локальной сети) — не пускают устройства
///   напрямую к порту xray и не пересылают их наружу;
/// * `guard` (есть при kill switch или без перехвата IPv6) — не выпускает наружу
///   то, что не попало в перехват.
///
/// Входящие соединения хоста правила не трогают: ответы на них (`ct direction
/// reply`) идут мимо перехвата и kill switch, а tcp-соединения, начало которых
/// правила не видели (открытые до запуска), не перехватываются вовсе: у них нет
/// метки соединения.
pub fn ruleset(rules: &Rules) -> Result<String> {
    rules.validate()?;
    let (v4, v6) = direct_sets(rules);
    let parts = Parts {
        own: format!("{:#x}", rules.own_mark),
        mark: format!("{:#x}", rules.intercept_mark),
        port: rules.tproxy_port,
        v4,
        v6,
        v4_only: if rules.intercept_ipv6 {
            ""
        } else {
            "meta nfproto ipv4 "
        },
        lan: rules.lan.as_ref().map(LanParts::new),
    };

    let mut lines: Vec<String> = Vec::new();
    let mut push = |depth: usize, text: String| {
        lines.push(format!("{}{text}", "    ".repeat(depth)));
    };

    push(
        0,
        "# raycat: перехват трафика, файл создаёт демон".to_owned(),
    );
    push(0, format!("add table {FAMILY} {TABLE}"));
    push(0, format!("delete table {FAMILY} {TABLE}"));
    push(0, format!("table {FAMILY} {TABLE} {{"));
    output_chain(&mut push, &parts);
    push(0, String::new());
    prerouting_chain(&mut push, &parts, rules.intercept_ipv6);
    if let Some(lan) = &parts.lan {
        push(0, String::new());
        input_chain(&mut push, &parts, lan);
        push(0, String::new());
        forward_chain(&mut push, &parts, lan, rules.kill_switch);
    }
    if rules.kill_switch || !rules.intercept_ipv6 {
        push(0, String::new());
        guard_chain(&mut push, &parts, rules.kill_switch);
    }
    push(0, "}".to_owned());
    let mut text = lines.join("\n");
    text.push('\n');
    Ok(text)
}

struct Parts {
    own: String,
    mark: String,
    port: u16,
    v4: String,
    v6: String,
    /// Условие, не пускающее правило на IPv6, пока IPv6 не перехватывается.
    v4_only: &'static str,
    lan: Option<LanParts>,
}

struct LanParts {
    interface: String,
    /// Подсети устройств в виде множества nft.
    nets: String,
}

impl LanParts {
    fn new(lan: &Lan) -> Self {
        let nets = normalize(lan.subnets.iter().copied());
        Self {
            interface: lan.interface.clone(),
            nets: render(&nets, true),
        }
    }

    /// Начало правила: пакет пришёл от устройства сети по IPv4.
    fn source(&self) -> String {
        format!(
            "iifname \"{}\" meta nfproto ipv4 ip saddr {}",
            self.interface, self.nets
        )
    }
}

type Push<'a> = &'a mut dyn FnMut(usize, String);

fn output_chain(push: Push, parts: &Parts) {
    let Parts {
        own,
        mark,
        v4,
        v6,
        v4_only,
        ..
    } = parts;
    push(1, "chain output {".to_owned());
    push(
        2,
        "type route hook output priority mangle; policy accept;".to_owned(),
    );
    push(2, "# трафик xray и демона идёт мимо перехвата".to_owned());
    push(2, format!("meta mark {own} return"));
    push(2, "# loopback, в том числе адреса самого хоста".to_owned());
    push(2, "oifname \"lo\" return".to_owned());
    push(2, "# ответы на входящие соединения".to_owned());
    push(2, "ct direction reply return".to_owned());
    push(
        2,
        "# DNS перехватывается всегда, даже к приватным серверам".to_owned(),
    );
    push(
        2,
        format!("{v4_only}meta l4proto {{ tcp, udp }} th dport 53 meta mark set {mark} return"),
    );
    push(
        2,
        "# приватные сети, multicast и broadcast идут напрямую".to_owned(),
    );
    push(2, format!("ip daddr {v4} return"));
    push(2, format!("ip6 daddr {v6} return"));
    push(
        2,
        "# tcp перехватывается, только если правила видели начало соединения (SYN):".to_owned(),
    );
    push(
        2,
        "# открытое до запуска (ssh к хосту) продолжает идти как шло".to_owned(),
    );
    push(
        2,
        format!(
            "{v4_only}meta l4proto tcp tcp flags & (fin | syn | rst | ack) == syn ct mark set {mark}"
        ),
    );
    push(
        2,
        format!("{v4_only}meta l4proto tcp ct mark != {mark} return"),
    );
    push(2, "# остальные tcp и udp уходят в xray".to_owned());
    push(
        2,
        format!("{v4_only}meta l4proto {{ tcp, udp }} meta mark set {mark}"),
    );
    push(1, "}".to_owned());
}

fn prerouting_chain(push: Push, parts: &Parts, ipv6: bool) {
    let Parts { mark, port, .. } = parts;
    push(1, "chain prerouting {".to_owned());
    push(
        2,
        "type filter hook prerouting priority mangle; policy accept;".to_owned(),
    );
    push(
        2,
        "# помеченные пакеты достаются прозрачному сокету xray".to_owned(),
    );
    for proto in ["tcp", "udp"] {
        push(
            2,
            format!(
                "meta nfproto ipv4 meta l4proto {proto} meta mark {mark} tproxy ip to :{port} accept"
            ),
        );
    }
    if ipv6 {
        for proto in ["tcp", "udp"] {
            push(
                2,
                format!(
                    "meta nfproto ipv6 meta l4proto {proto} meta mark {mark} tproxy ip6 to :{port} accept"
                ),
            );
        }
    }
    if let Some(lan) = &parts.lan {
        lan_intercept(push, parts, lan);
    }
    push(1, "}".to_owned());
}

/// Пакеты устройств метятся здесь же, до выбора маршрута: по метке политика
/// маршрутизации доставляет их локально, а `tproxy` отдаёт сокету xray. Без
/// слушающего xray пакет всё равно уходит на loopback и получает отказ.
fn lan_intercept(push: Push, parts: &Parts, lan: &LanParts) {
    let Parts { mark, port, v4, .. } = parts;
    let from = lan.source();
    push(
        2,
        format!(
            "# устройства сети ({}): DNS идёт в xray, даже если сервер — сам хост",
            lan.interface
        ),
    );
    for proto in ["tcp", "udp"] {
        push(
            2,
            format!(
                "{from} meta l4proto {proto} th dport 53 meta mark set {mark} tproxy ip to :{port} accept"
            ),
        );
    }
    push(
        2,
        "# адреса самого хоста, multicast и broadcast не трогаем".to_owned(),
    );
    push(
        2,
        format!("{from} fib daddr type {{ local, broadcast, multicast }} return"),
    );
    push(
        2,
        "# приватные сети и подсети устройств идут напрямую".to_owned(),
    );
    push(2, format!("{from} ip daddr {v4} return"));
    push(2, "# остальные tcp и udp устройств уходят в xray".to_owned());
    for proto in ["tcp", "udp"] {
        push(
            2,
            format!(
                "{from} meta l4proto {proto} meta mark set {mark} tproxy ip to :{port} accept"
            ),
        );
    }
}

/// Перехваченные пакеты доставляются сокету xray со своим (чужим) адресом
/// назначения, а настоящее обращение к порту xray — это попытка устройства
/// воспользоваться им напрямую.
fn input_chain(push: Push, parts: &Parts, lan: &LanParts) {
    let port = parts.port;
    push(1, "chain input {".to_owned());
    push(
        2,
        "type filter hook input priority filter; policy accept;".to_owned(),
    );
    push(
        2,
        "# к порту xray устройства сети напрямую не ходят".to_owned(),
    );
    push(
        2,
        format!(
            "iifname \"{}\" meta nfproto ipv4 fib daddr type local meta l4proto {{ tcp, udp }} th dport {port} drop",
            lan.interface
        ),
    );
    push(1, "}".to_owned());
}

/// Перехваченный трафик устройств до `forward` не доходит. Сюда попадает то, что
/// перехват не затрагивает (ICMP, другие протоколы, IPv6), и пересылка
/// на приватные адреса, например в сети Docker.
fn forward_chain(push: Push, parts: &Parts, lan: &LanParts, kill_switch: bool) {
    let Parts { v4, v6, .. } = parts;
    let interface = &lan.interface;
    push(1, "chain forward {".to_owned());
    push(
        2,
        "type filter hook forward priority filter; policy accept;".to_owned(),
    );
    push(
        2,
        "# приватные сети и подсети устройств: пересылка как обычно".to_owned(),
    );
    if kill_switch {
        push(2, format!("iifname \"{interface}\" ip daddr {v4} return"));
    }
    push(2, format!("iifname \"{interface}\" ip6 daddr {v6} return"));
    if kill_switch {
        push(
            2,
            "# kill switch: всё остальное от устройств наружу не пересылается, \
             даже если на хосте включён ip_forward"
                .to_owned(),
        );
        push(2, format!("iifname \"{interface}\" drop"));
    } else {
        push(
            2,
            "# IPv6 не перехватывается, наружу его не выпускаем".to_owned(),
        );
        push(
            2,
            format!("iifname \"{interface}\" meta nfproto ipv6 drop"),
        );
    }
    push(1, "}".to_owned());
}

fn guard_chain(push: Push, parts: &Parts, kill_switch: bool) {
    let Parts {
        own, mark, v4, v6, ..
    } = parts;
    push(1, "chain guard {".to_owned());
    push(
        2,
        "type filter hook output priority filter; policy accept;".to_owned(),
    );
    push(2, "oifname \"lo\" accept".to_owned());
    push(2, format!("meta mark {own} accept"));
    push(
        2,
        "# перехваченные пакеты: после смены маршрута oifname ещё показывает прежний интерфейс"
            .to_owned(),
    );
    push(2, format!("meta mark {mark} accept"));
    push(2, "ct direction reply accept".to_owned());
    push(2, format!("ip daddr {v4} accept"));
    push(2, format!("ip6 daddr {v6} accept"));
    push(
        2,
        "# соединение, начала которого правила не видели (открыто до запуска): не рвём"
            .to_owned(),
    );
    push(
        2,
        "meta l4proto tcp tcp flags & (fin | syn | rst | ack) != syn accept".to_owned(),
    );
    if kill_switch {
        push(2, "# kill switch: всё остальное отклоняется".to_owned());
        push(2, "reject".to_owned());
    } else {
        push(
            2,
            "# IPv6 не перехватывается, наружу его не выпускаем".to_owned(),
        );
        push(2, "meta nfproto ipv6 reject".to_owned());
    }
    push(1, "}".to_owned());
}

/// Текст для `nft -f`, который удаляет таблицу, если она есть, и ничего не делает,
/// если её нет.
pub(crate) fn removal() -> String {
    format!("add table {FAMILY} {TABLE}\ndelete table {FAMILY} {TABLE}\n")
}

fn direct_sets(rules: &Rules) -> (String, String) {
    let lan = rules
        .lan
        .iter()
        .flat_map(|lan| lan.subnets.iter().copied());
    let nets = normalize(rules.bypass.iter().copied().chain(ALWAYS_DIRECT).chain(lan));
    (render(&nets, true), render(&nets, false))
}

fn render(nets: &[Cidr], v4: bool) -> String {
    let items: Vec<String> = nets
        .iter()
        .filter(|net| net.is_ipv4() == v4)
        .map(ToString::to_string)
        .collect();
    format!("{{ {} }}", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(rules: &Rules) -> Vec<String> {
        ruleset(rules)
            .unwrap()
            .lines()
            .map(|line| line.trim().to_owned())
            .collect()
    }

    fn position(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("нет строки с «{needle}»"))
    }

    /// Строки цепочки от её заголовка до закрывающей скобки (без неё).
    fn chain<'a>(lines: &'a [String], name: &str) -> &'a [String] {
        let start = position(lines, &format!("chain {name} {{"));
        let length = lines[start..]
            .iter()
            .position(|line| line == "}")
            .unwrap_or_else(|| panic!("цепочка {name} не закрыта"));
        &lines[start..start + length]
    }

    fn lan_rules(kill_switch: bool) -> Rules {
        Rules {
            kill_switch,
            lan: Some(Lan {
                interface: "eth1".to_owned(),
                subnets: vec!["10.77.0.0/24".parse().unwrap()],
            }),
            ..Rules::default()
        }
    }

    #[test]
    fn own_traffic_and_loopback_come_before_any_marking() {
        let lines = lines_of(&Rules::default());
        let own = position(&lines, "meta mark 0x52430000 return");
        let lo = position(&lines, "oifname \"lo\" return");
        let reply = position(&lines, "ct direction reply return");
        let dns = position(&lines, "th dport 53");
        let private = position(&lines, "ip daddr");
        let all = position(&lines, "meta l4proto { tcp, udp } meta mark set");
        assert!(own < lo && lo < reply && reply < dns && dns < private && private < all);
    }

    #[test]
    fn dns_is_intercepted_before_private_networks_are_skipped() {
        let text = ruleset(&Rules::default()).unwrap();
        let dns = text.find("th dport 53").unwrap();
        let private = text.find("ip daddr").unwrap();
        assert!(
            dns < private,
            "DNS к приватному серверу тоже должен перехватываться"
        );
    }

    #[test]
    fn only_connections_started_under_the_rules_are_intercepted() {
        let lines = lines_of(&Rules::default());
        let private = position(&lines, "ip6 daddr");
        let start = position(&lines, "tcp flags & (fin | syn | rst | ack) == syn ct mark set");
        let foreign = position(&lines, "meta l4proto tcp ct mark != 0x52540000 return");
        let all = position(&lines, "meta l4proto { tcp, udp } meta mark set");
        assert!(private < start && start < foreign && foreign < all);
        assert!(lines[start].ends_with("ct mark set 0x52540000"));
    }

    #[test]
    fn ipv6_is_blocked_unless_intercepted() {
        let text = ruleset(&Rules::default()).unwrap();
        assert!(text.contains("meta nfproto ipv6 reject"));
        assert!(!text.contains("tproxy ip6"));
        assert!(text.contains("meta nfproto ipv4 meta l4proto { tcp, udp } meta mark set"));
        assert!(text.contains("meta nfproto ipv4 meta l4proto tcp ct mark != 0x52540000 return"));

        let rules = Rules {
            intercept_ipv6: true,
            ..Rules::default()
        };
        let text = ruleset(&rules).unwrap();
        assert!(text.contains("tproxy ip6 to :12345"));
        assert!(
            !text.contains("chain guard"),
            "без kill switch защищать нечего"
        );
        assert!(!text.contains("meta nfproto ipv4 meta l4proto { tcp, udp }"));
        assert!(text.contains("\n        meta l4proto tcp ct mark != 0x52540000 return\n"));
    }

    #[test]
    fn kill_switch_allows_only_safe_paths_and_rejects_last() {
        let rules = Rules {
            kill_switch: true,
            ..Rules::default()
        };
        let lines = lines_of(&rules);
        let guard = position(&lines, "chain guard");
        let reject = position(&lines, "reject");
        assert_eq!(lines[reject], "reject");
        assert_eq!(lines[reject + 1], "}");
        let allowed: Vec<&str> = lines[guard..reject]
            .iter()
            .filter(|line| line.ends_with(" accept"))
            .map(|line| line.trim_end_matches(" accept"))
            .collect();
        assert_eq!(allowed.len(), 7);
        assert_eq!(
            &allowed[..4],
            [
                "oifname \"lo\"",
                "meta mark 0x52430000",
                "meta mark 0x52540000",
                "ct direction reply"
            ]
        );
        assert!(allowed[4].starts_with("ip daddr {") && allowed[5].starts_with("ip6 daddr {"));
        assert_eq!(
            allowed[6],
            "meta l4proto tcp tcp flags & (fin | syn | rst | ack) != syn"
        );
    }

    #[test]
    fn bypass_is_merged_and_sets_are_never_empty() {
        let rules = Rules {
            bypass: vec![
                "10.0.0.0/8".parse().unwrap(),
                "10.1.0.0/16".parse().unwrap(),
                "10.0.0.0/8".parse().unwrap(),
            ],
            ..Rules::default()
        };
        let text = ruleset(&rules).unwrap();
        assert!(text.contains("ip daddr { 10.0.0.0/8, 224.0.0.0/4, 255.255.255.255/32 } return"));
        assert!(text.contains("ip6 daddr { fe80::/10, ff00::/8 } return"));

        let empty = Rules {
            bypass: Vec::new(),
            ..Rules::default()
        };
        let text = ruleset(&empty).unwrap();
        assert!(text.contains("ip daddr { 224.0.0.0/4, 255.255.255.255/32 } return"));
    }

    #[test]
    fn invalid_rules_are_refused() {
        let rules = Rules {
            tproxy_port: 0,
            ..Rules::default()
        };
        assert!(ruleset(&rules).is_err());
    }

    #[test]
    fn removal_is_idempotent_text() {
        assert_eq!(
            removal(),
            "add table inet raycat\ndelete table inet raycat\n"
        );
    }

    #[test]
    fn without_lan_there_are_no_lan_chains() {
        let text = ruleset(&Rules {
            kill_switch: true,
            ..Rules::default()
        })
        .unwrap();
        for chain in ["chain input", "chain forward", "iifname"] {
            assert!(!text.contains(chain), "{chain}");
        }
    }

    #[test]
    fn lan_devices_are_intercepted_before_routing_with_the_same_mark() {
        let lines = lines_of(&lan_rules(true));
        let dns = position(&lines, "th dport 53 meta mark set 0x52540000 tproxy");
        let host = position(&lines, "fib daddr type { local, broadcast, multicast } return");
        let direct = position(
            &lines,
            "iifname \"eth1\" meta nfproto ipv4 ip saddr { 10.77.0.0/24 } ip daddr",
        );
        let all = position(
            &lines,
            "meta l4proto tcp meta mark set 0x52540000 tproxy ip to :12345 accept",
        );
        assert!(dns < host && host < direct && direct < all);
        let intercepting = lines
            .iter()
            .filter(|line| line.contains("tproxy") && line.starts_with("iifname \"eth1\""))
            .count();
        assert_eq!(intercepting, 4, "dns и остальное, tcp и udp");
        assert!(
            lines
                .iter()
                .filter(|line| line.contains("tproxy") && line.starts_with("iifname"))
                .all(|line| line.contains("ip saddr { 10.77.0.0/24 }")),
            "перехватываются только устройства из своих подсетей"
        );
    }

    #[test]
    fn lan_subnets_never_go_through_xray() {
        let text = ruleset(&lan_rules(false)).unwrap();
        assert!(
            text.contains("ip daddr { 10.0.0.0/8, 100.64.0.0/10, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4, 255.255.255.255/32 } return"),
            "10.77.0.0/24 уже внутри 10.0.0.0/8"
        );
        let public = Rules {
            lan: Some(Lan {
                interface: "eth1".to_owned(),
                subnets: vec!["203.0.113.0/24".parse().unwrap()],
            }),
            ..Rules::default()
        };
        let text = ruleset(&public).unwrap();
        assert!(text.contains("ip daddr { 10.0.0.0/8, 100.64.0.0/10, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 203.0.113.0/24, 224.0.0.0/4, 255.255.255.255/32 } return"));
    }

    #[test]
    fn the_xray_port_is_closed_to_devices_and_nothing_else_is_dropped_in_input() {
        let lines = lines_of(&lan_rules(false));
        let input = position(&lines, "chain input");
        assert_eq!(
            lines[input + 3],
            "iifname \"eth1\" meta nfproto ipv4 fib daddr type local meta l4proto { tcp, udp } th dport 12345 drop"
        );
        assert_eq!(lines[input + 4], "}");
    }

    #[test]
    fn the_kill_switch_stops_forwarding_from_devices() {
        let lines = lines_of(&lan_rules(true));
        let body = chain(&lines, "forward");
        assert_eq!(body.last().unwrap(), "iifname \"eth1\" drop");
        assert!(
            body.iter()
                .any(|line| line.starts_with("iifname \"eth1\" ip daddr {"))
        );
    }

    #[test]
    fn without_the_kill_switch_only_ipv6_is_not_forwarded() {
        let lines = lines_of(&lan_rules(false));
        let body = chain(&lines, "forward");
        assert_eq!(
            body.last().unwrap(),
            "iifname \"eth1\" meta nfproto ipv6 drop"
        );
        assert!(!body.iter().any(|line| line.ends_with("eth1\" drop")));
    }
}
