//! Артефакты стенда захвата: запрос приложения и данные, при которых он сделан.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use super::clock::{self, Instant};
use crate::{Arch, Platform};

/// Запрос подписки не бывает большим: всё сверх этого — не захват, а мусор.
const MAX_FILE: u64 = 16 * 1024;

#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) text: String,
    pub(super) target: String,
    pub(super) headers: Vec<(String, String)>,
}

impl Request {
    pub(super) fn parse(raw: &[u8]) -> Result<Self> {
        let text = String::from_utf8(raw.to_vec()).map_err(|_| anyhow!("запрос не в UTF-8"))?;
        let Some(head) = text.strip_suffix("\r\n\r\n") else {
            bail!("запрос не заканчивается пустой строкой или у него есть тело");
        };
        if head.replace("\r\n", "").contains(['\r', '\n']) {
            bail!("окончания строк запроса изменены: ожидается CRLF");
        }
        let mut lines = head.split("\r\n");
        let first = lines.next().unwrap_or_default();
        let target = first
            .strip_prefix("GET ")
            .and_then(|rest| rest.strip_suffix(" HTTP/1.1"))
            .filter(|target| {
                target.starts_with('/') && target.bytes().all(|b| b.is_ascii_graphic())
            })
            .ok_or_else(|| anyhow!("первая строка не «GET /путь HTTP/1.1»: {first}"))?
            .to_owned();
        let headers = lines
            .map(|line| {
                line.split_once(": ")
                    .filter(|(name, _)| !name.is_empty())
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .ok_or_else(|| anyhow!("строка заголовка без «: »: {line}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            text,
            target,
            headers,
        })
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn names(&self) -> Vec<String> {
        self.headers.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// Один прогон стенда: каталог `capture-<приложение>-<платформа>-<архитектура>-…`.
#[derive(Debug, Clone)]
pub(super) struct Artifact {
    pub(super) dir_name: String,
    pub(super) arch: Arch,
    pub(super) odd_day: bool,
    pub(super) request: Request,
    pub(super) instants: Vec<Instant>,
    pub(super) version_code: Option<String>,
}

/// `versionCode` из `apk-info.txt` (вывод `aapt dump badging`).
pub(super) fn version_code(apk_info: &str) -> Option<String> {
    let line = apk_info.lines().find(|line| line.starts_with("package:"))?;
    let digits: String = line
        .split_once("versionCode='")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// Артефакты приложения из каталога, куда скачаны все артефакты прогона.
pub(super) fn load(dir: &Path, app: &str, platform: Platform) -> Result<Vec<Artifact>> {
    let prefix = format!("capture-{app}-{platform}-");
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("не прочитать {}", dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && entry.path().is_dir() {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        bail!("в {} нет артефактов {prefix}*", dir.display());
    }
    names
        .iter()
        .map(|name| load_one(&dir.join(name), name, &prefix))
        .collect()
}

fn read_limited(path: &Path) -> Result<Option<Vec<u8>>> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(None);
    };
    if metadata.len() > MAX_FILE {
        bail!("{} больше {MAX_FILE} байт", path.display());
    }
    Ok(Some(fs::read(path)?))
}

fn load_one(dir: &Path, name: &str, prefix: &str) -> Result<Artifact> {
    let rest = name.strip_prefix(prefix).unwrap_or_default();
    let (arch, _) = rest
        .split_once('-')
        .ok_or_else(|| anyhow!("{name}: в имени артефакта нет архитектуры"))?;
    let arch: Arch = arch.parse().with_context(|| name.to_owned())?;
    let raw = read_limited(&dir.join("01.http"))?.ok_or_else(|| {
        anyhow!(
            "в артефакте {name} нет захвата 01.http: приложение не запросило подписку, \
             причину ищите в server.log, снимках экрана и logcat-app.txt этого артефакта"
        )
    })?;
    let request = Request::parse(&raw).with_context(|| format!("{name}/01.http"))?;
    let clock = read_limited(&dir.join("clock.txt"))?.unwrap_or_default();
    let instants = clock::parse(&String::from_utf8_lossy(&clock));
    if instants.is_empty() {
        bail!("{name}: в clock.txt нет ни одного момента времени");
    }
    let version_code = read_limited(&dir.join("apk-info.txt"))?
        .and_then(|raw| version_code(&String::from_utf8_lossy(&raw)));
    Ok(Artifact {
        dir_name: name.to_owned(),
        arch,
        odd_day: name.ends_with("-odd-day"),
        request,
        instants,
        version_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(lines: &[&str]) -> Vec<u8> {
        format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes()
    }

    #[test]
    fn parses_a_captured_request() {
        let request = Request::parse(&raw(&[
            "GET /sub/capture-windows HTTP/1.1",
            "Host: 127.0.0.1:18080",
            "X-Hwid: abc",
        ]))
        .unwrap();
        assert_eq!(request.target, "/sub/capture-windows");
        assert_eq!(request.names(), ["Host", "X-Hwid"]);
        assert_eq!(request.header("host"), Some("127.0.0.1:18080"));
        assert_eq!(request.header("missing"), None);
    }

    #[test]
    fn rejects_what_is_not_a_plain_get() {
        assert!(Request::parse(b"GET / HTTP/1.1\r\nHost: x\r\n").is_err());
        assert!(Request::parse(b"GET / HTTP/1.1\nHost: x\n\n").is_err());
        assert!(Request::parse(&raw(&["POST / HTTP/1.1"])).is_err());
        assert!(Request::parse(&raw(&["GET / HTTP/1.1", "no-colon"])).is_err());
        assert!(Request::parse(&raw(&["GET / HTTP/1.1", "A: b", "", "body"])).is_err());
        assert!(Request::parse(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn version_code_comes_from_the_package_line() {
        let info = "package: name='x.y' versionCode='370' versionName='3.7.0'\nsdkVersion:'24'\n";
        assert_eq!(version_code(info).as_deref(), Some("370"));
        assert_eq!(version_code("sdkVersion:'24'"), None);
        assert_eq!(version_code("package: name='x' versionCode=''"), None);
    }
}
