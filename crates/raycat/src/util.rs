//! Мелкие помощники: время, размеры, хеш, очистка текста.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Алгоритм Говарда Хиннанта: дни с 1970-01-01 в (год, месяц, день).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-09-30T12:00:00Z`
pub(crate) fn format_time(unix: u64) -> String {
    let secs = i64::try_from(unix).unwrap_or(i64::MAX);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// `2026-09-30`
pub(crate) fn format_date(unix: u64) -> String {
    format_time(unix).chars().take(10).collect()
}

/// Двоичные единицы с одним знаком после точки: `1.5 КиБ`.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"];
    let mut whole = bytes;
    let mut tenths = 0;
    let mut unit = 0;
    while whole >= 1024 && unit + 1 < UNITS.len() {
        tenths = whole % 1024 * 10 / 1024;
        whole /= 1024;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or_default();
    if unit == 0 {
        format!("{whole} {name}")
    } else {
        format!("{whole}.{tenths} {name}")
    }
}

/// Две старшие единицы: `1 д 4 ч`, `5 мин 30 с`.
pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (days, hours, minutes, seconds) = (
        secs / 86_400,
        secs % 86_400 / 3_600,
        secs % 3_600 / 60,
        secs % 60,
    );
    if days > 0 {
        format!("{days} д {hours} ч")
    } else if hours > 0 {
        format!("{hours} ч {minutes} мин")
    } else if minutes > 0 {
        format!("{minutes} мин {seconds} с")
    } else {
        format!("{seconds} с")
    }
}

/// FNV-1a: не криптографический отпечаток для имён файлов и ключей кэша.
pub(crate) fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Управляющие символы (в том числе переводы строк и escape-последовательности
/// терминала) заменяются пробелами: текст от провайдера и xray попадает в лог.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time_in_utc() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_time(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_time(1_790_596_800), "2026-09-28T12:00:00Z");
        assert_eq!(format_date(1_790_596_800 + 43_199), "2026-09-28");
        assert_eq!(format_date(1_790_596_800 + 43_200), "2026-09-29");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(0), "0 Б");
        assert_eq!(format_bytes(1023), "1023 Б");
        assert_eq!(format_bytes(1536), "1.5 КиБ");
        assert_eq!(format_bytes(100 * 1024 * 1024 * 1024), "100.0 ГиБ");
        assert!(format_bytes(u64::MAX).ends_with("ТиБ"));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_secs(7)), "7 с");
        assert_eq!(format_duration(Duration::from_secs(330)), "5 мин 30 с");
        assert_eq!(
            format_duration(Duration::from_secs(12 * 3_600)),
            "12 ч 0 мин"
        );
        assert_eq!(format_duration(Duration::from_secs(100_000)), "1 д 3 ч");
    }

    #[test]
    fn fnv1a_matches_known_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn sanitize_replaces_control_characters() {
        assert_eq!(sanitize("a\r\nb\u{1b}[31mc"), "a  b [31mc");
        assert_eq!(sanitize("обычный текст"), "обычный текст");
    }
}
