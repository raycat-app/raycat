//! Правка файла настроек без потери комментариев: `toml_edit` меняет только нужные ключи,
//! закомментированный образец из примера раскомментируется, запись атомарная.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

/// Что меняем: таблица верхнего уровня или подписка по имени (`main`, `backup`, `3`…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Table(&'static str),
    Subscription(String),
}

#[derive(Debug, Clone)]
pub struct Change {
    pub target: Target,
    pub key: &'static str,
    pub value: Value,
}

#[derive(Debug)]
pub enum Error {
    Parse(String),
    NoSubscription(String),
    ReadOnly(String),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(text) => write!(f, "не удалось разобрать файл настроек: {text}"),
            Self::NoSubscription(name) => write!(f, "подписки «{name}» нет в файле настроек"),
            Self::ReadOnly(path) => write!(f, "файл настроек только для чтения: {path}"),
            Self::Io(text) => write!(f, "не удалось записать файл настроек: {text}"),
        }
    }
}

/// Имя подписки, как его видит raycat: явное `name`, иначе по порядку main, backup, 3…
fn subscription_name(index: usize, table: &Table) -> String {
    match table.get("name").and_then(Item::as_str) {
        Some(name) => name.to_owned(),
        None => match index {
            0 => "main".to_owned(),
            1 => "backup".to_owned(),
            _ => (index + 1).to_string(),
        },
    }
}

/// Применяет изменения к тексту файла. Комментарии и порядок сохраняются.
pub fn apply(text: &str, changes: &[Change]) -> Result<String, Error> {
    let mut text = text.to_owned();
    for change in changes {
        if let Target::Table(table) = change.target {
            text = uncomment(&text, table, change.key, &change.value.to_string());
        }
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| Error::Parse(error.to_string()))?;
    for change in changes {
        match &change.target {
            Target::Table(name) => {
                let Some(table) = doc
                    .as_table_mut()
                    .entry(name)
                    .or_insert(toml_edit::table())
                    .as_table_mut()
                else {
                    return Err(Error::Parse(format!("[{name}] — не таблица")));
                };
                set(table, change.key, change.value.clone());
            }
            Target::Subscription(name) => {
                let subscriptions = doc
                    .as_table_mut()
                    .get_mut("subscription")
                    .and_then(Item::as_array_of_tables_mut)
                    .ok_or_else(|| Error::NoSubscription(name.clone()))?;
                let mut found = None;
                for (index, table) in subscriptions.iter_mut().enumerate() {
                    if subscription_name(index, table) == *name {
                        found = Some(table);
                        break;
                    }
                }
                let table = found.ok_or_else(|| Error::NoSubscription(name.clone()))?;
                set(table, change.key, change.value.clone());
            }
        }
    }
    Ok(doc.to_string())
}

/// Значение ключа. Меняется только значение: комментарии над ключом и хвостовой
/// комментарий остаются (`insert` стёр бы их вместе с ключом).
fn set(table: &mut Table, key: &str, value: Value) {
    if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
        let mut value = value;
        *value.decor_mut() = old.decor().clone();
        *old = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

/// Имя таблицы из строки `[name]`; `[[name]]` и прочее — `None`.
fn header_name(trimmed: &str) -> Option<&str> {
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    (!inner.starts_with('[')).then_some(inner.trim())
}

/// Строка начинается с ключа и знака `=`, после которого идёт значение.
fn starts_key(text: &str, key: &str) -> bool {
    text.strip_prefix(key)
        .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('='))
}

/// Раскомментирует образец `# key = …` в таблице `table`, если активного ключа нет.
/// Закомментированный заголовок `# [table]` тоже становится заголовком.
fn uncomment(text: &str, table: &str, key: &str, literal: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = format!("[{table}]");
    if !lines.iter().any(|line| line.trim() == header)
        && let Some(line) = lines
            .iter_mut()
            .find(|line| line.trim() == format!("# {header}"))
    {
        *line = header;
    }
    let mut section = String::new();
    let mut active = false;
    for line in &lines {
        let trimmed = line.trim();
        if let Some(name) = header_name(trimmed) {
            section = name.to_owned();
        } else if section == table && starts_key(trimmed, key) {
            active = true;
        }
    }
    if active {
        return join(text, &lines);
    }
    section.clear();
    for line in &mut lines {
        let trimmed = line.trim().to_owned();
        if let Some(name) = header_name(&trimmed) {
            section = name.to_owned();
            continue;
        }
        if section != table {
            continue;
        }
        let Some(body) = trimmed.strip_prefix('#') else {
            continue;
        };
        if starts_key(body.trim_start(), key) {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            *line = format!("{indent}{key} = {literal}");
            break;
        }
    }
    join(text, &lines)
}

