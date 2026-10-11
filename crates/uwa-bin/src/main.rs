//! `uwa` — the daemon. Parses the CLI and hands everything to [`wiring`]
//! or [`commands`].

mod commands;
mod wiring;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Universal Web API — Rust bridge")]
struct Cli {
    /// Path to TOML config.
    #[arg(
        short,
        long,
        env = "UWA_CONFIG",
        default_value = "uwa.toml",
        global = true
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Build nothing, just start: find Chromium, launch it if needed,
    /// start uwa, print the dashboard URL. (Default when no subcommand.)
    Start(commands::start::StartArgs),

    /// Manage the background daemon.
    Daemon {
        #[command(subcommand)]
        action: commands::daemon::DaemonAction,
    },

    /// Print resolved config, provider list and detected browser.
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout belongs to the MCP stdio transport; logs go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config_path = cli.config.clone();

    // `uwa` with no subcommand == `uwa start`.
    let command = cli
        .command
        .unwrap_or(Command::Start(commands::start::StartArgs::default()));

    let result = match command {
        Command::Start(args) => commands::start::run(config_path.clone(), args).await,
        Command::Daemon { action } => commands::daemon::run(config_path.clone(), action).await,
        Command::Doctor => commands::doctor::run(config_path.clone()).await,
    };

    result.with_context(|| format!("uwa failed for config {config_path:?}"))
}
