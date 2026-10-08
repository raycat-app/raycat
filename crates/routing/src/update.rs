//! Сборка `data/ru-ipv4.txt` из статистики RIPE NCC (`delegated-ripencc-extended-latest`):
//! записи `RU` со статусом `allocated` или `assigned`, диапазоны адресов разбиваются на
//! CIDR и агрегируются.
//!
//! Сети инструмент не трогает: файл скачивает workflow и проверяет его до запуска.
//! Одинаковый вход даёт байт-в-байт одинаковый результат.

use std::fmt::Write as _;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

const SOURCE_URL: &str = "https://ftp.ripe.net/pub/stats/ripencc/delegated-ripencc-extended-latest";
const SPACE_END: u64 = 1 << 32;
/// Блок не крупнее /8: длины префиксов остаются в диапазоне 8..=32, который проверяет `ru_ipv4`.
const MAX_BLOCK: u64 = 1 << 24;

/// Итог сборки: дата снимка и число подсетей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub date: String,
    pub prefixes: usize,
}

/// Читает статистику RIPE из `input` и записывает подсети в `output`.
pub fn run(input: &Path, output: &Path) -> Result<Summary> {
    let text = fs::read_to_string(input).with_context(|| format!("чтение {}", input.display()))?;
    let (body, summary) = build(&text)?;
    fs::write(output, body).with_context(|| format!("запись {}", output.display()))?;
    Ok(summary)
}

fn build(text: &str) -> Result<(String, Summary)> {
    let (date, ranges) = parse(text)?;
    let prefixes = aggregate(ranges);
    let body = render(&date, &prefixes)?;
    Ok((
        body,
        Summary {
            date,
            prefixes: prefixes.len(),
        },
    ))
}

/// Дата снимка и диапазоны `[начало, конец)` записей `RU`.
fn parse(text: &str) -> Result<(String, Vec<(u64, u64)>)> {
    let mut lines = text.lines().filter(|line| !line.starts_with('#'));
    let header = lines.next().context("пустой файл статистики")?;
    let date = match header.split('|').collect::<Vec<_>>().as_slice() {
        ["2", "ripencc", serial, ..] => snapshot_date(serial)?,
        _ => bail!("заголовок статистики не распознан: нужны версия 2 и ripencc"),
    };

    let mut ranges = Vec::new();
    let mut ipv4_records: u64 = 0;
    let mut summary_count = None;
    for line in lines {
        let fields: Vec<&str> = line.split('|').collect();
        match fields.as_slice() {
            ["ripencc", "*", "ipv4", "*", count, "summary"] => {
                summary_count = Some(count.parse::<u64>().context("число записей в summary")?);
            }
            ["ripencc", cc, "ipv4", start, count, _, status, ..] => {
                ipv4_records += 1;
                if *cc == "RU" && matches!(*status, "allocated" | "assigned") {
                    ranges.push(record_range(start, count)?);
                }
            }
            _ => {}
        }
    }

    let summary_count = summary_count.context("в статистике нет строки summary для ipv4")?;
    ensure!(
        summary_count == ipv4_records,
        "записей ipv4: {ipv4_records}, а в summary: {summary_count}"
    );
    ensure!(!ranges.is_empty(), "в статистике нет записей RU с ipv4");
    Ok((date, ranges))
}

fn snapshot_date(serial: &str) -> Result<String> {
    ensure!(
        serial.len() == 8 && serial.bytes().all(|b| b.is_ascii_digit()),
        "дата снимка не в формате ГГГГММДД: {serial}"
    );
    Ok(format!(
        "{}-{}-{}",
        &serial[..4],
        &serial[4..6],
        &serial[6..]
    ))
}

fn record_range(start: &str, count: &str) -> Result<(u64, u64)> {
    let start: Ipv4Addr = start
        .parse()
        .with_context(|| format!("адрес записи: {start}"))?;
    let count: u64 = count
        .parse()
        .with_context(|| format!("число адресов: {count}"))?;
    ensure!(count > 0, "запись {start} с нулевым числом адресов");
    let first = u64::from(u32::from(start));
    let end = first
        .checked_add(count)
        .context("диапазон выходит за пределы IPv4")?;
    ensure!(end <= SPACE_END, "диапазон {start} выходит за пределы IPv4");
    Ok((first, end))
}

/// Объединяет соседние и перекрывающиеся диапазоны и разбивает их на CIDR.
fn aggregate(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
        .into_iter()
        .flat_map(|(start, end)| split(start, end))
        .collect()
}

/// Минимальное разбиение `[start, end)` на блоки CIDR (адрес сети, длина префикса).
fn split(mut start: u64, end: u64) -> Vec<(u64, u32)> {
    let mut blocks = Vec::new();
    while start < end {
        let align = if start == 0 {
            SPACE_END
        } else {
            1 << start.trailing_zeros()
        };
        let fit: u64 = 1 << (63 - (end - start).leading_zeros());
        let size = align.min(fit).min(MAX_BLOCK);
        blocks.push((start, 32 - size.trailing_zeros()));
        start += size;
    }
    blocks
}

