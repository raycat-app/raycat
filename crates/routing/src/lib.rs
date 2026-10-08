//! Данные раздельной маршрутизации для пресета «Россия напрямую»: доменные зоны РФ и
//! российские IPv4-подсети из статистики RIPE NCC.
//!
//! Данные встроены в бинарник через `include_str!`. Зоны разбираются при первом обращении,
//! подсети читаются построчно по мере итерации. Файлы обновляет бот инструментом
//! `update-ru-ipv4` (feature `tool`, в бинарник raycat не попадает).

#[cfg(feature = "tool")]
pub mod update;

use std::net::Ipv4Addr;
use std::sync::OnceLock;

const ZONES_TEXT: &str = include_str!("../data/ru-zones.txt");
const IPV4_TEXT: &str = include_str!("../data/ru-ipv4.txt");
const SNAPSHOT_PREFIX: &str = "# Снимок: ";

/// Доменные зоны РФ без точки, в ASCII (кириллические зоны в punycode), по алфавиту.
pub fn ru_zones() -> &'static [&'static str] {
    static ZONES: OnceLock<Vec<&'static str>> = OnceLock::new();
    ZONES.get_or_init(|| data_lines(ZONES_TEXT).collect()).as_slice()
}

/// Российские подсети `IPv4`: адрес сети и длина префикса.
pub fn ru_ipv4() -> impl Iterator<Item = (Ipv4Addr, u8)> {
    data_lines(IPV4_TEXT).filter_map(parse_cidr)
}

/// Дата снимка RIPE NCC, из которого собраны подсети, в формате `ГГГГ-ММ-ДД`.
pub fn data_date() -> Option<&'static str> {
    IPV4_TEXT.lines().find_map(|line| line.strip_prefix(SNAPSHOT_PREFIX))
}

fn data_lines(text: &'static str) -> impl Iterator<Item = &'static str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn parse_cidr(line: &str) -> Option<(Ipv4Addr, u8)> {
    let (addr, len) = line.split_once('/')?;
    let addr: Ipv4Addr = addr.parse().ok()?;
    let len: u8 = len.parse().ok()?;
    if !(8..=32).contains(&len) {
        return None;
    }
    ((u32::from(addr) & !prefix_mask(len)) == 0).then_some((addr, len))
}

fn prefix_mask(len: u8) -> u32 {
    u32::MAX << (32 - u32::from(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(net: Ipv4Addr, len: u8, addr: Ipv4Addr) -> bool {
        (u32::from(net) & prefix_mask(len)) == (u32::from(addr) & prefix_mask(len))
    }

    fn ru_contains(addr: Ipv4Addr) -> bool {
        ru_ipv4().any(|(net, len)| contains(net, len, addr))
    }

    #[test]
    fn zones_are_sorted_unique_and_ascii() {
        let zones = ru_zones();
        assert!(zones.len() >= 8);
        assert!(zones.windows(2).all(|pair| pair[0] < pair[1]));
        for zone in zones {
            assert!(
                zone.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            );
        }
    }

    #[test]
    fn zones_include_russian_segment() {
        let zones = ru_zones();
        for expected in ["ru", "su", "xn--p1ai", "xn--d1acj3b", "xn--80adxhks", "xn--p1acf"] {
            assert!(zones.contains(&expected), "нет зоны {expected}");
        }
        assert!(!zones.contains(&"xn--80asehdb"));
    }

    #[test]
    fn parse_accepts_only_aligned_prefixes() {
        assert_eq!(parse_cidr("10.0.0.0/24"), Some((Ipv4Addr::new(10, 0, 0, 0), 24)));
        assert_eq!(parse_cidr("10.0.0.1/24"), None);
        assert_eq!(parse_cidr("10.0.0.0/7"), None);
        assert_eq!(parse_cidr("10.0.0.0/33"), None);
        assert_eq!(parse_cidr("10.0.0.0"), None);
        assert_eq!(parse_cidr("10.0.0/24"), None);
    }

    #[test]
    fn ipv4_lines_all_parse() {
        let lines = data_lines(IPV4_TEXT).count();
        let parsed: Vec<_> = ru_ipv4().collect();
        assert_eq!(parsed.len(), lines);
        assert!(parsed.iter().all(|(_, len)| (8..=32).contains(len)));
    }

    #[test]
    fn ipv4_prefixes_do_not_overlap() {
        let mut ranges: Vec<(u64, u64)> = ru_ipv4()
            .map(|(net, len)| {
                let start = u64::from(u32::from(net));
                (start, start + (1u64 << (32 - u32::from(len))))
            })
            .collect();
        ranges.sort_unstable();
        assert!(ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    }

    #[test]
    fn data_date_is_absent_until_snapshot_is_built() {
        assert_eq!(data_date(), None);
    }

    #[test]
    fn foreign_address_is_not_listed() {
        assert!(!ru_contains(Ipv4Addr::new(8, 8, 8, 8)));
    }
}
