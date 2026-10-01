//! Процесс xray: запуск, вывод в лог, остановка.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::log::{debug, info, warn};
use crate::util::sanitize;

/// Переменные окружения, которые получает xray; остальное (ссылки подписок, свои
/// настройки) ему не нужно.
const PASSED_ENV: [&str; 4] = ["PATH", "TZ", "SSL_CERT_FILE", "SSL_CERT_DIR"];
const STOP_GRACE: Duration = Duration::from_secs(5);
const CHECK_TAIL_LINES: usize = 20;

pub(crate) struct Exit {
    pub(crate) status: String,
    pub(crate) uptime: Duration,
}

struct Running {
    child: Child,
    started: Instant,
}

pub(crate) struct Process {
    bin: PathBuf,
    config: PathBuf,
    memory_limit: u64,
    running: Option<Running>,
}

impl Process {
    pub(crate) fn new(bin: PathBuf, config: PathBuf, memory_limit: u64) -> Self {
        Self {
            bin,
            config,
            memory_limit,
            running: None,
        }
    }

    /// Номер процесса, пока xray работает.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.running.as_ref().and_then(|running| running.child.id())
    }

    /// `xray run -c <файл>`; должен вызываться внутри рантайма tokio.
    pub(crate) fn start(&mut self) -> Result<()> {
        let mut command = Command::new(&self.bin);
        command
            .arg("run")
            .arg("-c")
            .arg(&self.config)
            .env_clear()
            .env("GOMEMLIMIT", format!("{}B", self.memory_limit))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        pass_environment(|key, value| {
            command.env(key, value);
        });
        die_with_parent(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("не удалось запустить xray ({})", self.bin.display()))?;
        info!("xray запущен (pid {})", child.id().unwrap_or_default());
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(forward(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(forward(stderr));
        }
        self.running = Some(Running {
            child,
            started: Instant::now(),
        });
        Ok(())
    }

    /// Ждёт завершения процесса; пока процесса нет, не завершается никогда.
    pub(crate) async fn exited(&mut self) -> Exit {
        let Some(running) = &mut self.running else {
            return std::future::pending::<Exit>().await;
        };
        let status = running.child.wait().await;
        let uptime = running.started.elapsed();
        self.running = None;
        Exit {
            status: match status {
                Ok(status) => status.to_string(),
                Err(error) => format!("ошибка ожидания: {error}"),
            },
            uptime,
        }
    }

    /// SIGTERM, через 5 секунд SIGKILL.
    pub(crate) async fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        if let Some(pid) = running
            .child
            .id()
            .and_then(|id| libc::pid_t::try_from(id).ok())
        {
            terminate(pid);
        }
        if tokio::time::timeout(STOP_GRACE, running.child.wait())
            .await
            .is_err()
        {
            warn!(
                "xray не завершился за {} с, останавливаю принудительно",
                STOP_GRACE.as_secs()
            );
            let _ = running.child.kill().await;
        }
        info!("xray остановлен");
    }
}

fn pass_environment(mut set: impl FnMut(&str, std::ffi::OsString)) {
    for key in PASSED_ENV {
        if let Some(value) = std::env::var_os(key) {
            set(key, value);
        }
    }
}

