use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::envvars;
use crate::error::{Error, Problems};
use crate::model::Config;
use crate::raw::Raw;
use crate::validate;

/// Переменные окружения: `std::env::vars().collect()`.
pub type Env = BTreeMap<String, String>;

const MAX_FILE_BYTES: u64 = 1 << 20;

impl Config {
    /// Файл: явный путь, иначе `RAYCAT_CONFIG`; без файла настройки берутся
    /// из окружения. Ошибки проверки собираются списком.
    pub fn load(path: Option<&Path>, env: &Env) -> Result<Self, Error> {
        let path = path
            .map(Path::to_path_buf)
            .or_else(|| envvars::value(env, envvars::CONFIG).map(PathBuf::from));
        let text = match path {
            Some(path) => read(&path)?,
            None => String::new(),
        };
        Self::from_toml_str(&text, env)
    }

    pub fn from_toml_str(text: &str, env: &Env) -> Result<Self, Error> {
        if !u64::try_from(text.len()).is_ok_and(|len| len <= MAX_FILE_BYTES) {
            return Err(Error::Parse("файл настроек больше 1 МиБ".to_owned()));
        }
        let mut raw = parse(text)?;
        let mut problems = Problems::default();
        envvars::apply(&mut raw, env, &mut problems);
        validate::build(&raw, problems)
    }
}

fn read(path: &Path) -> Result<String, Error> {
    let fail = |source: io::Error| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(fail)?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(fail)?;
    Ok(text)
}

/// Сообщение разбора не содержит значений: в них могут быть ссылка или seed,
/// поэтому вместо цитаты toml берётся только номер строки и имя ключа.
fn parse(text: &str) -> Result<Raw, Error> {
    toml::from_str(text).map_err(|error| {
        let meaning = describe(error.message());
        let message = match error.span() {
            Some(span) => {
                let (line, key) = locate(text, span.start);
                match key {
                    Some(key) => format!("строка {line} ({key}): {meaning}"),
                    None => format!("строка {line}: {meaning}"),
                }
            }
            None => meaning,
        };
        Error::Parse(message)
    })
}

fn describe(message: &str) -> String {
    let quoted = |prefix: &str| {
        message
            .strip_prefix(prefix)
            .and_then(|rest| rest.split('`').next())
            .map(str::to_owned)
    };
    if let Some(name) = quoted("unknown field `") {
        format!("неизвестный ключ `{name}`")
    } else if let Some(name) = quoted("missing field `") {
        format!("не хватает обязательного ключа `{name}`")
    } else if message.starts_with("duplicate key") {
        "ключ указан дважды".to_owned()
    } else if message.starts_with("invalid type") || message.starts_with("invalid value") {
        "значение неподходящего типа или вне допустимого набора".to_owned()
    } else {
        format!("ошибка синтаксиса TOML ({message})")
    }
}

fn locate(text: &str, offset: usize) -> (usize, Option<&str>) {
    let before = text.get(..offset.min(text.len())).unwrap_or_default();
    let line_number = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let key = text
        .get(line_start..)
        .and_then(|rest| rest.lines().next())
        .and_then(|line| line.split_once('='))
        .map(|(key, _)| key.trim())
        .filter(|key| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        });
    (line_number, key)
}
