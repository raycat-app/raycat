//! `raycat init`: мастер файла настроек. Спрашивает то, что не задано флагами, пишет файл
//! с правами 0600 и проверяет его той же загрузкой, что делает демон.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufRead, IsTerminal as _, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::ArgMatches;
use raycat_config::{Config, Env, Error as ConfigError};
use raycat_subscription::redact;

use crate::cli;
use crate::paths;

const DEFAULT_CONFIG: &str = "/etc/raycat/config.toml";
const SCHEMA_LINE: &str =
    "#:schema https://raw.githubusercontent.com/raycat-app/raycat/main/deploy/config.schema.json";
const SYSTEMD_DIR: &str = "/run/systemd/system";
const SUBSCRIPTION_NAME: &str = "main";
const MAX_LINK_FILE_BYTES: usize = 4096;
const INTERRUPT: char = '\u{3}';
const APP_OPTIONS: [&str; 2] = ["Happ", "INCY"];
const PLATFORM_OPTIONS: [&str; 2] = ["Windows", "Android"];
const MODE_OPTIONS: [&str; 3] = [
    "прокси 127.0.0.1:7890",
    "шлюз для всего сервера",
    "шлюз для сервера и локальной сети",
];
const PLATFORM_HINT: &str =
    "Если провайдер не отдаст узлы, попробуйте другую платформу: raycat init --force";
const DEVICE_HINT: &str = "
# Одно и то же слово даёт одно и то же устройство у провайдера на любом сервере.
# Без него устройство создаётся при первом запуске и хранится в /var/lib/raycat.
# [device]
# seed = \"любое слово или фраза\"
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppChoice {
    Happ,
    Incy,
}

impl AppChoice {
    fn key(self) -> &'static str {
        match self {
            Self::Happ => "happ",
            Self::Incy => "incy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformChoice {
    Windows,
    Android,
}

impl PlatformChoice {
    fn key(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Android => "android",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeChoice {
    Proxy,
    Gateway,
    GatewayLan,
}

#[derive(Debug, PartialEq, Eq)]
struct Answers {
    url: String,
    app: AppChoice,
    platform: PlatformChoice,
    mode: ModeChoice,
}

#[derive(Debug, Default)]
struct Flags {
    config: Option<PathBuf>,
    force: bool,
    subscription: Option<String>,
    subscription_file: Option<PathBuf>,
    app: Option<AppChoice>,
    platform: Option<PlatformChoice>,
    mode: Option<ModeChoice>,
}

impl Flags {
    fn from_matches(sub: &ArgMatches) -> Result<Self> {
        let app = sub
            .get_one::<String>("app")
            .map(|value| match value.as_str() {
                "incy" => AppChoice::Incy,
                _ => AppChoice::Happ,
            });
        let platform = sub
            .get_one::<String>("platform")
            .map(|value| match value.as_str() {
                "android" => PlatformChoice::Android,
                _ => PlatformChoice::Windows,
            });
        if app == Some(AppChoice::Incy) && platform == Some(PlatformChoice::Windows) {
            bail!("для incy доступна только платформа android");
        }
        let mode = combine_mode(
            sub.get_one::<String>("mode").map(String::as_str),
            sub.get_flag("lan"),
        )?;
        Ok(Self {
            config: cli::config_path(sub).cloned(),
            force: sub.get_flag("force"),
            subscription: sub.get_one::<String>("subscription").cloned(),
            subscription_file: sub.get_one::<PathBuf>("subscription-file").cloned(),
            app,
            platform,
            mode,
        })
    }

    /// Ссылка из флага, проверенная теми же правилами, что и настройки.
    fn link(&self) -> Result<Option<String>> {
        let (source, text) = match (&self.subscription, &self.subscription_file) {
            (Some(url), _) => ("ссылка подписки".to_owned(), url.clone()),
            (None, Some(path)) => (format!("файл {}", path.display()), read_link_file(path)?),
            (None, None) => return Ok(None),
        };
        let url = text.trim().to_owned();
        match link_problem(&url) {
            None => Ok(Some(url)),
            Some(problem) => bail!("{source}: {problem}"),
        }
    }
}

fn combine_mode(mode: Option<&str>, lan: bool) -> Result<Option<ModeChoice>> {
    Ok(match (mode, lan) {
        (Some("proxy"), true) => bail!("--lan работает только с --mode gateway"),
        (Some("proxy"), false) => Some(ModeChoice::Proxy),
        (_, true) => Some(ModeChoice::GatewayLan),
        (Some(_), false) => Some(ModeChoice::Gateway),
        (None, false) => None,
    })
}

fn read_link_file(path: &Path) -> Result<String> {
    let bytes =
        fs::read(path).with_context(|| format!("не удалось прочитать {}", path.display()))?;
    if bytes.len() > MAX_LINK_FILE_BYTES {
        bail!("файл {} длиннее 4 КиБ", path.display());
    }
    String::from_utf8(bytes).with_context(|| format!("файл {} не в UTF-8", path.display()))
}

/// Ошибка ссылки так, как её выдают настройки (с замаскированной ссылкой), или `None`.
fn link_problem(url: &str) -> Option<String> {
    let text = format!(
        "[[subscription]]\nname = \"{SUBSCRIPTION_NAME}\"\nurl = {}\napp = \"happ\"\nplatform = \"windows\"\n",
        toml_string(url)
    );
    match Config::from_toml_str(&text, &Env::new()) {
        Ok(_) => None,
        Err(ConfigError::Invalid(problems)) => problems
            .iter()
            .find(|problem| problem.field() == "subscription[0].url")
            .map(|problem| problem.message().to_owned()),
        Err(_) => Some("ссылка не похожа на ссылку подписки".to_owned()),
    }
}

pub(crate) fn run(sub: &ArgMatches, env: &Env) -> Result<()> {
    let flags = Flags::from_matches(sub)?;
    // Тот же выбор, что у демона (`--config`, затем RAYCAT_CONFIG). Файл по умолчанию
    // берётся всегда: `|_| true` говорит, что он есть, и путь не зависит от наличия файла.
    let path = paths::config_file(flags.config.as_deref(), env, |_| true)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    check_overwrite(&path, flags.force)?;
    let link = flags.link()?;
    let answers = if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let mut dialog = Dialog {
            input: io::stdin().lock(),
            output: io::stdout().lock(),
            terminal: true,
        };
        ask_all(&mut dialog, &flags, link)?
    } else {
        from_flags(&flags, link)?
    };
    let backup = save(&path, &render(&answers), env)?;
    let mut out = io::stdout().lock();
    writeln!(out, "Готово: {}", path.display())?;
    if let Some(backup) = backup {
        writeln!(out, "Прежний файл сохранён: {}", backup.display())?;
    }
    writeln!(out, "{}", next_steps(Path::new(SYSTEMD_DIR).is_dir()))?;
    Ok(())
}

fn check_overwrite(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "Файл настроек уже есть: {}. Перезаписать: raycat init --force",
            path.display()
        );
    }
    Ok(())
}

