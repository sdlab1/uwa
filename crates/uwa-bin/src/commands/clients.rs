//! `uwa clients list | install | uninstall | status | env | watch`.
//!
//! Every write is backed up first; every patch is idempotent. The
//! installer never touches a file it doesn't recognize — see
//! `crate::clients` for the rules.

use anyhow::{anyhow, Context, Result};
use clap::{Arg, ArgAction, Command};
use std::path::PathBuf;

use super::paths;
use crate::clients::{self, Payload};

#[derive(Debug, Clone)]
pub struct ClientsArgs {
    pub action: ClientsAction,
}

impl ClientsArgs {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        ClientsAction::clap_subcommands()
    }

    pub fn from_matches(matches: &clap::ArgMatches) -> Result<Self, String> {
        let name = matches.subcommand_name().unwrap_or("");
        let sub_matches = matches
            .subcommand_matches(name)
            .ok_or_else(|| format!("missing subcommand matches for '{name}'"))?;
        let action = ClientsAction::from_matches(name, sub_matches)?;
        Ok(ClientsArgs { action })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ClientsAction {
    /// Print every supported client and its current state.
    List {
        /// Output as JSON for programmatic use.
        json: bool,
    },
    /// Patch a client's config to point at this uwa instance.
    Install {
        /// Client id (see `uwa clients list`).
        id: String,
        /// Override the detected config path.
        path: Option<PathBuf>,
        /// Base URL to write. Defaults to http://127.0.0.1:<port>.
        base_url: Option<String>,
        /// API key to write. Defaults to the config's key or `dummy-key`
        /// on loopback.
        api_key: Option<String>,
        /// Model id. Defaults to the first alias in the config.
        model: Option<String>,
    },
    /// Undo a previous `install`.
    Uninstall {
        id: String,
        /// Override the detected config path.
        path: Option<PathBuf>,
        /// Restore from the newest backup instead of stripping our block.
        /// Use this if our install is the *only* thing that ever wrote
        /// the file (env-export clients), and a plain strip would leave
        /// an empty shell.
        from_backup: bool,
    },
    /// Print just the state of one client.
    Status { id: String },
    /// Print a shell snippet the user can `source` for env-only clients.
    Env,
    /// Watch patched files and re-apply the uwa block if an app
    /// overwrites it. Foreground task.
    Watch { interval_secs: u64 },
}

impl ClientsAction {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        vec![
            Command::new("list")
                .about("Print every supported client and its current state.")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue),
                ),
            Command::new("install")
                .about("Patch a client's config to point at this uwa instance.")
                .arg(Arg::new("id").index(1).action(ArgAction::Set))
                .arg(
                    Arg::new("path")
                        .long("path")
                        .action(ArgAction::Set),
                )
                .arg(
                    Arg::new("base_url")
                        .long("base-url")
                        .action(ArgAction::Set),
                )
                .arg(
                    Arg::new("api_key")
                        .long("api-key")
                        .action(ArgAction::Set),
                )
                .arg(
                    Arg::new("model")
                        .long("model")
                        .action(ArgAction::Set),
                ),
            Command::new("uninstall")
                .about("Undo a previous `install`.")
                .arg(Arg::new("id").index(1).action(ArgAction::Set))
                .arg(
                    Arg::new("path")
                        .long("path")
                        .action(ArgAction::Set),
                )
                .arg(
                    Arg::new("from_backup")
                        .long("from-backup")
                        .action(ArgAction::SetTrue),
                ),
            Command::new("status")
                .about("Print just the state of one client.")
                .arg(Arg::new("id").index(1).action(ArgAction::Set)),
            Command::new("env")
                .about("Print a shell snippet the user can `source` for env-only clients."),
            Command::new("watch")
                .about("Watch patched files and re-apply the uwa block if an app overwrites it. Foreground task.")
                .arg(
                    Arg::new("interval_secs")
                        .long("interval-sec")
                        .default_value("3")
                        .action(ArgAction::Set),
                ),
        ]
    }

    pub fn from_matches(name: &str, matches: &clap::ArgMatches) -> Result<Self, String> {
        match name {
            "list" => Ok(ClientsAction::List {
                json: matches.get_flag("json"),
            }),
            "install" => Ok(ClientsAction::Install {
                id: matches
                    .get_one::<String>("id")
                    .ok_or_else(|| "missing required argument 'id'".to_string())?
                    .clone(),
                path: matches.get_one::<PathBuf>("path").cloned(),
                base_url: matches.get_one::<String>("base_url").cloned(),
                api_key: matches.get_one::<String>("api_key").cloned(),
                model: matches.get_one::<String>("model").cloned(),
            }),
            "uninstall" => Ok(ClientsAction::Uninstall {
                id: matches
                    .get_one::<String>("id")
                    .ok_or_else(|| "missing required argument 'id'".to_string())?
                    .clone(),
                path: matches.get_one::<PathBuf>("path").cloned(),
                from_backup: matches.get_flag("from_backup"),
            }),
            "status" => Ok(ClientsAction::Status {
                id: matches
                    .get_one::<String>("id")
                    .ok_or_else(|| "missing required argument 'id'".to_string())?
                    .clone(),
            }),
            "env" => Ok(ClientsAction::Env),
            "watch" => Ok(ClientsAction::Watch {
                interval_secs: matches
                    .get_one::<String>("interval_secs")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(3),
            }),
            _ => Err(format!("unknown clients subcommand: {name}")),
        }
    }
}

