//! Где лежат настройки и состояние.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use raycat_config::Env;

const CONFIG_VAR: &str = "RAYCAT_CONFIG";
const STATE_VAR: &str = "RAYCAT_STATE_DIR";
const DEFAULT_CONFIG: &str = "/etc/raycat/config.toml";
const ROOT_STATE_DIR: &str = "/var/lib/raycat";
const SOCKET_VAR: &str = "RAYCAT_SOCKET";
const ROOT_SOCKET: &str = "/run/raycat/raycat.sock";
const IMAGE_SOCKET: &str = "/var/lib/raycat/raycat.sock";
/// Где клиент ищет демона, если его собственный путь не существует.
const FALLBACK_SOCKETS: [&str; 2] = [ROOT_SOCKET, IMAGE_SOCKET];

pub(crate) fn environment() -> Env {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Пустая переменная считается незаданной.
fn set<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// Файл настроек: `--config`, затем `RAYCAT_CONFIG`, затем файл по умолчанию, если
/// он есть. `None` — настройки целиком из окружения (контейнер без файла).
pub(crate) fn config_file(
    explicit: Option<&Path>,
    env: &Env,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = set(env, CONFIG_VAR) {
        return Some(PathBuf::from(path));
    }
    let default = Path::new(DEFAULT_CONFIG);
    exists(default).then(|| default.to_path_buf())
}

/// `RAYCAT_STATE_DIR`, иначе `/var/lib/raycat` для root, иначе
/// `$XDG_STATE_HOME/raycat` или `~/.local/state/raycat`.
pub(crate) fn state_dir(env: &Env, root: bool) -> Result<PathBuf> {
    if let Some(dir) = set(env, STATE_VAR) {
        return Ok(PathBuf::from(dir));
    }
    if root {
        return Ok(PathBuf::from(ROOT_STATE_DIR));
    }
    if let Some(dir) = set(env, "XDG_STATE_HOME")
        && Path::new(dir).is_absolute()
    {
        return Ok(Path::new(dir).join("raycat"));
    }
    if let Some(home) = set(env, "HOME") {
        return Ok(Path::new(home).join(".local/state/raycat"));
    }
    bail!("не удалось выбрать каталог состояния: задайте {STATE_VAR}")
}

/// Сокет API: `RAYCAT_SOCKET`, иначе `/run/raycat/raycat.sock` для root, иначе
/// `$XDG_RUNTIME_DIR/raycat.sock`, а без него — `raycat.sock` в каталоге состояния.
pub(crate) fn socket_path(env: &Env, root: bool, state: &Path) -> PathBuf {
    if let Some(path) = set(env, SOCKET_VAR) {
        return PathBuf::from(path);
    }
    if root {
        return PathBuf::from(ROOT_SOCKET);
    }
    if let Some(dir) = set(env, "XDG_RUNTIME_DIR")
        && Path::new(dir).is_absolute()
    {
        return Path::new(dir).join("raycat.sock");
    }
    state.join("raycat.sock")
}

/// Сокет для клиентских команд. `RAYCAT_SOCKET` задан: только он. Иначе первый
/// существующий из: путь, который выбрал бы демон, затем служба от root, затем образ Docker.
pub(crate) fn client_socket(env: &Env, root: bool) -> Result<PathBuf> {
    client_socket_where(env, root, Path::exists)
}

fn client_socket_where(env: &Env, root: bool, exists: impl Fn(&Path) -> bool) -> Result<PathBuf> {
    let own = own_socket(env, root);
    if set(env, SOCKET_VAR).is_some() {
        return own;
    }
    if let Ok(path) = own.as_ref()
        && exists(path)
    {
        return Ok(path.clone());
    }
    match FALLBACK_SOCKETS
        .into_iter()
        .map(PathBuf::from)
        .find(|path| exists(path))
    {
        Some(path) => Ok(path),
        None => own,
    }
}

/// Путь, который выбрал бы демон. Без каталога состояния годится, только если
/// сокет лежит вне него.
fn own_socket(env: &Env, root: bool) -> Result<PathBuf> {
    match state_dir(env, root) {
        Ok(state) => Ok(socket_path(env, root, &state)),
        Err(error) => {
            let path = socket_path(env, root, Path::new(""));
            if path
                .parent()
                .is_some_and(|parent| !parent.as_os_str().is_empty())
            {
                Ok(path)
            } else {
                Err(error)
            }
        }
    }
}

#[allow(unsafe_code)]
pub(crate) fn euid() -> u32 {
    // SAFETY: geteuid не принимает аргументов и не может завершиться ошибкой.
    unsafe { libc::geteuid() }
}

pub(crate) fn is_root() -> bool {
    euid() == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn explicit_config_wins() {
        let env = env(&[("RAYCAT_CONFIG", "/from/env.toml")]);
        let path = config_file(Some(Path::new("/from/flag.toml")), &env, |_| true);
        assert_eq!(path, Some(PathBuf::from("/from/flag.toml")));
    }

    #[test]
    fn environment_config_comes_next() {
        let env = env(&[("RAYCAT_CONFIG", "/from/env.toml")]);
        let path = config_file(None, &env, |_| true);
        assert_eq!(path, Some(PathBuf::from("/from/env.toml")));
    }

    #[test]
    fn default_config_is_used_only_when_it_exists() {
        let path = config_file(None, &env(&[]), |path| path == Path::new(DEFAULT_CONFIG));
        assert_eq!(path, Some(PathBuf::from("/etc/raycat/config.toml")));
        assert_eq!(config_file(None, &env(&[]), |_| false), None);
    }

    #[test]
    fn an_empty_variable_is_the_same_as_none() {
        let env = env(&[("RAYCAT_CONFIG", "")]);
        assert_eq!(config_file(None, &env, |_| false), None);
    }

    #[test]
    fn state_dir_prefers_the_variable() {
        let env = env(&[("RAYCAT_STATE_DIR", "/data"), ("HOME", "/home/u")]);
        assert_eq!(state_dir(&env, true).unwrap(), PathBuf::from("/data"));
        assert_eq!(state_dir(&env, false).unwrap(), PathBuf::from("/data"));
    }

    #[test]
    fn root_uses_var_lib() {
        let env = env(&[("HOME", "/root"), ("XDG_STATE_HOME", "/x")]);
        assert_eq!(
            state_dir(&env, true).unwrap(),
            PathBuf::from("/var/lib/raycat")
        );
    }

    #[test]
    fn a_user_follows_xdg_then_home() {
        let xdg = env(&[("XDG_STATE_HOME", "/xdg/state"), ("HOME", "/home/u")]);
        assert_eq!(
            state_dir(&xdg, false).unwrap(),
            PathBuf::from("/xdg/state/raycat")
        );
        let home = env(&[("HOME", "/home/u")]);
        assert_eq!(
            state_dir(&home, false).unwrap(),
            PathBuf::from("/home/u/.local/state/raycat")
        );
        let relative = env(&[("XDG_STATE_HOME", "relative"), ("HOME", "/home/u")]);
        assert_eq!(
            state_dir(&relative, false).unwrap(),
            PathBuf::from("/home/u/.local/state/raycat")
        );
    }

    #[test]
    fn the_socket_path_follows_the_variable_then_the_user() {
        let state = Path::new("/state");
        let explicit = env(&[
            ("RAYCAT_SOCKET", "/tmp/x.sock"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        assert_eq!(
            socket_path(&explicit, true, state),
            PathBuf::from("/tmp/x.sock")
        );
        assert_eq!(
            socket_path(&env(&[]), true, state),
            PathBuf::from("/run/raycat/raycat.sock")
        );
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        assert_eq!(
            socket_path(&xdg, false, state),
            PathBuf::from("/run/user/1000/raycat.sock")
        );
        let relative = env(&[("XDG_RUNTIME_DIR", "run")]);
        assert_eq!(
            socket_path(&relative, false, state),
            PathBuf::from("/state/raycat.sock")
        );
        assert_eq!(
            socket_path(&env(&[]), false, state),
            PathBuf::from("/state/raycat.sock")
        );
    }

    fn present(list: &'static [&'static str]) -> impl Fn(&Path) -> bool + Copy {
        move |path| list.iter().any(|item| path == Path::new(*item))
    }

    #[test]
    fn the_client_needs_a_state_dir_only_when_the_socket_is_there() {
        let nothing = present(&[]);
        let explicit = env(&[("RAYCAT_SOCKET", "/tmp/x.sock")]);
        assert_eq!(
            client_socket_where(&explicit, false, nothing).unwrap(),
            PathBuf::from("/tmp/x.sock")
        );
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        assert_eq!(
            client_socket_where(&xdg, false, nothing).unwrap(),
            PathBuf::from("/run/user/1000/raycat.sock")
        );
        assert_eq!(
            client_socket_where(&env(&[]), true, nothing).unwrap(),
            PathBuf::from("/run/raycat/raycat.sock")
        );
        let error = client_socket_where(&env(&[]), false, nothing).unwrap_err();
        assert!(error.to_string().contains("RAYCAT_STATE_DIR"));
    }

    #[test]
    fn an_explicit_socket_is_the_only_one_tried() {
        let explicit = env(&[("RAYCAT_SOCKET", "/tmp/x.sock")]);
        let path = client_socket_where(&explicit, false, present(&[ROOT_SOCKET])).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/x.sock"));
    }

    #[test]
    fn the_own_socket_wins_when_it_exists() {
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let all = &["/run/user/1000/raycat.sock", ROOT_SOCKET, IMAGE_SOCKET];
        let path = client_socket_where(&xdg, false, present(all)).unwrap();
        assert_eq!(path, PathBuf::from("/run/user/1000/raycat.sock"));
    }

    #[test]
    fn a_user_finds_the_service_socket_when_its_own_is_missing() {
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let path = client_socket_where(&xdg, false, present(&[ROOT_SOCKET])).unwrap();
        assert_eq!(path, PathBuf::from(ROOT_SOCKET));
    }

    #[test]
    fn the_service_socket_beats_the_image_one() {
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let both = &[IMAGE_SOCKET, ROOT_SOCKET];
        let path = client_socket_where(&xdg, false, present(both)).unwrap();
        assert_eq!(path, PathBuf::from(ROOT_SOCKET));
    }

    #[test]
    fn the_image_socket_is_the_last_resort() {
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let path = client_socket_where(&xdg, false, present(&[IMAGE_SOCKET])).unwrap();
        assert_eq!(path, PathBuf::from(IMAGE_SOCKET));
    }

    #[test]
    fn without_a_state_dir_the_service_socket_is_still_found() {
        let path = client_socket_where(&env(&[]), false, present(&[ROOT_SOCKET])).unwrap();
        assert_eq!(path, PathBuf::from(ROOT_SOCKET));
    }

    #[test]
    fn without_any_socket_the_own_path_is_named() {
        let xdg = env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let path = client_socket_where(&xdg, false, present(&[])).unwrap();
        assert_eq!(path, PathBuf::from("/run/user/1000/raycat.sock"));
    }

    #[test]
    fn without_any_hint_the_error_names_the_variable() {
        let error = state_dir(&env(&[]), false).unwrap_err();
        assert!(error.to_string().contains("RAYCAT_STATE_DIR"));
    }
}
