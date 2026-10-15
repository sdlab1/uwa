//! Continue's `~/.continue/config.json`.
//!
//! Continue stores an array of models. We append one entry with
//! `"title": "uwa"` and leave everything else exactly as it was. On
//! uninstall, we remove every entry with our marker title.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

use super::{paths, Payload};

const MARKER: &str = "uwa";

pub fn config_path() -> Option<PathBuf> {
    paths::home().map(|h| h.join(".continue").join("config.json"))
}

pub fn install(p: &std::path::Path, payload: &Payload) -> Result<()> {
    let text = super::read_or_empty(p)?;
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?
    };

    // Normalise to object form.
    if !v.is_object() {
        anyhow::bail!("{}: expected a JSON object at the top level", p.display());
    }

    let models = v
        .as_object_mut()
        .unwrap()
        .entry("models")
        .or_insert_with(|| Value::Array(vec![]));
    if !models.is_array() {
        anyhow::bail!("{}: `models` is not an array", p.display());
    }

    let arr = models.as_array_mut().unwrap();
    arr.retain(|m| m.get("title").and_then(Value::as_str) != Some(MARKER));
    arr.push(json!({
        "title": MARKER,
        "provider": "openai",
        "model": payload.model,
        "apiBase": payload.base_url,
        "apiKey": payload.api_key,
    }));

    let rendered = serde_json::to_string_pretty(&v)?;
    std::fs::write(p, rendered).with_context(|| format!("write {}", p.display()))?;
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
    let Some(models) = obj.get_mut("models").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let before = models.len();
    models.retain(|m| m.get("title").and_then(Value::as_str) != Some(MARKER));
    let changed = models.len() != before;
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
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .any(|m| m.get("title").and_then(|t| t.as_str()) == Some("uwa"))
        })
        .unwrap_or(false)
}
