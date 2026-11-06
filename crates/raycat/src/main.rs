mod api;
mod cli;
mod client;
mod commands;
mod ctl;
mod daemon;
mod gateway;
mod init;
mod log;
mod manual;
mod names;
mod paths;
mod plan;
mod render;
mod schedule;
mod selection;
mod speedtest;
mod store;
mod term;
#[cfg(test)]
mod testing;
mod tui;
mod tuning;
mod updater;
mod util;
mod xray;

use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use raycat_config::Config;

use store::Store;

/// Номер выпуска: его вычисляет CI и передаёт сборке (`RAYCAT_RELEASE_VERSION`), без CI —
/// версия из Cargo.toml. Один номер для `--version`, API, журнала и User-Agent.
const VERSION: &str = env!("RAYCAT_RELEASE");

fn main() -> ExitCode {
    let matches = match cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => return parse_failed(&error),
    };
    let result = run(&matches);
    log::flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "ошибка: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Ошибки разбора аргументов выходят с кодом 2, как у clap. Частые виды печатаются по-русски,
/// справка и версия (и редкие виды ошибок) остаются за clap.
fn parse_failed(error: &clap::Error) -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match cli::parse_error_text(error, &args) {
        Some(text) => {
            let _ = writeln!(io::stderr(), "{text}");
            ExitCode::from(2)
        }
        None => error.exit(),
    }
}

/// Куда попадает команда верхнего уровня. Тест перебирает все команды clap и требует маршрут у каждой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// Разговор с запущенным демоном через `ctl::run`.
    Ctl,
    Health,
    Tui,
    Init,
    Config,
    Completions,
    Man,
    Help,
    /// Нужны загруженный файл настроек и стор.
    Settings,
}

fn route(name: &str) -> Option<Route> {
    Some(match name {
        "status" | "nodes" | "use" | "update" | "reload" | "speedtest" | "events" => Route::Ctl,
        "health" => Route::Health,
        "tui" => Route::Tui,
        "init" => Route::Init,
        "config" => Route::Config,
        "completions" => Route::Completions,
        "man" => Route::Man,
        "help" => Route::Help,
        "daemon" | "check" | "fetch" | "identity" => Route::Settings,
        _ => return None,
    })
}

fn run(matches: &ArgMatches) -> Result<()> {
    let (name, sub) = matches.subcommand().context("не указана команда")?;
    let env = paths::environment();
    let kind = route(name).with_context(|| format!("неизвестная команда {name}"))?;
    match kind {
        Route::Ctl => return ctl::run(name, sub, &env),
        Route::Health => return ctl::health(&env),
        Route::Tui => return tui::run(&env),
        Route::Init => return init::run(sub, &env),
        Route::Config => return commands::show_config(sub, &env),
        Route::Completions => return manual::completions(sub),
        Route::Man => return manual::man(),
        Route::Help => return cli::show_help(sub),
        Route::Settings => {}
    }
    let file = paths::config_file(
        cli::config_path(sub).map(std::path::PathBuf::as_path),
        &env,
        Path::exists,
    );
    let config = Config::load(file.as_deref(), &env, paths::is_root())?;
    log::init(config.log.level.into());
    let store = Store::open(paths::state_dir(&env, paths::is_root())?)?;
    match name {
        "daemon" => {
            let socket = paths::socket_path(&env, paths::is_root(), store.root());
            let file = daemon::ConfigFile {
                path: cli::config_path(sub).cloned(),
                env: env.clone(),
                root: paths::is_root(),
            };
            daemon::run(config, file, store, socket)
        }
        "check" => commands::check(&config, &store),
        "fetch" => {
            let subscription = sub
                .get_one::<String>("subscription")
                .context("не указана подписка")?;
            commands::fetch(&config, &store, subscription)
        }
        "identity" => commands::identity(&config, &store),
        other => bail!("неизвестная команда {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_route() {
        for sub in cli::command().get_subcommands() {
            let name = sub.get_name();
            assert!(route(name).is_some(), "{name}");
        }
    }

    #[test]
    fn reload_goes_to_the_daemon() {
        assert_eq!(route("reload"), Some(Route::Ctl));
    }
}
