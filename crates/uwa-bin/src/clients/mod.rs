//! Patch the config files of the editors that point at uwa.
//!
//! Only file-based clients are supported. Cursor, VS Code, and JetBrains
//! store their settings in a database or in files that get rewritten by
//! the app on every save — patching them fights a losing battle. For
//! those, `uwa clients env` prints a shell snippet the user can source.
//!
//! Every patch is idempotent and backed up first. Re-running `install`
//! replaces our marker block; running `uninstall` restores the file to
//! its pre-patch state (or to the last user-authored version if they
//! edited it while we were patched — see `restore_plan`).
//!
//! ## Detection
//!
//! A file is "ours" if it contains the `__UWA_BEGIN__` marker. A file
//! that has the marker but a different payload is treated as "ours,
//! stale" — `install` replaces it in place. A file with no marker is
//! never touched without an explicit `install`.

pub mod codex_cli;
pub mod continue_vscode;
pub mod env_file;
pub mod generic_json;
pub mod paths;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which kind of file the client keeps its config in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JSON object; we merge keys.
    Json,
    /// TOML; we append a marker-delimited block.
    Toml,
    /// Shell snippet; we own the whole file.
    Shell,
}

/// A client we know how to patch.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClientDef {
    /// Stable id used on the command line.
    pub id: &'static str,
    /// Human name for the UI and `list`.
    pub name: &'static str,
    pub format: Format,
    /// Absolute path resolution. `None` means the client has no file
    /// and only supports the env-export path.
    pub path: Option<fn() -> Option<PathBuf>>,
    /// How the client is best set up if we don't have a file to patch.
    /// Shown by `uwa clients status` and in the dashboard.
    pub env_hint: &'static str,
    /// Homepage / docs URL.
    pub docs: &'static str,
}

pub const REGISTRY: &[ClientDef] = &[
    ClientDef {
        id: "continue",
        name: "Continue (VS Code / JetBrains)",
        format: Format::Json,
        path: Some(continue_vscode::config_path),
        env_hint: "",
        docs: "https://docs.continue.dev",
    },
    ClientDef {
        id: "codex",
        name: "Codex CLI",
        format: Format::Toml,
        path: Some(codex_cli::config_path),
        env_hint: "",
        docs: "https://github.com/openai/codex",
    },
    ClientDef {
        id: "claude-code",
        name: "Claude Code",
        format: Format::Shell,
        path: None,
        env_hint: "export ANTHROPIC_BASE_URL=... ANTHROPIC_API_KEY=...",
        docs: "https://docs.anthropic.com/en/docs/claude-code",
    },
    ClientDef {
        id: "aider",
        name: "Aider",
        format: Format::Shell,
        path: None,
        env_hint: "export OPENAI_API_BASE=... OPENAI_API_KEY=...",
        docs: "https://aider.chat/docs/llms/openai-compat.html",
    },
    ClientDef {
        id: "zcode",
        name: "ZCode",
        format: Format::Json,
        path: Some(generic_json::zcode_config_path),
        env_hint: "",
        docs: "",
    },
    ClientDef {
        id: "autoclaw",
        name: "AutoClaw",
        format: Format::Json,
        path: Some(generic_json::autoclaw_config_path),
        env_hint: "",
        docs: "",
    },
];

pub fn find(id: &str) -> Option<&'static ClientDef> {
    REGISTRY.iter().find(|c| c.id == id)
}

/// State of a client on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientStatus {
    pub id: String,
    pub name: String,
    pub format: String,
    /// `None` when the client has no file and only uses env vars.
    pub path: Option<String>,
    /// `patched` / `not_patched` / `missing_file` / `env_only` /
    /// `unknown`.
    pub state: String,
    /// Freeform note for the UI.
    pub note: String,
    /// Where the last backup landed, if any.
    pub last_backup: Option<String>,
}

