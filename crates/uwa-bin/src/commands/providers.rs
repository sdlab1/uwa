//! `uwa providers list | show | diff | update | canary`.
//!
//! The registry commands never mutate `uwa.toml` — user overrides always
//! survive. `update` writes to the cache dir only.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

use uwa_config::registry::Registry;
use uwa_config::{Config, ProviderCfg};

use super::paths;
use super::update::{hex, http_get_string};

#[derive(Args)]
pub struct ProvidersArgs {
    #[command(subcommand)]
    action: ProvidersAction,
}

#[derive(Subcommand)]
enum ProvidersAction {
    /// List registry entries and, for each, whether the user config
    /// overrides them.
    List {
        #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
        config: PathBuf,
    },

    /// Show the merged view of one provider: registry entry + every
    /// user override, with the source of each field labelled.
    Show {
        name: String,
        #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
        config: PathBuf,
    },

    /// Print the fields the user config overrides for a provider, and
    /// nothing else. Answers "why did the registry update not affect me?".
    Diff {
        name: String,
        #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
        config: PathBuf,
    },

    /// Fetch the latest registry from GitHub, verify each entry against
    /// the manifest's SHA256, and write to `~/.uwa/providers/`.
    Update {
        #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
        config: PathBuf,
        /// Skip verification (do not).
        #[arg(long)]
        no_verify: bool,
        /// Print the plan but write nothing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run a live selector check against every registered provider whose
    /// site is currently open in the browser.
    Canary {
        /// Config file whose providers should be checked.
        #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
        config: PathBuf,
        /// Where to write the JSON report.
        #[arg(long, default_value_os_t = default_canary_path())]
        out: PathBuf,
        /// Print a Markdown table to stdout.
        #[arg(long)]
        markdown: bool,
    },
}

fn default_canary_path() -> PathBuf {
    paths::uwa_home().join("canary").join("latest.json")
}

pub async fn run(args: ProvidersArgs) -> Result<()> {
    match args.action {
        ProvidersAction::List { config } => list(&config),
        ProvidersAction::Show { name, config } => show(&name, &config),
        ProvidersAction::Diff { name, config } => diff(&name, &config),
        ProvidersAction::Update {
            config,
            no_verify,
            dry_run,
        } => update(&config, no_verify, dry_run).await,
        ProvidersAction::Canary {
            config,
            out,
            markdown,
        } => canary(&config, &out, markdown).await,
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list(config_path: &std::path::Path) -> Result<()> {
    let registry = Registry::load();
    let cfg = load_or_empty(config_path);

    let mut overridden: Vec<String> = Vec::new();
    if let Some(cfg) = cfg.as_ref() {
        for (name, p) in &cfg.providers {
            if registry.get(name).is_some() && is_overridden(p) {
                overridden.push(name.clone());
            }
        }
    }

    println!("registry entries: {}", registry.names().len());
    if let Some(m) = registry.manifest() {
        println!("manifest version: {}", m.registry_version);
    }
    match registry.cache_dir() {
        Some(dir) => println!("cache dir:        {}", dir.display()),
        None => println!("cache dir:        (none; using built-in)"),
    }
    println!();
    println!("{:<16} {:<14} status", "provider", "version");
    for name in registry.names() {
        let Some(entry) = registry.get(&name) else {
            continue;
        };
        let status = if overridden.contains(&name) {
            "user overrides some fields"
        } else if cfg
            .as_ref()
            .map(|c| c.providers.contains_key(&name))
            .unwrap_or(false)
        {
            "user config, no overrides"
        } else {
            "not in user config"
        };
        println!("{:<16} {:<14} {}", name, entry.meta.version, status);
    }
    Ok(())
}

fn load_or_empty(path: &std::path::Path) -> Option<Config> {
    if !path.exists() {
        return None;
    }
    Config::load_from_path_with_registry(path, &Registry::from_builtin()).ok()
}

/// Whether the user config for this provider says anything the registry
/// entry does not already say. "Anything" = any selector, url pattern,
/// or net rule.
fn is_overridden(p: &ProviderCfg) -> bool {
    p.selectors.input.is_some()
        || p.selectors.send_button.is_some()
        || p.selectors.stop_button.is_some()
        || p.selectors.assistant_message.is_some()
        || p.selectors.conversation_root.is_some()
        || !p.url_patterns.is_empty()
        || p.net.is_some()
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

fn show(name: &str, config_path: &std::path::Path) -> Result<()> {
    let registry = Registry::load();
    let entry = registry
        .get(name)
        .ok_or_else(|| anyhow!("provider `{name}` is not in the registry"))?;

    let user = load_user_only(config_path)?.providers.get(name).cloned();

    println!("provider: {name}");
    println!("registry version: {}", entry.meta.version);
    println!();

    print_field("input", entry, user.as_ref());
    print_field("send_button", entry, user.as_ref());
    print_field("stop_button", entry, user.as_ref());
    print_field("assistant_message", entry, user.as_ref());
    print_field("conversation_root", entry, user.as_ref());

    println!();
    println!("alternates:");
    for field in ["input", "send_button", "stop_button", "assistant_message"] {
        let alts = registry.alternates_for(name, field);
        if alts.is_empty() {
            println!("  {field}: —");
        } else {
            println!("  {field}:");
            for a in alts {
                println!("    - {a}");
            }
        }
    }

    if let Some(net) = &entry.net {
        println!();
        println!("net url_contains: {:?}", net.url_contains);
        println!("net mime_contains: {:?}", net.mime_contains);
    }
    Ok(())
}

fn print_field(name: &str, entry: &uwa_config::RegistryEntry, user: Option<&ProviderCfg>) {
    let registry_val = selector_field(entry, name);
    let user_val = user.and_then(|p| selector_field_user(p, name));
    let (source, value) = match (user_val.as_ref(), registry_val.as_ref()) {
        (Some(u), Some(r)) if u != r => ("user", Some(u.clone())),
        (Some(_), Some(r)) => ("user (=registry)", Some(r.clone())),
        (Some(u), None) => ("user", Some(u.clone())),
        (None, Some(r)) => ("registry", Some(r.clone())),
        (None, None) => ("—", None),
    };
    println!(
        "{:<18} [{:<15}] {}",
        name,
        source,
        value.unwrap_or_else(|| "(unset)".into())
    );
}

fn selector_field(entry: &uwa_config::RegistryEntry, field: &str) -> Option<String> {
    let s = &entry.selectors;
    match field {
        "input" => s.input.clone(),
        "send_button" => s.send_button.clone(),
        "stop_button" => s.stop_button.clone(),
        "assistant_message" => s.assistant_message.clone(),
        "conversation_root" => s.conversation_root.clone(),
        _ => None,
    }
}

fn selector_field_user(p: &ProviderCfg, field: &str) -> Option<String> {
    let s = &p.selectors;
    match field {
        "input" => s.input.clone(),
        "send_button" => s.send_button.clone(),
        "stop_button" => s.stop_button.clone(),
        "assistant_message" => s.assistant_message.clone(),
        "conversation_root" => s.conversation_root.clone(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

fn diff(name: &str, config_path: &std::path::Path) -> Result<()> {
    let registry = Registry::load();
    let entry = registry
        .get(name)
        .ok_or_else(|| anyhow!("provider `{name}` is not in the registry"))?;

    let user_only = load_user_only(config_path)?.providers.get(name).cloned();

    let mut any = false;
    let mut print = |field: &str, user: Option<String>, registry_val: Option<String>| {
        let Some(user) = user else { return };
        any = true;
        let reg = registry_val.unwrap_or_else(|| "(unset)".into());
        println!("{field}:");
        println!("  - {reg}");
        println!("  + {user}");
    };

    print(
        "input",
        user_only.as_ref().and_then(|p| p.selectors.input.clone()),
        entry.selectors.input.clone(),
    );
    print(
        "send_button",
        user_only
            .as_ref()
            .and_then(|p| p.selectors.send_button.clone()),
        entry.selectors.send_button.clone(),
    );
    print(
        "stop_button",
        user_only
            .as_ref()
            .and_then(|p| p.selectors.stop_button.clone()),
        entry.selectors.stop_button.clone(),
    );
    print(
        "assistant_message",
        user_only
            .as_ref()
            .and_then(|p| p.selectors.assistant_message.clone()),
        entry.selectors.assistant_message.clone(),
    );
    print(
        "conversation_root",
        user_only
            .as_ref()
            .and_then(|p| p.selectors.conversation_root.clone()),
        entry.selectors.conversation_root.clone(),
    );

    if let Some(p) = &user_only {
        if !p.url_patterns.is_empty() {
            any = true;
            println!("url_patterns:");
            println!("  - {:?}", entry.url_patterns);
            println!("  + {:?}", p.url_patterns);
        }
        if p.net.is_some() {
            any = true;
            println!("net: user override present");
        }
    }

    if !any {
        println!("`{name}`: no user overrides — registry values are in effect");
    }
    Ok(())
}

fn load_user_only(path: &std::path::Path) -> Result<Config> {
    // A fresh registry with no entries: the merge becomes a no-op, so
    // the resulting config is exactly what the user's TOML says.
    let empty = Registry::default();
    Config::load_from_path_with_registry(path, &empty)
        .with_context(|| format!("loading {}", path.display()))
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/sdlab1/uwa/main/providers/manifest.toml";
const RAW_BASE: &str = "https://raw.githubusercontent.com/sdlab1/uwa/main/providers";

async fn update(config_path: &std::path::Path, no_verify: bool, dry_run: bool) -> Result<()> {
    let builtin = Registry::from_builtin();
    let cache_dir = paths::uwa_home().join("providers");
    std::fs::create_dir_all(&cache_dir)?;

    println!("→ fetching manifest from {MANIFEST_URL}");
    let manifest_text = http_get_string(MANIFEST_URL)?;
    let manifest: uwa_config::RegistryManifest =
        toml::from_str(&manifest_text).map_err(|e| anyhow!("manifest parse: {e}"))?;

    let mut planned: Vec<(String, String, String, bool)> = Vec::new();
    for (name, m) in &manifest.providers {
        let current = builtin
            .get(name)
            .map(|e| e.meta.version.clone())
            .unwrap_or_else(|| "(none)".into());
        let newer = m.version.as_str() > current.as_str();
        planned.push((name.clone(), current, m.version.clone(), newer));
    }
    planned.sort_by(|a, b| a.0.cmp(&b.0));

    println!();
    println!(
        "{:<16} {:<14} {:<14} action",
        "provider", "current", "registry"
    );
    for (name, cur, reg, newer) in &planned {
        let action = if *newer {
            "update"
        } else if cur == "(none)" {
            "add"
        } else {
            "up-to-date"
        };
        println!("{:<16} {:<14} {:<14} {}", name, cur, reg, action);
    }
    let _ = config_path; // reserved: per-provider auth for live checks

    if dry_run {
        println!();
        println!("(dry run — nothing written)");
        return Ok(());
    }

    let mut updated = 0usize;
    for (name, _cur, version, newer) in &planned {
        if !*newer {
            continue;
        }
        let Some(m) = manifest.providers.get(name) else {
            bail!("manifest changed mid-run");
        };
        let url = format!("{RAW_BASE}/{}", m.file);
        println!("→ fetching {url}");
        let body = http_get_string(&url)?;
        if !no_verify {
            let actual = hex(&sha256(body.as_bytes()));
            if actual != m.sha256 {
                bail!(
                    "checksum mismatch for `{name}`: expected {}, got {}",
                    m.sha256,
                    actual
                );
            }
        }
        let path = cache_dir.join(&m.file);
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        println!("  ✓ {name} → {version}");
        updated = updated.saturating_add(1);
    }

    // Write the manifest last so a partial update is not "valid".
    let manifest_path = cache_dir.join("manifest.toml");
    std::fs::write(&manifest_path, &manifest_text)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    println!();
    println!(
        "updated {updated} entr{}",
        if updated == 1 { "y" } else { "ies" }
    );
    println!("restart the daemon to pick up changes: uwa daemon stop && uwa daemon start");
    Ok(())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// canary
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CanaryReport {
    pub generated_at: String,
    pub uwa_version: String,
    pub providers: Vec<CanaryEntry>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CanaryEntry {
    pub name: String,
    pub registry_version: String,
    /// `ok` / `degraded` / `broken` / `unavailable`.
    pub status: String,
    pub checks: std::collections::BTreeMap<String, CanaryCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks_used: Vec<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CanaryCheck {
    pub primary: String,
    pub matched: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

async fn canary(
    config_path: &std::path::Path,
    out: &std::path::Path,
    markdown: bool,
) -> Result<()> {
    let registry = Registry::load();
    let config = std::sync::Arc::new(
        Config::load_from_path_with_registry(config_path, &registry)
            .with_context(|| format!("loading {}", config_path.display()))?,
    );

    let cdp = std::env::var("UWA_CHROMIUM_WS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:9222".into());

    let transport: std::sync::Arc<dyn uwa_core::Transport> = std::sync::Arc::new(
        uwa_browser::CdpTransport::connect(&cdp, Duration::from_secs(300), None)
            .await
            .with_context(|| format!("connect Chromium at {cdp}"))?,
    );

    // Snapshot the current tabs once so we can attribute each provider.
    let tabs = transport.list_tabs().await?;
    let mut tab_urls: Vec<(uwa_core::TabId, String)> = Vec::new();
    for t in &tabs {
        if let Ok(page) = transport.page(t).await {
            if let Ok(u) = page.url().await {
                tab_urls.push((t.clone(), u.to_string()));
            }
        }
    }

    let mut entries: Vec<CanaryEntry> = Vec::new();
    for (name, p) in &config.providers {
        let reg_version = registry
            .get(name)
            .map(|e| e.meta.version.clone())
            .unwrap_or_else(|| "(unregistered)".into());

        // Find a tab whose URL matches the provider.
        let matching = tab_urls.iter().find_map(|(tab, url)| {
            let parsed = url::Url::parse(url).ok()?;
            if p.url_patterns.iter().any(|pat| url_matches(pat, &parsed)) {
                Some(tab.clone())
            } else {
                None
            }
        });
        let Some(tab) = matching else {
            entries.push(CanaryEntry {
                name: name.clone(),
                registry_version: reg_version,
                status: "unavailable".into(),
                checks: Default::default(),
                fallbacks_used: Vec::new(),
            });
            continue;
        };

        let page = transport.page(&tab).await?;
        let mut checks = std::collections::BTreeMap::new();
        let mut fallbacks = Vec::new();
        let mut all_ok = true;
        let mut any_ok = false;

        for field in ["input", "send_button", "assistant_message"] {
            let primary = match field {
                "input" => p.selectors.input.clone(),
                "send_button" => p.selectors.send_button.clone(),
                _ => p.selectors.assistant_message.clone(),
            };
            let Some(primary) = primary else {
                continue;
            };

            // Try the primary first, then fallbacks, in order.
            let mut matched = 0u32;
            let mut matched_selector: Option<String> = None;
            let mut error: Option<String> = None;
            let mut tried = vec![primary.clone()];
            tried.extend(registry.alternates_for(name, field));

            for (i, sel) in tried.iter().enumerate() {
                match eval_selector(page.as_ref(), sel).await {
                    Ok(n) if n > 0 => {
                        matched = n;
                        matched_selector = Some(sel.clone());
                        if i > 0 {
                            fallbacks.push(format!("{field}:{sel}"));
                        }
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error = Some(e.to_string());
                    }
                }
            }

            let ok = matched > 0 && matched_selector.as_deref() == Some(primary.as_str());
            if matched > 0 {
                any_ok = true;
            }
            if !ok {
                all_ok = false;
            }
            checks.insert(
                field.to_string(),
                CanaryCheck {
                    primary,
                    matched,
                    ok,
                    matched_selector,
                    error,
                },
            );
        }

        let status = if !any_ok {
            "broken"
        } else if !all_ok || !fallbacks.is_empty() {
            "degraded"
        } else {
            "ok"
        };

        entries.push(CanaryEntry {
            name: name.clone(),
            registry_version: reg_version,
            status: status.into(),
            checks,
            fallbacks_used: fallbacks,
        });
    }

    let report = CanaryReport {
        generated_at: now_iso(),
        uwa_version: env!("UWA_VERSION").into(),
        providers: entries,
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(out, json).with_context(|| format!("write {}", out.display()))?;
    println!("→ wrote {}", out.display());

    if markdown {
        print!("{}", canary_to_markdown(&report));
    }
    Ok(())
}

async fn eval_selector(page: &dyn uwa_core::Page, selector: &str) -> Result<u32> {
    let js = format!(
        "document.querySelectorAll({}).length",
        serde_json::to_string(selector)?
    );
    let v = page.eval(&js).await?;
    Ok(v.as_u64().unwrap_or(0) as u32)
}

fn url_matches(pattern: &str, url: &url::Url) -> bool {
    let u = url.as_str();
    match pattern.split_once('*') {
        None => pattern == u,
        Some((head, tail)) => u.starts_with(head) && (tail.is_empty() || u.ends_with(tail)),
    }
}

fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86_400;
    let rem = now % 86_400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    // Machine timestamp without a chrono dependency (see uwa-conformance).
    format!("epoch+{days}dT{hh:02}:{mm:02}:{ss:02}Z")
}

fn canary_to_markdown(r: &CanaryReport) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "_Generated {} · uwa {}_\n\n",
        r.generated_at, r.uwa_version
    ));
    s.push_str("| Provider | Registry | Status | Input | Send | Assistant | Fallbacks |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for p in &r.providers {
        let cell = |k: &str| {
            p.checks
                .get(k)
                .map(|c| {
                    if c.ok {
                        "ok".to_string()
                    } else if c.matched > 0 {
                        "fallback".to_string()
                    } else {
                        "miss".to_string()
                    }
                })
                .unwrap_or_else(|| "—".into())
        };
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            p.name,
            p.registry_version,
            p.status,
            cell("input"),
            cell("send_button"),
            cell("assistant_message"),
            if p.fallbacks_used.is_empty() {
                "—".into()
            } else {
                p.fallbacks_used.join(", ")
            }
        ));
    }
    s
}
