//! Когда обновлять подписки и перезапускать xray.

use std::time::Duration;

use raycat_config::DEFAULT_UPDATE_INTERVAL;

/// Чаще панель не опрашивается, что бы ни просил провайдер.
const MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(10 * 60);
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

/// Пауза перед повтором после `failures`-й подряд неудачи (считая с единицы):
/// 30 с, 1 мин, 2 мин, … не дольше 10 мин.
pub(crate) fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    RETRY_FIRST
        .saturating_mul(1 << doublings)
        .min(RETRY_MAX)
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
    fn retries_back_off_from_thirty_seconds_to_ten_minutes() {
        let seconds: Vec<u64> = (1..=7).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, [30, 60, 120, 240, 480, 600, 600]);
        assert_eq!(retry_delay(0).as_secs(), 30);
        assert_eq!(retry_delay(u32::MAX).as_secs(), 600);
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
