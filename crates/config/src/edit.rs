//! Правка файла настроек без потери комментариев: `toml_edit` меняет только нужные ключи,
//! закомментированный образец из примера раскомментируется, запись атомарная.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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
    /// `None` убирает ключ из файла: значение становится значением по умолчанию.
    pub value: Option<Value>,
}

#[derive(Debug)]
pub enum Error {
    Parse(String),
    NoSubscription(String),
    ReadOnly(String),
    /// Файл смонтирован отдельным файлом (Docker): его нельзя заменить через rename.
    SingleFileMount,
    /// Не удалось передать владельца и группу исходного файла: файл не заменён.
    Owner(String),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(text) => write!(f, "не удалось разобрать файл настроек: {text}"),
            Self::NoSubscription(name) => write!(f, "подписки «{name}» нет в файле настроек"),
            Self::ReadOnly(path) => write!(
                f,
                "файл настроек только для чтения: {path}\nснимите запрет записи или поправьте файл вручную и выполните raycat reload"
            ),
            Self::SingleFileMount => write!(
                f,
                "файл настроек смонтирован отдельным файлом, его нельзя заменить: смонтируйте каталог или правьте файл на хосте и выполните raycat reload"
            ),
            Self::Owner(text) => write!(
                f,
                "не удалось сохранить владельца файла настроек, файл не изменён: {text}"
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
        match (&change.target, &change.value) {
            (Target::Table(table), Some(value)) => {
                text = uncomment(&text, table, change.key, &value.to_string());
            }
            (Target::Table(table), None) => text = comment_out(&text, table, change.key),
            (Target::Subscription(name), None) => {
                text = comment_out_subscription(&text, name, change.key)?;
            }
            (Target::Subscription(_), Some(_)) => {}
        }
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| Error::Parse(error.to_string()))?;
    for change in changes {
        match &change.target {
            Target::Table(name) => {
                if change.value.is_none() {
                    let table = doc
                        .as_table_mut()
                        .get_mut(name)
                        .and_then(Item::as_table_mut);
                    if let Some(table) = table {
                        remove(table, change.key);
                    }
                    continue;
                }
                let Some(table) = doc
                    .as_table_mut()
                    .entry(name)
                    .or_insert(toml_edit::table())
                    .as_table_mut()
                else {
                    return Err(Error::Parse(format!("[{name}] — не таблица")));
                };
                put(table, change.key, change.value.as_ref());
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
                put(table, change.key, change.value.as_ref());
            }
        }
    }
    Ok(doc.to_string())
}

fn put(table: &mut Table, key: &str, value: Option<&Value>) {
    match value {
        Some(value) => set(table, key, value.clone()),
        None => remove(table, key),
    }
}

/// Убирает ключ вместе с его комментариями. Пустую таблицу не удаляет: её заголовок
/// мог быть нужен другим ключам или читателю файла.
fn remove(table: &mut Table, key: &str) {
    let _ = table.remove(key);
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

/// Убирает активный ключ `key` из таблицы `table`, оставляя его строку закомментированной:
/// так файл выглядит как образец по умолчанию, а соседние комментарии остаются на месте.
/// Ключ в несколько строк не трогается: для таких значений raycat сам не пишет.
fn comment_out(text: &str, table: &str, key: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut section = String::new();
    for line in &mut lines {
        let trimmed = line.trim().to_owned();
        if trimmed.starts_with('[') {
            header_name(&trimmed)
                .unwrap_or_default()
                .clone_into(&mut section);
            continue;
        }
        if section == table && starts_key(&trimmed, key) {
            comment(line);
            break;
        }
    }
    join(text, &lines)
}

/// То же для ключа подписки `name`: её строка становится образцом, соседние строки и
/// комментарии остаются. Массив на несколько строк закомментируется целиком.
fn comment_out_subscription(text: &str, name: &str, key: &str) -> Result<String, Error> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| Error::Parse(error.to_string()))?;
    let tables = doc
        .get("subscription")
        .and_then(Item::as_array_of_tables)
        .ok_or_else(|| Error::NoSubscription(name.to_owned()))?;
    let index = tables
        .iter()
        .enumerate()
        .position(|(index, table)| subscription_name(index, table) == name)
        .ok_or_else(|| Error::NoSubscription(name.to_owned()))?;
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("[[subscription]]"))
        .map(|(number, _)| number)
        .collect();
    if headers.len() != tables.len() {
        return Err(Error::Parse(
            "заголовки [[subscription]] записаны нестандартно, маска не снята".to_owned(),
        ));
    }
    let Some(&header) = headers.get(index) else {
        return Err(Error::NoSubscription(name.to_owned()));
    };
    let end = lines[header + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| header + 1 + offset);
    let Some(start) = (header + 1..end).find(|&number| starts_key(lines[number].trim(), key))
    else {
        return Ok(join(text, &lines));
    };
    let mut last = start;
    if opens_array(lines[start].trim()) {
        while last + 1 < end {
            last += 1;
            if closes_array(lines[last].trim()) {
                break;
            }
        }
    }
    for line in &mut lines[start..=last] {
        comment(line);
    }
    Ok(join(text, &lines))
}

