//! Мелкие помощники: время, размеры, хеш, очистка текста.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

const MINUTE: u64 = 60;
const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Часовой пояс сервера: `TZ` или `/etc/localtime`, без них UTC.
pub(crate) fn local_zone() -> &'static TimeZone {
    static ZONE: OnceLock<TimeZone> = OnceLock::new();
    ZONE.get_or_init(|| TimeZone::try_system().unwrap_or(TimeZone::UTC))
}

/// Пометка «время UTC» нужна только там, где часы действительно идут по UTC.
pub(crate) fn is_utc(zone: &TimeZone) -> bool {
    *zone == TimeZone::UTC || matches!(zone.iana_name(), Some("UTC" | "Etc/UTC"))
}

fn local(unix: u64, zone: &TimeZone) -> Zoned {
    let secs = i64::try_from(unix).unwrap_or(i64::MAX);
    Timestamp::from_second(secs)
        .unwrap_or(Timestamp::MAX)
        .to_zoned(zone.clone())
}

/// Момент в местном времени: `16:43:05` сегодня, `08.10 16:43` в этом году,
/// `08.10.2025 16:43` в другие годы. `now` определяет, что считать сегодняшним днём.
pub(crate) fn format_moment(unix: u64, now: u64, zone: &TimeZone) -> String {
    let at = local(unix, zone);
    let today = local(now, zone);
    let clock = format!("{:02}:{:02}", at.hour(), at.minute());
    if at.date() == today.date() {
        format!("{clock}:{:02}", at.second())
    } else if at.year() == today.year() {
        format!("{:02}.{:02} {clock}", at.day(), at.month())
    } else {
        format!("{:02}.{:02}.{:04} {clock}", at.day(), at.month(), at.year())
    }
}

/// Полная дата и время для журнала: `2026-10-08 16:43:05`.
pub(crate) fn format_stamp(unix: u64, zone: &TimeZone) -> String {
    let at = local(unix, zone);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        at.year(),
        at.month(),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// Дата срока подписки: `08.10.2026`.
pub(crate) fn format_date(unix: u64, zone: &TimeZone) -> String {
    let at = local(unix, zone);
    format!("{:02}.{:02}.{:04}", at.day(), at.month(), at.year())
}

/// Момент в местном времени относительно текущего: для журнала и вывода команд.
pub(crate) fn moment(unix: u64) -> String {
    format_moment(unix, now_unix(), local_zone())
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

/// Старшая единица и следующая за ней, нулевая опускается: `45 с`, `3 мин 20 с`,
/// `2 ч`, `2 ч 15 мин`, `5 дн 3 ч`.
pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= DAY {
        pair(secs / DAY, "дн", secs % DAY / HOUR, "ч")
    } else if secs >= HOUR {
        pair(secs / HOUR, "ч", secs % HOUR / MINUTE, "мин")
    } else if secs >= MINUTE {
        pair(secs / MINUTE, "мин", secs % MINUTE, "с")
    } else {
        format!("{secs} с")
    }
}

fn pair(major: u64, major_unit: &str, minor: u64, minor_unit: &str) -> String {
    if minor == 0 {
        format!("{major} {major_unit}")
    } else {
        format!("{major} {major_unit} {minor} {minor_unit}")
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

/// Не больше `max` символов; обрезанный текст заканчивается многоточием.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use jiff::tz::offset;

    use super::*;

    const NOW: u64 = 1_790_596_800;

    #[test]
    fn truncation_keeps_short_text_and_marks_the_cut() {
        assert_eq!(truncate("коротко", 7), "коротко");
        assert_eq!(truncate("длинный текст", 8), "длинный…");
        assert_eq!(truncate("раз два три", 5), "раз…");
        assert_eq!(truncate("абвгд", 0), "…");
    }

    #[test]
    fn moment_shows_clock_today_and_more_below() {
        let utc = &TimeZone::UTC;
        assert_eq!(format_moment(NOW + 3_661, NOW, utc), "13:01:01");
        assert_eq!(format_moment(NOW - DAY, NOW, utc), "27.09 12:00");
        assert_eq!(format_moment(1_759_941_785, NOW, utc), "08.10.2025 16:43");
        assert_eq!(format_moment(951_782_400, NOW, utc), "29.02.2000 00:00");
        assert_eq!(format_moment(0, NOW, utc), "01.01.1970 00:00");
    }

    #[test]
    fn the_zone_decides_the_clock_and_the_day() {
        let late = NOW + 37_800; // 2026-09-28 22:30 UTC
        let moscow = TimeZone::fixed(offset(3));
        assert_eq!(format_moment(late, NOW, &TimeZone::UTC), "22:30:00");
        assert_eq!(format_moment(late, NOW, &moscow), "29.09 01:30");
        assert_eq!(format_date(late, &TimeZone::UTC), "28.09.2026");
        assert_eq!(format_date(late, &moscow), "29.09.2026");
    }

    #[test]
    fn log_stamp_has_the_full_date_in_the_zone() {
        let late = NOW + 37_800;
        let moscow = TimeZone::fixed(offset(3));
        assert_eq!(format_stamp(0, &TimeZone::UTC), "1970-01-01 00:00:00");
        assert_eq!(format_stamp(NOW, &TimeZone::UTC), "2026-09-28 12:00:00");
        assert_eq!(format_stamp(late, &moscow), "2026-09-29 01:30:00");
    }

    #[test]
    fn expiry_date_is_written_day_month_year() {
        assert_eq!(format_date(NOW, &TimeZone::UTC), "28.09.2026");
        assert_eq!(format_date(NOW + 40 * DAY, &TimeZone::UTC), "07.11.2026");
    }

    #[test]
    fn only_a_real_utc_clock_is_labelled_as_utc() {
        assert!(is_utc(&TimeZone::UTC));
        assert!(!is_utc(&TimeZone::fixed(offset(3))));
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
    fn durations_keep_two_units_and_drop_zeros() {
        let secs = |value: u64| format_duration(Duration::from_secs(value));
        assert_eq!(secs(0), "0 с");
        assert_eq!(secs(45), "45 с");
        assert_eq!(secs(59), "59 с");
        assert_eq!(secs(60), "1 мин");
        assert_eq!(secs(200), "3 мин 20 с");
        assert_eq!(secs(3_599), "59 мин 59 с");
        assert_eq!(secs(3_600), "1 ч");
        assert_eq!(secs(7_230), "2 ч");
        assert_eq!(secs(8_100), "2 ч 15 мин");
        assert_eq!(secs(86_399), "23 ч 59 мин");
        assert_eq!(secs(86_400), "1 дн");
        assert_eq!(secs(100_000), "1 дн 3 ч");
        assert_eq!(secs(442_800), "5 дн 3 ч");
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
