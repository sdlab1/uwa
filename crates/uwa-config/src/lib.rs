//! # uwa-config
//!
//! Loads and validates the TOML/ENV configuration.
//!
//! ## Passport (public API)
//! - [`Config`] – top-level config
//! - [`Config::load_from_path`], [`Config::load_from_str`]
//! - [`Config::provider_for_model`], [`Config::provider_for_url`]
//! - [`ProviderCfg`] – per-provider settings (selectors, caps, TOML)
//! - [`url_matches`] – the `*`-glob used by `url_patterns`
//!
//! Environment overrides use the prefix `UWA__`, e.g. `UWA__SERVER__PORT=9090`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use url::Url;
use uwa_core::{Capabilities, UwaError};

pub mod features;
pub mod groups;
pub mod preset;

pub use features::{FilePasteCfg, MediaCfg, PromptPaddingCfg, ProxyCfg};
pub use groups::{GroupCfg, GroupMember, GroupStrategy};
pub use preset::{EffectiveProviderCfg, PresetCfg};

pub type Result<T> = std::result::Result<T, UwaError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerCfg,
    #[serde(default)]
    pub providers: HashMap<String, ProviderCfg>,
    /// model-name -> provider-name
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    /// Stealth configuration.
    #[serde(default)]
    pub stealth: StealthCfg,
    /// Backend selection (cdp vs nodriver) with per-provider overrides.
    #[serde(default)]
    pub backend: BackendCfg,
    /// Proxy configuration.
    #[serde(default)]
    pub proxy: ProxyCfg,
    /// Scheduled restart configuration.
    #[serde(default)]
    pub scheduled_restart: ScheduledRestartCfg,
    /// Routing groups configuration.
    #[serde(default)]
    pub groups: HashMap<String, GroupCfg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StealthCfg {
    #[serde(default = "default_stealth_pack")]
    pub pack: String,
    #[serde(default)]
    pub user_scripts_dir: Option<std::path::PathBuf>,
}

fn default_stealth_pack() -> String {
    "default".into()
}

impl Default for StealthCfg {
    fn default() -> Self {
        Self {
            pack: default_stealth_pack(),
            user_scripts_dir: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Backend configuration: chromiumoxide (CDP) vs nodriver (Python sidecar).
// ---------------------------------------------------------------------------

/// Global backend settings. Per-provider override in `[providers.*.backend]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendCfg {
    /// Global default backend kind. Providers without an explicit
    /// `backend = "..."` use this.
    #[serde(default)]
    pub kind: BackendKind,
    #[serde(default)]
    pub cdp: CdpBackendCfg,
    #[serde(default)]
    pub nodriver: NodriverBackendCfg,
}

/// Which browser backend drives the pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// chromiumoxide attach to a running Chrome. Oracle / debug / CI.
    Cdp,
    /// Python sidecar + nodriver. Stealth. Default for aggressive sites.
    #[default]
    Nodriver,
}

/// CDP (chromiumoxide) backend settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpBackendCfg {
    #[serde(default = "default_cdp_ws")]
    pub ws_url: String,
    #[serde(default = "default_idle_ttl_secs")]
    pub idle_ttl_secs: u64,
}

fn default_cdp_ws() -> String {
    // http:// resolves through /json/version inside CdpTransport::connect.
    // A bare ws://.../devtools/browser (no browser GUID) is NOT a valid
    // connect target — Chrome answers 404.
    "http://127.0.0.1:9222".into()
}

fn default_idle_ttl_secs() -> u64 {
    1800
}

impl Default for CdpBackendCfg {
    fn default() -> Self {
        Self {
            ws_url: default_cdp_ws(),
            idle_ttl_secs: default_idle_ttl_secs(),
        }
    }
}

/// Nodriver (Python sidecar) backend settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodriverBackendCfg {
    #[serde(default = "default_python")]
    pub python: String,
    #[serde(default = "default_sidecar_script")]
    pub script: String,
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub user_data_dir: Option<std::path::PathBuf>,
    #[serde(default)]
    pub browser_path: Option<std::path::PathBuf>,
    /// Extra Chrome launch flags.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// How long to wait for the sidecar to answer `initialize`.
    #[serde(default = "default_sidecar_timeout_ms")]
    pub init_timeout_ms: u64,
}

