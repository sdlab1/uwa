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

pub mod cache;
pub mod challenge;
pub mod features;
pub mod groups;
pub mod preset;
pub mod rate_limit;
pub mod registry;
pub mod timing;
pub use timing::{TimingCfg, TimingProfile};

pub use cache::CacheCfg;
pub use challenge::ChallengeCfg;
pub use features::{FilePasteCfg, MediaCfg, PromptPaddingCfg, ProxyCfg};
pub use groups::{GroupCfg, GroupMember, GroupStrategy};
pub use preset::{EffectiveProviderCfg, PresetCfg};
pub use rate_limit::RateLimitCfg;
pub use registry::{
    Alternates, Candidates, ManifestEntry, Registry, RegistryEntry, RegistryManifest,
};

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
    /// Named browser contexts: separate cookie jars, separate logins.
    /// A request carries `X-UWA-Session: <name>` to pick one; the
    /// default (unnamed) context is used otherwise.
    #[serde(default)]
    pub profiles: HashMap<String, ProfileCfg>,
    /// Human-like timing between DOM actions (keystrokes, focus, send).
    #[serde(default)]
    pub timing: TimingCfg,

    /// Response cache — opt-in, off by default.
    #[serde(default)]
    pub cache: CacheCfg,

    /// Challenge detection.
    #[serde(default)]
    pub challenge: ChallengeCfg,
}

/// One profile in `[profiles.<name>]`. The map key is the profile name.
///
/// `default` is reserved: it means the browser's own context (the one
/// the user already logged into when they launched Chromium).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileCfg {
    /// Human-readable label shown in the dashboard.
    #[serde(default)]
    pub label: String,
    /// Optional per-provider concurrency cap. Overrides the provider's
    /// own `max_concurrency` for requests that use this profile.
    #[serde(default)]
    pub provider_limits: HashMap<String, u32>,
    /// Free-form notes shown in the dashboard.
    #[serde(default)]
    pub notes: String,
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
    /// Defaults to empty so the registry overlay can fill it for a
    /// registered provider (parse → overlay → validate; validation still
    /// rejects an empty pattern list when nothing filled it).
    #[serde(default)]
    pub url_patterns: Vec<String>,
    #[serde(default)]
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
    /// When set, an unknown model name that shares this provider's stem
    /// (e.g. `deepseek-flash` against `deepseek-chat`) is routed to this
    /// model instead of 404. The error still lists the canonical names.
    #[serde(default)]
    pub fallback_model: Option<String>,

    /// Optional per-provider request-rate budget. Disabled when absent.
    #[serde(default)]
    pub rate_limit: RateLimitCfg,

    /// Concurrent in-flight requests for this provider. Overrides the
    /// global default of 2 (four for undefined providers). Higher values
    /// risk tripping site rate limits; a value of 1 serializes requests
    /// per provider, which is safest for aggressive sites.
    #[serde(default = "default_provider_concurrency")]
    pub max_concurrency: u32,

    /// How many times to ask the browser LLM to re-emit a
    /// whose arguments failed schema validation. 0 disables retry;
    /// failures then surface to the client as a 400.
    #[serde(default = "default_tool_retries")]
    pub max_tool_retries: u8,

    /// When true (default), UWA stores what each tab has already seen and
    /// sends only the new turn on repeat requests. Skips the system prompt
    /// (tool schemas) on the second and later turns of a conversation.
    /// Safe to disable; the only cost is a few hundred characters of
    /// re-injection per round.
    #[serde(default = "default_pinning")]
    pub conversation_pinning: bool,
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
            fallback_model: None,
            rate_limit: RateLimitCfg::default(),
            max_concurrency: default_provider_concurrency(),
            max_tool_retries: default_tool_retries(),
            conversation_pinning: default_pinning(),
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

    /// Present when the user is signed in. Used by
    /// `/admin/profiles/:name/health` to answer "is this profile still
    /// logged in?" without sending a message. Optional; the health
    /// endpoint reports `unknown` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logged_in: Option<String>,

    /// Present when the user is signed out (login form, "Sign in"
    /// button). Optional; when both this and `logged_in` are set, the
    /// health endpoint uses whichever matches — a match on this
    /// overrides a match on `logged_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logged_out: Option<String>,
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
fn default_provider_concurrency() -> u32 {
    2
}
fn default_tool_retries() -> u8 {
    2
}
fn default_pinning() -> bool {
    true
}