pub async fn run(args: ClientsArgs) -> Result<()> {
    match args.action {
        ClientsAction::List { json } => list(json),
        ClientsAction::Status { id } => status(&id),
        ClientsAction::Env => env(),
        ClientsAction::Install {
            id,
            path,
            base_url,
            api_key,
            model,
        } => install(&id, path.as_deref(), base_url, api_key, model),
        ClientsAction::Uninstall {
            id,
            path,
            from_backup,
        } => uninstall(&id, path.as_deref(), from_backup),
        ClientsAction::Watch { interval_secs } => {
            crate::commands::clients_watch::run(interval_secs).await
        }
    }
}

// ---------------------------------------------------------------------------
// payload resolution
// ---------------------------------------------------------------------------

/// Build the payload from the config + CLI overrides. The config is
/// optional — a fresh install that hasn't written `uwa.toml` yet still
/// gets sensible defaults.
fn resolve_payload(
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
) -> Payload {
    let cfg = uwa_config::Config::load_from_path("uwa.toml").ok();

    let origin = base_url.unwrap_or_else(|| {
        let port = cfg.as_ref().map(|c| c.server.port).unwrap_or(8080);
        format!("http://127.0.0.1:{port}")
    });
    let base = if origin.ends_with("/v1") {
        origin.clone()
    } else {
        format!("{origin}/v1")
    };

    let key = api_key.unwrap_or_else(|| {
        cfg.as_ref()
            .and_then(|c| c.server.api_key.clone())
            .unwrap_or_else(|| "dummy-key".into())
    });

    let m = model.unwrap_or_else(|| {
        let mut aliases: Vec<String> = cfg
            .as_ref()
            .map(|c| c.model_aliases.keys().cloned().collect())
            .unwrap_or_default();
        aliases.sort();
        aliases
            .into_iter()
            .next()
            .unwrap_or_else(|| "gpt-4o".into())
    });

    Payload {
        base_url: base,
        origin,
        api_key: key,
        model: m,
    }
}

// ---------------------------------------------------------------------------
// list / status
// ---------------------------------------------------------------------------

fn list(json: bool) -> Result<()> {
    let all = clients::status_all();
    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    println!("{:<12} {:<32} {:<12} path", "id", "name", "state");
    for s in all {
        println!(
            "{:<12} {:<32} {:<12} {}",
            s.id,
            s.name,
            s.state,
            s.path.unwrap_or_else(|| "—".into())
        );
    }
    Ok(())
}

fn status(id: &str) -> Result<()> {
    let def = clients::find(id).ok_or_else(|| anyhow!("unknown client `{id}`"))?;
    let all = clients::status_all();
    let s = all
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow!("no status for `{id}`"))?;
    println!("id:     {}", s.id);
    println!("name:   {}", s.name);
    println!("format: {}", s.format);
    println!("state:  {}", s.state);
    if let Some(p) = &s.path {
        println!("path:   {p}");
    }
    if !s.note.is_empty() {
        println!("hint:   {}", s.note);
    }
    if let Some(b) = &s.last_backup {
        println!("backup: {b}");
    }
    let _ = def;
    Ok(())
}

fn env() -> Result<()> {
    let payload = resolve_payload(None, None, None);
    let p = clients::env_file::path();
    if p.exists() {
        println!("already written to {}. Source it with:", p.display());
        println!("  source {}", p.display());
        return Ok(());
    }
    println!("No env file yet. Run `uwa clients install claude-code` (or any");
    println!("env-only client) to write {}.\n", p.display());
    println!("Manual snippet, if you prefer:");
    println!("  export OPENAI_BASE_URL={}", payload.base_url);
    println!("  export OPENAI_API_KEY={}", payload.api_key);
    println!("  export ANTHROPIC_BASE_URL={}", payload.origin);
    println!("  export ANTHROPIC_API_KEY={}", payload.api_key);
    Ok(())
}