/// Строка становится комментарием, отступ остаётся на месте.
fn comment(line: &mut String) {
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let trimmed = line.trim().to_owned();
    *line = format!("{indent}# {trimmed}");
}

/// Значение начато массивом `[`, который не закрыт на этой строке.
fn opens_array(trimmed: &str) -> bool {
    trimmed
        .split_once('=')
        .is_some_and(|(_, value)| value.trim_start().starts_with('[') && !closes_array(value))
}

/// В строке есть `]` вне строки в кавычках: массив закрыт.
fn closes_array(text: &str) -> bool {
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ']' if !quoted => return true,
            _ => {}
        }
    }
    false
}

/// Переставляет блок `[[subscription]]` с именем `name` на место `to` (с нуля). Блоки идут
/// подряд; комментарии сразу над заголовком едут вместе с блоком, пустые строки между
/// блоками остаются на своих местах.
pub fn move_subscription(text: &str, name: &str, to: usize) -> Result<String, Error> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| Error::Parse(error.to_string()))?;
    let tables = doc
        .get("subscription")
        .and_then(Item::as_array_of_tables)
        .ok_or_else(|| Error::NoSubscription(name.to_owned()))?;
    let names: Vec<String> = tables
        .iter()
        .enumerate()
        .map(|(index, table)| subscription_name(index, table))
        .collect();
    let from = names
        .iter()
        .position(|candidate| candidate == name)
        .ok_or_else(|| Error::NoSubscription(name.to_owned()))?;
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim().starts_with("[[subscription]]"))
        .map(|(index, _)| index)
        .collect();
    if headers.len() != names.len() {
        return Err(Error::Parse(
            "заголовки [[subscription]] записаны нестандартно, подписки не переставлены".to_owned(),
        ));
    }
    let last = headers[headers.len() - 1];
    let next_table = lines[last + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| last + 1 + offset);
    let mut tail = next_table;
    while tail > last + 1 && lines[tail - 1].trim_start().starts_with('#') {
        tail -= 1;
    }
    let mut starts = Vec::with_capacity(headers.len());
    for (index, header) in headers.iter().enumerate() {
        let floor = if index == 0 {
            0
        } else {
            headers[index - 1] + 1
        };
        let mut start = *header;
        while start > floor && lines[start - 1].trim_start().starts_with('#') {
            start -= 1;
        }
        starts.push(start);
    }
    // Каждый блок: (начало, конец содержимого, конец вместе с пустыми строками).
    let mut blocks = Vec::with_capacity(headers.len());
    for (index, header) in headers.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(tail);
        let mut content_end = end;
        while content_end > header + 1 && lines[content_end - 1].trim().is_empty() {
            content_end -= 1;
        }
        blocks.push((starts[index], content_end, end));
    }
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    let moved = order.remove(from);
    order.insert(to.min(order.len()), moved);
    let mut out: Vec<String> = lines[..blocks[0].0].to_vec();
    for (slot, source) in order.iter().enumerate() {
        let (start, content_end, _) = blocks[*source];
        out.extend_from_slice(&lines[start..content_end]);
        let (_, slot_content_end, slot_end) = blocks[slot];
        out.extend_from_slice(&lines[slot_content_end..slot_end]);
    }
    out.extend_from_slice(&lines[tail..]);
    let result = join(text, &out);
    result
        .parse::<DocumentMut>()
        .map_err(|error| Error::Parse(error.to_string()))?;
    Ok(result)
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
    let (target, meta) = writable_target(path)?;
    let tmp = target.with_extension("raycat-tmp");
    // Остаток прошлого сбоя мешал бы создать tmp. remove_file не идёт по ссылке.
    let _ = fs::remove_file(&tmp);
    let result = replace(&target, &tmp, text, &meta);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Можно ли записать файл настроек. Проверка та же, что у `write`: в каталоге создаётся и
/// сразу удаляется временный файл, так видны права каталога и файловая система только для чтения.
pub fn writable(path: &Path) -> Result<(), Error> {
    let (target, _) = writable_target(path)?;
    let tmp = target.with_extension("raycat-tmp");
    let _ = fs::remove_file(&tmp);
    open_tmp(&tmp).map_err(|error| classify(&target, &error))?;
    fs::remove_file(&tmp).map_err(|error| classify(&target, &error))
}

