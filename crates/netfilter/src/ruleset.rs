use anyhow::Result;

use crate::cidr::{Cidr, normalize};
use crate::rules::Rules;

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
/// * `prerouting` — отдаёт помеченные пакеты прозрачному сокету xray (TPROXY);
/// * `guard` (есть при kill switch или без перехвата IPv6) — не выпускает наружу то,
///   что не попало в перехват.
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
    let nets = normalize(rules.bypass.iter().copied().chain(ALWAYS_DIRECT));
    let render = |v4: bool| {
        let items: Vec<String> = nets
            .iter()
            .filter(|net| net.is_ipv4() == v4)
            .map(ToString::to_string)
            .collect();
        format!("{{ {} }}", items.join(", "))
    };
    (render(true), render(false))
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
    fn ipv6_is_blocked_unless_intercepted() {
        let text = ruleset(&Rules::default()).unwrap();
        assert!(text.contains("meta nfproto ipv6 reject"));
        assert!(!text.contains("tproxy ip6"));
        assert!(text.contains("meta nfproto ipv4 meta l4proto { tcp, udp } meta mark set"));

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
        assert_eq!(allowed.len(), 6);
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
}