// ---------------------------------------------------------------------------
// install / uninstall
// ---------------------------------------------------------------------------

fn install(
    id: &str,
    path_override: Option<&std::path::Path>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let def = clients::find(id).ok_or_else(|| anyhow!("unknown client `{id}`"))?;
    let payload = resolve_payload(base_url, api_key, model);

    if def.format == clients::Format::Shell {
        let p = clients::env_file::install(&payload)?;
        println!("✓ wrote {}", p.display());
        println!();
        println!("Source it in your shell:");
        println!("  source {}", p.display());
        println!();
        println!("Or add that line to your shell's rc file yourself. We");
        println!("don't modify ~/.bashrc or ~/.zshrc — that's yours.");
        return Ok(());
    }

    let path = match path_override {
        Some(p) => p.to_path_buf(),
        None => def
            .path
            .and_then(|f| f())
            .ok_or_else(|| anyhow!("could not determine config path for `{id}`"))?,
    };

    // Back up whatever is there now (even if we're about to overwrite
    // our own previous patch — a chain of backups is a nicety).
    let backup = if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Some(clients::write_backup(def.id, &path, &contents)?)
    } else {
        None
    };

    match def.id {
        "continue" => clients::continue_vscode::install(&path, &payload)?,
        "codex" => clients::codex_cli::install(&path, &payload)?,
        "zcode" | "autoclaw" => clients::generic_json::install(&path, &payload)?,
        other => anyhow::bail!("client `{other}` has no installer"),
    }

    println!("✓ patched {}", path.display());
    if let Some(b) = &backup {
        println!("  backup: {}", b.display());
    }
    println!();
    println!("Restart {} to pick up the change.", def.name);
    Ok(())
}

fn uninstall(id: &str, path_override: Option<&std::path::Path>, from_backup: bool) -> Result<()> {
    let def = clients::find(id).ok_or_else(|| anyhow!("unknown client `{id}`"))?;

    if matches!(def.format, clients::Format::Shell) {
        match clients::env_file::uninstall()? {
            Some(p) => println!("✓ removed {}", p.display()),
            None => println!("nothing to remove"),
        }
        return Ok(());
    }

    let path = match path_override {
        Some(p) => p.to_path_buf(),
        None => def
            .path
            .and_then(|f| f())
            .ok_or_else(|| anyhow!("could not determine config path for `{id}`"))?,
    };
    if !path.exists() {
        println!("nothing to remove ({} not found)", path.display());
        return Ok(());
    }

    if from_backup {
        let dir = clients::backup_dir(def.id);
        let newest = newest_backup(&dir)?;
        let Some(b) = newest else {
            anyhow::bail!("no backup found under {}", dir.display());
        };
        std::fs::copy(&b, &path)
            .with_context(|| format!("restore {} -> {}", b.display(), path.display()))?;
        println!("✓ restored {} from {}", path.display(), b.display());
        return Ok(());
    }

    let changed = match def.id {
        "continue" => clients::continue_vscode::uninstall(&path)?,
        "codex" => clients::codex_cli::uninstall(&path)?,
        "zcode" | "autoclaw" => clients::generic_json::uninstall(&path)?,
        other => anyhow::bail!("client `{other}` has no uninstaller"),
    };

    if changed {
        // Back up the post-uninstall state too, so a subsequent re-install
        // is undoable.
        let after = std::fs::read_to_string(&path)?;
        let _ = clients::write_backup(def.id, &path, &after);
        println!("✓ unpatched {}", path.display());
    } else {
        println!("nothing to remove from {}", path.display());
    }
    Ok(())
}

fn newest_backup(dir: &std::path::Path) -> Result<Option<PathBuf>> {
    let mut newest: Option<(u64, PathBuf)> = None;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stamp: Option<u64> = name.split('.').next().and_then(|s| s.parse().ok());
        if let Some(t) = stamp {
            let p = entry.path();
            if newest.as_ref().map(|(cur, _)| t > *cur).unwrap_or(true) {
                newest = Some((t, p));
            }
        }
    }
    Ok(newest.map(|(_, p)| p))
}

// Silences the unused-import warning when the `paths` helper isn't used
// in this build (e.g. a future refactor moves `env_file` away).
#[allow(dead_code)]
fn _touch_paths() {
    let _ = paths::uwa_home();
}
