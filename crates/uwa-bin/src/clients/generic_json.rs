//! Two clients whose config file shapes we can't pin down exactly.
//! We do the least surprising thing: if the file already exists, merge a
//! `uwa` entry into a top-level `providers` array (a shape shared by
//! both apps in their published screenshots); if it doesn't exist,
//! `install` fails with a clear message and the user is told to create
//! an empty object first. We never invent a file for a client we don't
//! fully understand.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

use super::{paths, Payload};

const MARKER: &str = "uwa";

pub fn zcode_config_path() -> Option<PathBuf> {
    paths::xdg_config().map(|c| c.join("zcode").join("settings.json"))
}

pub fn autoclaw_config_path() -> Option<PathBuf> {
    paths::home().map(|h| h.join(".autoclaw").join("config.json"))
}

pub fn install(p: &std::path::Path, payload: &Payload) -> Result<()> {
    if !p.exists() {
        anyhow::bail!(
            "{} does not exist; open the app once so it creates its \
             config, then run `uwa clients install` again",
            p.display()
        );
    }
    let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
    let mut v: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: expected an object", p.display()))?;

    let providers = obj
        .entry("providers")
        .or_insert_with(|| Value::Array(vec![]));
    if !providers.is_array() {
        anyhow::bail!("{}: `providers` exists but is not an array", p.display());
    }
    let arr = providers.as_array_mut().unwrap_or_else(|| unreachable!());
    arr.retain(|p| p.get("name").and_then(Value::as_str) != Some(MARKER));
    arr.push(json!({
        "name": MARKER,
        "baseUrl": payload.base_url,
        "apiKey": payload.api_key,
        "models": [{ "id": payload.model, "name": format!("uwa · {}", payload.model) }],
    }));

    std::fs::write(p, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

pub fn uninstall(p: &std::path::Path) -> Result<bool> {
    if !p.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(p)?;
    let mut v: Value = serde_json::from_str(&text)?;
    let Some(obj) = v.as_object_mut() else {
        return Ok(false);
    };
    let Some(arr) = obj.get_mut("providers").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let before = arr.len();
    arr.retain(|p| p.get("name").and_then(Value::as_str) != Some(MARKER));
    let changed = arr.len() != before;
    if changed {
        std::fs::write(p, serde_json::to_string_pretty(&v)?)?;
    }
    Ok(changed)
}

/// Check if the config file contains our uwa entry.
pub fn is_patched(p: &std::path::Path) -> bool {
    if !p.exists() {
        return false;
    }
    let text = std::fs::read_to_string(p).unwrap_or_default();
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v.get("providers")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .any(|m| m.get("name").and_then(|t| t.as_str()) == Some("uwa"))
        })
        .unwrap_or(false)
}
