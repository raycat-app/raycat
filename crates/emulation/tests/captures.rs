//! Запрос эмуляции против захватов настоящих приложений: побайтно.
//!
//! Каждый захват `captures/<имя>.http` лежит рядом с `<имя>.toml` — условиями, при
//! которых он сделан. Новый захват становится тестом без изменения кода.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use raycat_emulation::{Arch, Device, Emulation, Platform, Url};
use serde::Deserialize;

const PORT: &str = "18080";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    app: String,
    platform: String,
    arch: String,
    url: String,
    unix: u64,
    machine_id: String,
    hwid: Option<String>,
    hostname: Option<String>,
    model: Option<String>,
    os_version: Option<String>,
    locale: Option<String>,
}

fn captures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("captures")
}

/// Имена файлов каталога с расширением `extension`, без расширения.
fn stems(extension: &str) -> BTreeSet<String> {
    fs::read_dir(captures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(extension))
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect()
}

fn generate(input: &Input) -> String {
    let device = Device {
        machine_id: input.machine_id.clone(),
        hostname: input.hostname.clone(),
        model: input.model.clone(),
        os_version: input.os_version.clone(),
        hwid: input.hwid.clone(),
        locale: input.locale.clone().unwrap_or_default(),
    };
    let platform: Platform = input.platform.parse().unwrap();
    let arch: Arch = input.arch.parse().unwrap();
    let emulation = Emulation::new(&input.app, platform, arch, &device).unwrap();
    let url = Url::parse(&input.url.replace("{PORT}", PORT)).unwrap();
    let mut request = format!("GET {} HTTP/1.1\r\n", url.target);
    for (name, value) in emulation.headers(&url, input.unix) {
        write!(request, "{name}: {value}\r\n").unwrap();
    }
    request.push_str("\r\n");
    request
}

#[test]
fn every_capture_is_reproduced_byte_for_byte() {
    let captures = stems("http");
    assert!(
        captures.len() >= 2,
        "захваты не найдены в {}",
        captures_dir().display()
    );
    for stem in &captures {
        let raw = fs::read(captures_dir().join(format!("{stem}.http"))).unwrap();
        let capture = String::from_utf8(raw).unwrap();
        assert!(
            !capture.replace("\r\n", "").contains('\n'),
            "{stem}.http: окончания строк изменены, ожидается CRLF (см. .gitattributes)"
        );
        let text = fs::read_to_string(captures_dir().join(format!("{stem}.toml")))
            .unwrap_or_else(|e| panic!("{stem}.toml: {e}"));
        let input: Input = toml::from_str(&text).unwrap_or_else(|e| panic!("{stem}.toml: {e}"));
        assert_eq!(
            generate(&input),
            capture.replace("{PORT}", PORT),
            "запрос отличается от захвата {stem}"
        );
    }
}

#[test]
fn every_capture_has_its_inputs_and_the_other_way_round() {
    assert_eq!(stems("http"), stems("toml"));
}