fn next_steps(systemd: bool) -> String {
    let start = if systemd {
        "sudo systemctl start raycat"
    } else {
        "raycat daemon"
    };
    format!("Запустите: {start}\nПроверить: sudo raycat status")
}

fn from_flags(flags: &Flags, link: Option<String>) -> Result<Answers> {
    let Some(url) = link else {
        bail!(
            "без терминала не хватает флагов: --subscription или --subscription-file. Справка: raycat init --help"
        );
    };
    let app = flags.app.unwrap_or(AppChoice::Happ);
    let platform = match app {
        AppChoice::Incy => PlatformChoice::Android,
        AppChoice::Happ => flags.platform.unwrap_or(PlatformChoice::Windows),
    };
    Ok(Answers {
        url,
        app,
        platform,
        mode: flags.mode.unwrap_or(ModeChoice::Proxy),
    })
}

fn ask_all<R: BufRead, W: Write>(
    dialog: &mut Dialog<R, W>,
    flags: &Flags,
    link: Option<String>,
) -> Result<Answers> {
    let url = match link {
        Some(url) => url,
        None => dialog.ask_link()?,
    };
    let app = match flags.app {
        Some(app) => app,
        None => match dialog.choose(
            "Какое приложение указано у провайдера?",
            &APP_OPTIONS,
            0,
            None,
        )? {
            0 => AppChoice::Happ,
            _ => AppChoice::Incy,
        },
    };
    let platform = match (app, flags.platform) {
        (AppChoice::Incy, _) => PlatformChoice::Android,
        (AppChoice::Happ, Some(platform)) => platform,
        (AppChoice::Happ, None) => {
            match dialog.choose("Платформа:", &PLATFORM_OPTIONS, 0, Some(PLATFORM_HINT))? {
                0 => PlatformChoice::Windows,
                _ => PlatformChoice::Android,
            }
        }
    };
    let mode = match flags.mode {
        Some(mode) => mode,
        None => match dialog.choose("Режим:", &MODE_OPTIONS, 0, None)? {
            0 => ModeChoice::Proxy,
            1 => ModeChoice::Gateway,
            _ => ModeChoice::GatewayLan,
        },
    };
    Ok(Answers {
        url,
        app,
        platform,
        mode,
    })
}