fn render(date: &str, prefixes: &[(u64, u32)]) -> Result<String> {
    let mut body = String::new();
    writeln!(
        body,
        "# Российские IPv4-подсети: код страны RU в статистике RIPE NCC (allocated и assigned), агрегированные."
    )?;
    writeln!(body, "# Источник: {SOURCE_URL}")?;
    writeln!(body, "# Снимок: {date}")?;
    writeln!(body, "# Подсетей: {}", prefixes.len())?;
    writeln!(
        body,
        "# Файл создаёт инструмент update-ru-ipv4, вручную не править."
    )?;
    for &(start, len) in prefixes {
        let addr = Ipv4Addr::from(u32::try_from(start)?);
        writeln!(body, "{addr}/{len}")?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# Фикстура без сети
2|ripencc|20260101|4|19830101|20260101|+0000
ripencc|*|ipv4|*|4|summary
ripencc|RU|ipv4|10.0.0.0|256|20100101|allocated|x
ripencc|RU|ipv4|10.0.1.0|256|20100101|assigned|x
ripencc|NL|ipv4|192.0.2.0|256|20100101|allocated|x
ripencc||ipv4|198.51.100.0|256|||available
";

    const FIXTURE_REVERSED: &str = "\
2|ripencc|20260101|4|19830101|20260101|+0000
ripencc||ipv4|198.51.100.0|256|||available
ripencc|NL|ipv4|192.0.2.0|256|20100101|allocated|x
ripencc|RU|ipv4|10.0.1.0|256|20100101|assigned|x
ripencc|RU|ipv4|10.0.0.0|256|20100101|allocated|x
ripencc|*|ipv4|*|4|summary
";

    fn ip(text: &str) -> u64 {
        u64::from(u32::from(text.parse::<Ipv4Addr>().unwrap()))
    }

    fn prefixes_of(body: &str) -> Vec<&str> {
        body.lines().filter(|line| !line.starts_with('#')).collect()
    }

    #[test]
    fn builds_aggregated_body_from_fixture() {
        let (body, summary) = build(FIXTURE).unwrap();
        assert_eq!(
            summary,
            Summary {
                date: "2026-01-01".to_owned(),
                prefixes: 1,
            }
        );
        assert_eq!(prefixes_of(&body), ["10.0.0.0/23"]);
        assert!(body.contains("# Снимок: 2026-01-01\n"));
        assert!(body.ends_with("10.0.0.0/23\n"));
    }

    #[test]
    fn output_does_not_depend_on_record_order() {
        let (forward, _) = build(FIXTURE).unwrap();
        let (reversed, _) = build(FIXTURE_REVERSED).unwrap();
        assert_eq!(forward, reversed);
    }

    #[test]
    fn split_is_minimal_cidr_cover() {
        assert_eq!(split(1, 4), [(1, 32), (2, 31)]);
        assert_eq!(
            split(ip("10.0.0.0"), ip("10.0.0.0") + 768),
            [(ip("10.0.0.0"), 23), (ip("10.0.2.0"), 24)]
        );
        assert_eq!(
            split(ip("10.0.0.0"), ip("10.0.0.0") + 256),
            [(ip("10.0.0.0"), 24)]
        );
    }

    #[test]
    fn split_caps_blocks_at_slash_eight() {
        assert_eq!(split(0, 1 << 25), [(0, 8), (1 << 24, 8)]);
    }

    #[test]
    fn aggregate_merges_adjacent_and_overlapping_ranges() {
        let ranges = vec![
            (ip("10.0.1.0"), ip("10.0.1.0") + 256),
            (ip("10.0.0.0"), ip("10.0.0.0") + 256),
            (ip("10.0.0.128"), ip("10.0.0.128") + 128),
        ];
        assert_eq!(aggregate(ranges), [(ip("10.0.0.0"), 23)]);
    }

    #[test]
    fn rejects_summary_mismatch() {
        let bad = FIXTURE.replace("|4|summary", "|5|summary");
        assert!(build(&bad).is_err());
    }

    #[test]
    fn rejects_range_past_end_of_ipv4() {
        let bad = FIXTURE.replace("10.0.1.0|256", "255.255.255.0|512");
        assert!(build(&bad).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let bad = FIXTURE.replace("2|ripencc|", "3|ripencc|");
        assert!(build(&bad).is_err());
    }

    #[test]
    fn rejects_statistics_without_ru_records() {
        let bad = FIXTURE.replace("|RU|", "|DE|");
        assert!(build(&bad).is_err());
    }

    #[test]
    fn rejects_malformed_snapshot_date() {
        assert!(snapshot_date("2026-01-01").is_err());
        assert_eq!(snapshot_date("20260101").unwrap(), "2026-01-01");
    }
}
