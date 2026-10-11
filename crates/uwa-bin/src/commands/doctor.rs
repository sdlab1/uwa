//! `uwa doctor` — quick smoke output: config, providers, browser.
//! Handy when filing an issue.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::browser;

pub async fn run(config_path: PathBuf) -> Result<()> {
    println!("uwa doctor");
    println!("config:      {}", config_path.display());
    println!("uwa_home:    {}", super::paths::uwa_home().display());

    let cfg = uwa_config::Config::load_from_path(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    println!("bind:        {}:{}", cfg.server.bind, cfg.server.port);
    println!(
        "api_key:     {}",
        if cfg.server.api_key.is_some() {
            "set"
        } else {
            "—"
        }
    );

    println!("providers:   {}", cfg.providers.len());
    for name in cfg.providers.keys() {
        println!("  · {name}");
    }
    println!("models:      {}", cfg.model_aliases.len());

    match browser::detect_chrome() {
        Some(p) => println!("chromium:    {}", p.display()),
        None => println!("chromium:    not found (set UWA_CHROME_BIN)"),
    }
    let cdp = std::env::var("UWA_CHROMIUM_WS").unwrap_or_else(|_| browser::DEFAULT_CDP.into());
    println!(
        "cdp:         {cdp} ({})",
        if browser::cdp_ready(&cdp) {
            "ready"
        } else {
            "not responding"
        }
    );
    Ok(())
}
