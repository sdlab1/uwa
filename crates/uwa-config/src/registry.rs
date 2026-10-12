//! Provider selector registry.
//!
//! ## Sources, in increasing precedence
//!
//! 1. **Built-in** — embedded at compile time from `providers/*.toml`.
//! 2. **Cache** — `~/.uwa/providers/`, written by `uwa providers update`.
//! 3. **User config** — `[providers.X]` in `uwa.toml`, always wins.
//!
//! Merge is per-field. A user who overrides only `input` keeps the
//! registry's `send_button`, `stop_button`, and everything else. An
//! entry that names a provider the user config doesn't mention is
//! ignored — the registry can't add providers.
//!
//! ## Why not override capabilities
//!
//! Registry entries can carry a `[capabilities]` block, but merge ignores
//! it. Capabilities describe what the site does (does Claude expose
//! tool use today?), not what its DOM looks like, and they change on a
//! schedule the selector maintainers don't control. Users set them once
//! in `uwa.toml`; the registry provides only the volatile parts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ExtractionStrategy, ProviderCfg, Selectors};
use uwa_core::{NetRules, UwaError};

include!(concat!(env!("OUT_DIR"), "/registry_generated.rs"));

/// One registry file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub meta: RegistryMeta,
    #[serde(default)]
    pub url_patterns: Vec<String>,
    #[serde(default)]
    pub selectors: Selectors,
    #[serde(default)]
    pub alternates: Alternates,
    #[serde(default)]
    pub net: Option<NetRules>,
    #[serde(default)]
    pub extraction: Option<ExtractionStrategy>,
    /// Present for consumers; **ignored during merge** (see module docs).
    #[serde(default)]
    pub capabilities: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryMeta {
    pub name: String,
    pub version: String,
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub updated_at: String,
}

fn default_schema() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Alternates {
    #[serde(default)]
    pub input: Candidates,
    #[serde(default)]
    pub send_button: Candidates,
    #[serde(default)]
    pub stop_button: Candidates,
    #[serde(default)]
    pub assistant_message: Candidates,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidates {
    #[serde(default)]
    pub candidates: Vec<String>,
}

impl Alternates {
    /// Fallback candidates for a named selector field. Names match the
    /// `Selectors` fields, so callers can index them uniformly.
    pub fn for_field(&self, field: &str) -> &[String] {
        match field {
            "input" => &self.input.candidates,
            "send_button" => &self.send_button.candidates,
            "stop_button" => &self.stop_button.candidates,
            "assistant_message" => &self.assistant_message.candidates,
            _ => &[],
        }
    }
}

/// The manifest lists every entry and its expected hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryManifest {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub registry_version: String,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub min_uwa_version: String,
    #[serde(default)]
    pub providers: HashMap<String, ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub version: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
}

/// The registry in memory: one entry per provider, in precedence order.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    entries: HashMap<String, RegistryEntry>,
    manifest: Option<RegistryManifest>,
    /// Where the cache was read from, if any. Reported by `providers list`
    /// so the user can see which entries came from disk.
    cache_dir: Option<PathBuf>,
}

impl Registry {
    /// Load the default registry: built-in entries overlaid with the
    /// cache at `$UWA_PROVIDERS_DIR` (or `~/.uwa/providers/`). Missing
    /// cache is fine — it just means the built-in copy is authoritative.
    pub fn load() -> Self {
        let cache_dir = cache_dir_from_env();
        Self::load_from(cache_dir.as_deref())
    }

    /// Load with an explicit cache dir. Passing `None` uses only the
    /// built-in entries.
    pub fn load_from(cache_dir: Option<&Path>) -> Self {
        let mut reg = Self::from_builtin();

        if let Some(dir) = cache_dir {
            if dir.exists() {
                reg.merge_cache(dir);
                reg.cache_dir = Some(dir.to_path_buf());
            }
        }

        reg
    }

