use std::env;

const COMMIT_LEN: usize = 7;

/// Номер выпуска для программы. CI вычисляет его по тегам и коммитам
/// (`.github/release/next-version.sh`) и передаёт в `RAYCAT_RELEASE_VERSION`: так
/// бинарник знает номер, под которым выйдет, и при продвижении из dev в stable без
/// пересборки называет себя верно. Без переменной — версия из Cargo.toml.
fn release_version() -> String {
    env::var("RAYCAT_RELEASE_VERSION")
        .ok()
        .map(|version| version.trim().to_owned())
        .filter(|version| {
            !version.is_empty()
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        })
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_default())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RAYCAT_COMMIT");
    println!("cargo:rerun-if-env-changed=RAYCAT_RELEASE_VERSION");

    let version = release_version();
    let commit: String = env::var("RAYCAT_COMMIT")
        .unwrap_or_default()
        .trim()
        .chars()
        .take(COMMIT_LEN)
        .collect();
    let full = if !commit.is_empty() && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{version} ({commit})")
    } else {
        version.clone()
    };
    println!("cargo:rustc-env=RAYCAT_RELEASE={version}");
    println!("cargo:rustc-env=RAYCAT_VERSION={full}");
}
