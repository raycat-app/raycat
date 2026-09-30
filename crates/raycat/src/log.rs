//! Лог только в stderr: `2026-09-30T12:00:00Z INFO сообщение`. Хранение и ротацию
//! берут на себя journald и Docker. Одинаковые сообщения подряд схлопываются.

use std::fmt;
use std::io::{self, Write as _};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use raycat_config::LogLevel;

use crate::util::{format_time, now_unix, sanitize};

const REPEAT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::Error,
            LogLevel::Warn => Self::Warn,
            LogLevel::Info => Self::Info,
            LogLevel::Debug => Self::Debug,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Line {
    level: Level,
    text: String,
}

struct Last {
    level: Level,
    text: String,
    written: Instant,
    suppressed: u32,
}

impl Last {
    fn summary(&self) -> Option<Line> {
        (self.suppressed > 0).then(|| Line {
            level: self.level,
            text: format!("{} (повторилось {} раз)", self.text, self.suppressed),
        })
    }
}

/// Решает, что из потока сообщений действительно пишется.
struct Dedup {
    last: Option<Last>,
}

impl Dedup {
    fn push(&mut self, now: Instant, level: Level, text: String) -> Vec<Line> {
        let mut lines = Vec::new();
        if let Some(last) = &mut self.last {
            let same = last.level == level && last.text == text;
            if same && now.saturating_duration_since(last.written) < REPEAT_WINDOW {
                last.suppressed = last.suppressed.saturating_add(1);
                return lines;
            }
            lines.extend(last.summary());
        }
        lines.push(Line {
            level,
            text: text.clone(),
        });
        self.last = Some(Last {
            level,
            text,
            written: now,
            suppressed: 0,
        });
        lines
    }

    fn flush(&mut self) -> Option<Line> {
        self.last.take()?.summary()
    }
}

struct Logger {
    level: Level,
    dedup: Dedup,
    /// Полная ссылка подписки и её маска: ссылка не должна попасть в лог ни в каком виде.
    secrets: Vec<(String, String)>,
}

static LOGGER: Mutex<Logger> = Mutex::new(Logger {
    level: Level::Info,
    dedup: Dedup { last: None },
    secrets: Vec::new(),
});

fn lock() -> MutexGuard<'static, Logger> {
    LOGGER.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn init(level: Level) {
    lock().level = level;
}

/// Заменяет `secret` на `masked` во всех сообщениях.
pub(crate) fn hide(secret: &str, masked: &str) {
    if secret.is_empty() {
        return;
    }
    let mut logger = lock();
    if !logger.secrets.iter().any(|(known, _)| known == secret) {
        logger.secrets.push((secret.to_owned(), masked.to_owned()));
    }
}

pub(crate) fn write(level: Level, args: fmt::Arguments<'_>) {
    let mut logger = lock();
    if level > logger.level {
        return;
    }
    let mut text = sanitize(&args.to_string());
    for (secret, masked) in &logger.secrets {
        text = text.replace(secret.as_str(), masked);
    }
    let lines = logger.dedup.push(Instant::now(), level, text);
    emit(&lines);
}

/// Дописывает счётчик повторов последнего сообщения; вызывается при выходе.
pub(crate) fn flush() {
    let line = lock().dedup.flush();
    if let Some(line) = line {
        emit(&[line]);
    }
}

fn emit(lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    let time = format_time(now_unix());
    let mut stderr = io::stderr().lock();
    for line in lines {
        let _ = writeln!(stderr, "{time} {} {}", line.level.label(), line.text);
    }
}

macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Error, ::std::format_args!($($arg)*))
    };
}

macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Warn, ::std::format_args!($($arg)*))
    };
}

macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Info, ::std::format_args!($($arg)*))
    };
}

macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Debug, ::std::format_args!($($arg)*))
    };
}

pub(crate) use {debug, error, info, warn};

#[cfg(test)]
mod tests {
    use super::*;

    fn dedup() -> Dedup {
        Dedup { last: None }
    }

    fn texts(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn first_message_is_written() {
        let now = Instant::now();
        let lines = dedup().push(now, Level::Info, "старт".to_owned());
        assert_eq!(texts(&lines), ["старт"]);
        assert_eq!(lines[0].level, Level::Info);
    }

    #[test]
    fn repeats_within_a_minute_are_swallowed() {
        let mut dedup = dedup();
        let start = Instant::now();
        assert_eq!(dedup.push(start, Level::Warn, "сбой".to_owned()).len(), 1);
        for second in [1, 20, 59] {
            let now = start + Duration::from_secs(second);
            assert!(dedup.push(now, Level::Warn, "сбой".to_owned()).is_empty());
        }
    }

    #[test]
    fn a_different_message_reports_the_repeat_count() {
        let mut dedup = dedup();
        let start = Instant::now();
        dedup.push(start, Level::Warn, "сбой".to_owned());
        dedup.push(
            start + Duration::from_secs(1),
            Level::Warn,
            "сбой".to_owned(),
        );
        dedup.push(
            start + Duration::from_secs(2),
            Level::Warn,
            "сбой".to_owned(),
        );
        let lines = dedup.push(start + Duration::from_secs(3), Level::Info, "ок".to_owned());
        assert_eq!(texts(&lines), ["сбой (повторилось 2 раз)", "ок"]);
        assert_eq!(lines[0].level, Level::Warn);
        assert_eq!(lines[1].level, Level::Info);
    }

    #[test]
    fn no_summary_without_repeats() {
        let mut dedup = dedup();
        let start = Instant::now();
        dedup.push(start, Level::Info, "раз".to_owned());
        let lines = dedup.push(start, Level::Info, "два".to_owned());
        assert_eq!(texts(&lines), ["два"]);
    }

    #[test]
    fn same_text_at_another_level_is_a_different_message() {
        let mut dedup = dedup();
        let start = Instant::now();
        dedup.push(start, Level::Info, "текст".to_owned());
        let lines = dedup.push(start, Level::Warn, "текст".to_owned());
        assert_eq!(texts(&lines), ["текст"]);
    }

    #[test]
    fn the_message_is_written_again_after_the_window() {
        let mut dedup = dedup();
        let start = Instant::now();
        dedup.push(start, Level::Warn, "сбой".to_owned());
        dedup.push(
            start + Duration::from_secs(10),
            Level::Warn,
            "сбой".to_owned(),
        );
        let lines = dedup.push(
            start + Duration::from_secs(61),
            Level::Warn,
            "сбой".to_owned(),
        );
        assert_eq!(texts(&lines), ["сбой (повторилось 1 раз)", "сбой"]);
        let again = dedup.push(
            start + Duration::from_secs(62),
            Level::Warn,
            "сбой".to_owned(),
        );
        assert!(again.is_empty());
    }

    #[test]
    fn flush_reports_pending_repeats_once() {
        let mut dedup = dedup();
        let start = Instant::now();
        dedup.push(start, Level::Error, "падение".to_owned());
        dedup.push(start, Level::Error, "падение".to_owned());
        let line = dedup.flush().unwrap();
        assert_eq!(line.text, "падение (повторилось 1 раз)");
        assert!(dedup.flush().is_none());
    }

    #[test]
    fn levels_are_ordered_from_quiet_to_noisy() {
        assert!(
            Level::Error < Level::Warn && Level::Warn < Level::Info && Level::Info < Level::Debug
        );
        assert_eq!(Level::from(LogLevel::Debug), Level::Debug);
        assert_eq!(Level::from(LogLevel::Error), Level::Error);
    }
}