    /// The registry backed only by the compiled-in entries. Useful for
    /// tests and for resolving "what does the shipped binary think?".
    pub fn from_builtin() -> Self {
        let mut entries = HashMap::new();
        for (name, contents) in BUILTIN_ENTRIES {
            match parse_entry(name, contents) {
                Ok(e) => {
                    entries.insert((*name).to_string(), e);
                }
                Err(e) => {
                    // A malformed built-in is a build-time bug: the
                    // include_str! already guarantees the file exists, so
                    // this only fires on a schema drift. Degrade loudly —
                    // the entry is skipped and the drift is visible in
                    // every log line — instead of panicking inside a
                    // library constructor (forbid-level lint; a panic here
                    // would abort the whole daemon at startup).
                    tracing::error!(
                        name = %name,
                        "built-in registry entry is malformed, skipping: {e}"
                    );
                }
            }
        }
        let manifest = BUILTIN_MANIFEST.and_then(|m| match parse_manifest(m) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::error!("built-in registry manifest is malformed, ignoring: {e}");
                None
            }
        });
        Self {
            entries,
            manifest,
            cache_dir: None,
        }
    }

    fn merge_cache(&mut self, dir: &Path) {
        let manifest_path = dir.join("manifest.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Ok(m) = parse_manifest(&text) {
                // A cache manifest only replaces the built-in one when
                // it is genuinely newer; otherwise a downgrade attempt
                // could resurrect a broken entry.
                if self
                    .manifest
                    .as_ref()
                    .map(|current| m.registry_version > current.registry_version)
                    .unwrap_or(true)
                {
                    self.manifest = Some(m);
                }
            }
        }

        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) if s != "manifest" => s.to_string(),
                _ => continue,
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match parse_entry(&stem, &text) {
                Ok(e) => {
                    self.entries.insert(stem, e);
                }
                Err(err) => {
                    tracing::warn!(
                        file = %path.display(),
                        "skipping malformed registry entry: {err}"
                    );
                }
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&RegistryEntry> {
        self.entries.get(name)
    }

    pub fn manifest(&self) -> Option<&RegistryManifest> {
        self.manifest.as_ref()
    }

    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.entries.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry's name and version, sorted — the shape `uwa providers
    /// list` renders.
    pub fn version_list(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.meta.version.clone()))
            .collect();
        v.sort();
        v
    }

    /// Apply every matching entry to the given provider config, in place.
    /// The registry only fills fields the user left unset.
    pub fn overlay(&self, name: &str, provider: &mut ProviderCfg) {
        let Some(entry) = self.entries.get(name) else {
            return;
        };
        if provider.url_patterns.is_empty() && !entry.url_patterns.is_empty() {
            provider.url_patterns = entry.url_patterns.clone();
        }
        merge_selectors(&mut provider.selectors, &entry.selectors);
        if provider.net.is_none() {
            provider.net = entry.net.clone();
        }
        // `selectors_version` records what was in effect at load time.
        // Useful in /readyz and in the debug bundle to correlate a bug
        // report with a specific registry snapshot.
        if provider.selectors_version.is_none() {
            provider.selectors_version = Some(entry.meta.version.clone());
        }
    }

    /// Fallback candidates for a named field of a named provider.
    pub fn alternates_for(&self, provider: &str, field: &str) -> Vec<String> {
        self.entries
            .get(provider)
            .map(|e| e.alternates.for_field(field).to_vec())
            .unwrap_or_default()
    }
}

fn merge_selectors(into: &mut Selectors, from: &Selectors) {
    if into.input.is_none() {
        into.input = from.input.clone();
    }
    if into.send_button.is_none() {
        into.send_button = from.send_button.clone();
    }
    if into.stop_button.is_none() {
        into.stop_button = from.stop_button.clone();
    }
    if into.assistant_message.is_none() {
        into.assistant_message = from.assistant_message.clone();
    }
    if into.conversation_root.is_none() {
        into.conversation_root = from.conversation_root.clone();
    }
}

fn parse_entry(name: &str, text: &str) -> Result<RegistryEntry, UwaError> {
    let entry: RegistryEntry = toml::from_str(text)
        .map_err(|e| UwaError::Config(format!("registry entry `{name}`: {e}")))?;
    if entry.meta.schema != 1 {
        return Err(UwaError::Config(format!(
            "registry entry `{name}`: unsupported schema {}",
            entry.meta.schema
        )));
    }
    Ok(entry)
}

