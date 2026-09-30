//! Дневной маркер User-Agent Happ.

const MOSCOW_OFFSET: u64 = 3 * 3600;

/// Маркер Happ для Windows из бинарника: `(day(now_utc + 10800) & 1) ? '5' : '6'`,
/// то есть меняется каждые сутки по московскому времени (UTC+3) в зависимости от
/// чётности числа месяца, каков бы ни был часовой пояс компьютера.
pub(crate) fn moscow_day_parity(unix: u64) -> char {
    day_parity(unix, MOSCOW_OFFSET)
}

/// Маркер Happ для Android: чётность числа месяца по местной дате устройства
/// (проверено на эмуляторе в UTC). raycat эмулирует телефон в часовом поясе
/// Москвы, поэтому местная дата совпадает с московской.
pub(crate) fn device_local_day_parity(unix: u64) -> char {
    day_parity(unix, MOSCOW_OFFSET)
}

fn day_parity(unix: u64, offset: u64) -> char {
    if day_of_month(unix.saturating_add(offset)) % 2 == 1 {
        '5'
    } else {
        '6'
    }
}

/// Число месяца (1..=31) по UTC для момента `unix`.
fn day_of_month(unix: u64) -> u64 {
    // Гражданская дата из числа дней: алгоритм Говарда Хиннанта, смещение 719468
    // переносит начало отсчёта на 0000-03-01, чтобы високосный день был последним.
    let z = unix / 86_400 + 719_468;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    day_of_year - (153 * month + 2) / 5 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    // 2026-01-01 00:00:00 UTC.
    const JAN_1_2026: u64 = 1_767_225_600;

    #[test]
    fn day_of_month_follows_the_calendar() {
        assert_eq!(day_of_month(0), 1);
        assert_eq!(day_of_month(DAY - 1), 1);
        assert_eq!(day_of_month(DAY), 2);
        // 2000-02-29: високосный год, делящийся на 400.
        assert_eq!(day_of_month(951_782_400), 29);
        assert_eq!(day_of_month(951_782_400 + DAY), 1);
        assert_eq!(day_of_month(JAN_1_2026), 1);
        assert_eq!(day_of_month(JAN_1_2026 + 30 * DAY), 31);
        assert_eq!(day_of_month(JAN_1_2026 + 31 * DAY), 1);
        // 2026 не високосный: за 28 февраля сразу 1 марта.
        assert_eq!(day_of_month(JAN_1_2026 + (31 + 27) * DAY), 28);
        assert_eq!(day_of_month(JAN_1_2026 + (31 + 28) * DAY), 1);
        // 2028-02-29.
        let jan_1_2028 = JAN_1_2026 + 730 * DAY;
        assert_eq!(day_of_month(jan_1_2028 + (31 + 28) * DAY), 29);
        assert_eq!(day_of_month(jan_1_2028 + (31 + 29) * DAY), 1);
        // Последний день года.
        assert_eq!(day_of_month(JAN_1_2026 + 364 * DAY), 31);
        assert_eq!(day_of_month(JAN_1_2026 + 365 * DAY), 1);
    }

    #[test]
    fn marker_depends_on_the_parity_of_the_moscow_date() {
        // 2026-09-25 21:20:27 UTC — уже 26 сентября по Москве: чётное число.
        assert_eq!(moscow_day_parity(1_790_371_227), '6');
        assert_eq!(moscow_day_parity(1_790_371_227 - DAY), '5');
        assert_eq!(moscow_day_parity(1_790_371_227 + DAY), '5');
    }

    #[test]
    fn marker_flips_at_moscow_midnight() {
        // 2026-09-25 21:00:00 UTC — полночь по Москве, начало 26 сентября.
        let midnight = 1_790_370_000;
        assert_eq!(moscow_day_parity(midnight - 1), '5');
        assert_eq!(moscow_day_parity(midnight), '6');
        assert_eq!(moscow_day_parity(midnight + DAY - 1), '6');
        assert_eq!(moscow_day_parity(midnight + DAY), '5');
    }

    #[test]
    fn marker_repeats_across_month_boundary() {
        // 31 января и 1 февраля — оба нечётные: подряд два дня одинаковый маркер.
        let noon_msk = 9 * 3600;
        let jan_31 = JAN_1_2026 + 30 * DAY + noon_msk;
        assert_eq!(moscow_day_parity(jan_31), '5');
        assert_eq!(moscow_day_parity(jan_31 + DAY), '5');
        assert_eq!(moscow_day_parity(jan_31 + 2 * DAY), '6');
    }
}
