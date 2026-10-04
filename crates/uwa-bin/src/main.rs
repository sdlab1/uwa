//! `uwa` — the daemon. Wires config + providers + transport into `uwa-api`.

use anyhow::Context;
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use uwa_api::{router, AppState, ProviderRegistry};
use uwa_config::Config;

#[derive(Parser)]
#[command(version, about = "Universal Web API — Rust bridge")]
struct Cli {
    /// Path to TOML config.
    #[arg(short, long, env = "UWA_CONFIG", default_value = "uwa.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let config = Config::load_from_path(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;
    let config = Arc::new(config);

    // NOTE: real transport lands in Фаза 2 (`uwa-browser`). For now — stub
    // so the HTTP surface is testable end-to-end via trait objects.
    let providers = ProviderRegistry::new();
    let transport: Arc<dyn uwa_core::Transport> = Arc::new(StubTransport);

    let state = AppState {
        config: config.clone(),
        providers: Arc::new(providers),
        transport,
    };

    let addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.port)
        .parse()
        .context("bad bind/port")?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "uwa listening");

    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown requested");
}

// --- Stub transport (removed in Фаза 2) ---

struct StubTransport;

#[async_trait::async_trait]
impl uwa_core::Transport for StubTransport {
    async fn page(&self, _tab: &uwa_core::TabId) -> uwa_core::Result<Box<dyn uwa_core::Page>> {
        Err(uwa_core::UwaError::Unavailable(
            "browser transport not wired yet".into(),
        ))
    }
    async fn list_tabs(&self) -> uwa_core::Result<Vec<uwa_core::TabId>> {
        Ok(vec![])
    }
    async fn health(&self, _tab: &uwa_core::TabId) -> uwa_core::Result<()> {
        Ok(())
    }
}
