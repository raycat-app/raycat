//! Моменты захвата из `clock.txt`: часы раннера или эмулятора в UTC.

/// Момент в секундах Unix и строка, из которой он получен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Instant {
    pub(super) unix: u64,
    pub(super) label: String,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Все моменты, которые удалось прочитать: `2026-09-30T00:03:04.0878601Z` (Windows)
/// и `Wed Sep 30 00:02:04 UTC 2026` (вывод `date -u` на Android).
pub(super) fn parse(text: &str) -> Vec<Instant> {
    text.lines()
        .filter_map(|line| {
            let unix = iso_utc(line).or_else(|| date_u(line))?;
            Some(Instant {
                unix,
                label: printable(line),
            })
        })
        .collect()
}

fn printable(line: &str) -> String {
    line.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn is_iso_at(window: &[u8]) -> bool {
    window.len() == 19
        && window.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            _ => b.is_ascii_digit(),
        })
}

fn iso_utc(line: &str) -> Option<u64> {
    let bytes = line.as_bytes();
    let start = (0..bytes.len().saturating_sub(18)).find(|&i| is_iso_at(&bytes[i..i + 19]))?;
    let stamp = &line[start..start + 19];
    let rest = line[start + 19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if !rest.starts_with('Z') {
        return None;
    }
    let number = |from: usize, to: usize| stamp[from..to].parse::<i64>().ok();
    seconds(
        number(0, 4)?,
        number(5, 7)?,
        number(8, 10)?,
        [number(11, 13)?, number(14, 16)?, number(17, 19)?],
    )
}

fn date_u(line: &str) -> Option<u64> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let [_weekday, month, day, time, zone, year] = parts.as_slice() else {
        return None;
    };
    if *zone != "UTC" {
        return None;
    }
    let month = i64::try_from(MONTHS.iter().position(|name| name == month)? + 1).ok()?;
    let clock: Vec<i64> = time
        .split(':')
        .map(|part| part.parse::<i64>().ok())
        .collect::<Option<_>>()?;
    let [hour, minute, second] = clock.as_slice() else {
        return None;
    };
    seconds(
        year.parse().ok()?,
        month,
        day.parse().ok()?,
        [*hour, *minute, *second],
    )
}

fn seconds(year: i64, month: i64, day: i64, [hour, minute, second]: [i64; 3]) -> Option<u64> {
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..61).contains(&second);
    if !valid {
        return None;
    }
    let total = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(total).ok()
}

/// Число дней от 1970-01-01 до гражданской даты (алгоритм Говарда Хиннанта).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-09-28 12:00:00 UTC.
    const EVEN_DAY: u64 = 1_790_596_800;

    #[test]
    fn reads_the_windows_clock_file() {
        let text = "before link: 2026-09-28T12:00:00.0021731Z\r\nafter wait: 2026-09-28T12:00:25.0372839Z\r\n";
        let instants = parse(text);
        assert_eq!(
            instants.iter().map(|i| i.unix).collect::<Vec<_>>(),
            [EVEN_DAY, EVEN_DAY + 25]
        );
        assert_eq!(
            instants[0].label,
            "before link: 2026-09-28T12:00:00.0021731Z"
        );
    }

    #[test]
    fn reads_the_android_clock_file() {
        let instants = parse("Mon Sep 28 12:00:00 UTC 2026\n");
        assert_eq!(instants.len(), 1);
        assert_eq!(instants[0].unix, EVEN_DAY);
        assert_eq!(parse("Thu Jan  1 00:00:00 UTC 1970")[0].unix, 0);
    }

    #[test]
    fn leap_days_and_month_ends_are_counted() {
        // 2028-02-29 00:00:00 UTC.
        assert_eq!(parse("2028-02-29T00:00:00Z")[0].unix, 1_835_395_200);
        assert_eq!(parse("2026-12-31T23:59:59Z")[0].unix, 1_798_761_599);
    }

    #[test]
    fn rubbish_gives_no_instants() {
        assert_eq!(parse(""), Vec::<Instant>::new());
        assert_eq!(parse("before link: сегодня"), Vec::<Instant>::new());
        assert_eq!(parse("2026-13-40T25:61:61Z"), Vec::<Instant>::new());
        assert_eq!(parse("2026-09-28T12:00:00"), Vec::<Instant>::new());
        assert_eq!(parse("Mon Sep 28 12:00:00 MSK 2026"), Vec::<Instant>::new());
        assert_eq!(parse("Mon Sep 28 12:00 UTC 2026"), Vec::<Instant>::new());
    }
}