impl Config {
    pub fn load_from_path(p: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load();
        Self::load_from_path_with_registry(p, &registry)
    }

    pub fn load_from_str(s: &str) -> Result<Self> {
        let registry = Registry::load();
        Self::load_from_str_with_registry(s, &registry)
    }

    /// Same as [`load_from_path`], but with a caller-supplied registry.
    /// Tests pass [`Registry::from_builtin`] to make assertions
    /// independent of the user's cache directory.
    pub fn load_from_path_with_registry(p: impl AsRef<Path>, registry: &Registry) -> Result<Self> {
        let text = std::fs::read_to_string(p.as_ref())
            .map_err(|e| UwaError::Config(format!("read {}: {e}", p.as_ref().display())))?;
        Self::load_from_str_with_registry(&text, registry)
    }

    /// Parse, overlay the registry, validate. Overlay happens **before**
    /// validation so a user config that omits a selector the registry
    /// provides still passes.
    pub fn load_from_str_with_registry(s: &str, registry: &Registry) -> Result<Self> {
        let de = toml::Deserializer::new(s);
        let mut cfg: Config = serde_path_to_error::deserialize(de)
            .map_err(|e| UwaError::Config(format!("{}: {}", e.path(), e.inner())))?;
        cfg.apply_registry(registry);
        // Overlay the named profile's ranges. `Custom` means the user
        // supplied their own ranges and we must not overwrite them.
        let profile = cfg.timing.profile;
        if profile != TimingProfile::Custom {
            cfg.timing = cfg.timing.apply_profile(profile);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Overlay the registry onto every provider the user config mentions.
    /// Providers the user config does not mention are never added — the
    /// registry is a source of overrides, not a source of providers.
    pub fn apply_registry(&mut self, registry: &Registry) {
        for (name, p) in self.providers.iter_mut() {
            registry.overlay(name, p);
        }
    }

    /// Single place where a [`Config`] is declared sound. Everything
    /// downstream (`build_providers`, wiring, presets) trusts this.
    ///
    /// Runs on every `load_from_str`/`load_from_path`.
    pub fn validate(&self) -> Result<()> {
        // 1. Loopback / API-key invariant: an open API is only acceptable
        //    when the socket cannot be reached from the network.
        let bind = self.server.bind.as_str();
        let is_loopback = matches!(bind, "127.0.0.1" | "::1" | "localhost" | "0:0:0:0:0:0:0:1");
        if !is_loopback && self.server.api_key.is_none() {
            return Err(UwaError::Config(format!(
                "server.bind = `{bind}` is not loopback but server.api_key is missing; \
                 set api_key or change bind to 127.0.0.1"
            )));
        }

        // 2. Every model alias points at a provider that exists.
        for (model, prov) in &self.model_aliases {
            if !self.providers.contains_key(prov) {
                return Err(UwaError::Config(format!(
                    "model alias `{model}` -> unknown provider `{prov}`"
                )));
            }
        }

        // 3. Provider tables: key == name, patterns present, and the three
        //    selectors the generic flow cannot work without.
        for (key, p) in &self.providers {
            if p.name != *key {
                return Err(UwaError::Config(format!(
                    "provider key `{key}` != provider.name `{}`",
                    p.name
                )));
            }
            if p.url_patterns.is_empty() {
                return Err(UwaError::Config(format!(
                    "providers.{key}.url_patterns is empty"
                )));
            }
            for (field, sel) in [
                ("input", &p.selectors.input),
                ("send_button", &p.selectors.send_button),
                ("assistant_message", &p.selectors.assistant_message),
            ] {
                if sel.is_none() {
                    return Err(UwaError::Config(format!(
                        "providers.{key}.selectors.{field} is required"
                    )));
                }
            }
            if let Some(default) = &p.default_preset {
                if !p.presets.contains_key(default) {
                    return Err(UwaError::Config(format!(
                        "provider `{key}` has default_preset `{default}` that does not exist"
                    )));
                }
            }
            if p.max_concurrency == 0 {
                return Err(UwaError::Config(format!(
                    "providers.{key}.max_concurrency must be ≥ 1"
                )));
            }
            p.rate_limit
                .validate()
                .map_err(|e| UwaError::Config(format!("providers.{key}.{e}")))?;
        }

        // 4. Groups reference real providers (and real presets per member).
        for (name, g) in &self.groups {
            g.validate(&self.providers)
                .map_err(|e| UwaError::Config(format!("group `{name}`: {e}")))?;
        }

        // 4b. Profiles: names must be identifiers; `default` is reserved;
        //     every named provider limit must reference a real provider.
        for (name, p) in &self.profiles {
            if name == "default" {
                return Err(UwaError::Config(
                    "profiles.default is reserved: use the default context \
                     directly (omit the header) instead of declaring it"
                        .into(),
                ));
            }
            if name.is_empty() {
                return Err(UwaError::Config("profile name must not be empty".into()));
            }
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(UwaError::Config(format!(
                    "profile `{name}`: names may contain only ASCII letters, \
                     digits, `-` and `_`"
                )));
            }
            for (pname, limit) in &p.provider_limits {
                if *limit == 0 {
                    return Err(UwaError::Config(format!(
                        "profiles.{name}.provider_limits.{pname} must be ≥ 1"
                    )));
                }
                if !self.providers.contains_key(pname) {
                    return Err(UwaError::Config(format!(
                        "profiles.{name}.provider_limits references unknown provider `{pname}`"
                    )));
                }
            }
        }

        // 5. CDP backend endpoint must be usable.
        if self.backend.kind == BackendKind::Cdp {
            validate_cdp_ws_url(&self.backend.cdp.ws_url)?;
        }

        // 6. Scheduled restart needs a well-formed HH:MM when enabled.
        if self.scheduled_restart.enabled {
            let at = self.scheduled_restart.at.as_deref().ok_or_else(|| {
                UwaError::Config("scheduled_restart.enabled = true but `at` is missing".into())
            })?;
            validate_hhmm(at)?;
        }

        // 7. A enabled proxy needs a parseable address.
        if self.proxy.enabled {
            if self.proxy.address.is_empty() {
                return Err(UwaError::Config(
                    "proxy.enabled = true but proxy.address is empty".into(),
                ));
            }

            Url::parse(&self.proxy.address).map_err(|e| {
                UwaError::Config(format!("proxy.address `{}`: {e}", self.proxy.address))
            })?;
        }

        // 8. Timing config must have sane ranges.
        self.timing.validate().map_err(UwaError::Config)?;

        // 9. Response cache ranges (only when enabled).
        self.cache.validate().map_err(UwaError::Config)?;

        // 10. Challenge detection ranges (only when enabled).
        self.challenge.validate().map_err(UwaError::Config)?;

        Ok(())
    }

    pub fn provider_for_model(&self, model: &str) -> Option<&ProviderCfg> {
        let prov = self.model_aliases.get(model)?;
        self.providers.get(prov)
    }

    /// Every model name this config serves (aliases), sorted.
    pub fn available_models(&self) -> Vec<String> {
        let mut v: Vec<String> = self.model_aliases.keys().cloned().collect();
        v.sort();
        v
    }

    /// Look up a model alias; on miss, try the provider stem fallback.
    ///
    /// Returns `(provider, resolved_model_name)`. `resolved_model_name` is
    /// what the caller should pass back in the response so the client sees
    /// which model actually answered.
    pub fn resolve_model(
        &self,
        requested: &str,
    ) -> std::result::Result<(&ProviderCfg, String), uwa_core::UwaError> {
        // Vendor prefixes: Aider and some OpenAI-compatible SDKs prepend
        // the backend name (`openai/`, `azure/`, `anthropic/`). Strip it
        // before alias lookup — the config only ever lists canonical names.
        let requested = requested
            .split_once('/')
            .map(|(_, rest)| rest)
            .filter(|rest| !rest.is_empty())
            .unwrap_or(requested);

        if let Some(p) = self.provider_for_model(requested) {
            return Ok((p, requested.to_string()));
        }

        // Stem fallback: "deepseek-flash" → provider whose name matches the
        // stem, if it declares `fallback_model`.
        let stem = requested
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_lowercase();
        if !stem.is_empty() {
            for p in self.providers.values() {
                if !p.name.to_lowercase().starts_with(&stem) {
                    continue;
                }
                if let Some(fb) = &p.fallback_model {
                    return Ok((p, fb.clone()));
                }
            }
        }

        Err(uwa_core::UwaError::UnknownModel(
            uwa_core::ModelNotFound::new(requested, self.available_models()),
        ))
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

/// `backend.cdp.ws_url` sanity. Two accepted shapes:
///
/// * `http(s)://host:port` — resolved through `/json/version` at connect
///   time, so no browser GUID is needed.
/// * `ws://…/devtools/browser/<GUID>` — a direct attach target.
///
/// A bare `ws://…/devtools/browser` (no GUID) is rejected: Chrome answers
/// `404 Not Found` for it, and the failure only surfaces mid-request
/// (CI lesson #3).
fn validate_cdp_ws_url(ws: &str) -> Result<()> {
    let u =
        Url::parse(ws).map_err(|e| UwaError::Config(format!("backend.cdp.ws_url `{ws}`: {e}")))?;
    match u.scheme() {
        "http" | "https" => Ok(()),
        "ws" | "wss" => {
            if u.path() == "/devtools/browser" {
                Err(UwaError::Config(format!(
                    "backend.cdp.ws_url `{ws}`: bare /devtools/browser without a browser \
                     GUID is rejected by Chrome (404); use http://host:port and let \
                     CdpTransport::connect resolve it"
                )))
            } else {
                Ok(())
            }
        }
        other => Err(UwaError::Config(format!(
            "backend.cdp.ws_url `{ws}`: scheme `{other}` is not supported (http:// or ws://)"
        ))),
    }
}

/// `HH:MM` (24h, UTC) sanity for `scheduled_restart.at`.
fn validate_hhmm(at: &str) -> Result<()> {
    let (h, m) = at
        .split_once(':')
        .ok_or_else(|| UwaError::Config(format!("scheduled_restart.at `{at}`: expected HH:MM")))?;
    let h: u32 = h
        .parse()
        .map_err(|_| UwaError::Config(format!("scheduled_restart.at `{at}`: bad hour")))?;
    let m: u32 = m
        .parse()
        .map_err(|_| UwaError::Config(format!("scheduled_restart.at `{at}`: bad minute")))?;
    if h >= 24 || m >= 60 {
        return Err(UwaError::Config(format!(
            "scheduled_restart.at `{at}`: out of range"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod validate_helper_tests {
    use super::*;

    #[test]
    fn cdp_ws_url_accepts_http_and_guid_forms() {
        assert!(validate_cdp_ws_url("http://127.0.0.1:9222").is_ok());
        assert!(validate_cdp_ws_url("ws://127.0.0.1:9222/devtools/browser/4f1a-9c2d").is_ok());
    }

    #[test]
    fn cdp_ws_url_rejects_bare_browser_path() {
        let e = match validate_cdp_ws_url("ws://127.0.0.1:9222/devtools/browser") {
            Err(e) => e,
            Ok(_) => unreachable!(),
        };
        assert!(e.to_string().contains("GUID"), "{e}");
    }

    #[test]
    fn hhmm_accepts_valid_and_rejects_garbage() {
        assert!(validate_hhmm("04:00").is_ok());
        assert!(validate_hhmm("23:59").is_ok());
        assert!(validate_hhmm("25:99").is_err());
        assert!(validate_hhmm("4am").is_err());
        assert!(validate_hhmm("04:60").is_err());
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
        [providers.claude.selectors]
        input = "div[contenteditable=true]"
        send_button = "button[aria-label='Send message']"
        assistant_message = "[data-testid=assistant-message]"
    "#;

    #[test]
    fn login_selectors_parse() {
        let toml = r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [providers.x]
            name = "x"
            url_patterns = ["https://x/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.x.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
            logged_in = "[data-testid='user-menu']"
            logged_out = "a[href='/login']"
        "##;
        let cfg = Config::load_from_str(toml).unwrap_or_else(|e| unreachable!("parse: {e}"));
        let s = cfg
            .providers
            .get("x")
            .map(|p| &p.selectors)
            .unwrap_or_else(|| unreachable!("provider x"));
        assert_eq!(s.logged_in.as_deref(), Some("[data-testid='user-menu']"));
        assert_eq!(s.logged_out.as_deref(), Some("a[href='/login']"));
    }

    #[test]
    fn loads_sample_config() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.provider_for_model("gpt-4o").is_some());
        assert!(cfg.provider_for_model("unknown").is_none());
    }

    #[test]
    fn resolve_model_hits_alias_exactly() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        let (p, name) = cfg
            .resolve_model("gpt-4o")
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(p.name, "chatgpt");
        assert_eq!(name, "gpt-4o");
    }

    #[test]
    fn resolve_model_falls_back_when_provider_declares_it() {
        let toml = r####"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [model_aliases]
            "deepseek-chat" = "deepseek"
            [providers.deepseek]
            name = "deepseek"
            url_patterns = ["https://chat.deepseek.com/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            fallback_model = "deepseek-chat"
            [providers.deepseek.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
        "####;
        let cfg = Config::load_from_str(toml).unwrap_or_else(|_| unreachable!());
        let (p, name) = cfg
            .resolve_model("deepseek-flash")
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(p.name, "deepseek");
        assert_eq!(name, "deepseek-chat");
    }

    #[test]
    fn resolve_model_unknown_without_fallback_reports_candidates() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        let err = match cfg.resolve_model("claude-3-opus") {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        let msg = err.to_string();
        assert!(msg.contains("claude-3-opus"));
        assert!(msg.contains("gpt-4o") || msg.contains("claude-3-5-sonnet"));
    }

    #[test]
    fn provider_concurrency_defaults_and_overrides() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .max_concurrency,
            2
        );
        assert_eq!(
            cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .max_tool_retries,
            2
        );

        let toml = SAMPLE.replace(
            "[providers.chatgpt.selectors]",
            "max_concurrency = 4\nmax_tool_retries = 0\n\n[providers.chatgpt.selectors]",
        );
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .max_concurrency,
            4
        );
        assert_eq!(
            cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .max_tool_retries,
            0
        );
    }

    #[test]
    fn zero_concurrency_is_rejected() {
        let toml = SAMPLE.replace(
            "[providers.chatgpt.selectors]",
            "max_concurrency = 0\n\n[providers.chatgpt.selectors]",
        );
        let err = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(err.to_string().contains("max_concurrency"), "{err}");
    }

    #[test]
    fn conversation_pinning_defaults_true_and_can_be_disabled() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert!(
            cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .conversation_pinning
        );

        let toml = SAMPLE.replace(
            "[providers.chatgpt.selectors]",
            "conversation_pinning = false\n\n[providers.chatgpt.selectors]",
        );
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert!(
            !cfg.providers
                .get("chatgpt")
                .unwrap_or_else(|| unreachable!())
                .conversation_pinning
        );
    }

    #[test]
    fn vendor_prefix_is_stripped_before_alias_lookup() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        let (p, name) = cfg
            .resolve_model("openai/gpt-4o")
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(p.name, "chatgpt");
        assert_eq!(name, "gpt-4o");
    }

    #[test]
    fn provider_rate_limit_parses_and_validates() {
        let toml = SAMPLE.replace(
            "[providers.chatgpt.selectors]",
            "rate_limit = { enabled = true, requests_per_minute = 12, burst = 3 }\n\n[providers.chatgpt.selectors]",
        );
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|e| unreachable!("parse: {e}"));
        let rl = &cfg
            .providers
            .get("chatgpt")
            .unwrap_or_else(|| unreachable!("provider chatgpt"))
            .rate_limit;
        assert!(rl.enabled);
        assert_eq!(rl.requests_per_minute, 12.0);
        assert_eq!(rl.burst, Some(3));
    }

    #[test]
    fn provider_rate_limit_rejects_zero_rpm() {
        let toml = SAMPLE.replace(
            "[providers.chatgpt.selectors]",
            "rate_limit = { enabled = true, requests_per_minute = 0 }\n\n[providers.chatgpt.selectors]",
        );
        let e = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(e.to_string().contains("requests_per_minute"), "{e}");
    }

    #[test]
    fn rejects_provider_missing_send_button_when_registry_has_no_entry() {
        // `demo` is not a registered name, so nothing fills the gap.
        let toml = r####"
        [server]
        bind = "127.0.0.1"
        port = 8080
        [providers.demo]
        name = "demo"
        url_patterns = ["https://demo.test/*"]
        capabilities = { streams = true, tool_calls = false, vision = false }
        [providers.demo.selectors]
        input = "#prompt"
        assistant_message = ".assistant"
        "####;
        let err = match Config::load_from_str_with_registry(toml, &Registry::from_builtin()) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(err.to_string().contains("send_button"), "{err}");
    }

    #[test]
    fn registry_fills_missing_selectors_for_known_provider() {
        // `chatgpt` is registered, so the user only has to override what
        // they actually want to change.
        let toml = r####"
        [server]
        bind = "127.0.0.1"
        port = 8080
        [providers.chatgpt]
        name = "chatgpt"
        [providers.chatgpt.selectors]
        input = "#custom-input"
        "####;
        let cfg = Config::load_from_str_with_registry(toml, &Registry::from_builtin())
            .unwrap_or_else(|_| unreachable!());
        let p = cfg
            .providers
            .get("chatgpt")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(p.selectors.input.as_deref(), Some("#custom-input"));
        // Filled in from the registry.
        assert!(p.selectors.send_button.is_some());
        assert!(p.selectors.assistant_message.is_some());
        assert!(p.selectors_version.is_some());
    }

    #[test]
    fn existing_config_without_pid_file_parses() {
        // No `pid_file` key at all: the field defaults to `None`.
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.server.pid_file, None);

        // Keys before the next table header still belong to `[server]`.
        let text = SAMPLE.replace(
            "[model_aliases]",
            "pid_file = \"run/uwa.pid\"\n\n[model_aliases]",
        );
        let cfg = Config::load_from_str(&text).unwrap_or_else(|_| unreachable!());
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
        let err = match Config::load_from_str(bad) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
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
        let err = match Config::load_from_str(bad) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
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
        .unwrap_or_else(|_| unreachable!());
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
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.backend.kind, BackendKind::Nodriver);
        assert_eq!(
            cfg.providers
                .get("x")
                .unwrap_or_else(|| unreachable!())
                .backend,
            Some(BackendKind::Cdp)
        );
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
            ws_url = "ws://localhost:9999/devtools/browser/8f4a1b2e-1234-4c5d-9e8f-a1b2c3d4e5f6"
            idle_ttl_secs = 600
        "#,
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.backend.kind, BackendKind::Cdp);
        assert_eq!(
            cfg.backend.cdp.ws_url,
            "ws://localhost:9999/devtools/browser/8f4a1b2e-1234-4c5d-9e8f-a1b2c3d4e5f6"
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
        .unwrap_or_else(|_| unreachable!());
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
    fn profiles_parse_and_validate() {
        let toml = format!(
            r#"{SAMPLE}
            [profiles.work]
            label = "Work accounts"
            provider_limits = {{ chatgpt = 1 }}

            [profiles.personal]
            label = "Personal"
            "#
        );
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.profiles.len(), 2);
        let work = cfg.profiles.get("work").unwrap_or_else(|| unreachable!());
        assert_eq!(work.label, "Work accounts");
        assert_eq!(work.provider_limits.get("chatgpt"), Some(&1));
        assert_eq!(
            cfg.profiles.get("personal").map(|p| p.label.as_str()),
            Some("Personal")
        );
    }

    #[test]
    fn profiles_default_to_empty() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn profile_default_name_is_rejected() {
        let toml = format!("{SAMPLE}\n[profiles.default]\nlabel = \"x\"\n");
        let err = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn profile_bad_name_is_rejected() {
        let toml = format!("{SAMPLE}\n[profiles.\"with space\"]\nlabel = \"x\"\n");
        assert!(Config::load_from_str(&toml).is_err());
    }

    #[test]
    fn profile_zero_limit_is_rejected() {
        let toml = format!("{SAMPLE}\n[profiles.work]\nprovider_limits = {{ chatgpt = 0 }}\n");
        let err = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(err.to_string().contains("≥ 1"), "{err}");
    }

    #[test]
    fn profile_limit_for_unknown_provider_is_rejected() {
        let toml = format!("{SAMPLE}\n[profiles.work]\nprovider_limits = {{ nosuch = 1 }}\n");
        let err = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(err.to_string().contains("nosuch"), "{err}");
    }

    #[test]
    fn url_glob_matches() {
        let url: Url = "https://chatgpt.com/c/abc"
            .parse()
            .unwrap_or_else(|_| unreachable!());
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
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.stealth.pack, "default");
        assert!(cfg.stealth.user_scripts_dir.is_none());
    }

    #[test]
    fn timing_defaults_to_natural() {
        let cfg = Config::load_from_str(SAMPLE).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.timing.profile, TimingProfile::Natural);
    }

