//! Настройки вида TUI в `tui.toml`. Это не настройки демона: их меняет только сам
//! интерфейс, а настройки демона идут через его API.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use raycat_config::Env;
use serde::Deserialize;

use crate::names::NameStyle;

const FILE: &str = "tui.toml";

/// Вид интерфейса: значки в именах и маски скрытых узлов.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Prefs {
    pub(crate) names: NameStyle,
    pub(crate) hidden: Vec<String>,
}

#[derive(Deserialize)]
struct File {
    names: Option<String>,
    hidden: Option<String>,
}

/// Пустая переменная считается незаданной.
fn set<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// `$XDG_CONFIG_HOME/raycat/tui.toml`, иначе `~/.config/raycat/tui.toml`.
pub(crate) fn path(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = set(env, "XDG_CONFIG_HOME")
        && Path::new(dir).is_absolute()
    {
        return Some(Path::new(dir).join("raycat").join(FILE));
    }
    set(env, "HOME").map(|home| Path::new(home).join(".config/raycat").join(FILE))
}

/// Вид из файла; без файла или с непонятными значениями — значения по умолчанию.
pub(crate) fn load(path: &Path) -> Prefs {
    fs::read_to_string(path).map_or_else(|_| Prefs::default(), |text| parse(&text))
}

fn parse(text: &str) -> Prefs {
    let file = toml::from_str::<File>(text).ok();
    let names = file
        .as_ref()
        .and_then(|file| file.names.as_deref())
        .and_then(NameStyle::parse)
        .unwrap_or_default();
    let hidden = file
        .and_then(|file| file.hidden)
        .map_or_else(Vec::new, |list| split_masks(&list));
    Prefs { names, hidden }
}

/// Маски через запятую; пробелы по краям и пустые элементы отбрасываются.
pub(crate) fn split_masks(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|mask| !mask.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn save(path: &Path, prefs: &Prefs) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text(prefs))
}

fn text(prefs: &Prefs) -> String {
    format!(
        "names = \"{}\"\nhidden = \"{}\"\n",
        prefs.names.key(),
        escape(&prefs.hidden.join(", "))
    )
}

/// Строка для TOML: кавычки и обратная косая черта экранируются.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_file_follows_xdg_then_home() {
        let xdg = env(&[("XDG_CONFIG_HOME", "/xdg/conf"), ("HOME", "/home/u")]);
        assert_eq!(path(&xdg), Some(PathBuf::from("/xdg/conf/raycat/tui.toml")));
        let relative = env(&[("XDG_CONFIG_HOME", "conf"), ("HOME", "/home/u")]);
        assert_eq!(
            path(&relative),
            Some(PathBuf::from("/home/u/.config/raycat/tui.toml"))
        );
        assert_eq!(path(&env(&[])), None);
    }

    #[test]
    fn a_saved_style_is_read_back() {
        assert_eq!(parse("names = \"as-is\"\n").names, NameStyle::AsIs);
        assert_eq!(parse("names = \"country\"\n").names, NameStyle::Country);
    }

    #[test]
    fn anything_unclear_means_the_defaults() {
        assert_eq!(parse(""), Prefs::default());
        assert_eq!(parse("names = \"флаги\""), Prefs::default());
        assert_eq!(parse("names = [broken"), Prefs::default());
    }

    #[test]
    fn masks_are_split_by_commas() {
        let prefs = parse("hidden = \"Обход*,  Резерв* , \"\n");
        assert_eq!(prefs.hidden, ["Обход*", "Резерв*"]);
        assert_eq!(split_masks("  , "), Vec::<String>::new());
    }

    #[test]
    fn what_is_saved_comes_back_unchanged() {
        let prefs = Prefs {
            names: NameStyle::AsIs,
            hidden: vec!["Обход*".to_owned(), "a\"b\\c".to_owned()],
        };
        assert_eq!(parse(&text(&prefs)), prefs);
    }
}
