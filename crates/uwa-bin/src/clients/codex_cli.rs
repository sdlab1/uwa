//! Codex CLI's `~/.codex/config.toml`.
//!
//! Codex uses `model_provider` + `[model_providers.X]`. We set the
//! provider and write a block delimited by `# __UWA_BEGIN__` /
//! `# __UWA_END__` so uninstall is a mechanical strip and the user's
//! own edits outside the markers survive.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::{paths, Payload};

pub const BEGIN: &str = "# __UWA_BEGIN__";
pub const END: &str = "# __UWA_END__";

pub fn config_path() -> Option<PathBuf> {
    paths::home().map(|h| h.join(".codex").join("config.toml"))
}

pub fn install(p: &std::path::Path, payload: &Payload) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let original = super::read_or_empty(p)?;
    let stripped = strip_block(&original);

    // Top-level `model` and `model_provider` are set if and only if not
    // already present — we don't want to overwrite a user's own choice.
    let mut head = String::new();
    let has_model = stripped
        .lines()
        .any(|l| l.trim_start().starts_with("model ="));
    let has_provider = stripped
        .lines()
        .any(|l| l.trim_start().starts_with("model_provider ="));
    if !has_model {
        head.push_str(&format!("model = \"{}\"\n", payload.model));
    }
    if !has_provider {
        head.push_str("model_provider = \"uwa\"\n");
    }

    let block = format!(
        "{BEGIN}\n\
         [model_providers.uwa]\n\
         name = \"uwa\"\n\
         base_url = \"{}\"\n\
         env_key = \"UWA_API_KEY\"\n\
         wire_api = \"chat\"\n\
         {END}\n",
        payload.base_url
    );

    let mut out = String::new();
    out.push_str(&head);
    out.push_str(stripped.trim_end());
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);

    std::fs::write(p, out).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

pub fn uninstall(p: &std::path::Path) -> Result<bool> {
    if !p.exists() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(p)?;
    let stripped = strip_block(&original);
    if stripped == original {
        return Ok(false);
    }
    std::fs::write(p, stripped)?;
    Ok(true)
}

/// Remove every `# __UWA_BEGIN__` ... `# __UWA_END__` block, inclusive
/// of the surrounding blank line.
pub fn strip_block(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_block = false;
    for line in s.lines() {
        let t = line.trim();
        if t == BEGIN {
            in_block = true;
            continue;
        }
        if t == END {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Check if the config file contains our uwa block.
pub fn is_patched(p: &std::path::Path) -> bool {
    if !p.exists() {
        return false;
    }
    std::fs::read_to_string(p)
        .map(|s| s.contains("__UWA_BEGIN__"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_only_our_block() {
        let s = "\
model = \"gpt-4o\"\n\
\n\
# __UWA_BEGIN__\n\
[model_providers.uwa]\n\
base_url = \"http://x/v1\"\n\
# __UWA_END__\n\
\n\
[model_providers.other]\n\
base_url = \"http://y/v1\"\n";
        let out = strip_block(s);
        assert!(out.contains("model = \"gpt-4o\""));
        assert!(out.contains("[model_providers.other]"));
        assert!(!out.contains("uwa"));
    }

    #[test]
    fn strip_on_empty_is_empty() {
        assert_eq!(strip_block(""), "");
    }
}