    #[test]
    fn timing_profile_override_applies() {
        let toml = format!("{}\n[timing]\nprofile = \"slow\"\n", SAMPLE);
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.timing.profile, TimingProfile::Slow);
        assert!(cfg.timing.key_ms.first().unwrap_or_else(|| unreachable!()) >= &100);
    }

    #[test]
    fn timing_fast_profile_zeroes_ranges() {
        let toml = format!("{}\n[timing]\nprofile = \"fast\"\n", SAMPLE);
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.timing.profile, TimingProfile::Fast);
        assert_eq!(cfg.timing.key_ms, [0, 0]);
        assert_eq!(cfg.timing.pre_send_ms, [0, 0]);
        assert!(!cfg.timing.burst);
    }

    #[test]
    fn timing_custom_values_survive() {
        let toml = format!(
            "{}\n[timing]\nprofile = \"custom\"\nkey_ms = [5, 10]\n",
            SAMPLE
        );
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.timing.key_ms, [5, 10]);
    }

    #[test]
    fn timing_bad_poll_is_rejected() {
        let toml = format!("{}\n[timing]\npoll_ms = 10\n", SAMPLE);
        let e = match Config::load_from_str(&toml) {
            Err(e) => e,
            Ok(_) => unreachable!(),
        };
        assert!(e.to_string().contains("poll_ms"), "{e}");
    }

    #[test]
    fn cache_section_parses() {
        let toml = format!("{SAMPLE}\n[cache]\nenabled = true\nttl_secs = 120\ncapacity = 64\n");
        let cfg = Config::load_from_str(&toml).unwrap_or_else(|e| unreachable!("parse: {e}"));
        assert!(cfg.cache.enabled);
        assert_eq!(cfg.cache.ttl_secs, 120);
        assert_eq!(cfg.cache.capacity, 64);
    }

    #[test]
    fn cache_bad_ttl_is_rejected() {
        let toml = format!("{SAMPLE}\n[cache]\nenabled = true\nttl_secs = 0\n");
        let e = match Config::load_from_str(&toml) {
            Err(err) => err,
            Ok(_) => unreachable!(),
        };
        assert!(e.to_string().contains("ttl_secs"), "{e}");
    }
}
