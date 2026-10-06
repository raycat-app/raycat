//! Когда обновлять подписки и перезапускать xray.

use std::time::Duration;

use raycat_config::DEFAULT_UPDATE_INTERVAL;

/// Чаще панель не опрашивается, что бы ни просил провайдер.
const MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);
const RETRY_FIRST: Duration = Duration::from_secs(30);
/// Узлы уже есть (кэш), xray работает: панель можно не беспокоить чаще.
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// Без кэша xray стоит, пока подписка не получена: после сбоя связи ждать долго нельзя.
const RETRY_MAX_COLD: Duration = Duration::from_secs(10 * 60);
const RETRY_AFTER_MAX: Duration = Duration::from_secs(24 * 3_600);
/// Ответ провайдера, который применить нельзя (заглушка, 4xx), перепроверяется не реже.
const REJECTED_RECHECK_MAX: Duration = Duration::from_secs(3_600);
pub(crate) const RESTART_FIRST: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(30);
/// Процесс, проработавший столько, считается здоровым: пауза перед перезапуском
/// снова начинается с минимальной.
pub(crate) const HEALTHY_UPTIME: Duration = Duration::from_secs(60);

/// Настройка подписки, иначе `profile-update-interval` провайдера, иначе 12 часов.
pub(crate) fn interval(configured: Option<Duration>, provider: Option<Duration>) -> Duration {
    configured
        .or(provider)
        .unwrap_or(DEFAULT_UPDATE_INTERVAL)
        .max(MIN_INTERVAL)
}

/// Пауза перед повтором после `failures`-го подряд сбоя связи или панели (считая с
/// единицы): 30 с, 1 мин, 2 мин, … не дольше 30 мин, а без кэша — не дольше 10 мин.
pub(crate) fn retry_delay(failures: u32, cached: bool) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    let cap = if cached { RETRY_MAX } else { RETRY_MAX_COLD };
    RETRY_FIRST.saturating_mul(1 << doublings).min(cap)
}

/// Статусы, после которых панель, скорее всего, скоро оправится: повтор с нарастающей
/// паузой. Остальные не-2xx — ответ панели по существу (нет подписки, нет доступа,
/// клиент не пускают): долбить её бессмысленно, следующая попытка по обычному интервалу.
pub(crate) fn is_outage(status: u16) -> bool {
    matches!(status, 408 | 500..=599)
}

/// Пауза после ответа провайдера, который нельзя применить: обычный интервал, но не
/// дольше часа (не короче 10 минут, это гарантирует [`interval`]), и не раньше
/// срока из `Retry-After`.
pub(crate) fn rejected_delay(regular: Duration, retry_after: Option<&str>) -> Duration {
    after_retry_header(regular.min(REJECTED_RECHECK_MAX), retry_after)
}

/// Не раньше срока из `Retry-After` (в секундах, не больше суток): дату HTTP не разбираем,
/// такой заголовок остаётся без внимания.
pub(crate) fn after_retry_header(delay: Duration, header: Option<&str>) -> Duration {
    let requested = header
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs).min(RETRY_AFTER_MAX));
    requested.map_or(delay, |requested| delay.max(requested))
}

