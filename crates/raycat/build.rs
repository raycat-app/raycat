use std::env;

const COMMIT_LEN: usize = 7;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RAYCAT_COMMIT");

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let commit: String = env::var("RAYCAT_COMMIT")
        .unwrap_or_default()
        .trim()
        .chars()
        .take(COMMIT_LEN)
        .collect();
    let full = if !commit.is_empty() && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{version} ({commit})")
    } else {
        version
    };
    println!("cargo:rustc-env=RAYCAT_VERSION={full}");
}
