use anyhow::{Result, bail};

use crate::cidr::Cidr;

/// Метка собственных сокетов xray и демона. Единственная метка, которую правила
/// пропускают мимо перехвата и kill switch. Старшие 16 бит заняты, младшие нулевые:
/// биты 0x4000 и 0x8000 использует kube-proxy, младшие — swarm и Cilium, а
/// Tailscale сравнивает только байт 0xff0000 с другим значением.
pub const DEFAULT_OWN_MARK: u32 = 0x5243_0000;
/// Метка перехваченных пакетов: по ней политика маршрутизации отправляет пакет
/// на loopback, а правило `prerouting` отдаёт его прозрачному сокету xray.
pub const DEFAULT_INTERCEPT_MARK: u32 = 0x5254_0000;
pub const DEFAULT_TPROXY_PORT: u16 = 12345;
/// Номер таблицы маршрутизации: вне зарезервированных (0, 253–255) и вне
/// обычных для Tailscale (52) и wg-quick (51820).
pub const DEFAULT_ROUTE_TABLE: u32 = 7263;
/// Должен быть меньше 32766: правило с меньшим числом просматривается раньше `main`.
pub const DEFAULT_RULE_PRIORITY: u32 = 7263;

/// Сети, к которым трафик идёт напрямую: приватные, link-local, CGNAT.
pub const DEFAULT_BYPASS: [Cidr; 7] = [
    Cidr::v4(10, 0, 0, 0, 8),
    Cidr::v4(172, 16, 0, 0, 12),
    Cidr::v4(192, 168, 0, 0, 16),
    Cidr::v4(169, 254, 0, 0, 16),
    Cidr::v4(100, 64, 0, 0, 10),
    Cidr::v6(0xfc00, 7),
    Cidr::v6(0xfe80, 10),
];

const MAX_BYPASS: usize = 1024;
const RESERVED_TABLES: [u32; 4] = [0, 253, 254, 255];
const MAX_RULE_PRIORITY: u32 = 32765;

/// Всё, что нужно правилам перехвата. Значения по умолчанию — константы крейта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// Порт прозрачного inbound xray.
    pub tproxy_port: u16,
    /// Метка сокетов xray и демона (`sockopt.mark` в xray, `SO_MARK` в демоне).
    pub own_mark: u32,
    pub intercept_mark: u32,
    pub route_table: u32,
    pub rule_priority: u32,
    /// Назначения, которые идут мимо перехвата и которые kill switch выпускает.
    pub bypass: Vec<Cidr>,
    pub kill_switch: bool,
    /// Перехватывать и IPv6. Если выключено, исходящий IPv6 запрещён.
    pub intercept_ipv6: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            tproxy_port: DEFAULT_TPROXY_PORT,
            own_mark: DEFAULT_OWN_MARK,
            intercept_mark: DEFAULT_INTERCEPT_MARK,
            route_table: DEFAULT_ROUTE_TABLE,
            rule_priority: DEFAULT_RULE_PRIORITY,
            bypass: DEFAULT_BYPASS.to_vec(),
            kill_switch: false,
            intercept_ipv6: false,
        }
    }
}

impl Rules {
    pub fn validate(&self) -> Result<()> {
        if self.tproxy_port == 0 {
            bail!("порт прозрачного inbound xray не может быть нулём");
        }
        if self.own_mark == 0 || self.intercept_mark == 0 {
            bail!("метки не могут быть нулевыми: нулевая метка у любого непомеченного пакета");
        }
        if self.own_mark == self.intercept_mark {
            bail!("метка собственного трафика и метка перехвата должны различаться");
        }
        if RESERVED_TABLES.contains(&self.route_table) {
            bail!(
                "таблица маршрутизации {} зарезервирована системой",
                self.route_table
            );
        }
        if !(1..=MAX_RULE_PRIORITY).contains(&self.rule_priority) {
            bail!("приоритет правила маршрутизации должен быть от 1 до {MAX_RULE_PRIORITY}");
        }
        if self.bypass.len() > MAX_BYPASS {
            bail!("в списке сетей мимо перехвата больше {MAX_BYPASS} записей");
        }
        if let Some(net) = self.bypass.iter().find(|net| net.prefix() == 0) {
            bail!("сеть {net} отключила бы перехват целиком");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_do_not_collide() {
        let rules = Rules::default();
        rules.validate().unwrap();
        assert_ne!(rules.own_mark, rules.intercept_mark);
        assert_eq!(rules.bypass.len(), 7);
        assert!(!rules.kill_switch);
        assert!(!rules.intercept_ipv6);
        for mark in [rules.own_mark, rules.intercept_mark] {
            assert_eq!(mark & 0xffff, 0, "младшие 16 бит заняты другими программами");
        }
    }

    #[test]
    fn rejects_dangerous_values() {
        let broken = |change: fn(&mut Rules)| {
            let mut rules = Rules::default();
            change(&mut rules);
            rules.validate().unwrap_err().to_string()
        };
        assert!(broken(|r| r.tproxy_port = 0).contains("порт"));
        assert!(broken(|r| r.own_mark = 0).contains("метки"));
        assert!(broken(|r| r.intercept_mark = 0).contains("метки"));
        assert!(broken(|r| r.intercept_mark = r.own_mark).contains("различаться"));
        assert!(broken(|r| r.route_table = 254).contains("зарезервирована"));
        assert!(broken(|r| r.route_table = 0).contains("зарезервирована"));
        assert!(broken(|r| r.rule_priority = 0).contains("приоритет"));
        assert!(broken(|r| r.rule_priority = 32766).contains("приоритет"));
        assert!(broken(|r| r.bypass = vec![Cidr::v4(0, 0, 0, 0, 0)]).contains("целиком"));
        assert!(broken(|r| r.bypass = vec![Cidr::v4(10, 0, 0, 0, 8); 1025]).contains("больше"));
    }
}
