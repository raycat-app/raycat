//! Инструмент обновления профиля по захватам настоящего приложения (бинарник
//! `update-profiles`, собирается только с feature `tool`).
//!
//! ```text
//! update-profiles <приложение> <платформа> --release <тег> --artifacts <каталог> [--root <каталог>]
//! ```
//!
//! В `--artifacts` лежат артефакты стенда захвата `capture-<приложение>-<платформа>-…`.
//! Инструмент читает из запросов версию, сборку и хвост User-Agent, обновляет
//! `profiles/<приложение>/<платформа>.toml`, кладёт запросы и условия их получения
//! в `captures/`, а образцы прежних версий удаляет: golden-тесты проверяют все
//! образцы текущим профилем.
//!
//! Ничего не угадывается. Если профиль не воспроизводит захват байт в байт (новый
//! заголовок, другой порядок, другой маркер), файлы остаются как были, а отчёт о
//! различиях печатается и инструмент завершается с ошибкой.

mod capture;
mod clock;
mod edit;
mod plan;
mod report;
mod unrender;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::Platform;

const DEFAULT_ROOT: &str = "crates/emulation";

/// Запуск из командной строки: отчёт и ошибки идут в stdout, чтобы стенд сохранял их
/// одним перенаправлением.
pub fn cli(args: &[String]) -> ExitCode {
    match run(args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    app: String,
    platform: Platform,
    release: String,
    artifacts: PathBuf,
    root: PathBuf,
}

fn is_name(value: &str, extra: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || extra.contains(&b))
}

impl Args {
    fn parse(args: &[String]) -> Result<Self> {
        const USAGE: &str = "update-profiles <приложение> <платформа> --release <тег> --artifacts <каталог> [--root <каталог>]";
        let mut positional = Vec::new();
        let mut release = None;
        let mut artifacts = None;
        let mut root = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let slot = match arg.as_str() {
                "--release" => &mut release,
                "--artifacts" => &mut artifacts,
                "--root" => &mut root,
                flag if flag.starts_with("--") => bail!("неизвестный параметр {flag}\n{USAGE}"),
                _ => {
                    positional.push(arg.clone());
                    continue;
                }
            };
            *slot = Some(
                iter.next()
                    .ok_or_else(|| anyhow!("у параметра {arg} нет значения\n{USAGE}"))?
                    .clone(),
            );
        }
        let [app, platform] = positional.as_slice() else {
            bail!("ожидаются приложение и платформа\n{USAGE}");
        };
        if !is_name(app, b"-") {
            bail!("приложение: только строчные латинские буквы, цифры и «-»");
        }
        let release = release.ok_or_else(|| anyhow!("не задан --release\n{USAGE}"))?;
        let valid_release = !release.is_empty()
            && release.len() <= 64
            && release
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !valid_release {
            bail!("--release: только латинские буквы, цифры, «.», «-» и «_»");
        }
        let artifacts = artifacts.ok_or_else(|| anyhow!("не задан --artifacts\n{USAGE}"))?;
        Ok(Self {
            app: app.clone(),
            platform: platform.parse()?,
            release,
            artifacts: PathBuf::from(artifacts),
            root: PathBuf::from(root.unwrap_or_else(|| DEFAULT_ROOT.to_owned())),
        })
    }
}

fn run(args: &[String]) -> Result<String> {
    let args = Args::parse(args)?;
    let profile_path = args
        .root
        .join("profiles")
        .join(&args.app)
        .join(format!("{}.toml", args.platform));
    let profile_text = fs::read_to_string(&profile_path)
        .with_context(|| format!("профиль {} не прочитан", profile_path.display()))?;
    let artifacts = capture::load(&args.artifacts, &args.app, args.platform)?;
    let output = plan::build(&plan::Inputs {
        app: &args.app,
        platform: args.platform,
        release: &args.release,
        profile_text: &profile_text,
        artifacts: &artifacts,
    })?;
    apply(&args, &profile_path, &output)?;
    Ok(output.summary)
}

#[derive(Deserialize)]
struct Identity {
    app: String,
    platform: String,
}

/// Имена (без расширения) образцов приложения на платформе.
fn existing_stems(dir: &Path, app: &str, platform: Platform) -> Result<Vec<String>> {
    let mut stems = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("не прочитать {}", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let identity: Identity =
            toml::from_str(&text).with_context(|| format!("{} не разобран", path.display()))?;
        if identity.app == app
            && identity.platform.parse::<Platform>().ok() == Some(platform)
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            stems.push(stem.to_owned());
        }
    }
    stems.sort();
    Ok(stems)
}

