//! Автодополнение для оболочек и man-страница из описания командной строки.

use std::io::{self, Write as _};

use anyhow::{Context, Result, bail};
use clap::ArgMatches;
use clap_complete::Shell;

use crate::cli;

fn completions_for(shell: Shell) -> Vec<u8> {
    let mut command = cli::command();
    let mut buffer = Vec::new();
    clap_complete::generate(shell, &mut command, "raycat", &mut buffer);
    buffer
}

fn man_page() -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    clap_mangen::Man::new(cli::command())
        .render(&mut buffer)
        .context("не удалось собрать man-страницу")?;
    Ok(buffer)
}

/// Закрытый канал (`| head`) не повод падать.
fn write_out(bytes: &[u8]) {
    let _ = io::stdout().lock().write_all(bytes);
}

/// `raycat completions <оболочка>`.
pub(crate) fn completions(sub: &ArgMatches) -> Result<()> {
    let shell = match sub.get_one::<String>("shell").map(String::as_str) {
        Some("bash") => Shell::Bash,
        Some("zsh") => Shell::Zsh,
        Some("fish") => Shell::Fish,
        _ => bail!("укажите оболочку: bash, zsh или fish"),
    };
    write_out(&completions_for(shell));
    Ok(())
}

/// `raycat man`: страница в формате roff в stdout.
pub(crate) fn man() -> Result<()> {
    write_out(&man_page()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_know_the_commands_of_every_shell() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let text = String::from_utf8(completions_for(shell)).unwrap();
            for word in ["raycat", "status", "nodes", "completions"] {
                assert!(text.contains(word), "{shell}: нет «{word}»");
            }
            assert!(!text.contains("--nope"));
        }
    }

    #[test]
    fn the_man_page_is_roff_with_our_commands() {
        let text = String::from_utf8(man_page().unwrap()).unwrap();
        assert!(text.contains(".TH raycat"), "{text}");
        for word in ["status", "nodes", "events"] {
            assert!(text.contains(word), "нет «{word}»");
        }
    }

    #[test]
    fn the_man_command_is_hidden_from_help() {
        let help = cli::command().render_help().to_string();
        assert!(!help.contains(" man "), "{help}");
        assert!(cli::command().find_subcommand("man").is_some());
    }
}
