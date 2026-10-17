//! `uwa profiles list | export | import | health`.
//!
//! All commands hit the running daemon's HTTP API. That's deliberate:
//! the alternative - spin up an isolated CDP connection in the CLI -
//! would need a second copy of the transport plumbing, and if the
//! daemon is already running, its tabs are the ones we want to inspect.

use anyhow::{bail, Context, Result};
use clap::{Arg, ArgAction, Command};
use std::path::PathBuf;
use std::time::Duration;

use uwa_core::{CookieExport, CookieRecord};

#[derive(Debug, Clone)]
pub enum ProfilesAction {
    /// List profiles and their per-provider login status.
    List { base: Option<String> },
    /// Write every cookie in a profile to a JSON file (`-` = stdout).
    Export {
        name: String,
        out: Option<String>,
        base: Option<String>,
    },
    /// Read a cookie export and install it in a profile.
    Import {
        name: String,
        from: String,
        base: Option<String>,
    },
    /// Probe login state per provider. No name = every named profile.
    Health {
        name: Option<String>,
        base: Option<String>,
    },
}

impl ProfilesAction {
    pub fn clap_subcommands() -> Vec<Command> {
        let base = || {
            Arg::new("base")
                .long("base")
                .env("UWA_BASE_URL")
                .action(ArgAction::Set)
                .help("Daemon base URL (default: derived from the config's [server].port)")
        };
        vec![
            Command::new("list")
                .about("List profiles and their per-provider login status.")
                .arg(base()),
            Command::new("export")
                .about("Write every cookie in a profile to a JSON file.")
                .arg(
                    Arg::new("name")
                        .required(true)
                        .help("Profile name (`default` allowed)"),
                )
                .arg(
                    Arg::new("out")
                        .short('o')
                        .long("out")
                        .action(ArgAction::Set)
                        .help("Output path; `-` or omitted means stdout"),
                )
                .arg(base()),
            Command::new("import")
                .about("Read a cookie export (file) and install it in a profile.")
                .arg(Arg::new("name").required(true).help("Profile name"))
                .arg(
                    Arg::new("from")
                        .short('f')
                        .long("from")
                        .required(true)
                        .action(ArgAction::Set)
                        .help(
                            "JSON file written by `uwa profiles export` (or a bare cookie array)",
                        ),
                )
                .arg(base()),
            Command::new("health")
                .about("Probe login state per provider in a profile (no name = all).")
                .arg(
                    Arg::new("name")
                        .action(ArgAction::Set)
                        .help("Profile name; omit for all named profiles"),
                )
                .arg(base()),
        ]
    }

    pub fn from_matches(name: &str, matches: &clap::ArgMatches) -> Result<Self, String> {
        let base = matches.get_one::<String>("base").cloned();
        let str_arg = |k: &str| matches.get_one::<String>(k).cloned();
        match name {
            "list" => Ok(ProfilesAction::List { base }),
            "export" => Ok(ProfilesAction::Export {
                name: str_arg("name").unwrap_or_default(),
                out: str_arg("out"),
                base,
            }),
            "import" => Ok(ProfilesAction::Import {
                name: str_arg("name").unwrap_or_default(),
                from: str_arg("from").unwrap_or_default(),
                base,
            }),
            "health" => Ok(ProfilesAction::Health {
                name: str_arg("name"),
                base,
            }),
            _ => Err(format!("unknown profiles subcommand: {name}")),
        }
    }
}

