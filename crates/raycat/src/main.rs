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

fn run(matches: &ArgMatches) -> Result<()> {
    let (name, sub) = matches.subcommand().context("не указана команда")?;
    let env = paths::environment();
    match name {
        "status" | "nodes" | "use" | "update" | "speedtest" | "events" => {
            return ctl::run(name, sub, &env);
        }
        "health" => return ctl::health(&env),
        "tui" => return tui::run(&env),
        "init" => return init::run(sub, &env),
        "completions" => return manual::completions(sub),
        "man" => return manual::man(),
        "help" => return cli::show_help(sub),
        _ => {}
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
            daemon::run(config, store, socket)
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
