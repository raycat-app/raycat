//! Правка значений в тексте профиля с сохранением остального текста как есть.

use std::fmt::Write as _;

use anyhow::{Result, bail};

/// Заменяет `key = "…"` в таблице `section` (`None` — до первой таблицы).
pub(super) fn set_value(
    text: &str,
    section: Option<&str>,
    key: &str,
    value: &str,
) -> Result<String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        bail!("значение {key} содержит недопустимые символы");
    }
    let mut current: Option<String> = None;
    let mut done = false;
    let mut out = String::with_capacity(text.len() + value.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            current = Some(table_name(trimmed));
        }
        if !done && current.as_deref() == section && assigns(trimmed, key) {
            let indent = &line[..line.len() - trimmed.len()];
            let newline = if line.ends_with('\n') { "\n" } else { "" };
            write!(out, "{indent}{key} = \"{value}\"{newline}")?;
            done = true;
        } else {
            out.push_str(line);
        }
    }
    if !done {
        let place = section.unwrap_or("верхний уровень");
        bail!("в профиле нет {key} ({place})");
    }
    Ok(out)
}

fn assigns(trimmed: &str, key: &str) -> bool {
    trimmed
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// `[builds.x64]` → `builds.x64`, `[[headers]]` → `headers`.
fn table_name(trimmed: &str) -> String {
    let inner = trimmed
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or_default();
    inner.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "\
app = \"happ\"
version = \"4.3.0\"
user_agent = \"Happ/{app_version}\"

[builds.x64]
build = \"1\"
tail = \"03\"

[builds.arm64]
build = \"2\"
tail = \"03\"

[[headers]]
name = \"version\"
value = \"{host}\"
";

    #[test]
    fn replaces_a_top_level_value_only() {
        let text = set_value(PROFILE, None, "version", "4.4.8").unwrap();
        assert!(text.contains("version = \"4.4.8\"\n"));
        assert!(text.contains("name = \"version\""));
        assert_eq!(text.len(), PROFILE.len());
    }

    #[test]
    fn replaces_a_value_in_the_named_table() {
        let text = set_value(PROFILE, Some("builds.arm64"), "build", "20").unwrap();
        assert!(text.contains("[builds.x64]\nbuild = \"1\""));
        assert!(text.contains("[builds.arm64]\nbuild = \"20\"\ntail = \"03\""));
        let text = set_value(&text, Some("builds.arm64"), "tail", "07").unwrap();
        assert!(text.contains("build = \"20\"\ntail = \"07\"\n\n[[headers]]"));
        assert!(text.contains("[builds.x64]\nbuild = \"1\"\ntail = \"03\""));
    }

    #[test]
    fn missing_keys_and_tables_are_errors() {
        assert!(set_value(PROFILE, Some("builds.any"), "build", "1").is_err());
        assert!(set_value(PROFILE, Some("builds.x64"), "cpu", "1").is_err());
        assert!(set_value(PROFILE, None, "tail", "1").is_err());
    }

    #[test]
    fn unsafe_values_are_refused() {
        assert!(set_value(PROFILE, None, "version", "").is_err());
        assert!(set_value(PROFILE, None, "version", "4\"\nx = 1").is_err());
        assert!(set_value(PROFILE, None, "version", "4 4").is_err());
    }

    #[test]
    fn the_last_line_without_a_newline_is_kept_that_way() {
        let text = set_value("version = \"1\"", None, "version", "2").unwrap();
        assert_eq!(text, "version = \"2\"");
    }
}