/// Вопросы мастера. `terminal` — ввод с настоящего терминала: ссылку тогда не показываем.
struct Dialog<R, W> {
    input: R,
    output: W,
    terminal: bool,
}

impl<R: BufRead, W: Write> Dialog<R, W> {
    fn ask(&mut self, prompt: &str, secret: bool) -> Result<String> {
        write!(self.output, "{prompt}")?;
        self.output.flush()?;
        let hide = secret && self.terminal;
        let mut line = String::new();
        let read = {
            let _echo = hide.then(SecretEcho::enable).transpose()?;
            self.input.read_line(&mut line)?
        };
        if hide {
            writeln!(self.output)?;
        }
        if read == 0 {
            bail!("ввод закончился до ответа, настройки не записаны");
        }
        if line.contains(INTERRUPT) {
            bail!("ввод прерван, настройки не записаны");
        }
        Ok(line)
    }

    fn ask_link(&mut self) -> Result<String> {
        loop {
            let url = self.ask("Ссылка подписки: ", true)?.trim().to_owned();
            match link_problem(&url) {
                None => {
                    writeln!(self.output, "Ссылка принята: {}", redact(&url))?;
                    return Ok(url);
                }
                Some(problem) => writeln!(self.output, "ошибка: {problem}")?,
            }
        }
    }

    /// Номер варианта с нуля. Пустой ответ — вариант по умолчанию.
    fn choose(
        &mut self,
        question: &str,
        options: &[&str],
        default: usize,
        hint: Option<&str>,
    ) -> Result<usize> {
        let mut line = question.to_owned();
        for (index, option) in options.iter().enumerate() {
            let mark = if index == default {
                " (по умолчанию)"
            } else {
                ""
            };
            line = format!("{line} [{}] {option}{mark}", index + 1);
        }
        writeln!(self.output, "{line}")?;
        if let Some(hint) = hint {
            writeln!(self.output, "  {hint}")?;
        }
        loop {
            let answer = self.ask(&format!("Номер [{}]: ", default + 1), false)?;
            match choice_index(&answer, options.len(), default) {
                Some(index) => return Ok(index),
                None => writeln!(self.output, "Введите номер от 1 до {}.", options.len())?,
            }
        }
    }
}

fn choice_index(answer: &str, count: usize, default: usize) -> Option<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Some(default);
    }
    let number: usize = answer.parse().ok()?;
    if (1..=count).contains(&number) {
        Some(number - 1)
    } else {
        None
    }
}

/// Выключает эхо и сигналы терминала на время ввода ссылки. Тогда Ctrl+C приходит
/// символом и не убивает процесс, оставив терминал без эха. Прежние настройки
/// возвращаются при выходе из области видимости.
struct SecretEcho {
    original: libc::termios,
}

impl SecretEcho {
    fn enable() -> io::Result<Self> {
        let original = read_attributes()?;
        let mut hidden = read_attributes()?;
        hidden.c_lflag &= !(libc::ECHO | libc::ISIG);
        write_attributes(&hidden)?;
        Ok(Self { original })
    }
}

impl Drop for SecretEcho {
    fn drop(&mut self) {
        let _ = write_attributes(&self.original);
    }
}