#[allow(unsafe_code)]
fn terminate(pid: libc::pid_t) {
    // SAFETY: kill(2) не трогает память процесса; pid принадлежит нашему дочернему
    // процессу, который ещё не собран (wait не завершался).
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Если raycat убит, xray получает SIGTERM и не остаётся держать порт.
#[allow(unsafe_code)]
fn die_with_parent(command: &mut Command) {
    // SAFETY: getpid не имеет побочных эффектов.
    let parent = unsafe { libc::getpid() };
    let signal = libc::c_ulong::try_from(libc::SIGTERM).unwrap_or(15);
    // SAFETY: между fork и exec вызываются только prctl, getppid и _exit: все
    // async-signal-safe и не выделяют память.
    unsafe {
        command.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, signal);
            // Родитель мог умереть до вызова prctl.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

async fn forward(reader: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        report(&line);
    }
}

/// Вывод xray идёт на уровне debug; предупреждением становится только то, что говорит
/// о проблеме самого xray. Ошибки соединений с мёртвыми узлами (`failed to dial`,
/// `failed to resolve ip`) сюда не относятся: состояние узлов показывает выбор узла.
fn report(line: &str) {
    let text = sanitize(strip_timestamp(line.trim()));
    if text.is_empty() {
        return;
    }
    if is_xray_problem(&text) {
        warn!("xray: {text}");
    } else {
        debug!("xray: {text}");
    }
}

fn is_xray_problem(text: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "failed to start",
        "failed to load config",
        "failed to listen",
        "address already in use",
        "panic:",
        "fatal error",
    ];
    let lower = text.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
        || (text.contains("[Error]") && text.contains("infra/conf"))
}

/// xray начинает строку со своей даты (`2026/09/30 12:00:00.123456 [Warning] …`).
fn strip_timestamp(line: &str) -> &str {
    let Some(index) = line.find('[') else {
        return line;
    };
    let head = line.get(..index).unwrap_or_default();
    let is_date = !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '/' | ':' | '.' | ' '));
    if is_date {
        line.get(index..).unwrap_or(line)
    } else {
        line
    }
}

/// `xray run -test -c <файл>`: разбор конфига без запуска.
pub(crate) fn test_config(bin: &Path, config: &Path) -> Result<()> {
    let mut command = std::process::Command::new(bin);
    command
        .args(["run", "-test", "-c"])
        .arg(config)
        .env_clear()
        .stdin(Stdio::null());
    pass_environment(|key, value| {
        command.env(key, value);
    });
    let output = command
        .output()
        .with_context(|| format!("не удалось запустить xray ({})", bin.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines
        .iter()
        .skip(lines.len().saturating_sub(CHECK_TAIL_LINES))
        .map(|line| sanitize(line))
        .collect::<Vec<_>>()
        .join("\n");
    bail!("xray отклонил конфиг ({}):\n{tail}", output.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_stripped() {
        assert_eq!(
            strip_timestamp("2026/09/30 12:00:00.123456 [Warning] core: something"),
            "[Warning] core: something"
        );
        assert_eq!(
            strip_timestamp("2026/09/30 12:00:00 [Error] app/dns: failed"),
            "[Error] app/dns: failed"
        );
    }

    #[test]
    fn dead_node_errors_are_not_xray_problems() {
        for line in [
            "[Error] transport/internet/websocket: failed to dial to (wss://example.com/ws): dial tcp: i/o timeout",
            "[Error] app/proxyman/outbound: failed to resolve ip > returning nil for domain 33",
            "[Warning] proxy/http: failed to read response from 203.0.113.5:80 > unexpected EOF",
            "[Info] infra/conf/serial: Reading config: &{Name:/x/xray.json Format:json}",
        ] {
            assert!(!is_xray_problem(line), "{line}");
        }
    }

    #[test]
    fn config_and_port_failures_are_xray_problems() {
        for line in [
            "Failed to start: main: failed to load config files: [/x/xray.json] > invalid",
            "[Error] infra/conf: unknown protocol foo",
            "[Warning] app/proxyman/inbound: failed to listen TCP on 127.0.0.1:7890 > listen tcp 127.0.0.1:7890: bind: address already in use",
            "panic: runtime error: invalid memory address",
        ] {
            assert!(is_xray_problem(line), "{line}");
        }
    }

    #[test]
    fn other_lines_are_left_alone() {
        assert_eq!(
            strip_timestamp("Xray 26.9.9 started"),
            "Xray 26.9.9 started"
        );
        assert_eq!(strip_timestamp("[Info] core: ok"), "[Info] core: ok");
        assert_eq!(
            strip_timestamp("failed to load [config]"),
            "failed to load [config]"
        );
        assert_eq!(strip_timestamp(""), "");
    }
}
