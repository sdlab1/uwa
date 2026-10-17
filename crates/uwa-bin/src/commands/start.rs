//! `uwa start` — the two-step onboarding path:
//!   1. find / launch Chromium with a debug port,
//!   2. run the daemon in the foreground, printing the dashboard URL.

use anyhow::{Context, Result};
use clap::{Arg, ArgAction};
use std::path::PathBuf;

use super::browser;

#[derive(Default, Debug, Clone)]
pub struct StartArgs {
    /// Don't try to launch Chromium; fail if no CDP endpoint is up.
    pub no_chromium: bool,

    /// CDP endpoint. Defaults to http://127.0.0.1:9222.
    pub cdp: Option<String>,

    /// Don't print the "open this URL" hint.
    pub quiet: bool,
}

impl StartArgs {
    pub fn clap_args() -> Vec<clap::Arg> {
        vec![
            Arg::new("no_chromium")
                .long("no-chromium")
                .action(ArgAction::SetTrue),
            Arg::new("cdp")
                .long("cdp")
                .env("UWA_CHROMIUM_WS")
                .action(ArgAction::Set),
            Arg::new("quiet").long("quiet").action(ArgAction::SetTrue),
        ]
    }

    pub fn from_matches(matches: &clap::ArgMatches) -> Self {
        StartArgs {
            no_chromium: matches.get_flag("no_chromium"),
            cdp: matches.get_one::<String>("cdp").cloned(),
            quiet: matches.get_flag("quiet"),
        }
    }
}

pub async fn run(config_path: PathBuf, args: StartArgs) -> Result<()> {
    let cdp = args.cdp.unwrap_or_else(|| browser::DEFAULT_CDP.to_string());

    // 1. Chromium.
    let (_endpoint, chrome_child) =
        browser::ensure_cdp(&cdp, !args.no_chromium).context("preparing Chromium")?;

    // Tell the wiring where to connect. Set as env so we don't have to
    // thread it through `Config`.
    std::env::set_var("UWA_CHROMIUM_WS", &cdp);

    if !args.quiet {
        eprintln!("→ Chromium CDP ready at {cdp}");
    }

    // 2. Ensure config exists (copy from example on first run).
    ensure_config(&config_path)?;

    // 3. Kick the daemon in the foreground.
    let result = crate::wiring::run(config_path).await;

    // 4. Clean up the browser we launched — but only after the daemon
    //    exits, so the transport can be dropped first.
    if let Some(mut c) = chrome_child {
        let _ = c.kill();
        let _ = c.wait();
    }
    result
}

fn ensure_config(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let ex = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
    if ex.exists() {
        std::fs::copy(&ex, path).with_context(|| format!("copy example to {}", path.display()))?;
        eprintln!("→ Created {} from example", path.display());
    }
    Ok(())
}
