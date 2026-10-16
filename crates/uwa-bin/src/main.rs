//! `uwa` — the daemon. Parses the CLI and hands everything to [`wiring`]
//! or [`commands`].

mod clients;
mod commands;
mod wiring;

use anyhow::Context;
use clap::{Arg, ArgAction, Command as ClapCommand};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

struct Cli {
    /// Path to TOML config.
    config: PathBuf,
    command: Option<CliCommand>,
}

#[derive(Debug, Clone)]
enum CliCommand {
    /// Build nothing, just start: find Chromium, launch it if needed,
    /// start uwa, print the dashboard URL. (Default when no subcommand.)
    Start(commands::start::StartArgs),

    /// Manage the background daemon.
    Daemon {
        action: commands::daemon::DaemonAction,
    },

    /// Install / uninstall uwa as a system service (systemd, launchd, sc.exe).
    Service {
        action: commands::service::ServiceAction,
    },

    /// Check for a newer release and (optionally) install it.
    Update {
        /// Only print the version comparison; do not download.
        check: bool,
        /// Reinstall even if the versions match.
        force: bool,
    },

    /// Tool-calling conformance suite: run the corpus, generate reports.
    Conformance(commands::conformance::ConformanceArgs),

    /// Provider selector registry: list, show, diff, update, canary.
    Providers(commands::providers::ProvidersArgs),

    /// Point installed editors at this uwa instance.
    Clients(commands::clients::ClientsArgs),

    /// Print resolved config, provider list and detected browser.
    Doctor,
}

/// Build the top-level clap [`ClapCommand`] tree manually, without
/// clap's `Parser`/`Subcommand`/`FromArgMatches` derive traits (those
/// emit `#[allow(clippy::restriction)]` which conflicts with
/// `-F clippy::restriction`).
fn build_clap_command() -> ClapCommand {
    ClapCommand::new("uwa")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Universal Web API — Rust bridge")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .env("UWA_CONFIG")
                .default_value("uwa.toml")
                .global(true)
                .action(ArgAction::Set),
        )
        .subcommand(
            ClapCommand::new("start")
                .about("Build nothing, just start: find Chromium, launch it if needed, start uwa, print the dashboard URL.")
                .args(commands::start::StartArgs::clap_args()),
        )
        .subcommand(
            ClapCommand::new("daemon")
                .about("Manage the background daemon.")
                .subcommands(commands::daemon::DaemonAction::clap_subcommands()),
        )
        .subcommand(
            ClapCommand::new("service")
                .about("Install / uninstall uwa as a system service (systemd, launchd, sc.exe).")
                .subcommands(commands::service::ServiceAction::clap_subcommands()),
        )
        .subcommand(
            ClapCommand::new("update")
                .about("Check for a newer release and (optionally) install it.")
                .arg(
                    Arg::new("check")
                        .long("check")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            ClapCommand::new("conformance")
                .about("Tool-calling conformance suite: run the corpus, generate reports.")
                .subcommands(commands::conformance::ConformanceArgs::clap_subcommands()),
        )
        .subcommand(
            ClapCommand::new("providers")
                .about("Provider selector registry: list, show, diff, update, canary.")
                .subcommands(commands::providers::ProvidersArgs::clap_subcommands()),
        )
        .subcommand(
            ClapCommand::new("clients")
                .about("Point installed editors at this uwa instance.")
                .subcommands(commands::clients::ClientsArgs::clap_subcommands()),
        )
        .subcommand(
            ClapCommand::new("doctor")
                .about("Print resolved config, provider list and detected browser."),
        )
}

