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

pub type Result<T> = std::result::Result<T, UwaError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerCfg,
    #[serde(default)]
    pub providers: HashMap<String, ProviderCfg>,
    /// model-name -> provider-name
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    /// External MCP servers to connect to as stdio subprocesses.
    #[serde(default)]
    pub mcp_clients: Vec<McpClientConfig>,
    /// Expose this bridge itself via stdio as an MCP server.
    #[serde(default)]
    pub mcp_server: McpServerConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
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
    fn url_glob_matches() {
        let url: Url = "https://chatgpt.com/c/abc".parse().unwrap();
        assert!(url_matches("https://chatgpt.com/*", &url));
        assert!(!url_matches("https://gemini.google.com/*", &url));
    }
}
