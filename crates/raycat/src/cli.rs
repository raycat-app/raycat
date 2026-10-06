//! Командная строка: справка на русском, цвета как у остальных утилит проекта.

use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};

const HELP_TEMPLATE: &str = "{about}\n\nИспользование: {usage}\n\n{all-args}";
const OPTIONS: &str = "Параметры";
const ARGUMENTS: &str = "Аргументы";

fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::BrightRed.on_default().effects(Effects::BOLD))
        .valid(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
        .invalid(AnsiColor::Yellow.on_default().effects(Effects::BOLD))
}

fn help_flag() -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help("Показать справку")
        .help_heading(OPTIONS)
}

fn version_flag() -> Arg {
    Arg::new("version")
        .short('V')
        .long("version")
        .action(ArgAction::Version)
        .help("Показать версию")
        .help_heading(OPTIONS)
}

fn config_option() -> Arg {
    Arg::new("config")
        .long("config")
        .value_name("ПУТЬ")
        .value_parser(value_parser!(PathBuf))
        .global(true)
        .help("Файл настроек (по умолчанию RAYCAT_CONFIG, иначе /etc/raycat/config.toml)")
        .help_heading(OPTIONS)
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Вывести сырой JSON без цветов")
        .help_heading(OPTIONS)
}

fn subcommand(name: &'static str, about: &'static str, usage: &'static str) -> Command {
    Command::new(name)
        .about(about)
        .override_usage(usage)
        .styles(styles())
        .help_template(HELP_TEMPLATE)
        .disable_help_flag(true)
        .arg(help_flag())
}

fn base_command() -> Command {
    Command::new("raycat")
        .version(env!("RAYCAT_VERSION"))
        .about("Серверный клиент VPN-подписок: шлюз и прокси для хоста и Docker-контейнеров")
        .override_usage("raycat [ПАРАМЕТРЫ] <КОМАНДА>")
        .styles(styles())
        .help_template(HELP_TEMPLATE)
        .subcommand_help_heading("Команды")
        .subcommand_value_name("КОМАНДА")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(help_flag())
        .arg(version_flag())
        .arg(config_option())
        .subcommand(subcommand(
            "daemon",
            "Работать в переднем плане: получать подписки и держать xray (точка входа службы и контейнера)",
            "raycat daemon [ПАРАМЕТРЫ]",
        ))
        .subcommand(subcommand(
            "check",
            "Проверить настройки, собрать конфиг xray из кэша подписок и прогнать xray run -test",
            "raycat check [ПАРАМЕТРЫ]",
        ))
        .subcommand(
            subcommand(
                "fetch",
                "Разово запросить подписку без применения: заголовки, сведения провайдера, узлы, проблемы",
                "raycat fetch [ПАРАМЕТРЫ] <ПОДПИСКА>",
            )
            .arg(
                Arg::new("subscription")
                    .value_name("ПОДПИСКА")
                    .required(true)
                    .help("Имя подписки из настроек")
                    .help_heading(ARGUMENTS),
            ),
        )
        .subcommand(subcommand(
            "identity",
            "Показать эмулируемое устройство по каждой подписке: приложение, User-Agent, HWID, модель",
            "raycat identity [ПАРАМЕТРЫ]",
        ))
}

pub(crate) fn command() -> Command {
    control_commands(base_command())
}