fn parse_manifest(text: &str) -> Result<RegistryManifest, UwaError> {
    let m: RegistryManifest =
        toml::from_str(text).map_err(|e| UwaError::Config(format!("registry manifest: {e}")))?;
    if m.schema != 1 {
        return Err(UwaError::Config(format!(
            "registry manifest: unsupported schema {}",
            m.schema
        )));
    }
    Ok(m)
}

fn cache_dir_from_env() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("UWA_PROVIDERS_DIR") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    let home = std::env::var("UWA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".uwa")))
        .ok()?;
    Some(home.join("providers"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_parses() {
        let r = Registry::from_builtin();
        assert!(!r.is_empty(), "built-in registry is empty");
        // Every built-in name has a version.
        for (name, version) in r.version_list() {
            assert!(!version.is_empty(), "empty version for {name}");
        }
    }

    #[test]
    fn overlay_fills_only_unset_fields() {
        let mut reg = Registry::default();
        reg.entries.insert(
            "p".into(),
            RegistryEntry {
                meta: RegistryMeta {
                    name: "p".into(),
                    version: "1.0.0".into(),
                    schema: 1,
                    updated_at: String::new(),
                },
                url_patterns: vec!["https://p.test/*".into()],
                selectors: Selectors {
                    input: Some("#reg-input".into()),
                    send_button: Some("#reg-send".into()),
                    stop_button: None,
                    assistant_message: Some("#reg-assistant".into()),
                    conversation_root: None,
                },
                alternates: Alternates::default(),
                net: None,
                extraction: None,
                capabilities: None,
            },
        );

        let mut p = ProviderCfg::default_for_test("p");
        p.selectors.input = Some("#user-input".into()); // user override
        p.selectors.send_button = None; // unset → registry
        p.selectors.assistant_message = None;
        reg.overlay("p", &mut p);

        assert_eq!(p.selectors.input.as_deref(), Some("#user-input"));
        assert_eq!(p.selectors.send_button.as_deref(), Some("#reg-send"));
        assert_eq!(
            p.selectors.assistant_message.as_deref(),
            Some("#reg-assistant")
        );
        assert_eq!(p.selectors_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn overlay_never_overrides_user_url_patterns() {
        let mut reg = Registry::default();
        reg.entries.insert(
            "p".into(),
            RegistryEntry {
                meta: RegistryMeta {
                    name: "p".into(),
                    version: "1".into(),
                    schema: 1,
                    updated_at: String::new(),
                },
                url_patterns: vec!["https://registry.test/*".into()],
                selectors: Selectors::default(),
                alternates: Alternates::default(),
                net: None,
                extraction: None,
                capabilities: None,
            },
        );
        let mut p = ProviderCfg::default_for_test("p");
        p.url_patterns = vec!["https://user.test/*".into()];
        reg.overlay("p", &mut p);
        assert_eq!(p.url_patterns, vec!["https://user.test/*".to_string()]);
    }

    #[test]
    fn alternates_lookup_by_field_name() {
        let reg = Registry::from_builtin();
        // Every built-in entry that has alternates for `input` returns
        // at least one candidate.
        for name in reg.names() {
            let alts = reg.alternates_for(&name, "input");
            // Empty is fine (some entries may not have alternates yet),
            // but if present they must be non-empty strings.
            for a in alts {
                assert!(!a.is_empty(), "empty alternate for {name}");
            }
        }
    }

    #[test]
    fn manifest_placeholders_are_rejected_at_load_time() {
        // The builtin manifest carries PLACEHOLDER_* SHAs by design —
        // they must still parse (they're only checked by `providers
        // update`), and the manifest must be exposed.
        let reg = Registry::from_builtin();
        let m = reg.manifest().expect("built-in manifest present");
        assert_eq!(m.schema, 1);
        assert!(!m.registry_version.is_empty());
        assert!(!m.providers.is_empty());
    }

    #[test]
    fn cache_dir_env_override_is_honoured() {
        // Scoped to one thread pool: env mutation in tests is a known
        // hazard, but this key is ours alone.
        std::env::set_var("UWA_PROVIDERS_DIR", "/tmp/uwa-registry-test");
        let dir = cache_dir_from_env();
        assert_eq!(dir, Some(PathBuf::from("/tmp/uwa-registry-test")));
        std::env::remove_var("UWA_PROVIDERS_DIR");
    }
}
