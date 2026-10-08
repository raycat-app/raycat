//! Командная строка: справка на русском, цвета как у остальных утилит проекта.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};

use crate::speedtest;

const HELP_TEMPLATE: &str = "{about}\n\nИспользование: {usage}\n\n{all-args}{after-help}";
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
        .help("Файл настроек (по умолчанию RAYCAT_CONFIG, иначе /etc/raycat/config.toml)")
        .help_heading(OPTIONS)
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Вывод в JSON для скриптов")
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
        .subcommand(
            subcommand(
                "daemon",
                "Работать в переднем плане: получать подписки и держать xray (точка входа службы и контейнера)",
                "raycat daemon [ПАРАМЕТРЫ]",
            )
            .arg(config_option()),
        )
        .subcommand(
            subcommand(
                "check",
                "Проверить настройки, собрать конфиг xray из кэша подписок и прогнать xray run -test",
                "raycat check [ПАРАМЕТРЫ]",
            )
            .arg(config_option()),
        )
        .subcommand(
            subcommand(
                "fetch",
                "Разово запросить подписку без применения: заголовки, сведения провайдера, узлы, проблемы",
                "raycat fetch [ПАРАМЕТРЫ] <ПОДПИСКА>",
            )
            .arg(config_option())
            .arg(
                Arg::new("subscription")
                    .value_name("ПОДПИСКА")
                    .required(true)
                    .help("Имя подписки из настроек")
                    .help_heading(ARGUMENTS),
            ),
        )
        .subcommand(
            subcommand(
                "identity",
                "Показать эмулируемое устройство по каждой подписке: приложение, User-Agent, HWID, модель",
                "raycat identity [ПАРАМЕТРЫ]",
            )
            .arg(config_option()),
        )
        .subcommand(init_command())
}

fn speedtest_command() -> Command {
    subcommand(
        "speedtest",
        "Тест скорости через VPN: замеры через текущий узел или через узел из аргумента",
        "raycat speedtest [ПАРАМЕТРЫ] [УЗЕЛ]",
    )
    .after_help(
        "Примеры:\n  raycat speedtest\n  raycat speedtest Финляндия --streams 8\n  raycat speedtest --size 100MB --json",
    )
    .arg(
        Arg::new("node")
            .value_name("УЗЕЛ")
            .help("Узел, как в raycat use: на время теста закрепляется; без него тест идёт через текущий")
            .help_heading(ARGUMENTS),
    )
    .arg(
        Arg::new("size")
            .long("size")
            .value_name("РАЗМЕР")
            .value_parser(speedtest::parse_size)
            .help("Объём замера: 25MB (по умолчанию) или 25MiB, от 1 МБ до 200 МБ")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("streams")
            .long("streams")
            .value_name("N")
            .value_parser(value_parser!(u8).range(1..=8))
            .help("Один замер с N потоками (1–8); без параметра — замеры с 1 и 4 потоками")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("url")
            .long("url")
            .value_name("АДРЕС")
            .value_parser(speedtest::check_url)
            .help("Свой адрес https вместо тестового сервера: качается до --size или до конца ответа")
            .help_heading(OPTIONS),
    )
    .arg(json_flag())
}