pub async fn run(config_path: PathBuf, action: ProfilesAction) -> Result<()> {
    match action {
        ProfilesAction::List { base } => list(&config_path, base).await,
        ProfilesAction::Export { name, out, base } => export(&config_path, base, &name, out).await,
        ProfilesAction::Import { name, from, base } => {
            import(&config_path, base, &name, &from).await
        }
        ProfilesAction::Health { name, base } => health(&config_path, base, name.as_deref()).await,
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Daemon base URL: explicit `--base` / `$UWA_BASE_URL` wins; otherwise
/// derive `http://127.0.0.1:<port>` from the config the user pointed
/// the CLI at (`-c uwa.toml`), falling back to the daemon default.
fn resolve_base(config_path: &PathBuf, explicit: Option<String>) -> String {
    if let Some(b) = explicit {
        return b;
    }
    let port = uwa_config::Config::load_from_path(config_path)
        .ok()
        .map(|c| c.server.port)
        .unwrap_or(8080);
    format!("http://127.0.0.1:{port}")
}

/// `[server].api_key` from the config, when one is set. Loopback binds
/// accept any credential, and a config without a key means the daemon
/// requires none, so the absence is not fatal either way.
fn auth(config_path: &PathBuf) -> Option<String> {
    uwa_config::Config::load_from_path(config_path)
        .ok()
        .and_then(|c| c.server.api_key)
}

fn get_json(config_path: &PathBuf, url: &str) -> Result<serde_json::Value> {
    let mut req = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .get(url);
    if let Some(k) = auth(config_path) {
        req = req.set("authorization", &format!("Bearer {k}"));
    }
    let resp = req.call().with_context(|| format!("GET {url}"))?;
    let text = resp.into_string().context("read response body")?;
    serde_json::from_str(&text).context("parse response")
}

fn post_json(
    config_path: &PathBuf,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let payload = serde_json::to_string(body).context("serialize request body")?;
    let mut req = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .post(url);
    if let Some(k) = auth(config_path) {
        req = req.set("authorization", &format!("Bearer {k}"));
    }
    let resp = req
        .set("content-type", "application/json")
        .send_string(&payload)
        .with_context(|| format!("POST {url}"))?;
    let text = resp.into_string().context("read response body")?;
    serde_json::from_str(&text).context("parse response")
}

// ---------------------------------------------------------------------------
// actions
// ---------------------------------------------------------------------------

async fn list(config_path: &PathBuf, base: Option<String>) -> Result<()> {
    let base = resolve_base(config_path, base);
    let v = get_json(config_path, &format!("{base}/admin/profiles"))?;
    if v.get("supports_profiles").and_then(|v| v.as_bool()) == Some(false) {
        println!("(this transport does not support named profiles)");
    }
    let profiles = v
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!(
        "{name:<16} {label:<20} {tabs:<10} health",
        name = "name",
        label = "label",
        tabs = "tabs"
    );
    for p in profiles {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let label = p.get("label").and_then(|v| v.as_str()).unwrap_or_default();
        let tabs = p
            .get("tabs")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        // Health is a separate call per profile; only do it when the
        // profile is named (the default one usually has no selectors).
        let health = if p.get("named").and_then(|v| v.as_bool()) == Some(true) {
            match get_json(config_path, &format!("{base}/admin/profiles/{name}/health")) {
                Ok(h) => summarize_health(&h),
                Err(_) => "error".into(),
            }
        } else {
            String::new()
        };
        println!("{name:<16} {label:<20} {tabs:<10} {health}");
    }
    Ok(())
}

fn summarize_health(h: &serde_json::Value) -> String {
    let providers = h
        .get("providers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ok: usize = 0;
    let mut out: usize = 0;
    let mut unk: usize = 0;
    for p in &providers {
        match p.get("status").and_then(|v| v.as_str()).unwrap_or_default() {
            "logged_in" => ok = ok.saturating_add(1),
            "logged_out" => out = out.saturating_add(1),
            _ => unk = unk.saturating_add(1),
        }
    }
    format!("{ok} in / {out} out / {unk} unknown")
}

async fn export(
    config_path: &PathBuf,
    base: Option<String>,
    name: &str,
    out: Option<String>,
) -> Result<()> {
    let base = resolve_base(config_path, base);
    let v = get_json(
        config_path,
        &format!("{base}/admin/profiles/{name}/cookies"),
    )?;
    let export: CookieExport =
        serde_json::from_value(v).with_context(|| format!("parsing cookie export for `{name}`"))?;
    let text = serde_json::to_string_pretty(&export)?;

    match out.as_deref() {
        None | Some("-") => println!("{text}"),
        Some(path) => {
            std::fs::write(path, text).with_context(|| format!("write {path}"))?;
            eprintln!(
                "wrote {} cookie(s) from profile `{name}` to {path}",
                export.cookies.len()
            );
        }
    }
    Ok(())
}

async fn import(config_path: &PathBuf, base: Option<String>, name: &str, from: &str) -> Result<()> {
    let base = resolve_base(config_path, base);
    let raw = std::fs::read_to_string(from).with_context(|| format!("read {from}"))?;
    // Accept either the full export shape or a bare array.
    let body: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {from}"))?;
    let records: Vec<CookieRecord> = if body.is_array() {
        serde_json::from_value(body).context("parse cookie array")?
    } else {
        let e: CookieExport = serde_json::from_value(body).context("parse cookie export")?;
        e.cookies
    };
    if records.is_empty() {
        bail!("no cookies in {from}");
    }

    let v = post_json(
        config_path,
        &format!("{base}/admin/profiles/{name}/cookies"),
        &serde_json::to_value(&records)?,
    )?;
    let imported = v.get("imported").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("imported {imported} cookie(s) into profile `{name}`");
    Ok(())
}

async fn health(config_path: &PathBuf, base: Option<String>, name: Option<&str>) -> Result<()> {
    let base = resolve_base(config_path, base);
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => {
            let v = get_json(config_path, &format!("{base}/admin/profiles"))?;
            v.get("profiles")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.get("named").and_then(|v| v.as_bool()) == Some(true))
                .map(|p| {
                    p.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .filter(|s| !s.is_empty())
                .collect()
        }
    };
    if names.is_empty() {
        println!("no named profiles configured");
        return Ok(());
    }

    for n in names {
        let h = get_json(config_path, &format!("{base}/admin/profiles/{n}/health"))
            .with_context(|| format!("health for `{n}`"))?;
        println!("profile `{n}`");
        for p in h
            .get("providers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let prov = p
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let status = p.get("status").and_then(|v| v.as_str()).unwrap_or_default();
            let note = p.get("note").and_then(|v| v.as_str()).unwrap_or_default();
            let marker = match status {
                "logged_in" => "ok",
                "logged_out" => "!!",
                "no_tab" => "--",
                _ => "??",
            };
            println!("  [{marker}] {prov:<16} {status:<12} {note}");
        }
        println!();
    }
    Ok(())
}