fn default_python() -> String {
    "python3".into()
}

fn default_sidecar_script() -> String {
    "sidecar/uwa_nodriver_sidecar.py".into()
}

fn default_sidecar_timeout_ms() -> u64 {
    30_000
}

impl Default for NodriverBackendCfg {
    fn default() -> Self {
        Self {
            python: default_python(),
            script: default_sidecar_script(),
            headless: false,
            user_data_dir: None,
            browser_path: None,
            extra_args: vec![],
            init_timeout_ms: default_sidecar_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCfg {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional API key. If absent, API is open (only allowed on loopback).
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Single-instance pid file. Defaults to `uwa.pid` when omitted.
    #[serde(default)]
    pub pid_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduledRestartCfg {
    #[serde(default)]
    pub enabled: bool,
    /// 24h `HH:MM` in UTC. e.g. `"04:00"`.
    #[serde(default)]
    pub at: Option<String>,
    /// Maximum in-flight requests to drain before forcing shutdown.
    #[serde(default = "default_drain_secs")]
    pub drain_secs: u64,
}

fn default_drain_secs() -> u64 {
    30
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderCfg {
    pub name: String,
    /// URL patterns (glob-ish: `https://chatgpt.com/*`).
    pub url_patterns: Vec<String>,
    pub capabilities: Capabilities,
    /// Selectors — everything DOM-related lives here, never in code.
    #[serde(default)]
    pub selectors: Selectors,
    /// Extraction strategy preference.
    #[serde(default)]
    pub extraction: ExtractionStrategy,
    /// Network extraction rules; `None` forces the DOM fallback path.
    #[serde(default)]
    pub net: Option<uwa_core::NetRules>,
    /// Finisher tuning for `GenericProvider::wait_response`.
    #[serde(default)]
    pub finisher: uwa_core::FinisherTuning,
    /// Free-form version tag for the selectors. Shown in /readyz.
    #[serde(default)]
    pub selectors_version: Option<String>,
    /// Per-provider backend override. If absent, global `[backend].kind`
    /// is used.
    #[serde(default)]
    pub backend: Option<BackendKind>,
    // --- new ---
    #[serde(default)]
    pub presets: std::collections::HashMap<String, PresetCfg>,
    #[serde(default)]
    pub default_preset: Option<String>,
    #[serde(default)]
    pub file_paste: FilePasteCfg,
    #[serde(default)]
    pub prompt_padding: PromptPaddingCfg,
    #[serde(default)]
    pub stealth: bool,
    /// Declarative action list. When non-empty, `GenericProvider` runs it
    /// instead of the default fill+click+wait path.
    #[serde(default)]
    pub workflow: uwa_core::workflow::Workflow,
    /// Audio capture + video detection.
    #[serde(default)]
    pub media: MediaCfg,
}

impl ProviderCfg {
    /// Test-only constructor: a plausible provider config for `name`, so
    /// tests do not repeat eleven fields. Available in-crate (`test`) and
    /// to dependents through the `test-helpers` feature.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn default_for_test(name: &str) -> Self {
        Self {
            name: name.into(),
            url_patterns: vec![format!("https://{name}.com/*")],
            capabilities: Capabilities {
                streams: true,
                tool_calls: true,
                vision: false,
                max_context_tokens: None,
            },
            selectors: Selectors::default(),
            extraction: ExtractionStrategy::DomOnly,
            net: None,
            finisher: uwa_core::FinisherTuning::default(),
            selectors_version: None,
            backend: None,
            presets: std::collections::HashMap::new(),
            default_preset: None,
            file_paste: FilePasteCfg::default(),
            prompt_padding: PromptPaddingCfg::default(),
            stealth: false,
            workflow: Default::default(),
            media: MediaCfg::default(),
        }
    }

    /// Resolve the effective configuration for a preset.
    ///
    /// * If `preset` is `None` — uses `default_preset` (or top-level fields
    ///   if no presets are defined).
    /// * If `preset` is `Some("x")` and "x" exists — overrides.
    /// * If `preset` is `Some("x")` and "x" does not exist — `Err`.
    pub fn effective(&self, preset: Option<&str>) -> Result<EffectiveProviderCfg> {
        preset::effective_provider(self, preset)
    }

    pub fn preset_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.presets.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn has_preset(&self, name: &str) -> bool {
        self.presets.contains_key(name)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selectors {
    pub input: Option<String>,
    pub send_button: Option<String>,
    pub stop_button: Option<String>,
    pub assistant_message: Option<String>,
    pub conversation_root: Option<String>,
}

/// Canonical strategy type; `uwa-config` re-exports it so config and the
/// extraction pipeline never disagree on the wire format.
pub use uwa_core::ExtractionStrategy;

fn default_bind() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8080
}
fn default_request_timeout_ms() -> u64 {
    120_000
}

impl Config {
    pub fn load_from_path(p: impl AsRef<Path>) -> Result<Self> {
        let text = std::fs::read_to_string(p.as_ref())
            .map_err(|e| UwaError::Config(format!("read {}: {e}", p.as_ref().display())))?;
        Self::load_from_str(&text)
    }

    pub fn load_from_str(s: &str) -> Result<Self> {
        let de = toml::Deserializer::new(s);
        let cfg: Config = serde_path_to_error::deserialize(de)
            .map_err(|e| UwaError::Config(format!("{}: {}", e.path(), e.inner())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.server.bind != "127.0.0.1"
            && self.server.bind != "::1"
            && self.server.bind != "localhost"
            && self.server.api_key.is_none()
        {
            return Err(UwaError::Config(
                "refusing to bind non-loopback without server.api_key".into(),
            ));
        }
        // Every alias must point at an existing provider.
        for (model, prov) in &self.model_aliases {
            if !self.providers.contains_key(prov) {
                return Err(UwaError::Config(format!(
                    "model alias `{model}` -> unknown provider `{prov}`"
                )));
            }
        }
        // Validate groups if present.
        for (name, g) in &self.groups {
            g.validate(&self.providers)
                .map_err(|e| UwaError::Config(format!("group `{name}`: {e}")))?;
        }
        // Validate presets per provider.
        for (name, p) in &self.providers {
            if let Some(default) = &p.default_preset {
                if !p.presets.contains_key(default) {
                    return Err(UwaError::Config(format!(
                        "provider `{name}` has default_preset `{default}` that does not exist"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn provider_for_model(&self, model: &str) -> Option<&ProviderCfg> {
        let prov = self.model_aliases.get(model)?;
        self.providers.get(prov)
    }

    pub fn provider_for_url(&self, url: &Url) -> Option<&ProviderCfg> {
        self.providers
            .values()
            .find(|p| p.url_patterns.iter().any(|pat| url_matches(pat, url)))
    }
}

/// Tiny glob matcher: only `*` is special and matches any suffix/prefix.
/// Good enough for `https://chatgpt.com/*` and `https://gemini.google.com/*`.
pub fn url_matches(pattern: &str, url: &Url) -> bool {
    let u = url.as_str();
    match pattern.split_once('*') {
        None => pattern == u,
        Some((head, tail)) => u.starts_with(head) && (tail.is_empty() || u.ends_with(tail)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [server]
        bind = "127.0.0.1"
        port = 8080
        api_key = "secret"

        [model_aliases]
        "gpt-4o" = "chatgpt"
        "claude-3-5-sonnet" = "claude"

        [providers.chatgpt]
        name = "chatgpt"
        url_patterns = ["https://chatgpt.com/*"]
        capabilities = { streams = true, tool_calls = false, vision = false }
        [providers.chatgpt.selectors]
        input = "textarea#prompt-textarea"
        send_button = "button[data-testid=send-button]"
        assistant_message = "[data-message-author-role=assistant]"

        [providers.claude]
        name = "claude"
        url_patterns = ["https://claude.ai/*"]
        capabilities = { streams = true, tool_calls = true, vision = true }
    "#;

    #[test]
    fn loads_sample_config() {
        let cfg = Config::load_from_str(SAMPLE).unwrap();
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.provider_for_model("gpt-4o").is_some());
        assert!(cfg.provider_for_model("unknown").is_none());
    }

    #[test]
    fn existing_config_without_pid_file_parses() {
        // No `pid_file` key at all: the field defaults to `None`.
        let cfg = Config::load_from_str(SAMPLE).unwrap();
        assert_eq!(cfg.server.pid_file, None);

        // Keys before the next table header still belong to `[server]`.
        let text = SAMPLE.replace(
            "[model_aliases]",
            "pid_file = \"run/uwa.pid\"\n\n[model_aliases]",
        );
        let cfg = Config::load_from_str(&text).unwrap();
        assert_eq!(
            cfg.server.pid_file.as_deref(),
            Some(std::path::Path::new("run/uwa.pid"))
        );
    }

    #[test]
    fn default_for_test_is_a_usable_provider() {
        let p = ProviderCfg::default_for_test("chatgpt");
        assert_eq!(p.name, "chatgpt");
        assert_eq!(p.url_patterns, vec!["https://chatgpt.com/*".to_string()]);
        assert!(p.capabilities.streams);
        assert_eq!(p.extraction, ExtractionStrategy::DomOnly);
        assert!(p.net.is_none());
        assert_eq!(p.finisher, uwa_core::FinisherTuning::default());
    }

    #[test]
    fn rejects_non_loopback_without_key() {
        let bad = r#"
            [server]
            bind = "0.0.0.0"
            port = 8080
        "#;
        let err = Config::load_from_str(bad).unwrap_err();
        assert!(matches!(err, UwaError::Config(_)));
    }

    #[test]
    fn rejects_dangling_alias() {
        let bad = r#"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [model_aliases]
            "gpt-4o" = "nonexistent"
        "#;
        let err = Config::load_from_str(bad).unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn backend_defaults_to_nodriver() {
        let cfg = Config::load_from_str(
            r#"
            [server]
            bind = "127.0.0.1"
            port = 8080
        "#,
        )
        .unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Nodriver);
        assert_eq!(cfg.backend.cdp.ws_url, "http://127.0.0.1:9222");
        assert_eq!(cfg.backend.nodriver.python, "python3");
        assert!(!cfg.backend.nodriver.headless);
    }

    #[test]
    fn provider_can_override_backend() {
        let cfg = Config::load_from_str(
            r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [backend]
            kind = "nodriver"
            [providers.x]
            name = "x"
            url_patterns = ["https://x/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            backend = "cdp"
            [providers.x.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
        "##,
        )
        .unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Nodriver);
        assert_eq!(cfg.providers["x"].backend, Some(BackendKind::Cdp));
    }

    #[test]
    fn backend_cdp_config_parses() {
        let cfg = Config::load_from_str(
            r#"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [backend]
            kind = "cdp"
            [backend.cdp]
            ws_url = "ws://localhost:9999/devtools/browser"
            idle_ttl_secs = 600
        "#,
        )
        .unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Cdp);
        assert_eq!(
            cfg.backend.cdp.ws_url,
            "ws://localhost:9999/devtools/browser"
        );
        assert_eq!(cfg.backend.cdp.idle_ttl_secs, 600);
    }

    #[test]
    fn backend_nodriver_config_parses() {
        let cfg = Config::load_from_str(
            r#"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [backend.nodriver]
            headless = true
            python = "python3.11"
            script = "sidecar/my_sidecar.py"
            user_data_dir = "/tmp/uwa-chrome"
            extra_args = ["--lang=en-US"]
            init_timeout_ms = 5000
        "#,
        )
        .unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Nodriver);
        assert!(cfg.backend.nodriver.headless);
        assert_eq!(cfg.backend.nodriver.python, "python3.11");
        assert_eq!(cfg.backend.nodriver.script, "sidecar/my_sidecar.py");
        assert_eq!(
            cfg.backend.nodriver.user_data_dir,
            Some("/tmp/uwa-chrome".into())
        );
        assert_eq!(
            cfg.backend.nodriver.extra_args,
            vec!["--lang=en-US".to_string()]
        );
        assert_eq!(cfg.backend.nodriver.init_timeout_ms, 5000);
    }

    #[test]
    fn url_glob_matches() {
        let url: Url = "https://chatgpt.com/c/abc".parse().unwrap();
        assert!(url_matches("https://chatgpt.com/*", &url));
        assert!(!url_matches("https://gemini.google.com/*", &url));
    }

    #[test]
    fn stealth_defaults() {
        let cfg = Config::load_from_str(
            r#"
            [server]
            bind = "127.0.0.1"
            port = 8080
        "#,
        )
        .unwrap();
        assert_eq!(cfg.stealth.pack, "default");
        assert!(cfg.stealth.user_scripts_dir.is_none());
    }
}