/// Where the payload lives.
pub struct Payload {
    /// Base URL clients should use, e.g. `http://127.0.0.1:8080/v1`.
    pub base_url: String,
    /// Bare origin, for the Anthropic SDK's `base_url`.
    pub origin: String,
    /// The API key clients should send.
    pub api_key: String,
    /// First model id, for clients that require one.
    pub model: String,
}

/// Common result of an install/uninstall.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct ActionReport {
    pub id: String,
    pub path: Option<String>,
    pub changed: bool,
    pub backup: Option<String>,
    pub note: String,
}

/// What an uninstall should restore, if anything.
#[allow(dead_code)]
pub enum RestorePlan {
    /// Strip our block, keep user edits.
    Strip,
    /// Restore the file from a backup (the file only ever contained our
    /// content — e.g. the env-export file).
    Replace { from: PathBuf },
    /// Nothing to do.
    Nothing,
}

pub fn status_all() -> Vec<ClientStatus> {
    REGISTRY
        .iter()
        .map(|def| {
            let path = def.path.and_then(|f| f());
            match (def.id, path.as_ref()) {
                ("claude-code" | "aider", _) => {
                    // Shell/env clients
                    ClientStatus {
                        id: def.id.into(),
                        name: def.name.into(),
                        format: "shell".into(),
                        path: Some(env_file::path().display().to_string()),
                        state: if env_file::patched() {
                            "patched".into()
                        } else {
                            "not_patched".into()
                        },
                        note: def.env_hint.into(),
                        last_backup: None,
                    }
                }
                (_, None) => ClientStatus {
                    id: def.id.into(),
                    name: def.name.into(),
                    format: format_str(def.format).into(),
                    path: None,
                    state: "missing_file".into(),
                    note: "config file not found; nothing to patch".into(),
                    last_backup: None,
                },
                (_, Some(p)) => {
                    let exists = p.exists();
                    let patched = if exists {
                        match def.id {
                            "continue" => continue_vscode::is_patched(p),
                            "codex" => codex_cli::is_patched(p),
                            "zcode" | "autoclaw" => generic_json::is_patched(p),
                            _ => false,
                        }
                    } else {
                        false
                    };
                    ClientStatus {
                        id: def.id.into(),
                        name: def.name.into(),
                        format: format_str(def.format).into(),
                        path: Some(p.display().to_string()),
                        state: if !exists {
                            "missing_file".into()
                        } else if patched {
                            "patched".into()
                        } else {
                            "not_patched".into()
                        },
                        note: String::new(),
                        last_backup: last_backup_for(def.id),
                    }
                }
            }
        })
        .collect()
}

fn format_str(f: Format) -> &'static str {
    match f {
        Format::Json => "json",
        Format::Toml => "toml",
        Format::Shell => "shell",
    }
}

/// Backups live in `~/.uwa/client-backups/<id>/`. A file-per-run scheme
/// keeps history small; `last_backup_for` finds the newest.
pub fn backup_dir(id: &str) -> PathBuf {
    paths::uwa_home().join("client-backups").join(id)
}

pub fn write_backup(id: &str, src: &std::path::Path, contents: &str) -> Result<PathBuf> {
    let dir = backup_dir(id);
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("config");
    let path = dir.join(format!("{stamp}.{name}"));
    std::fs::write(&path, contents)?;
    Ok(path)
}

pub fn last_backup_for(id: &str) -> Option<String> {
    let dir = backup_dir(id);
    let mut latest: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stamp: Option<u64> = name.split('.').next().and_then(|s| s.parse().ok());
        if let Some(t) = stamp {
            let p = entry.path();
            if latest.as_ref().map(|(cur, _)| t > *cur).unwrap_or(true) {
                latest = Some((t, p));
            }
        }
    }
    latest.map(|(_, p)| p.display().to_string())
}

/// Convenience: read a file or return an empty string if it doesn't
/// exist. Used by patchers that want to merge into an existing file.
pub fn read_or_empty(p: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(p) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}