/// Команды, которые работают с запущенным демоном, и служебные.
fn control_commands(command: Command) -> Command {
    command
        .subcommand(
            subcommand(
                "status",
                "Показать состояние демона: режим, xray, текущий узел, подписки",
                "raycat status [ПАРАМЕТРЫ]",
            )
            .arg(json_flag()),
        )
        .subcommand(subcommand(
            "health",
            "Проверить готовность за секунды: демон отвечает, xray работает, узел выбран (код 0 или 1, для HEALTHCHECK)",
            "raycat health [ПАРАМЕТРЫ]",
        ))
        .subcommand(
            subcommand(
                "nodes",
                "Показать таблицу узлов: статус, задержка, трафик (по умолчанию живые и выбранный)",
                "raycat nodes [ПАРАМЕТРЫ]",
            )
            .arg(
                Arg::new("all")
                    .long("all")
                    .action(ArgAction::SetTrue)
                    .help("Показать все узлы, в том числе недоступные и непроверенные")
                    .help_heading(OPTIONS),
            )
            .arg(json_flag()),
        )
        .subcommand(
            subcommand(
                "use",
                "Закрепить узел вручную или вернуть автоматический выбор (raycat use auto)",
                "raycat use [ПАРАМЕТРЫ] <УЗЕЛ>",
            )
            .arg(
                Arg::new("node")
                    .value_name("УЗЕЛ")
                    .required(true)
                    .help("Имя узла, его уникальная часть без учёта регистра, «подписка/имя» или auto")
                    .help_heading(ARGUMENTS),
            )
            .arg(json_flag()),
        )
        .subcommand(
            subcommand(
                "update",
                "Обновить подписки прямо сейчас и показать результат",
                "raycat update [ПАРАМЕТРЫ] [ПОДПИСКА]",
            )
            .arg(
                Arg::new("subscription")
                    .value_name("ПОДПИСКА")
                    .help("Имя подписки из настроек; без неё обновляются все")
                    .help_heading(ARGUMENTS),
            )
            .arg(json_flag()),
        )
        .subcommand(
            subcommand(
                "events",
                "Следить за событиями демона до Ctrl+C: смена узла, обновления подписок, предупреждения",
                "raycat events [ПАРАМЕТРЫ]",
            )
            .arg(json_flag()),
        )
        .subcommand(subcommand(
            "tui",
            "Полноэкранный интерфейс: состояние, узлы, журнал событий; закрепление узла и обновление подписок",
            "raycat tui [ПАРАМЕТРЫ]",
        ))
        .subcommand(
            subcommand(
                "completions",
                "Вывести автодополнение для оболочки (bash, zsh или fish)",
                "raycat completions [ПАРАМЕТРЫ] <ОБОЛОЧКА>",
            )
            .arg(
                Arg::new("shell")
                    .value_name("ОБОЛОЧКА")
                    .required(true)
                    .value_parser(["bash", "zsh", "fish"])
                    .help("bash, zsh или fish")
                    .help_heading(ARGUMENTS),
            ),
        )
        .subcommand(
            subcommand(
                "man",
                "Вывести man-страницу в формате roff",
                "raycat man [ПАРАМЕТРЫ]",
            )
            .hide(true),
        )
}

