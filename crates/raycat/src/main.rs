mod api;
mod cli;
mod client;
mod commands;
mod ctl;
mod daemon;
mod gateway;
mod log;
mod manual;
mod paths;
mod plan;
mod render;
mod schedule;
mod selection;
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
    // Ошибки использования clap завершают процесс кодом 2, справка и версия — кодом 0.
    let matches = cli::command().get_matches();
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

fn run(matches: &ArgMatches) -> Result<()> {
    let (name, sub) = matches.subcommand().context("не указана команда")?;
    let env = paths::environment();
    match name {
        "status" | "nodes" | "use" | "update" | "events" => return ctl::run(name, sub, &env),
        "tui" => return tui::run(&env),
        "completions" => return manual::completions(sub),
        "man" => return manual::man(),
        _ => {}
    }
    let file = paths::config_file(
        cli::config_path(matches, sub).map(std::path::PathBuf::as_path),
        &env,
        Path::exists,
    );
    let config = Config::load(file.as_deref(), &env)?;
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
