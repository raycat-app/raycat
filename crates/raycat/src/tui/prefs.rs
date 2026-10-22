//! Настройки вида TUI в `tui.toml`. Это не настройки демона: их меняет только сам
//! интерфейс, а настройки демона идут через его API.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use raycat_config::Env;
use serde::Deserialize;

use crate::names::NameStyle;

const FILE: &str = "tui.toml";

#[derive(Deserialize)]
struct File {
    names: Option<String>,
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

/// Вид из файла; без файла или с непонятным значением — код страны.
pub(crate) fn load(path: &Path) -> NameStyle {
    fs::read_to_string(path).map_or(NameStyle::default(), |text| parse(&text))
}

fn parse(text: &str) -> NameStyle {
    toml::from_str::<File>(text)
        .ok()
        .and_then(|file| file.names)
        .and_then(|value| NameStyle::parse(&value))
        .unwrap_or_default()
}

pub(crate) fn save(path: &Path, style: NameStyle) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, format!("names = \"{}\"\n", style.key()))
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
        assert_eq!(parse("names = \"as-is\"\n"), NameStyle::AsIs);
        assert_eq!(parse("names = \"country\"\n"), NameStyle::Country);
    }

    #[test]
    fn anything_unclear_means_the_country_code() {
        assert_eq!(parse(""), NameStyle::Country);
        assert_eq!(parse("names = \"флаги\""), NameStyle::Country);
        assert_eq!(parse("names = [broken"), NameStyle::Country);
    }
}
