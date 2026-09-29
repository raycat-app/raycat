use std::time::Duration;

/// `500ms`, `30s`, `5m`, `6h`, `1d` и их сочетания вроде `1h30m`.
pub(crate) fn parse_duration(input: &str) -> Option<Duration> {
    let lower = input.trim().to_ascii_lowercase();
    let mut rest = lower.as_str();
    if rest.is_empty() {
        return None;
    }
    let mut total = Duration::ZERO;
    while !rest.is_empty() {
        let (number, after) = split_number(rest)?;
        let unit_len = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let part = match after.get(..unit_len)? {
            "ms" => Duration::from_millis(number),
            "s" => Duration::from_secs(number),
            "m" => Duration::from_secs(number.checked_mul(60)?),
            "h" => Duration::from_secs(number.checked_mul(3_600)?),
            "d" => Duration::from_secs(number.checked_mul(86_400)?),
            _ => return None,
        };
        total = total.checked_add(part)?;
        rest = after.get(unit_len..)?;
    }
    Some(total)
}

/// `512KiB`, `48MiB`, `2GiB` или число байт с суффиксом `B`.
pub(crate) fn parse_size(input: &str) -> Option<u64> {
    let lower = input.trim().to_ascii_lowercase();
    let (number, unit) = split_number(&lower)?;
    let factor: u64 = match unit {
        "b" => 1,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        _ => return None,
    };
    number.checked_mul(factor)
}

fn split_number(text: &str) -> Option<(u64, &str)> {
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text.get(..digits)?.parse().ok()?;
    Some((number, text.get(digits..)?))
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    for (suffix, unit) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("m", 60_000),
        ("s", 1_000),
    ] {
        let count = millis / unit;
        if count > 0 && count * unit == millis {
            return format!("{count}{suffix}");
        }
    }
    format!("{millis}ms")
}

pub(crate) fn format_size(bytes: u64) -> String {
    for (suffix, unit) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        let count = bytes / unit;
        if count > 0 && count * unit == bytes {
            return format!("{count}{suffix}");
        }
    }
    format!("{bytes}B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("150ms"), Some(Duration::from_millis(150)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("6h"), Some(Duration::from_secs(6 * 3_600)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration(" 1H30M "), Some(Duration::from_secs(5_400)));
        assert_eq!(parse_duration("0s"), Some(Duration::ZERO));
    }

    #[test]
    fn bad_durations() {
        for bad in ["", " ", "30", "s", "5x", "1.5h", "-5m", "5 m", "1h 30m", "мин"] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
        assert_eq!(parse_duration("99999999999999999999s"), None);
        assert_eq!(parse_duration("18446744073709551615d"), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("48MiB"), Some(48 << 20));
        assert_eq!(parse_size("512kib"), Some(512 << 10));
        assert_eq!(parse_size("2GiB"), Some(2 << 30));
        assert_eq!(parse_size("100B"), Some(100));
    }

    #[test]
    fn bad_sizes() {
        for bad in ["", "48", "MiB", "48MB", "1.5GiB", "-1MiB", "48 MiB"] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
        assert_eq!(parse_size("18446744073709551615GiB"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_duration(Duration::from_millis(150)), "150ms");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration::from_secs(30 * 86_400)), "30d");
        assert_eq!(format_duration(Duration::ZERO), "0ms");
        assert_eq!(format_size(48 << 20), "48MiB");
        assert_eq!(format_size(16 << 30), "16GiB");
        assert_eq!(format_size(1_500), "1500B");
    }
}