fn join(text: &str, lines: &[String]) -> String {
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Пишет файл через временный файл в том же каталоге и `rename`: читатель видит либо
/// старый, либо новый файл. Права старого файла сохраняются. Файл только для чтения
/// (в том числе из-за прав каталога) — понятная ошибка, файл не трогаем.
pub fn write(path: &Path, text: &str) -> Result<(), Error> {
    let meta = fs::metadata(path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    if meta.permissions().readonly() {
        return Err(Error::ReadOnly(path.display().to_string()));
    }
    let tmp = path.with_extension("raycat-tmp");
    match replace(path, &tmp, text, &meta) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            if error.kind() == io::ErrorKind::PermissionDenied {
                Err(Error::ReadOnly(path.display().to_string()))
            } else {
                Err(Error::Io(format!("{}: {error}", path.display())))
            }
        }
    }
}

fn replace(path: &Path, tmp: &Path, text: &str, meta: &fs::Metadata) -> io::Result<()> {
    let mut file = fs::File::create(tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(tmp, meta.permissions())?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../deploy/config.example.toml");

    fn comments(text: &str) -> usize {
        text.lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .count()
    }

    #[test]
    fn an_edit_of_the_example_keeps_every_comment() {
        let change = Change {
            target: Target::Table("routing"),
            key: "ru_direct",
            value: Value::from(true),
        };
        let out = apply(EXAMPLE, &[change]).unwrap();
        assert_eq!(comments(&out), comments(EXAMPLE));
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(doc["routing"]["ru_direct"].as_bool(), Some(true));
        assert!(out.starts_with("#:schema"));
    }

    #[test]
    fn a_commented_sample_becomes_the_value_and_its_header_too() {
        let text = "[mode]\ntype = \"proxy\"\n\n# [log]\n# level = \"info\"   # уровень\n";
        let change = Change {
            target: Target::Table("log"),
            key: "level",
            value: Value::from("debug"),
        };
        let out = apply(text, &[change]).unwrap();
        assert!(out.contains("\n[log]\nlevel = \"debug\"\n"), "{out}");
        assert!(!out.contains("# level"), "{out}");
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(doc["log"]["level"].as_str(), Some("debug"));
    }

    #[test]
    fn a_missing_key_is_added_to_its_table() {
        let text = "[selection]\nfailures = 3\n";
        let change = Change {
            target: Target::Table("selection"),
            key: "return_delay",
            value: Value::from("10m"),
        };
        let out = apply(text, &[change]).unwrap();
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(doc["selection"]["return_delay"].as_str(), Some("10m"));
        assert_eq!(doc["selection"]["failures"].as_integer(), Some(3));
    }

    #[test]
    fn a_subscription_is_found_by_its_name_or_its_order() {
        let text = "[[subscription]]\nurl = \"https://a.example.com/x\"\n\n[[subscription]]\nurl = \"https://b.example.com/x\"\n";
        let mut deny = toml_edit::Array::new();
        deny.push("*Обход*");
        let change = Change {
            target: Target::Subscription("backup".to_owned()),
            key: "deny",
            value: Value::Array(deny),
        };
        let out = apply(text, &[change]).unwrap();
        let doc: DocumentMut = out.parse().unwrap();
        let subs = doc["subscription"].as_array_of_tables().unwrap();
        assert!(subs.get(0).unwrap().get("deny").is_none());
        let deny = subs.get(1).unwrap()["deny"].as_array().unwrap();
        assert_eq!(deny.get(0).and_then(Value::as_str), Some("*Обход*"));
    }

    #[test]
    fn an_unknown_subscription_is_refused() {
        let change = Change {
            target: Target::Subscription("nope".to_owned()),
            key: "deny",
            value: Value::Array(toml_edit::Array::new()),
        };
        let error = apply(EXAMPLE, &[change]).unwrap_err();
        assert!(error.to_string().contains("«nope» нет"), "{error}");
    }

    #[test]
    fn broken_text_is_not_written_back() {
        let change = Change {
            target: Target::Table("routing"),
            key: "ru_direct",
            value: Value::from(true),
        };
        assert!(matches!(apply("[routing\n", &[change]), Err(Error::Parse(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_mode_and_a_read_only_file_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("raycat-edit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "old = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write(&path, "new = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = 2\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut readonly = fs::metadata(&path).unwrap().permissions();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let error = write(&path, "x = 3\n").unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("файл настроек только для чтения:"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = 2\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