/// Настоящий путь файла (симлинк раскрыт) и его метаданные. Файл только для чтения — ошибка.
/// Файл на другом устройстве, чем его каталог, смонтирован отдельно: заменить его нельзя.
fn writable_target(path: &Path) -> Result<(PathBuf, fs::Metadata), Error> {
    use std::os::unix::fs::MetadataExt as _;
    let target = fs::canonicalize(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let meta = fs::metadata(&target)
        .map_err(|error| Error::Io(format!("{}: {error}", target.display())))?;
    if meta.permissions().readonly() {
        return Err(Error::ReadOnly(target.display().to_string()));
    }
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let dir_meta =
        fs::metadata(dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?;
    if single_file_mount(meta.dev(), dir_meta.dev()) {
        return Err(Error::SingleFileMount);
    }
    Ok((target, meta))
}

/// Файл и его каталог на разных устройствах: так выглядит одиночный файл, смонтированный в
/// контейнер (`-v ./config.toml:/etc/raycat/config.toml`). `rename` поверх него не пройдёт.
fn single_file_mount(file_dev: u64, dir_dev: u64) -> bool {
    file_dev != dir_dev
}

/// Код ошибки (Linux): EBUSY и EXDEV бывают, когда файл смонтирован в контейнер отдельным
/// файлом и его нельзя заменить, EROFS — файловая система только для чтения.
const EBUSY: i32 = 16;
const EXDEV: i32 = 18;
const EROFS: i32 = 30;

fn classify(target: &Path, error: &io::Error) -> Error {
    match error.raw_os_error() {
        Some(EBUSY | EXDEV) => Error::SingleFileMount,
        Some(EROFS) => Error::ReadOnly(target.display().to_string()),
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

fn replace(target: &Path, tmp: &Path, text: &str, meta: &fs::Metadata) -> Result<(), Error> {
    let fail = |error: io::Error| classify(target, &error);
    let mut file = open_tmp(tmp).map_err(fail)?;
    file.write_all(text.as_bytes()).map_err(fail)?;
    file.sync_all().map_err(fail)?;
    drop(file);
    keep_owner(tmp, meta)
        .map_err(|error| Error::Owner(format!("{}: {error}", target.display())))?;
    fs::set_permissions(tmp, meta.permissions()).map_err(fail)?;
    fs::rename(tmp, target).map_err(fail)?;
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(fail)
}

/// Временный файл принадлежит процессу. Владелец и группа исходного файла переходят на него,
/// иначе после замены файл настроек достаётся пользователю процесса (root в службе).
fn keep_owner(tmp: &Path, meta: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let made = fs::metadata(tmp)?;
    if made.uid() == meta.uid() && made.gid() == meta.gid() {
        return Ok(());
    }
    std::os::unix::fs::chown(tmp, Some(meta.uid()), Some(meta.gid()))
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
            value: Some(Value::from(true)),
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
            value: Some(Value::from("debug")),
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
            value: Some(Value::from("10m")),
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
            value: Some(Value::Array(deny)),
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
            value: Some(Value::Array(toml_edit::Array::new())),
        };
        let error = apply(EXAMPLE, &[change]).unwrap_err();
        assert!(error.to_string().contains("«nope» нет"), "{error}");
    }

    #[test]
    fn broken_text_is_not_written_back() {
        let change = Change {
            target: Target::Table("routing"),
            key: "ru_direct",
            value: Some(Value::from(true)),
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

    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_owner_and_the_mode() {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("owner");
        let path = dir.join("config.toml");
        fs::write(&path, "old = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::metadata(&path).unwrap();
        write(&path, "new = 2\n").unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        assert_eq!(after.mode() & 0o777, 0o640);
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = 2\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_removed_key_becomes_a_commented_sample_and_its_neighbours_stay() {
        let text =
            "[xray]\n# сколько соединений\nxhttp_connections = 4   # быстрее\npath = \"/x\"\n";
        let change = Change {
            target: Target::Table("xray"),
            key: "xhttp_connections",
            value: None,
        };
        let out = apply(text, &[change]).unwrap();
        assert_eq!(
            out,
            "[xray]\n# сколько соединений\n# xhttp_connections = 4   # быстрее\npath = \"/x\"\n"
        );
    }

    #[test]
    fn removing_from_a_missing_table_adds_nothing() {
        let text = "[routing]\nru_direct = true\n";
        let change = Change {
            target: Target::Table("xray"),
            key: "xhttp_connections",
            value: None,
        };
        assert_eq!(apply(text, &[change]).unwrap(), text);
    }

    #[cfg(unix)]
    #[test]
    fn writable_says_so_without_touching_the_file() {
        let dir = scratch("writable");
        let path = dir.join("config.toml");
        fs::write(&path, "old = 1\n").unwrap();
        assert!(writable(&path).is_ok());
        let mut readonly = fs::metadata(&path).unwrap().permissions();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        assert!(matches!(writable(&path), Err(Error::ReadOnly(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), "old = 1\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subscription_moves_with_its_comments_and_the_rest_stays() {
        let text = "[routing]\nru_direct = false\n\n# основная\n[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n\n# резервная\n[[subscription]]\nname = \"backup\"\nurl = \"https://b.example.com/x\"\n\n[xray]\npath = \"/x\"\n";
        let out = move_subscription(text, "backup", 0).unwrap();
        let expected = "[routing]\nru_direct = false\n\n# резервная\n[[subscription]]\nname = \"backup\"\nurl = \"https://b.example.com/x\"\n\n# основная\n[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n\n[xray]\npath = \"/x\"\n";
        assert_eq!(out, expected);
        let back = move_subscription(&out, "main", 0).unwrap();
        assert_eq!(back, text);
    }

    #[test]
    fn moving_an_unknown_subscription_is_refused() {
        let text = "[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n";
        assert!(matches!(
            move_subscription(text, "nope", 0),
            Err(Error::NoSubscription(_))
        ));
    }

    #[test]
    fn a_busy_or_cross_device_rename_says_the_file_is_mounted_alone() {
        let target = Path::new("/etc/raycat/config.toml");
        let busy = classify(target, &io::Error::from_raw_os_error(16));
        assert!(matches!(busy, Error::SingleFileMount));
        let cross = classify(target, &io::Error::from_raw_os_error(18));
        assert!(cross.to_string().contains("смонтирован отдельным файлом"));
    }

    #[test]
    fn a_file_on_another_device_than_its_directory_is_a_single_file_mount() {
        assert!(single_file_mount(7, 3));
        assert!(!single_file_mount(3, 3));
    }

    #[test]
    fn a_read_only_filesystem_says_the_file_is_read_only() {
        let target = Path::new("/etc/raycat/config.toml");
        let error = classify(target, &io::Error::from_raw_os_error(30));
        assert!(matches!(error, Error::ReadOnly(_)), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_takes_no_new_file_is_refused_before_the_save() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("dir-readonly");
        let path = dir.join("config.toml");
        fs::write(&path, "old = 1\n").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        // Root пишет и в каталог без прав: тогда отказа нет и проверять нечего.
        let privileged = open_tmp(&dir.join("probe")).is_ok();
        let _ = fs::remove_file(dir.join("probe"));
        if !privileged {
            assert!(matches!(writable(&path), Err(Error::ReadOnly(_))));
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old = 1\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_emptied_mask_becomes_a_commented_sample_and_its_neighbours_stay() {
        let text = "[routing]\nru_direct = true\n\n[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n# маски\ndeny = [\"*Info*\"]   # скрыть\npriority = [\"*NL*\"]\n\n[xray]\npath = \"/x\"\n";
        let change = Change {
            target: Target::Subscription("main".to_owned()),
            key: "deny",
            value: None,
        };
        let out = apply(text, &[change]).unwrap();
        assert_eq!(
            out,
            "[routing]\nru_direct = true\n\n[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n# маски\n# deny = [\"*Info*\"]   # скрыть\npriority = [\"*NL*\"]\n\n[xray]\npath = \"/x\"\n"
        );
    }

    #[test]
    fn an_emptied_multi_line_mask_is_commented_out_whole() {
        let text = "[[subscription]]\nname = \"main\"\ndeny = [\n  \"*Info*\",\n  \"*[NL]*\",\n]\npriority = [\"*NL*\"]\n";
        let change = Change {
            target: Target::Subscription("main".to_owned()),
            key: "deny",
            value: None,
        };
        let out = apply(text, &[change]).unwrap();
        assert_eq!(
            out,
            "[[subscription]]\nname = \"main\"\n# deny = [\n  # \"*Info*\",\n  # \"*[NL]*\",\n# ]\npriority = [\"*NL*\"]\n"
        );
    }

    #[test]
    fn an_emptied_mask_that_is_not_in_the_file_changes_nothing() {
        let text = "[[subscription]]\nname = \"main\"\nurl = \"https://a.example.com/x\"\n";
        let change = Change {
            target: Target::Subscription("main".to_owned()),
            key: "deny",
            value: None,
        };
        assert_eq!(apply(text, &[change]).unwrap(), text);
    }

    #[test]
    fn an_emptied_mask_of_an_unknown_subscription_is_refused() {
        let change = Change {
            target: Target::Subscription("nope".to_owned()),
            key: "deny",
            value: None,
        };
        assert!(matches!(
            apply(EXAMPLE, &[change]),
            Err(Error::NoSubscription(_))
        ));
    }
}
