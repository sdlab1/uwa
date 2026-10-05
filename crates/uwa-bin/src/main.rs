//! `uwa` — the daemon. Parses the CLI and hands everything to [`wiring`].

mod wiring;

use anyhow::Context;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Universal Web API — Rust bridge")]
struct Cli {
    /// Path to TOML config.
    #[arg(short, long, env = "UWA_CONFIG", default_value = "uwa.toml")]
    config: PathBuf,
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
    wiring::run(cli.config)
        .await
        .with_context(|| format!("uwa failed to start from {}", config_path.display()))
}
