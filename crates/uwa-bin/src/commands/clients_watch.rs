//! `uwa clients watch` — poll the patched config files and re-apply the
//! install if a client rewrote them.
//!
//! Only re-applies when our marker is *gone*, i.e. the app overwrote us.
//! If the marker is present but the user changed other fields, we leave
//! the file alone: the app merged around us, and re-applying would
//! clobber their edits.
//!
//! Runs as a foreground task; wrap in `uwa daemon` or systemd for a
//! persistent watcher.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::clients::{self, Payload};

const POLL: Duration = Duration::from_secs(3);

/// Check if a client's config file is patched by uwa.
fn is_patched(def: &clients::ClientDef, p: &std::path::Path) -> bool {
    match def.id {
        "continue" => clients::continue_vscode::is_patched(p),
        "codex" => clients::codex_cli::is_patched(p),
        "zcode" | "autoclaw" => clients::generic_json::is_patched(p),
        _ => false,
    }
}

pub async fn run(interval_secs: u64) -> Result<()> {
    let poll = if interval_secs == 0 {
        POLL
    } else {
        Duration::from_secs(interval_secs)
    };

    let payload = resolve_payload()?;
    // Snapshot the set of patched clients at start. New installs during
    // the watch are picked up on the next `install` command, not here.
    let targets: Vec<(&'static clients::ClientDef, PathBuf)> = clients::REGISTRY
        .iter()
        .filter(|d| !matches!(d.format, clients::Format::Shell))
        .filter_map(|d| d.path.and_then(|f| f()).map(|p| (d, p)))
        .filter(|(d, p)| is_patched(d, p))
        .collect();

    if targets.is_empty() {
        println!("nothing patched — run `uwa clients install <id>` first");
        return Ok(());
    }

    println!("watching {} client(s) every {:?}", targets.len(), poll);
    for (d, p) in &targets {
        println!("  {} → {}", d.id, p.display());
    }

    let mut last: HashMap<PathBuf, SystemTime> = HashMap::new();
    for (_, p) in &targets {
        if let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) {
            last.insert(p.clone(), m);
        }
    }

    loop {
        tokio::time::sleep(poll).await;
        for (def, path) in &targets {
            let Ok(md) = std::fs::metadata(path) else {
                continue;
            };
            let Ok(m) = md.modified() else { continue };
            let changed = last.get(path).map(|p| *p != m).unwrap_or(true);
            if !changed {
                continue;
            }
            last.insert(path.clone(), m);

            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let still_patched = is_patched(def, path);
            if still_patched {
                // The app merged around us — leave it alone.
                continue;
            }

            // We were overwritten. Save their version first, then
            // re-apply. Saving the overwrite means the user can
            // `uninstall --from-backup` and get *their* file back, not
            // ours.
            let _ = clients::write_backup(def.id, path, &text);
            match reapply(def, path, &payload) {
                Ok(()) => println!(
                    "[{}] re-applied uwa block in {}",
                    chrono_lite_now(),
                    path.display()
                ),
                Err(e) => println!(
                    "[{}] re-apply failed for {}: {e}",
                    chrono_lite_now(),
                    path.display()
                ),
            }
        }
    }
}

fn reapply(def: &clients::ClientDef, path: &std::path::Path, payload: &Payload) -> Result<()> {
    match def.id {
        "continue" => clients::continue_vscode::install(path, payload),
        "codex" => clients::codex_cli::install(path, payload),
        "zcode" | "autoclaw" => clients::generic_json::install(path, payload),
        other => anyhow::bail!("no re-apply for `{other}`"),
    }
}

fn resolve_payload() -> Result<Payload> {
    let cfg = uwa_config::Config::load_from_path("uwa.toml").ok();
    let port = cfg.as_ref().map(|c| c.server.port).unwrap_or(8080);
    let origin = format!("http://127.0.0.1:{port}");
    let base_url = format!("{origin}/v1");
    let api_key = cfg
        .as_ref()
        .and_then(|c| c.server.api_key.clone())
        .unwrap_or_else(|| "dummy-key".into());
    let model = {
        let mut a: Vec<String> = cfg
            .as_ref()
            .map(|c| c.model_aliases.keys().cloned().collect())
            .unwrap_or_default();
        a.sort();
        a.into_iter().next().unwrap_or_else(|| "gpt-4o".into())
    };
    Ok(Payload {
        base_url,
        origin,
        api_key,
        model,
    })
}

fn chrono_lite_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rem = secs % 86_400;
    format!("{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}
