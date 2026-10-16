//! `uwa conformance run | report`.
//!
//! `run` drives the live pipeline for a given provider and writes a JSON
//! report. `report` regenerates the Markdown table from a JSON report.
//!
//! Live runs need a running browser with a logged-in tab — they're the
//! nightly CI path, not something a fresh install will hit by accident.
//! Fixture runs (the default when no `--provider` is given) exercise the
//! parser and every check without a browser.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use uwa_conformance::backend::{BackendOutcome, ConformanceBackend};
use uwa_conformance::scenario::Scenario;
use uwa_conformance::score::{ProviderScore, Report};
use uwa_conformance::{run_all, FixtureBackend, SCENARIOS};

use super::paths;

#[derive(Args)]
pub struct ConformanceArgs {
    #[command(subcommand)]
    action: ConformanceAction,
}

#[derive(Subcommand)]
enum ConformanceAction {
    /// Run the corpus. Without `--provider`, uses the fixture backend.
    Run(RunArgs),
    /// Regenerate `docs/CONFORMANCE.md` from a JSON report.
    Report {
        /// Path to a report produced by `run`.
        #[arg(long, default_value_os_t = default_report_path())]
        input: PathBuf,
        /// Where to write the Markdown.
        #[arg(long, default_value = "docs/CONFORMANCE.md")]
        out: PathBuf,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// Provider name to drive (e.g. `chatgpt`). Repeatable.
    /// When omitted, the fixture backend runs the entire corpus
    /// in-process — no browser, fast.
    #[arg(long)]
    provider: Vec<String>,
    /// Config file the live run should load.
    #[arg(long, env = "UWA_CONFIG", default_value = "uwa.toml")]
    config: PathBuf,
    /// Where to write the JSON report.
    #[arg(long, default_value_os_t = default_report_path())]
    out: PathBuf,
    /// Print a Markdown table to stdout after the run.
    #[arg(long)]
    markdown: bool,
}

fn default_report_path() -> PathBuf {
    paths::uwa_home().join("conformance").join("latest.json")
}

pub async fn run(args: ConformanceArgs) -> Result<()> {
    match args.action {
        ConformanceAction::Run(r) => run_corpus(r).await,
        ConformanceAction::Report { input, out } => regenerate_markdown(&input, &out).await,
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

async fn run_corpus(args: RunArgs) -> Result<()> {
    if args.provider.is_empty() {
        return run_fixture(&args).await;
    }
    run_live(&args).await
}

async fn run_fixture(args: &RunArgs) -> Result<()> {
    println!(
        "uwa conformance — fixture mode ({} scenarios)",
        SCENARIOS.len()
    );
    let backend = FixtureBackend::new("fixture");
    let results = run_all(&backend).await;
    let finished = SystemTime::now();
    let started = finished
        .checked_sub(Duration::from_secs(1))
        .unwrap_or(finished);
    let score = ProviderScore::build("fixture", started, finished, results);

    println!(
        "  {}/{} passed ({:.1}%)",
        score.passed,
        score.scenarios_total,
        score.success_rate * 100.0
    );
    for f in &score.failures {
        println!("  ✗ {} — {}", f.id, f.reason.as_deref().unwrap_or(""));
    }

    let report = Report::build(vec![score], env!("UWA_VERSION"));
    write_report(&args.out, &report)?;
    if args.markdown {
        print!("{}", report.to_markdown());
    }
    Ok(())
}

async fn run_live(args: &RunArgs) -> Result<()> {
    // Live runs need a real `AppState`. We assemble one through the same
    // wiring the daemon uses, but without binding the HTTP listener.
    let config = Arc::new(
        uwa_config::Config::load_from_path(&args.config)
            .with_context(|| format!("loading {}", args.config.display()))?,
    );

    let cdp = std::env::var("UWA_CHROMIUM_WS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:9222".into());

    let mut registry = uwa_api::ProviderRegistry::new();
    for (_name, p) in uwa_providers::build_providers(&config)? {
        registry.register(p);
    }
    let transport: Arc<dyn uwa_core::Transport> = Arc::new(
        uwa_browser::CdpTransport::connect(
            &cdp,
            Duration::from_secs(1800),
            Some(uwa_stealth::builtin::default_pack()),
        )
        .await
        .with_context(|| format!("connect Chromium at {cdp}"))?,
    );

    let sessions = Arc::new(uwa_session::SessionManager::new(Default::default()));
    let history =
        Arc::new(uwa_history::HistoryStore::new(uwa_history::HistoryCfg::default(), false).await?);

    let mut sem = uwa_resilience::semaphore::ProviderSemaphores::new(4);
    for (name, p) in &config.providers {
        sem = sem.with_limit(name, p.max_concurrency as usize);
    }

    let state = uwa_api::AppState::minimal(config.clone(), Arc::new(registry), transport)
        .with_sessions(sessions)
        .with_semaphores(Arc::new(sem))
        .with_history(history);

    let mut scores = Vec::new();
    for provider in &args.provider {
        println!(
            "uwa conformance — live mode, provider `{provider}` ({} scenarios)",
            SCENARIOS.len()
        );
        let backend = LiveBackend::new(state.clone(), provider.clone())?;
        let started = SystemTime::now();
        let results = run_all(&backend).await;
        let finished = SystemTime::now();

        // Echo pass/fail per scenario as we finish.
        for r in &results {
            match r.status.as_str() {
                "pass" => println!("  ✓ {}", r.id),
                _ => println!(
                    "  ✗ {} — {}",
                    r.id,
                    r.reason.as_deref().unwrap_or("(no reason)")
                ),
            }
        }
        let score = ProviderScore::build(provider, started, finished, results);
        println!(
            "  {}/{} passed ({:.1}%)\n",
            score.passed,
            score.scenarios_total,
            score.success_rate * 100.0
        );
        scores.push(score);
    }

    let report = Report::build(scores, env!("UWA_VERSION"));
    write_report(&args.out, &report)?;
    if args.markdown {
        print!("{}", report.to_markdown());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// live backend
// ---------------------------------------------------------------------------

struct LiveBackend {
    state: uwa_api::AppState,
    provider: String,
}

impl LiveBackend {
    fn new(state: uwa_api::AppState, provider: String) -> Result<Self> {
        // Fail fast if the provider is unknown.
        if !state.cfg().providers.contains_key(&provider) {
            bail!("provider `{provider}` is not in the config");
        }
        Ok(Self { state, provider })
    }
}

#[async_trait::async_trait]
impl ConformanceBackend for LiveBackend {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn run(&self, scenario: &Scenario) -> Result<BackendOutcome, String> {
        use uwa_core::types::openai::ChatCompletionRequest;
        use uwa_core::types::Role;

        // Find a model alias that routes to this provider; if there are
        // several, the first one wins — it only affects the response's
        // `model` field, which conformance ignores.
        let model = self.state.cfg().model_aliases
            .iter()
            .find(|(_, p)| p.as_str() == self.provider)
            .map(|(m, _)| m.clone())
            .ok_or_else(|| {
                format!(
                    "no model alias points at provider `{}` — add one to uwa.toml",
                    self.provider
                )
            })?;

        let tools: Vec<serde_json::Value> = (scenario.tools)()
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();

        let mut messages = Vec::new();
        if let Some(extra) = scenario.system {
            messages.push(uwa_core::types::openai::ChatMessage {
                role: Role::System,
                content: Some(uwa_core::types::openai::MessageContent::Text(extra.into())),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                uwa_media: Vec::new(),
            });
        }
        messages.push(uwa_core::types::openai::ChatMessage {
            role: Role::User,
            content: Some(uwa_core::types::openai::MessageContent::Text(
                scenario.user.to_string(),
            )),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            uwa_media: Vec::new(),
        });

        let req = ChatCompletionRequest {
            model,
            messages,
            stream: Some(false),
            stream_options: None,
            temperature: None,
            max_tokens: None,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: None,
            user: None,
        };

        let all_specs =
            uwa_api::routes::chat::local_tools(&req).map_err(|e| format!("local_tools: {e}"))?;

        let outcome = uwa_api::routes::chat::run_pipeline(
            &self.state,
            &req,
            &all_specs,
            None,
            &uwa_session::ProfileName::default_name(),
            &uwa_api::routing::RoutingHint::default(),
        )
        .await
        .map_err(|e| format!("pipeline: {e}"))?;

        Ok(BackendOutcome {
            outcome: uwa_tools::ToolParseOutcome {
                calls: outcome.tool_calls,
                text: outcome.text,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

fn write_report(path: &PathBuf, report: &Report) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, report.to_json_pretty())
        .with_context(|| format!("write {}", path.display()))?;
    println!("→ wrote {}", path.display());
    Ok(())
}

async fn regenerate_markdown(input: &PathBuf, out: &PathBuf) -> Result<()> {
    let raw =
        std::fs::read_to_string(input).with_context(|| format!("read {}", input.display()))?;
    let report: Report =
        serde_json::from_str(&raw).map_err(|e| anyhow!("parse {}: {e}", input.display()))?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, report.to_markdown())
        .with_context(|| format!("write {}", out.display()))?;
    println!("→ wrote {}", out.display());
    Ok(())
}