#[allow(unsafe_code)]
fn read_attributes() -> io::Result<libc::termios> {
    // SAFETY: termios состоит из целых чисел, нулевые значения допустимы, tcgetattr её заполнит.
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: tcgetattr пишет только в переданную структуру для стандартного входа.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &raw mut attributes) } == 0 {
        Ok(attributes)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[allow(unsafe_code)]
fn write_attributes(attributes: &libc::termios) -> io::Result<()> {
    // SAFETY: tcsetattr только читает переданную структуру и меняет атрибуты стандартного входа.
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, attributes) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn render(answers: &Answers) -> String {
    let mut text = format!(
        "{SCHEMA_LINE}
# Настройки raycat, созданы командой raycat init.
# После правки: sudo systemctl restart raycat
# Проверка файла и подписок: sudo raycat check
# Описание всех ключей: https://github.com/raycat-app/raycat

[[subscription]]
name = \"{SUBSCRIPTION_NAME}\"
# Ссылка подписки работает как пароль: никому её не показывайте.
url = {}
# Приложение и платформа, которые принимает провайдер. Изменить: raycat init --force
app = \"{}\"
platform = \"{}\"

",
        toml_string(&answers.url),
        answers.app.key(),
        answers.platform.key(),
    );
    text.push_str(mode_block(answers.mode));
    text.push_str(DEVICE_HINT);
    text
}

fn mode_block(mode: ModeChoice) -> &'static str {
    match mode {
        ModeChoice::Proxy => {
            "[mode]
# Прокси для программ на этом сервере: HTTP и SOCKS5.
type = \"proxy\"
listen = \"127.0.0.1:7890\"
"
        }
        ModeChoice::Gateway => {
            "[mode]
# Весь исходящий трафик сервера и его контейнеров идёт через VPN. Нужны nftables и iproute2.
type = \"gateway\"
# Не пускать трафик мимо VPN, пока он недоступен.
kill_switch = true
"
        }
        ModeChoice::GatewayLan => {
            "[mode]
# Трафик сервера и устройств локальной сети идёт через VPN. Нужны nftables и iproute2.
type = \"gateway\"
kill_switch = true
# Устройства локальной сети тоже проходят через шлюз.
lan = true
"
        }
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out = format!("{out}\\u{:04X}", u32::from(c)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Сохраняет файл и проверяет его загрузкой. Старый файл уходит в `.bak`; если новый не
/// прошёл проверку, возвращается старый, а если его не было, новый удаляется.
fn save(path: &Path, text: &str, env: &Env) -> Result<Option<PathBuf>> {
    let previous = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => bail!("не удалось прочитать {}: {error}", path.display()),
    };
    if let Some(dir) = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty() && !dir.exists())
    {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    }
    let backup = if let Some(bytes) = &previous {
        let backup = backup_path(path);
        write_private(&backup, bytes)?;
        Some(backup)
    } else {
        None
    };
    write_private(path, text.as_bytes())?;
    if let Err(error) = Config::load(Some(path), env, paths::is_root()) {
        if let Some(backup) = &backup {
            fs::rename(backup, path).with_context(|| {
                format!("не удалось вернуть прежний файл {}", path.display())
            })?;
            bail!("новые настройки не прошли проверку, прежний файл возвращён: {error}");
        }
        fs::remove_file(path)?;
        bail!("новые настройки не прошли проверку, файл не создан: {error}");
    }
    Ok(backup)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Пишет во временный файл рядом с целевым (права 0600) и переименовывает его: читатель
/// видит либо старое содержимое, либо новое.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .context("у файла настроек нет имени")?
        .to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temp);
    let result = write_then_rename(&temp, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("не удалось записать {}", path.display()))
}

