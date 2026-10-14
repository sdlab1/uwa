//! `uwa` — the daemon. Parses the CLI and hands everything to [`wiring`]
//! or [`commands`].

mod commands;
mod wiring;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Parser)]
#[allow(clippy::restriction)]
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

    /// Install / uninstall uwa as a system service (systemd, launchd, sc.exe).
    Service {
        #[command(subcommand)]
        action: commands::service::ServiceAction,
    },

    /// Check for a newer release and (optionally) install it.
    Update {
        /// Only print the version comparison; do not download.
        #[arg(long)]
        check: bool,
        /// Reinstall even if the versions match.
        #[arg(long)]
        force: bool,
    },

    /// Tool-calling conformance suite: run the corpus, generate reports.
    Conformance(commands::conformance::ConformanceArgs),

    /// Provider selector registry: list, show, diff, update, canary.
    Providers(commands::providers::ProvidersArgs),

    /// Print resolved config, provider list and detected browser.
    Doctor,
}

// Track whether we're in a context where we spawned child processes
static CHILD_PROCESSES_SPAWNED: AtomicBool = AtomicBool::new(false);

/// Register a panic hook to clean up child processes on panic.
fn set_panic_hook() {
    std::panic::set_hook(Box::new(|_panic_info| {
        // Kill any child processes we spawned
        if CHILD_PROCESSES_SPAWNED.load(Ordering::SeqCst) {
            eprintln!("\n[panic] cleaning up child processes...");
            #[cfg(unix)]
            {
                // Kill the entire process group (negative PID = PGID)
                let pgid = unsafe { libc::getpgrp() };
                if pgid > 0 {
                    let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
            }
            #[cfg(windows)]
            {
                // On Windows we can't easily kill the process tree from a panic hook
                // without external tools. Best effort: exit immediately.
            }
        }
    }));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    set_panic_hook();

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
        Command::Start(args) => {
            CHILD_PROCESSES_SPAWNED.store(true, Ordering::SeqCst);
            commands::start::run(config_path.clone(), args).await
        }
        Command::Daemon { action } => commands::daemon::run(config_path.clone(), action).await,
        Command::Service { action } => commands::service::run(action).await,
        Command::Update { check, force } => commands::update::run(check, force).await,
        Command::Conformance(args) => commands::conformance::run(args).await,
        Command::Providers(args) => commands::providers::run(args).await,
        Command::Doctor => commands::doctor::run(config_path.clone()).await,
    };

    CHILD_PROCESSES_SPAWNED.store(false, Ordering::SeqCst);
    result.with_context(|| format!("uwa failed for config {config_path:?}"))
}