fn init_command() -> Command {
    subcommand(
        "init",
        "Мастер настроек: спрашивает ссылку подписки, приложение, платформу и режим и пишет файл настроек",
        "raycat init [ПАРАМЕТРЫ]",
    )
    .after_help(
        "Примеры:\n  sudo raycat init\n  sudo raycat init --force\n  raycat init --config ./config.toml --subscription https://example.com/sub/… --mode gateway",
    )
    .arg(config_option())
    .arg(
        Arg::new("force")
            .long("force")
            .action(ArgAction::SetTrue)
            .help("Перезаписать существующий файл; старый сохранится как ФАЙЛ.bak")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("subscription")
            .long("subscription")
            .value_name("ССЫЛКА")
            .conflicts_with("subscription-file")
            .help("Ссылка подписки; без неё мастер спросит")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("subscription-file")
            .long("subscription-file")
            .value_name("ПУТЬ")
            .value_parser(value_parser!(PathBuf))
            .conflicts_with("subscription")
            .help("Файл с одной ссылкой подписки (до 4 КиБ)")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("app")
            .long("app")
            .value_name("ПРИЛОЖЕНИЕ")
            .value_parser(["happ", "incy"])
            .hide_possible_values(true)
            .help("happ (по умолчанию) или incy: какое приложение указано у провайдера")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("platform")
            .long("platform")
            .value_name("ПЛАТФОРМА")
            .value_parser(["windows", "android"])
            .hide_possible_values(true)
            .help("windows (по умолчанию) или android; для incy только android")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("mode")
            .long("mode")
            .value_name("РЕЖИМ")
            .value_parser(["proxy", "gateway"])
            .hide_possible_values(true)
            .help("proxy (по умолчанию) или gateway: шлюз для всего сервера")
            .help_heading(OPTIONS),
    )
    .arg(
        Arg::new("lan")
            .long("lan")
            .action(ArgAction::SetTrue)
            .help("Шлюз для сервера и устройств локальной сети (то же, что --mode gateway с локальной сетью)")
            .help_heading(OPTIONS),
    )
}

pub(crate) fn command() -> Command {
    control_commands(base_command())
}

/// Команды, которые работают с запущенным демоном, и служебные.
fn control_commands(command: Command) -> Command {
    utility_commands(client_commands(command))
}