/// Следующая пауза перед перезапуском: вдвое дольше предыдущей, не дольше 30 с.
pub(crate) fn next_restart_delay(previous: Duration) -> Duration {
    previous.saturating_mul(2).min(RESTART_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600;

    #[test]
    fn configured_interval_wins_over_the_provider() {
        let interval = interval(
            Some(Duration::from_secs(6 * HOUR)),
            Some(Duration::from_secs(24 * HOUR)),
        );
        assert_eq!(interval, Duration::from_secs(6 * HOUR));
    }

    #[test]
    fn provider_interval_is_used_without_a_setting() {
        let interval = interval(None, Some(Duration::from_secs(3 * HOUR)));
        assert_eq!(interval, Duration::from_secs(3 * HOUR));
    }

    #[test]
    fn default_is_twelve_hours() {
        assert_eq!(interval(None, None), Duration::from_secs(12 * HOUR));
    }

    #[test]
    fn interval_is_never_below_ten_minutes() {
        assert_eq!(
            interval(Some(Duration::from_secs(1)), None),
            Duration::from_secs(600)
        );
        assert_eq!(
            interval(None, Some(Duration::from_secs(60))),
            Duration::from_secs(600)
        );
        assert_eq!(
            interval(None, Some(Duration::from_secs(600))),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn retries_back_off_from_thirty_seconds_to_half_an_hour() {
        let seconds: Vec<u64> = (1..=9).map(|n| retry_delay(n, true).as_secs()).collect();
        assert_eq!(seconds, [30, 60, 120, 240, 480, 960, 1_800, 1_800, 1_800]);
        assert_eq!(retry_delay(0, true).as_secs(), 30);
        assert_eq!(retry_delay(u32::MAX, true).as_secs(), 1_800);
    }

    #[test]
    fn without_a_cache_retries_stop_at_ten_minutes() {
        let seconds: Vec<u64> = (1..=7).map(|n| retry_delay(n, false).as_secs()).collect();
        assert_eq!(seconds, [30, 60, 120, 240, 480, 600, 600]);
        assert_eq!(retry_delay(u32::MAX, false).as_secs(), 600);
    }

    #[test]
    fn only_server_side_statuses_are_outages() {
        for status in [408, 500, 502, 503, 504, 599] {
            assert!(is_outage(status), "{status}");
        }
        for status in [400, 401, 403, 404, 410, 429, 451, 301, 200] {
            assert!(!is_outage(status), "{status}");
        }
    }

    #[test]
    fn retry_after_only_postpones() {
        let delay = Duration::from_secs(120);
        assert_eq!(after_retry_header(delay, None), delay);
        assert_eq!(after_retry_header(delay, Some("30")), delay);
        assert_eq!(
            after_retry_header(delay, Some(" 3600 ")),
            Duration::from_secs(3_600)
        );
        assert_eq!(after_retry_header(delay, Some("0")), delay);
    }

    #[test]
    fn retry_after_is_capped_and_garbage_is_ignored() {
        let delay = Duration::from_secs(120);
        assert_eq!(
            after_retry_header(delay, Some("99999999999")),
            Duration::from_secs(24 * HOUR)
        );
        for garbage in ["", "завтра", "-5", "Wed, 21 Oct 2026 07:28:00 GMT", "1.5"] {
            assert_eq!(after_retry_header(delay, Some(garbage)), delay, "{garbage}");
        }
    }

    #[test]
    fn the_regular_interval_is_never_shortened_by_retry_after() {
        let regular = interval(None, None);
        assert_eq!(after_retry_header(regular, Some("60")), regular);
    }

    #[test]
    fn a_rejected_answer_is_rechecked_within_an_hour() {
        let delay = |regular_secs: u64, header: Option<&str>| {
            rejected_delay(Duration::from_secs(regular_secs), header).as_secs()
        };
        assert_eq!(delay(12 * HOUR, None), HOUR);
        assert_eq!(delay(HOUR, None), HOUR);
        assert_eq!(delay(1_800, None), 1_800);
        assert_eq!(delay(600, None), 600);
    }

    #[test]
    fn retry_after_still_postpones_a_rejected_answer() {
        let delay = |header: &str| rejected_delay(interval(None, None), Some(header)).as_secs();
        assert_eq!(delay("60"), HOUR);
        assert_eq!(delay("7200"), 2 * HOUR);
        assert_eq!(delay("999999999"), 24 * HOUR);
    }

    #[test]
    fn restarts_back_off_from_one_second_to_thirty() {
        let mut delay = RESTART_FIRST;
        let mut seen = vec![delay.as_secs()];
        for _ in 0..6 {
            delay = next_restart_delay(delay);
            seen.push(delay.as_secs());
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 30, 30]);
    }
}