fn write_then_rename(temp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::testing::TempDir;

    const URL: &str = "https://example.com/sub/abcd1234";

    fn answers(app: AppChoice, platform: PlatformChoice, mode: ModeChoice) -> Answers {
        Answers {
            url: URL.to_owned(),
            app,
            platform,
            mode,
        }
    }

    fn scripted(input: &str) -> Dialog<&[u8], Vec<u8>> {
        Dialog {
            input: input.as_bytes(),
            output: Vec::new(),
            terminal: false,
        }
    }

    fn output(dialog: Dialog<&[u8], Vec<u8>>) -> String {
        String::from_utf8(dialog.output).unwrap()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn matches(args: &[&str]) -> ArgMatches {
        let matches = cli::command()
            .try_get_matches_from(std::iter::once("raycat").chain(args.iter().copied()))
            .unwrap();
        matches.subcommand().unwrap().1.clone()
    }

    #[test]
    fn render_names_the_subscription_and_the_proxy() {
        let text = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::Proxy,
        ));
        assert!(text.starts_with(SCHEMA_LINE));
        assert!(text.contains("\n[[subscription]]\nname = \"main\"\n"));
        assert!(text.contains(&format!("url = \"{URL}\"\n")));
        assert!(text.contains("app = \"happ\"\nplatform = \"windows\"\n"));
        assert!(text.contains("type = \"proxy\"\nlisten = \"127.0.0.1:7890\"\n"));
    }

    #[test]
    fn gateway_modes_differ_only_by_lan() {
        let gateway = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::Gateway,
        ));
        assert!(gateway.contains("type = \"gateway\"\n"));
        assert!(gateway.contains("kill_switch = true\n"));
        assert!(!gateway.contains("lan = true"));
        let lan = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::GatewayLan,
        ));
        assert!(lan.contains("lan = true"));
    }

    #[test]
    fn every_combination_loads_with_the_daemon_loader() {
        let temp = TempDir::new("init-combinations");
        let path = temp.path().join("config.toml");
        let pairs = [
            (AppChoice::Happ, PlatformChoice::Windows),
            (AppChoice::Happ, PlatformChoice::Android),
            (AppChoice::Incy, PlatformChoice::Android),
        ];
        for (app, platform) in pairs {
            for mode in [
                ModeChoice::Proxy,
                ModeChoice::Gateway,
                ModeChoice::GatewayLan,
            ] {
                fs::write(&path, render(&answers(app, platform, mode))).unwrap();
                Config::load(Some(&path), &Env::new(), false).unwrap();
            }
        }
    }

    #[test]
    fn toml_strings_escape_quotes_and_control_characters() {
        assert_eq!(toml_string("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(toml_string("a\nb"), "\"a\\u000Ab\"");
    }

    #[test]
    fn link_question_uses_the_settings_rules() {
        assert_eq!(link_problem(URL), None);
        assert_eq!(link_problem("https://example.com/a\"b\\c"), None);
        assert_eq!(link_problem(""), Some("ссылка пустая (…)".to_owned()));
        assert!(
            link_problem("http://example.com/sub/abcd1234")
                .is_some_and(|problem| problem.contains("http небезопасен"))
        );
        assert!(
            link_problem("https://example.com/a\nb")
                .is_some_and(|problem| problem.contains("управляющие"))
        );
    }

    #[test]
    fn enter_takes_the_default_and_other_answers_are_checked() {
        assert_eq!(choice_index("", 2, 0), Some(0));
        assert_eq!(choice_index("  ", 3, 1), Some(1));
        assert_eq!(choice_index("2", 2, 0), Some(1));
        assert_eq!(choice_index("1", 2, 1), Some(0));
        assert_eq!(choice_index("0", 2, 0), None);
        assert_eq!(choice_index("3", 2, 0), None);
        assert_eq!(choice_index("x", 2, 0), None);
    }

    #[test]
    fn a_wrong_menu_answer_asks_again() {
        let mut dialog = scripted("9\n\n");
        let index = dialog.choose("Режим:", &MODE_OPTIONS, 0, None).unwrap();
        assert_eq!(index, 0);
        let text = output(dialog);
        assert!(text.contains("Введите номер от 1 до 3."));
        assert_eq!(text.matches("Номер [1]: ").count(), 2);
    }

    #[test]
    fn a_bad_link_asks_again_and_the_full_link_is_not_shown() {
        let mut dialog = scripted("нет ссылки\n\nhttps://example.com/sub/abcd1234\n");
        assert_eq!(dialog.ask_link().unwrap(), URL);
        let text = output(dialog);
        assert_eq!(text.matches("ошибка: ").count(), 2);
        assert!(text.contains("Ссылка принята: https://example.com/…1234"));
        assert!(!text.contains("sub/abcd1234"));
    }

    #[test]
    fn the_end_of_input_stops_the_link_question() {
        let mut dialog = scripted("");
        assert!(dialog.ask_link().is_err());
    }

    #[test]
    fn enter_on_every_question_gives_the_defaults() {
        let input = format!("{URL}\n\n\n\n");
        let mut dialog = scripted(&input);
        let answers = ask_all(&mut dialog, &Flags::default(), None).unwrap();
        assert_eq!(
            answers,
            self::answers(AppChoice::Happ, PlatformChoice::Windows, ModeChoice::Proxy)
        );
    }

    #[test]
    fn menus_follow_the_answers_and_incy_skips_the_platform() {
        let mut dialog = scripted("2\n3\n");
        let answers = ask_all(&mut dialog, &Flags::default(), Some(URL.to_owned())).unwrap();
        assert_eq!(
            answers,
            self::answers(
                AppChoice::Incy,
                PlatformChoice::Android,
                ModeChoice::GatewayLan
            )
        );
        assert!(!output(dialog).contains("Платформа:"));
    }

    #[test]
    fn flags_replace_their_questions() {
        let mut dialog = scripted("");
        let flags = Flags {
            app: Some(AppChoice::Incy),
            mode: Some(ModeChoice::Gateway),
            ..Flags::default()
        };
        let answers = ask_all(&mut dialog, &flags, Some(URL.to_owned())).unwrap();
        assert_eq!(
            answers,
            self::answers(
                AppChoice::Incy,
                PlatformChoice::Android,
                ModeChoice::Gateway
            )
        );
    }

    #[test]
    fn lan_implies_gateway_and_conflicts_with_proxy() {
        assert_eq!(combine_mode(None, false).unwrap(), None);
        assert_eq!(
            combine_mode(None, true).unwrap(),
            Some(ModeChoice::GatewayLan)
        );
        assert_eq!(
            combine_mode(Some("proxy"), false).unwrap(),
            Some(ModeChoice::Proxy)
        );
        assert!(combine_mode(Some("proxy"), true).is_err());
        assert_eq!(
            combine_mode(Some("gateway"), false).unwrap(),
            Some(ModeChoice::Gateway)
        );
        assert_eq!(
            combine_mode(Some("gateway"), true).unwrap(),
            Some(ModeChoice::GatewayLan)
        );
    }

    #[test]
    fn incy_with_windows_is_refused_at_the_flags() {
        let sub = matches(&["init", "--app", "incy", "--platform", "windows"]);
        assert!(Flags::from_matches(&sub).is_err());
    }

    #[test]
    fn without_a_terminal_the_link_flag_is_required() {
        let error = from_flags(&Flags::default(), None).unwrap_err();
        assert!(error.to_string().contains("--subscription"), "{error}");
    }

    #[test]
    fn without_a_terminal_the_other_values_default() {
        let answers = from_flags(&Flags::default(), Some(URL.to_owned())).unwrap();
        assert_eq!(
            answers,
            self::answers(AppChoice::Happ, PlatformChoice::Windows, ModeChoice::Proxy)
        );
        let flags = Flags {
            app: Some(AppChoice::Incy),
            ..Flags::default()
        };
        let answers = from_flags(&flags, Some(URL.to_owned())).unwrap();
        assert_eq!(answers.platform, PlatformChoice::Android);
    }

    #[test]
    fn an_existing_file_needs_force() {
        let temp = TempDir::new("init-refuse");
        let path = temp.path().join("config.toml");
        assert!(check_overwrite(&path, false).is_ok());
        fs::write(&path, "x").unwrap();
        let error = check_overwrite(&path, false).unwrap_err().to_string();
        assert!(error.contains("raycat init --force"), "{error}");
        assert!(check_overwrite(&path, true).is_ok());
    }

    #[test]
    fn a_new_file_is_private_and_its_directory_too() {
        let temp = TempDir::new("init-save");
        let path = temp.path().join("etc/config.toml");
        let text = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::Proxy,
        ));
        assert_eq!(save(&path, &text, &Env::new()).unwrap(), None);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(mode_of(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn force_keeps_the_old_file_as_a_private_backup() {
        let temp = TempDir::new("init-backup");
        let path = temp.path().join("config.toml");
        fs::write(&path, "old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let text = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::Proxy,
        ));
        let backup = save(&path, &text, &Env::new()).unwrap().unwrap();
        assert_eq!(backup, temp.path().join("config.toml.bak"));
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old\n");
        assert_eq!(mode_of(&backup), 0o600);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_file_that_fails_the_check_is_not_kept() {
        let temp = TempDir::new("init-invalid");
        let path = temp.path().join("config.toml");
        assert!(save(&path, "[[subscription]]\n", &Env::new()).is_err());
        assert!(!path.exists());

        let text = render(&answers(
            AppChoice::Happ,
            PlatformChoice::Windows,
            ModeChoice::Proxy,
        ));
        fs::write(&path, &text).unwrap();
        assert!(save(&path, "не TOML {", &Env::new()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!temp.path().join("config.toml.bak").exists());
    }

    #[test]
    fn next_steps_follow_the_init_system() {
        assert!(next_steps(true).contains("Запустите: sudo systemctl start raycat"));
        assert!(next_steps(false).contains("Запустите: raycat daemon"));
        assert!(next_steps(false).contains("Проверить: sudo raycat status"));
    }
}