/// Команды, которые работают с запущенным демоном.
fn client_commands(command: Command) -> Command {
    command
        .subcommand(
            subcommand(
                "status",
                "Показать состояние демона: режим, xray, текущий узел, подписки",
                "raycat status [ПАРАМЕТРЫ]",
            )
            .after_help("Примеры:\n  raycat status\n  raycat status --json")
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
            .after_help(
                "Примеры:\n  raycat nodes\n  raycat nodes --all\n  raycat nodes --subscription основная",
            )
            .arg(
                Arg::new("all")
                    .long("all")
                    .action(ArgAction::SetTrue)
                    .help("Показать все узлы, в том числе недоступные и непроверенные")
                    .help_heading(OPTIONS),
            )
            .arg(
                Arg::new("subscription")
                    .short('s')
                    .long("subscription")
                    .value_name("ИМЯ")
                    .help("Только узлы этой подписки")
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
            .after_help(
                "Примеры:\n  raycat use Финляндия\n  raycat use основная/NL-1\n  raycat use auto",
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
            .after_help("Примеры:\n  raycat update\n  raycat update основная")
            .arg(
                Arg::new("subscription")
                    .value_name("ПОДПИСКА")
                    .help("Имя подписки из настроек; без неё обновляются все")
                    .help_heading(ARGUMENTS),
            )
            .arg(json_flag()),
        )
        .subcommand(speedtest_command())
        .subcommand(
            subcommand(
                "events",
                "Следить за событиями демона до Ctrl+C: смена узла, обновления подписок, предупреждения",
                "raycat events [ПАРАМЕТРЫ]",
            )
            .after_help("Примеры:\n  raycat events\n  raycat events --json")
            .arg(json_flag()),
        )
        .subcommand(subcommand(
            "tui",
            "Полноэкранный интерфейс: состояние, узлы, журнал событий; закрепление узла и обновление подписок",
            "raycat tui [ПАРАМЕТРЫ]",
        ))
}

/// Автодополнение, man-страница и справка.
fn utility_commands(command: Command) -> Command {
    command
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
                    .hide_possible_values(true)
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
        .subcommand(
            subcommand(
                "help",
                "Показать справку: общую или по одной команде",
                "raycat help [КОМАНДА]",
            )
            .arg(
                Arg::new("command")
                    .value_name("КОМАНДА")
                    .help("Команда, справку по которой показать")
                    .help_heading(ARGUMENTS),
            ),
        )
}

/// Значение `--config`; есть только у команд, которые читают настройки.
pub(crate) fn config_path(sub: &ArgMatches) -> Option<&PathBuf> {
    sub.try_get_one::<PathBuf>("config").ok().flatten()
}

/// `raycat help [команда]`: общая справка или справка по одной команде.
pub(crate) fn show_help(sub: &ArgMatches) -> Result<()> {
    let mut root = command();
    match sub.get_one::<String>("command") {
        None => root.print_help()?,
        Some(name) => match root.find_subcommand_mut(name) {
            Some(found) => found.print_help()?,
            None => bail!("неизвестная команда «{name}»"),
        },
    }
    Ok(())
}

/// Текст ошибки разбора по-русски. Справку, версию и редкие виды ошибок отдаёт clap (`None`).
pub(crate) fn parse_error_text(error: &clap::Error, args: &[String]) -> Option<String> {
    let problem = match error.kind() {
        ErrorKind::InvalidSubcommand => {
            let name = joined(error, ContextKind::InvalidSubcommand);
            let similar = list(error, ContextKind::SuggestedSubcommand);
            with_similar(format!("неизвестная команда «{name}»"), &similar)
        }
        ErrorKind::UnknownArgument => {
            let arg = joined(error, ContextKind::InvalidArg);
            if arg.starts_with('-') {
                let similar = list(error, ContextKind::SuggestedArg);
                with_similar(format!("неизвестный параметр «{arg}»"), &similar)
            } else {
                format!("лишний аргумент «{arg}»")
            }
        }
        ErrorKind::TooManyValues => {
            format!(
                "лишний аргумент «{}»",
                joined(error, ContextKind::InvalidValue)
            )
        }
        ErrorKind::InvalidValue => {
            let arg = joined(error, ContextKind::InvalidArg);
            let value = joined(error, ContextKind::InvalidValue);
            let valid = list(error, ContextKind::ValidValue);
            if value.is_empty() {
                format!("не указано значение для «{arg}»")
            } else if valid.is_empty() {
                format!("недопустимое значение «{value}» для «{arg}»")
            } else {
                format!(
                    "недопустимое значение «{value}» для «{arg}»: допустимо {}",
                    join_alternatives(&valid)
                )
            }
        }
        ErrorKind::MissingRequiredArgument => {
            let missing = list(error, ContextKind::InvalidArg);
            let (verb, noun) = if missing.len() > 1 {
                ("указаны", "обязательные аргументы")
            } else {
                ("указан", "обязательный аргумент")
            };
            format!("не {verb} {noun} {}", missing.join(", "))
        }
        ErrorKind::MissingSubcommand => "не указана команда".to_owned(),
        ErrorKind::ArgumentConflict => {
            let arg = joined(error, ContextKind::InvalidArg);
            let prior = joined(error, ContextKind::PriorArg);
            format!("нельзя указывать вместе «{arg}» и «{prior}»")
        }
        _ => return None,
    };
    Some(format!("ошибка: {problem}\nСправка: {}", help_hint(args)))
}

fn with_similar(problem: String, similar: &[String]) -> String {
    // clap отдаёт подсказки от менее похожих к более похожим.
    match similar.last() {
        Some(name) => format!("{problem}\n  может быть, «{name}»?"),
        None => problem,
    }
}

fn list(error: &clap::Error, kind: ContextKind) -> Vec<String> {
    match error.get(kind) {
        Some(ContextValue::String(text)) => vec![text.clone()],
        Some(ContextValue::Strings(texts)) => texts.clone(),
        _ => Vec::new(),
    }
}

fn joined(error: &clap::Error, kind: ContextKind) -> String {
    list(error, kind).join(", ")
}

/// `bash, zsh или fish`
fn join_alternatives(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} или {last}", rest.join(", ")),
    }
}

fn help_hint(args: &[String]) -> String {
    match command_from_args(args) {
        Some(name) => format!("raycat {name} --help"),
        None => "raycat --help".to_owned(),
    }
}

