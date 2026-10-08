fn main() -> Result<(), serde_json::Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(&raycat_config::settings_schema())?
    );
    Ok(())
}
