//! Тексты журнала об обновлении подписок. Уровень решает вызывающий (`log::Latch`):
//! о новом состоянии пишется предупреждением, о неизменном — на уровне debug.

use std::time::Duration;

use raycat_subscription::SKIPPED_STUBS;

use crate::log::Level;
use crate::util::{format_duration, truncate};

/// Тексты провайдера (названия узлов-заглушек, причины отказа) бывают длинными.
const PROVIDER_TEXT_MAX: usize = 200;
/// Замечаний к одному ответу в журнале — не больше; остальные видны в `raycat fetch`.
const MAX_WARNING_LINES: usize = 5;

pub(super) fn rejected(name: &str, reason: &str, next_in: Duration) -> String {
    format!(
        "подписка «{name}»: ответ не применён ({}), остаются прежние узлы; следующая попытка через {}",
        truncate(reason, PROVIDER_TEXT_MAX),
        format_duration(next_in)
    )
}

pub(super) fn failed(name: &str, message: &str, next_in: Duration) -> String {
    format!(
        "подписка «{name}»: не удалось обновить ({}); повтор через {}",
        truncate(message, PROVIDER_TEXT_MAX),
        format_duration(next_in)
    )
}

pub(super) fn recovered(name: &str, summary: &str, next_in: Duration) -> String {
    format!(
        "подписка «{name}» снова работает: {summary}; следующее обновление через {}",
        format_duration(next_in)
    )
}

pub(super) fn updated(name: &str, summary: &str, next_in: Duration) -> String {
    format!(
        "подписка «{name}» обновлена: {summary}; следующее обновление через {}",
        format_duration(next_in)
    )
}

/// Замечания, которых не было в предыдущем ответе. Узлы-заглушки рядом с рабочими
/// (разделители провайдера вроде «обходы ниже») — обычная часть подписки, они идут
/// на уровне info; остальное — предупреждения.
pub(super) fn new_warnings(
    name: &str,
    previous: &[String],
    current: &[String],
) -> Vec<(Level, String)> {
    let fresh: Vec<&String> = current
        .iter()
        .filter(|warning| !previous.contains(warning))
        .collect();
    let mut lines: Vec<(Level, String)> = fresh
        .iter()
        .take(MAX_WARNING_LINES)
        .map(|warning| {
            let level = if warning.starts_with(SKIPPED_STUBS) {
                Level::Info
            } else {
                Level::Warn
            };
            (
                level,
                format!(
                    "подписка «{name}»: {}",
                    truncate(warning, PROVIDER_TEXT_MAX)
                ),
            )
        })
        .collect();
    if fresh.len() > MAX_WARNING_LINES {
        lines.push((
            Level::Warn,
            format!(
                "подписка «{name}»: замечаний к ответу ещё {} (все — в raycat fetch)",
                fresh.len() - MAX_WARNING_LINES
            ),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_lines() -> Vec<(Level, String)> {
        Vec::new()
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn long_provider_text_is_cut_in_the_log() {
        let reason = "ж".repeat(500);
        let line = rejected("s1", &reason, Duration::from_secs(43_200));
        assert!(line.contains(&format!("{}…)", "ж".repeat(199))), "{line}");
        assert!(
            line.ends_with("следующая попытка через 12 ч 0 мин"),
            "{line}"
        );
        let line = failed("s1", &reason, Duration::from_secs(30));
        assert!(line.chars().count() < 300, "{line}");
        assert!(line.contains("не удалось обновить"));
    }

    #[test]
    fn short_text_is_kept_as_is() {
        let line = rejected("s1", "HTTP 404", Duration::from_secs(600));
        assert_eq!(
            line,
            "подписка «s1»: ответ не применён (HTTP 404), остаются прежние узлы; следующая попытка через 10 мин 0 с"
        );
    }

    #[test]
    fn recovery_is_a_plain_statement() {
        let line = recovered("s1", "узлов: 4", Duration::from_secs(43_200));
        assert_eq!(
            line,
            "подписка «s1» снова работает: узлов: 4; следующее обновление через 12 ч 0 мин"
        );
        assert!(updated("s1", "узлов: 4", Duration::from_secs(60)).contains("обновлена"));
    }

    #[test]
    fn skipped_stubs_are_info_and_other_notes_are_warnings() {
        let current = texts(&[
            "пропущены узлы-заглушки: ⬇️ОБХОДЫ НИЖЕ⬇️",
            "узел «A»: провайдер просит отключить проверку сертификата",
        ]);
        let lines = new_warnings("s2", &[], &current);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0, Level::Info);
        assert_eq!(lines[1].0, Level::Warn);
        assert!(
            lines[0]
                .1
                .starts_with("подписка «s2»: пропущены узлы-заглушки")
        );
    }

    #[test]
    fn unchanged_notes_are_not_repeated() {
        let current = texts(&["пропущены узлы-заглушки: A", "узел «B»: что-то не так"]);
        assert_eq!(new_warnings("s2", &current, &current), no_lines());
    }

    #[test]
    fn only_new_or_changed_notes_are_written() {
        let before = texts(&["пропущены узлы-заглушки: A", "узел «B»: что-то не так"]);
        let after = texts(&["пропущены узлы-заглушки: A | C", "узел «B»: что-то не так"]);
        let lines = new_warnings("s2", &before, &after);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, Level::Info);
        assert!(lines[0].1.ends_with("A | C"));
        assert_eq!(new_warnings("s2", &after, &[]), no_lines());
    }

    #[test]
    fn a_flood_of_notes_is_summarised() {
        let current: Vec<String> = (0..20).map(|n| format!("узел «{n}»: сбой")).collect();
        let lines = new_warnings("s3", &[], &current);
        assert_eq!(lines.len(), MAX_WARNING_LINES + 1);
        assert!(lines[MAX_WARNING_LINES].1.contains("ещё 15"));
    }
}