/// Команда, к которой относится ошибка: первое слово, которое не параметр и не значение `--config`.
fn command_from_args(args: &[String]) -> Option<String> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--config" {
            words.next();
        } else if !word.starts_with('-') {
            return command()
                .get_subcommands()
                .map(|sub| sub.get_name().to_owned())
                .find(|name| name == word);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_error(args: &[&str]) -> String {
        let error = command()
            .try_get_matches_from(args.iter().copied())
            .unwrap_err();
        let words: Vec<String> = args.iter().skip(1).map(|word| (*word).to_owned()).collect();
        parse_error_text(&error, &words).unwrap()
    }

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
        for args in [
            vec!["raycat", "nodes", "-s", "основная"],
            vec!["raycat", "nodes", "--subscription", "основная"],
        ] {
            let matches = command().try_get_matches_from(args).unwrap();
            let (_, sub) = matches.subcommand().unwrap();
            assert_eq!(
                sub.get_one::<String>("subscription").map(String::as_str),
                Some("основная")
            );
        }
        let matches = command().try_get_matches_from(["raycat", "nodes"]).unwrap();
        assert_eq!(
            matches
                .subcommand()
                .unwrap()
                .1
                .get_one::<String>("subscription"),
            None
        );

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
    fn speedtest_takes_its_arguments() {
        let matches = command()
            .try_get_matches_from([
                "raycat",
                "speedtest",
                "Финляндия",
                "--size",
                "100MB",
                "--streams",
                "8",
                "--url",
                "https://speed.example.com/file.bin",
                "--json",
            ])
            .unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(name, "speedtest");
        assert_eq!(
            sub.get_one::<String>("node").map(String::as_str),
            Some("Финляндия")
        );
        assert_eq!(sub.get_one::<u64>("size"), Some(&100_000_000));
        assert_eq!(sub.get_one::<u8>("streams"), Some(&8));
        assert_eq!(
            sub.get_one::<String>("url").map(String::as_str),
            Some("https://speed.example.com/file.bin")
        );
        assert!(sub.get_flag("json"));
    }

    #[test]
    fn speedtest_refuses_bad_arguments() {
        for args in [
            vec!["raycat", "speedtest", "--size", "10"],
            vec!["raycat", "speedtest", "--size", "300MB"],
            vec!["raycat", "speedtest", "--streams", "9"],
            vec!["raycat", "speedtest", "--streams", "0"],
            vec!["raycat", "speedtest", "--url", "http://example.com/file"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
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
    fn config_is_read_only_by_the_commands_that_load_settings() {
        let root = command();
        let takes_config = |name: &str| {
            root.find_subcommand(name)
                .unwrap()
                .get_arguments()
                .any(|arg| arg.get_long() == Some("config"))
        };
        for name in ["daemon", "check", "fetch", "identity", "init"] {
            assert!(takes_config(name), "{name}");
        }
        for name in [
            "status",
            "health",
            "nodes",
            "use",
            "update",
            "events",
            "tui",
            "completions",
            "man",
            "help",
        ] {
            assert!(!takes_config(name), "{name}");
        }

        let after = command()
            .try_get_matches_from(["raycat", "daemon", "--config", "/a.toml"])
            .unwrap();
        let (_, sub) = after.subcommand().unwrap();
        assert_eq!(config_path(sub), Some(&PathBuf::from("/a.toml")));

        let none = command().try_get_matches_from(["raycat", "check"]).unwrap();
        let (_, sub) = none.subcommand().unwrap();
        assert_eq!(config_path(sub), None);
    }

    #[test]
    fn init_takes_its_flags() {
        let matches = command()
            .try_get_matches_from([
                "raycat",
                "init",
                "--force",
                "--app",
                "incy",
                "--lan",
                "--subscription",
                "https://example.com/sub/abcd1234",
            ])
            .unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(name, "init");
        assert!(sub.get_flag("force") && sub.get_flag("lan"));
        assert_eq!(
            sub.get_one::<String>("app").map(String::as_str),
            Some("incy")
        );
        assert_eq!(sub.get_one::<String>("mode"), None);
    }

    #[test]
    fn init_refuses_two_sources_of_the_link() {
        let args = [
            "raycat",
            "init",
            "--subscription",
            "https://example.com/a",
            "--subscription-file",
            "/tmp/link",
        ];
        let text = parse_error(&args);
        assert!(text.contains("нельзя указывать вместе"), "{text}");
        assert!(text.ends_with("\nСправка: raycat init --help"), "{text}");
    }

    #[test]
    fn help_subcommand_takes_a_command_name() {
        let matches = command()
            .try_get_matches_from(["raycat", "help", "use"])
            .unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(name, "help");
        assert_eq!(
            sub.get_one::<String>("command").map(String::as_str),
            Some("use")
        );

        let matches = command()
            .try_get_matches_from(["raycat", "help", "nope"])
            .unwrap();
        let (_, sub) = matches.subcommand().unwrap();
        assert!(show_help(sub).is_err());
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
            vec!["raycat", "nodes", "--subscription"],
            vec!["raycat", "nodes", "-s"],
            vec!["raycat", "tui", "--nope"],
            vec!["raycat", "tui", "лишний"],
            vec!["raycat", "status", "--config", "/x.toml"],
            vec!["raycat", "tui", "--config", "/x.toml"],
            vec!["raycat", "--config", "/b.toml", "check"],
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
            assert_eq!(parse_error_text(&error, &[]), None, "{flag}");
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
            "init",
            "status",
            "health",
            "nodes",
            "use",
            "update",
            "events",
            "tui",
            "completions",
            "help",
        ] {
            assert!(help.contains(word), "{word}: {help}");
        }
        assert!(!help.contains("Print help"));
    }

    #[test]
    fn commands_with_examples_show_them() {
        let mut root = command();
        let help = root
            .find_subcommand_mut("use")
            .unwrap()
            .render_help()
            .to_string();
        assert!(
            help.contains(
                "Примеры:\n  raycat use Финляндия\n  raycat use основная/NL-1\n  raycat use auto"
            ),
            "{help}"
        );
        for name in ["status", "nodes", "update", "events"] {
            let help = root
                .find_subcommand_mut(name)
                .unwrap()
                .render_help()
                .to_string();
            assert!(help.contains("Примеры:\n  raycat "), "{name}: {help}");
        }
    }

    #[test]
    fn option_and_shell_help_are_short() {
        let mut root = command();
        let status = root
            .find_subcommand_mut("status")
            .unwrap()
            .render_help()
            .to_string();
        assert!(status.contains("Вывод в JSON для скриптов"), "{status}");
        let completions = root
            .find_subcommand_mut("completions")
            .unwrap()
            .render_help()
            .to_string();
        assert!(!completions.contains("possible values"), "{completions}");
    }

    #[test]
    fn parse_errors_are_in_russian() {
        assert_eq!(
            parse_error(&["raycat", "completions", "powershell"]),
            "ошибка: недопустимое значение «powershell» для «<ОБОЛОЧКА>»: допустимо bash, zsh или fish\nСправка: raycat completions --help"
        );
        assert_eq!(
            parse_error(&["raycat", "use"]),
            "ошибка: не указан обязательный аргумент <УЗЕЛ>\nСправка: raycat use --help"
        );
        assert_eq!(
            parse_error(&["raycat", "health", "лишний"]),
            "ошибка: лишний аргумент «лишний»\nСправка: raycat health --help"
        );
        assert_eq!(
            parse_error(&["raycat", "nodez"]),
            "ошибка: неизвестная команда «nodez»\n  может быть, «nodes»?\nСправка: raycat --help"
        );

        let json = parse_error(&["raycat", "health", "--json"]);
        assert!(
            json.starts_with("ошибка: неизвестный параметр «--json»"),
            "{json}"
        );
        assert!(json.ends_with("\nСправка: raycat health --help"), "{json}");

        let config = parse_error(&["raycat", "--config", "/b.toml", "check"]);
        assert!(
            config.contains("неизвестный параметр «--config»"),
            "{config}"
        );
        assert!(
            config.ends_with("\nСправка: raycat check --help"),
            "{config}"
        );
    }
}