/// Parse command-line args into a [`Cli`] value. Returns `None` for the
/// command when no subcommand was given (caller defaults to `Start`).
fn parse_cli() -> Cli {
    let matches = build_clap_command().get_matches();

    let config = matches
        .get_one::<String>("config")
        .cloned()
        .unwrap_or_else(|| "uwa.toml".to_string())
        .into();

    let command = match matches.subcommand() {
        Some(("start", sub)) => Some(CliCommand::Start(
            commands::start::StartArgs::from_matches(sub),
        )),
        Some(("daemon", sub)) => {
            let name = sub.subcommand_name().unwrap_or_else(|| unreachable!());
            let sub_sub = sub
                .subcommand_matches(name)
                .unwrap_or_else(|| unreachable!());
            let action = commands::daemon::DaemonAction::from_matches(name, sub_sub)
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            Some(CliCommand::Daemon { action })
        }
        Some(("service", sub)) => {
            let name = sub.subcommand_name().unwrap_or_else(|| unreachable!());
            let sub_sub = sub
                .subcommand_matches(name)
                .unwrap_or_else(|| unreachable!());
            let action = commands::service::ServiceAction::from_matches(name, sub_sub)
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            Some(CliCommand::Service { action })
        }
        Some(("update", sub)) => Some(CliCommand::Update {
            check: sub.get_flag("check"),
            force: sub.get_flag("force"),
        }),
        Some(("conformance", sub)) => {
            let args = commands::conformance::ConformanceArgs::from_matches(sub)
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            Some(CliCommand::Conformance(args))
        }
        Some(("providers", sub)) => {
            let args = commands::providers::ProvidersArgs::from_matches(sub)
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            Some(CliCommand::Providers(args))
        }
        Some(("clients", sub)) => {
            let args = commands::clients::ClientsArgs::from_matches(sub)
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(2);
                });
            Some(CliCommand::Clients(args))
        }
        Some(("doctor", _)) => Some(CliCommand::Doctor),
        _ => None,
    };

    let command = Some(command.unwrap_or_else(|| {
        CliCommand::Start(commands::start::StartArgs::default())
    }));

    Cli { config, command }
}

// Track whether we're in a context where we spawned child processes
static CHILD_PROCESSES_SPAWNED: AtomicBool = AtomicBool::new(false);

/// Register a panic hook to clean up child processes on panic.
fn set_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        // A custom hook *replaces* the default one, so without this a
        // panic before `CHILD_PROCESSES_SPAWNED` is set (e.g. a clap
        // arg-id/access mismatch during parsing) exits 101 in total
        // silence — exactly the state that hid the `--no-chromium` bug.
        let location = panic_info.location();
        let payload = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            });
        match (location, payload) {
            (Some(loc), Some(msg)) => eprintln!("panic at {loc}: {msg}"),
            (Some(loc), None) => eprintln!("panic at {loc}"),
            (None, Some(msg)) => eprintln!("panic: {msg}"),
            (None, None) => eprintln!("panic"),
        }
        // Kill any child processes we spawned
        if CHILD_PROCESSES_SPAWNED.load(Ordering::SeqCst) {
            eprintln!("\n[panic] cleaning up child processes...");
            #[cfg(unix)]
            {
                // Kill the entire process group (negative PID = PGID)
                let pgid = unsafe { libc::getpgrp() };
                if pgid > 0 {
                    let _ = unsafe { libc::kill(pgid.wrapping_neg(), libc::SIGKILL) };
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

    let cli = parse_cli();
    let config_path = cli.config.clone();

    // `uwa` with no subcommand == `uwa start`.
    let command = cli
        .command
        .unwrap_or(CliCommand::Start(commands::start::StartArgs::default()));

    let result = match command {
        CliCommand::Start(args) => {
            CHILD_PROCESSES_SPAWNED.store(true, Ordering::SeqCst);
            commands::start::run(config_path.clone(), args).await
        }
        CliCommand::Daemon { action } => commands::daemon::run(config_path.clone(), action).await,
        CliCommand::Service { action } => commands::service::run(action).await,
        CliCommand::Update { check, force } => commands::update::run(check, force).await,
        CliCommand::Conformance(args) => commands::conformance::run(args).await,
        CliCommand::Providers(args) => commands::providers::run(args).await,
        CliCommand::Clients(args) => commands::clients::run(args).await,
        CliCommand::Doctor => commands::doctor::run(config_path.clone()).await,
    };

    CHILD_PROCESSES_SPAWNED.store(false, Ordering::SeqCst);
    result.with_context(|| format!("uwa failed for config {config_path:?}"))
}