/// Значение `--config` из подкоманды или из общих параметров.
pub(crate) fn config_path<'a>(matches: &'a ArgMatches, sub: &'a ArgMatches) -> Option<&'a PathBuf> {
    sub.try_get_one::<PathBuf>("config")
        .ok()
        .flatten()
        .or_else(|| matches.try_get_one::<PathBuf>("config").ok().flatten())
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn cli_definition_is_consistent() {
        command().debug_assert();
    }

    #[test]
    fn subcommands_parse() {
        for name in ["daemon", "check", "identity", "tui", "health"] {
            let matches = command().try_get_matches_from(["raycat", name]).unwrap();
            assert_eq!(matches.subcommand_name(), Some(name));
        }
        let matches = command()
            .try_get_matches_from(["raycat", "fetch", "основная"])
            .unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(name, "fetch");
        assert_eq!(
            sub.get_one::<String>("subscription").map(String::as_str),
            Some("основная")
        );
    }

    #[test]
    fn daemon_commands_parse_with_their_arguments() {
        for name in ["status", "events"] {
            let matches = command().try_get_matches_from(["raycat", name]).unwrap();
            let (_, sub) = matches.subcommand().unwrap();
            assert!(!sub.get_flag("json"), "{name}");
            let matches = command()
                .try_get_matches_from(["raycat", name, "--json"])
                .unwrap();
            let (_, sub) = matches.subcommand().unwrap();
            assert!(sub.get_flag("json"), "{name}");
        }

        let matches = command()
            .try_get_matches_from(["raycat", "nodes", "--all", "--json"])
            .unwrap();
        let (_, sub) = matches.subcommand().unwrap();
        assert!(sub.get_flag("all") && sub.get_flag("json"));
        let matches = command().try_get_matches_from(["raycat", "nodes"]).unwrap();
        assert!(!matches.subcommand().unwrap().1.get_flag("all"));

        let matches = command()
            .try_get_matches_from(["raycat", "use", "main/NL-1"])
            .unwrap();
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(
            sub.get_one::<String>("node").map(String::as_str),
            Some("main/NL-1")
        );

        let matches = command()
            .try_get_matches_from(["raycat", "update"])
            .unwrap();
        assert_eq!(
            matches
                .subcommand()
                .unwrap()
                .1
                .get_one::<String>("subscription"),
            None
        );
        let matches = command()
            .try_get_matches_from(["raycat", "update", "main"])
            .unwrap();
        assert_eq!(
            matches
                .subcommand()
                .unwrap()
                .1
                .get_one::<String>("subscription")
                .map(String::as_str),
            Some("main")
        );
    }

    #[test]
    fn completions_accept_only_known_shells() {
        for shell in ["bash", "zsh", "fish"] {
            assert!(
                command()
                    .try_get_matches_from(["raycat", "completions", shell])
                    .is_ok()
            );
        }
        for args in [
            vec!["raycat", "completions"],
            vec!["raycat", "completions", "powershell"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
    }

    #[test]
    fn man_exists_but_is_hidden() {
        assert!(command().try_get_matches_from(["raycat", "man"]).is_ok());
        let help = command().render_help().to_string();
        assert!(!help.contains("roff"), "{help}");
    }

    #[test]
    fn config_can_come_before_or_after_the_command() {
        let after = command()
            .try_get_matches_from(["raycat", "daemon", "--config", "/a.toml"])
            .unwrap();
        let (_, sub) = after.subcommand().unwrap();
        assert_eq!(config_path(&after, sub), Some(&PathBuf::from("/a.toml")));

        let before = command()
            .try_get_matches_from(["raycat", "--config", "/b.toml", "check"])
            .unwrap();
        let (_, sub) = before.subcommand().unwrap();
        assert_eq!(config_path(&before, sub), Some(&PathBuf::from("/b.toml")));

        let none = command().try_get_matches_from(["raycat", "check"]).unwrap();
        let (_, sub) = none.subcommand().unwrap();
        assert_eq!(config_path(&none, sub), None);
    }

    #[test]
    fn usage_mistakes_are_errors() {
        for args in [
            vec!["raycat"],
            vec!["raycat", "nope"],
            vec!["raycat", "fetch"],
            vec!["raycat", "daemon", "--nope"],
            vec!["raycat", "use"],
            vec!["raycat", "status", "--nope"],
            vec!["raycat", "health", "--json"],
            vec!["raycat", "health", "лишний"],
            vec!["raycat", "nodes", "--all", "лишний"],
            vec!["raycat", "tui", "--nope"],
            vec!["raycat", "tui", "лишний"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
    }

    #[test]
    fn help_and_version_are_not_errors() {
        for flag in ["--help", "-h", "--version", "-V"] {
            let error = command()
                .try_get_matches_from(["raycat", flag])
                .unwrap_err();
            assert!(!error.use_stderr(), "{flag}");
            assert!(matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ));
        }
        let error = command()
            .try_get_matches_from(["raycat", "daemon", "--help"])
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn help_is_in_russian() {
        let help = command().render_help().to_string();
        for word in [
            "Использование:",
            "Команды",
            "Параметры",
            "daemon",
            "check",
            "fetch",
            "identity",
            "status",
            "health",
            "nodes",
            "use",
            "update",
            "events",
            "tui",
            "completions",
        ] {
            assert!(help.contains(word), "{word}: {help}");
        }
        assert!(!help.contains("Print help"));
    }

    #[test]
    fn the_version_starts_with_the_package_version() {
        let version = command().render_version();
        assert!(
            version.starts_with(&format!("raycat {}", env!("CARGO_PKG_VERSION"))),
            "{version}"
        );
        assert_eq!(version.trim_end(), format!("raycat {}", env!("RAYCAT_VERSION")));
    }
}
