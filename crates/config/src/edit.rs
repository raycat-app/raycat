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
    /// Файл смонтирован отдельным файлом (Docker): его нельзя заменить через rename.
    SingleFileMount,
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(text) => write!(f, "не удалось разобрать файл настроек: {text}"),
            Self::NoSubscription(name) => write!(f, "подписки «{name}» нет в файле настроек"),
            Self::ReadOnly(path) => write!(f, "файл настроек только для чтения: {path}"),
            Self::SingleFileMount => write!(
                f,
                "файл настроек смонтирован отдельным файлом, его нельзя заменить: смонтируйте каталог или правьте файл на хосте и выполните raycat reload"
            ),
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

/// Записан ли ключ в файле (а не взят по умолчанию). Закомментированный образец — не запись.
pub fn has_key(text: &str, target: &Target, key: &str) -> bool {
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return false;
    };
    match target {
        Target::Table(name) => doc
            .get(name)
            .and_then(Item::as_table)
            .is_some_and(|table| table.contains_key(key)),
        Target::Subscription(name) => doc
            .get("subscription")
            .and_then(Item::as_array_of_tables)
            .is_some_and(|subscriptions| {
                subscriptions.iter().enumerate().any(|(index, table)| {
                    subscription_name(index, table) == *name && table.contains_key(key)
                })
            }),
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
            section.clear();
            section.push_str(name);
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
            section.clear();
            section.push_str(name);
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

/// Пишет файл через временный файл рядом с настоящим файлом и `rename` поверх него.
/// Читатель видит либо старый, либо новый файл. Симлинк остаётся симлинком: пишется
/// файл, на который он указывает. Временный файл создаётся с правами 0600 сразу, права
/// исходного файла выставляются после записи. Файл только для чтения — понятная ошибка.
pub fn write(path: &Path, text: &str) -> Result<(), Error> {
    let target = fs::canonicalize(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let meta = fs::metadata(&target)
        .map_err(|error| Error::Io(format!("{}: {error}", target.display())))?;
    if meta.permissions().readonly() {
        return Err(Error::ReadOnly(target.display().to_string()));
    }
    let tmp = target.with_extension("raycat-tmp");
    // Остаток прошлого сбоя мешал бы создать tmp. remove_file не идёт по ссылке.
    let _ = fs::remove_file(&tmp);
    match replace(&target, &tmp, text, &meta) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(classify(&target, &error))
        }
    }
}

/// Код ошибки `rename` (Linux): EBUSY и EXDEV бывают, когда файл смонтирован в контейнер
/// отдельным файлом и его нельзя заменить.
const EBUSY: i32 = 16;
const EXDEV: i32 = 18;

fn classify(target: &Path, error: &io::Error) -> Error {
    match error.raw_os_error() {
        Some(EBUSY | EXDEV) => Error::SingleFileMount,
        _ if error.kind() == io::ErrorKind::PermissionDenied => {
            Error::ReadOnly(target.display().to_string())
        }
        _ => Error::Io(format!("{}: {error}", target.display())),
    }
}

/// Временный файл создаётся сразу с правами 0600: никакой момент, когда он шире.
fn open_tmp(tmp: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

fn replace(target: &Path, tmp: &Path, text: &str, meta: &fs::Metadata) -> io::Result<()> {
    let mut file = open_tmp(tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(tmp, meta.permissions())?;
    fs::rename(tmp, target)?;
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    fs::File::open(dir)?.sync_all()
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
        assert!(matches!(
            apply("[routing\n", &[change]),
            Err(Error::Parse(_))
        ));
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

    #[cfg(unix)]
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("raycat-edit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn the_temporary_file_is_private_from_the_start() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("private-tmp");
        let tmp = dir.join("config.raycat-tmp");
        drop(open_tmp(&tmp).unwrap());
        assert_eq!(
            fs::metadata(&tmp).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // create_new не открывает уже существующий файл и не идёт по ссылке.
        assert!(open_tmp(&tmp).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_the_settings_stays_a_symlink() {
        let dir = scratch("symlink");
        let real = dir.join("real.toml");
        let link = dir.join("config.toml");
        fs::write(&real, "old = 1\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write(&link, "new = 2\n").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new = 2\n");
        assert!(!dir.join("real.raycat-tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_temporary_file_does_not_block_a_write() {
        let dir = scratch("leftover");
        let path = dir.join("config.toml");
        fs::write(&path, "old = 1\n").unwrap();
        fs::write(dir.join("config.raycat-tmp"), "stale").unwrap();
        write(&path, "new = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = 2\n");
        assert!(!dir.join("config.raycat-tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_busy_or_cross_device_rename_says_the_file_is_mounted_alone() {
        let target = Path::new("/etc/raycat/config.toml");
        let busy = classify(target, &io::Error::from_raw_os_error(16));
        assert!(matches!(busy, Error::SingleFileMount));
        let cross = classify(target, &io::Error::from_raw_os_error(18));
        assert!(cross.to_string().contains("смонтирован отдельным файлом"));
    }
}
