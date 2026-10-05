//! Эталонные правила. Те же файлы CI проверяет настоящим `nft -c` в отдельном
//! сетевом пространстве (`.github/e2e/netfilter.sh`).
#![allow(clippy::unwrap_used)]

use raycat_netfilter::{Cidr, Lan, Rules, ruleset};

fn nets(list: &[&str]) -> Vec<Cidr> {
    list.iter().map(|net| net.parse().unwrap()).collect()
}

#[test]
fn only_interception() {
    let text = ruleset(&Rules::default()).unwrap();
    assert_eq!(text, include_str!("golden/intercept.nft"));
}

#[test]
fn interception_with_kill_switch() {
    let rules = Rules {
        kill_switch: true,
        ..Rules::default()
    };
    assert_eq!(
        ruleset(&rules).unwrap(),
        include_str!("golden/kill-switch.nft")
    );
}

#[test]
fn interception_with_ipv6() {
    let rules = Rules {
        intercept_ipv6: true,
        ..Rules::default()
    };
    assert_eq!(ruleset(&rules).unwrap(), include_str!("golden/ipv6.nft"));
}

#[test]
fn interception_with_ipv6_and_kill_switch() {
    let rules = Rules {
        intercept_ipv6: true,
        kill_switch: true,
        ..Rules::default()
    };
    assert_eq!(
        ruleset(&rules).unwrap(),
        include_str!("golden/ipv6-kill-switch.nft")
    );
}

fn lan() -> Lan {
    Lan {
        interface: "rcnft-c".to_owned(),
        subnets: nets(&["10.99.0.0/24"]),
    }
}

#[test]
fn lan_gateway() {
    let rules = Rules {
        lan: Some(lan()),
        ..Rules::default()
    };
    assert_eq!(ruleset(&rules).unwrap(), include_str!("golden/lan.nft"));
}

#[test]
fn lan_gateway_with_kill_switch() {
    let rules = Rules {
        kill_switch: true,
        lan: Some(lan()),
        ..Rules::default()
    };
    assert_eq!(
        ruleset(&rules).unwrap(),
        include_str!("golden/lan-kill-switch.nft")
    );
}

#[test]
fn custom_networks_marks_and_port() {
    let rules = Rules {
        tproxy_port: 7893,
        own_mark: 0x1000_0001,
        intercept_mark: 0x1000_0002,
        bypass: nets(&["192.0.2.0/24", "10.0.0.0/8", "10.1.0.0/16", "2001:db8::/32"]),
        ..Rules::default()
    };
    assert_eq!(ruleset(&rules).unwrap(), include_str!("golden/custom.nft"));
}
