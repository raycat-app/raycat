use std::io::{ErrorKind, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result};

const MAX_STDOUT: u64 = 256 * 1024;
const MAX_STDERR: usize = 400;
const MAX_STDERR_READ: u64 = 1600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    /// Очищена от управляющих символов и обрезана: попадает в сообщения об ошибках.
    pub(crate) stderr: String,
}

/// Запуск внешних команд; подменяется в тестах.
pub(crate) trait Executor {
    /// `Err` только если команду не удалось запустить; ненулевой код возврата —
    /// обычный `Output` с `success == false`.
    fn run(&self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<Output>;
}

pub(crate) struct System;

impl Executor for System {
    fn run(&self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!("не удалось запустить `{program}`: установлен ли он и есть ли доступ к PATH")
            })?;

        // Ввод пишется в отдельном потоке: команда может писать в stdout, пока мы
        // ещё не дочитали, и обе стороны упрутся в буфер канала.
        let writer = match (stdin, child.stdin.take()) {
            (Some(text), Some(mut pipe)) => {
                let text = text.to_owned();
                Some(thread::spawn(move || pipe.write_all(text.as_bytes())))
            }
            _ => None,
        };
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_reader = thread::spawn(move || read_capped(stdout_pipe, MAX_STDOUT));
        let stderr_reader = thread::spawn(move || read_capped(stderr_pipe, MAX_STDERR_READ));
        let status = child
            .wait()
            .with_context(|| format!("ожидание `{program}`"))?;
        if let Some(writer) = writer
            && let Ok(Err(error)) = writer.join()
            && error.kind() != ErrorKind::BrokenPipe
        {
            return Err(error).with_context(|| format!("передача ввода в `{program}`"));
        }
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        Ok(Output {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: sanitize(&String::from_utf8_lossy(&stderr)),
        })
    }
}

fn read_capped(pipe: Option<impl Read>, limit: u64) -> Vec<u8> {
    let mut data = Vec::new();
    if let Some(pipe) = pipe {
        let mut limited = pipe.take(limit);
        let _ = limited.read_to_end(&mut data);
        // Остаток отбрасываем, чтобы команда не блокировалась на записи.
        let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
    }
    data
}

/// Оставляет печатные символы в одну строку и не больше `MAX_STDERR` знаков.
pub(crate) fn sanitize(text: &str) -> String {
    let flat: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = String::new();
    let mut last_space = false;
    for c in flat.chars() {
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    if out.chars().count() > MAX_STDERR {
        out = out.chars().take(MAX_STDERR).collect();
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_flattens_and_truncates() {
        assert_eq!(sanitize("  Error: bad\n\tline  two\r\n"), "Error: bad line two");
        assert_eq!(sanitize("a\u{1b}[31mred"), "a [31mred");
        let long = "x".repeat(1000);
        let cut = sanitize(&long);
        assert_eq!(cut.chars().count(), MAX_STDERR + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn missing_program_is_reported_in_russian() {
        let error = System
            .run("raycat-no-such-program", &[], None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("не удалось запустить"), "{error}");
    }
}