fn apply(args: &Args, profile_path: &Path, output: &plan::Output) -> Result<()> {
    let dir = args.root.join("captures");
    let old = existing_stems(&dir, &args.app, args.platform)?;
    fs::write(profile_path, &output.profile_text)
        .with_context(|| format!("{} не записан", profile_path.display()))?;
    for file in &output.captures {
        fs::write(
            dir.join(format!("{}.http", file.stem)),
            file.http.as_bytes(),
        )?;
        fs::write(
            dir.join(format!("{}.toml", file.stem)),
            file.toml.as_bytes(),
        )?;
    }
    for stem in old {
        if output.captures.iter().all(|file| file.stem != stem) {
            for extension in ["http", "toml"] {
                let path = dir.join(format!("{stem}.{extension}"));
                if path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("{} не удалён", path.display()))?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn arguments_are_parsed_and_validated() {
        let parsed = Args::parse(&args(&[
            "happ",
            "windows",
            "--release",
            "4.4.8",
            "--artifacts",
            "art",
        ]))
        .unwrap();
        assert_eq!(
            (parsed.app.as_str(), parsed.platform),
            ("happ", Platform::Windows)
        );
        assert_eq!(parsed.release, "4.4.8");
        assert_eq!(parsed.artifacts, PathBuf::from("art"));
        assert_eq!(parsed.root, PathBuf::from(DEFAULT_ROOT));

        let bad = [
            args(&["happ", "--release", "1", "--artifacts", "a"]),
            args(&["happ", "windows", "--artifacts", "a"]),
            args(&["happ", "windows", "--release", "1"]),
            args(&["../x", "windows", "--release", "1", "--artifacts", "a"]),
            args(&["happ", "ios", "--release", "1", "--artifacts", "a"]),
            args(&["happ", "windows", "--release", "1;rm", "--artifacts", "a"]),
            args(&["happ", "windows", "--release"]),
            args(&["happ", "windows", "--nope", "1"]),
        ];
        for list in &bad {
            assert!(Args::parse(list).is_err(), "{list:?}");
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("raycat-update-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn files_are_replaced_and_old_captures_removed() {
        let root = scratch("apply");
        let incy = include_str!("../../profiles/incy/android.toml");
        fs::create_dir_all(root.join("profiles/incy")).unwrap();
        fs::create_dir_all(root.join("captures")).unwrap();
        fs::write(root.join("profiles/incy/android.toml"), incy).unwrap();
        let old_toml = "app = \"incy\"\nplatform = \"android\"\n";
        let other_toml = "app = \"happ\"\nplatform = \"android\"\n";
        for (stem, toml) in [
            ("incy-0.0.1-android", old_toml),
            ("happ-1.0.0-android", other_toml),
        ] {
            fs::write(root.join(format!("captures/{stem}.toml")), toml).unwrap();
            fs::write(root.join(format!("captures/{stem}.http")), "x").unwrap();
        }

        let dir = root.join("artifacts/capture-incy-android-x86_64-9.9.9");
        fs::create_dir_all(&dir).unwrap();
        let request = [
            "GET /sub/capture-android HTTP/1.1",
            "User-Agent: INCY/9.9.9/android Dalvik/2.1.0",
            "Accept: */*",
            "Accept-Language: en-US",
            "x-hwid: 60C76286-FA42-CA7E-66EC-E0B059F47E23",
            "x-device-os: Android",
            "x-ver-os: 14",
            "x-device-model: Google sdk_gphone64_x86_64",
            "x-app-version: 9.9.9",
            "x-device-locale: en_US",
            "x-client: INCY",
            "Host: 127.0.0.1:18080",
            "Connection: Keep-Alive",
            "Accept-Encoding: gzip",
        ]
        .join("\r\n");
        fs::write(dir.join("01.http"), format!("{request}\r\n\r\n")).unwrap();
        fs::write(dir.join("clock.txt"), "Mon Sep 28 12:00:00 UTC 2026\n").unwrap();
        fs::write(
            dir.join("apk-info.txt"),
            "package: name='llc.itdev.incy' versionCode='999' versionName='9.9.9'\n",
        )
        .unwrap();

        let summary = run(&args(&[
            "incy",
            "android",
            "--release",
            "desktop-v9.9.9",
            "--artifacts",
            root.join("artifacts").to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ]))
        .unwrap();
        assert!(summary.contains("incy-9.9.9-android"), "{summary}");

        let profile = fs::read_to_string(root.join("profiles/incy/android.toml")).unwrap();
        assert!(profile.contains("version = \"9.9.9\"\n"), "{profile}");
        assert!(profile.contains("build = \"999\"\n"), "{profile}");
        let toml = fs::read_to_string(root.join("captures/incy-9.9.9-android.toml")).unwrap();
        assert!(toml.contains("release = \"desktop-v9.9.9\"\n"), "{toml}");
        let http = fs::read_to_string(root.join("captures/incy-9.9.9-android.http")).unwrap();
        assert_eq!(http, format!("{request}\r\n\r\n"));
        assert!(!root.join("captures/incy-0.0.1-android.toml").exists());
        assert!(!root.join("captures/incy-0.0.1-android.http").exists());
        assert!(root.join("captures/happ-1.0.0-android.toml").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_run_leaves_the_files_alone() {
        let root = scratch("untouched");
        let incy = include_str!("../../profiles/incy/android.toml");
        fs::create_dir_all(root.join("profiles/incy")).unwrap();
        fs::create_dir_all(root.join("captures")).unwrap();
        fs::write(root.join("profiles/incy/android.toml"), incy).unwrap();
        let dir = root.join("artifacts/capture-incy-android-x86_64-9.9.9");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("clock.txt"), "Mon Sep 28 12:00:00 UTC 2026\n").unwrap();

        let error = run(&args(&[
            "incy",
            "android",
            "--release",
            "9.9.9",
            "--artifacts",
            root.join("artifacts").to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ]))
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("нет захвата 01.http"),
            "{error:#}"
        );
        let profile = fs::read_to_string(root.join("profiles/incy/android.toml")).unwrap();
        assert_eq!(profile, incy);
        let _ = fs::remove_dir_all(&root);
    }
}
