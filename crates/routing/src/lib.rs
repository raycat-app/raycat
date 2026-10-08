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
    ZONES
        .get_or_init(|| data_lines(ZONES_TEXT).collect())
        .as_slice()
}

/// Российские подсети `IPv4`: адрес сети и длина префикса.
pub fn ru_ipv4() -> impl Iterator<Item = (Ipv4Addr, u8)> {
    data_lines(IPV4_TEXT).filter_map(parse_cidr)
}

/// Дата снимка RIPE NCC, из которого собраны подсети, в формате `ГГГГ-ММ-ДД`.
pub fn data_date() -> Option<&'static str> {
    IPV4_TEXT
        .lines()
        .find_map(|line| line.strip_prefix(SNAPSHOT_PREFIX))
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
        for expected in [
            "ru",
            "su",
            "xn--p1ai",
            "xn--d1acj3b",
            "xn--80adxhks",
            "xn--p1acf",
        ] {
            assert!(zones.contains(&expected), "нет зоны {expected}");
        }
        assert!(!zones.contains(&"xn--80asehdb"));
    }

    #[test]
    fn parse_accepts_only_aligned_prefixes() {
        assert_eq!(
            parse_cidr("10.0.0.0/24"),
            Some((Ipv4Addr::new(10, 0, 0, 0), 24))
        );
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

    fn range_of(net: Ipv4Addr, len: u8) -> (u64, u64) {
        let start = u64::from(u32::from(net));
        (start, start + (1u64 << (32 - u32::from(len))))
    }

    #[test]
    fn ipv4_prefixes_are_sorted_and_disjoint() {
        let ranges: Vec<(u64, u64)> = ru_ipv4().map(|(net, len)| range_of(net, len)).collect();
        assert!(ranges.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    }

    #[test]
    fn adjacent_prefixes_are_not_mergeable() {
        let prefixes: Vec<(Ipv4Addr, u8)> = ru_ipv4().collect();
        for pair in prefixes.windows(2) {
            let (a, a_len) = pair[0];
            let (b, b_len) = pair[1];
            let (start, end) = range_of(a, a_len);
            // Блоки /8 инструмент не объединяет дальше (см. MAX_BLOCK в update.rs).
            let mergeable = a_len == b_len
                && a_len > 8
                && end == u64::from(u32::from(b))
                && start.is_multiple_of(2 * (end - start));
            assert!(!mergeable, "объединяются {a}/{a_len} и {b}/{b_len}");
        }
    }

    #[test]
    fn ipv4_list_excludes_private_and_special_ranges() {
        const RESERVED: [(Ipv4Addr, u8); 7] = [
            (Ipv4Addr::new(0, 0, 0, 0), 8),
            (Ipv4Addr::new(10, 0, 0, 0), 8),
            (Ipv4Addr::new(100, 64, 0, 0), 10),
            (Ipv4Addr::new(127, 0, 0, 0), 8),
            (Ipv4Addr::new(172, 16, 0, 0), 12),
            (Ipv4Addr::new(192, 168, 0, 0), 16),
            (Ipv4Addr::new(224, 0, 0, 0), 4),
        ];
        let ranges: Vec<(u64, u64)> = ru_ipv4().map(|(net, len)| range_of(net, len)).collect();
        for (net, len) in RESERVED {
            let (start, end) = range_of(net, len);
            assert!(
                ranges.iter().all(|&(s, e)| e <= start || s >= end),
                "пересечение с {net}/{len}"
            );
        }
    }

    #[test]
    fn data_date_is_iso_date() {
        let date = data_date().unwrap();
        assert_eq!(date.len(), 10);
        assert!(date.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        }));
    }

    #[test]
    fn ipv4_list_has_plausible_size() {
        let count = ru_ipv4().count();
        assert!((3000..=50000).contains(&count), "подсетей: {count}");
    }

    #[test]
    fn header_count_matches_lines() {
        let declared = IPV4_TEXT
            .lines()
            .find_map(|line| line.strip_prefix("# Подсетей: "))
            .unwrap();
        assert_eq!(declared.parse::<usize>().unwrap(), ru_ipv4().count());
    }

    #[test]
    fn foreign_resolvers_are_not_listed() {
        assert!(!ru_contains(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!ru_contains("1.1.1.1".parse().unwrap()));
    }
}
