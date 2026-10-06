//! Режим шлюза: проверки при старте, правила перехвата и слежение за ними.

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use raycat_netfilter::Rules;

use crate::log::{Latch, Level, debug, error, info, warn};
use crate::plan;

/// Как часто проверяется, что таблица и правило маршрутизации на месте.
pub(crate) const GUARD_INTERVAL: Duration = Duration::from_secs(30);

const CAP_NET_ADMIN: u32 = 12;
const IP_FORWARD: &str = "/proc/sys/net/ipv4/ip_forward";

/// Проверки, после которых правила можно ставить: нужная привилегия и отсутствие
/// утечки DNS встроенного резолвера Docker.
pub(crate) fn preflight() -> Result<()> {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    if has_capability(&status, CAP_NET_ADMIN) == Some(false) {
        bail!(
            "режиму шлюза нужна привилегия CAP_NET_ADMIN (Docker compose: `cap_add: [NET_ADMIN]`, \
             docker run: `--cap-add NET_ADMIN`, systemd: `AmbientCapabilities=CAP_NET_ADMIN`)"
        );
    }
    if let Some(message) = raycat_netfilter::dns_leak() {
        bail!("{message}");
    }
    Ok(())
}

/// Есть ли привилегия `bit` среди действующих (`CapEff` в `/proc/self/status`);
/// `None`, если строку разобрать не удалось.
fn has_capability(status: &str, bit: u32) -> Option<bool> {
    let caps = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())?;
    Some(caps & (1 << bit) != 0)
}

pub(crate) fn install(rules: &Rules) -> Result<()> {
    raycat_netfilter::install(rules).context("не удалось поставить правила перехвата")?;
    info!(
        "правила перехвата установлены, kill switch {}",
        if rules.kill_switch {
            "включён"
        } else {
            "выключен"
        }
    );
    if let Some(lan) = &rules.lan {
        info!("{}", plan::describe_lan(lan));
        let forwarding = fs::read_to_string(IP_FORWARD).unwrap_or_default();
        if !rules.kill_switch && forwarding_enabled(&forwarding) {
            warn!(
                "на хосте включена пересылка пакетов (ip_forward), а kill switch выключен: \
                 ICMP и другие не tcp/udp пакеты устройств уходят наружу напрямую"
            );
        }
    }
    Ok(())
}

fn forwarding_enabled(text: &str) -> bool {
    text.trim() == "1"
}

/// Снимает правила при штатной остановке; ошибка только пишется в лог.
pub(crate) fn remove(rules: &Rules) {
    match raycat_netfilter::remove(rules) {
        Ok(()) => info!("правила перехвата сняты"),
        Err(error) => error!("{error:#}"),
    }
}

/// Возвращает правила, если их кто-то сбросил: kill switch держится на правиле
/// маршрутизации, а `docker restart` и сторонние инструменты сбрасывают его. Пока
/// восстановить не получается, о потере пишется один раз, а не при каждой проверке.
pub(crate) fn guard(rules: &Rules, lost: &mut Latch) {
    if raycat_netfilter::is_installed(rules) {
        lost.clear();
        return;
    }
    let first = lost.level("правила перехвата пропали") == Level::Warn;
    if first {
        warn!(
            "правила перехвата пропали (таблица nftables или правило маршрутизации), ставлю заново"
        );
    }
    match raycat_netfilter::install(rules) {
        Ok(()) => {
            lost.clear();
            info!("правила перехвата восстановлены");
        }
        Err(error) if first => error!("не удалось восстановить правила перехвата: {error:#}"),
        Err(error) => debug!("не удалось восстановить правила перехвата: {error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\traycat\nCapInh:\t0000000000000000\nCapPrm:\t00000000a80425fb\nCapEff:\t00000000a80425fb\nCapBnd:\t00000000a80425fb\n";

    #[test]
    fn docker_default_capabilities_lack_net_admin() {
        assert_eq!(has_capability(STATUS, CAP_NET_ADMIN), Some(false));
        // CAP_NET_RAW (13) в наборе Docker по умолчанию есть.
        assert_eq!(has_capability(STATUS, 13), Some(true));
    }

    #[test]
    fn net_admin_is_found_when_granted() {
        let granted = "CapEff:\t00000000a80435fb\n";
        assert_eq!(has_capability(granted, CAP_NET_ADMIN), Some(true));
        let root = "CapEff:\t000001ffffffffff\n";
        assert_eq!(has_capability(root, CAP_NET_ADMIN), Some(true));
    }

    #[test]
    fn forwarding_is_read_from_the_sysctl_value() {
        assert!(forwarding_enabled("1\n"));
        assert!(!forwarding_enabled("0\n"));
        assert!(!forwarding_enabled(""));
    }

    #[test]
    fn unreadable_status_is_not_a_verdict() {
        assert_eq!(has_capability("", CAP_NET_ADMIN), None);
        assert_eq!(has_capability("CapEff:\tzzz\n", CAP_NET_ADMIN), None);
    }
}
