//! `uwa` — the daemon. Wires config + providers + transport into `uwa-api`.

use anyhow::Context;
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use uwa_api::{router, AppState, ProviderRegistry};
use uwa_config::Config;
use uwa_mcp::{
    McpClientProvider, McpServer, ProviderLookupFn, StdioClient, ToolRouter, WebChatHandler,
    WebTabsHandler,
};

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
    let providers: Arc<ProviderRegistry> = Arc::new(ProviderRegistry::new());
    let transport: Arc<dyn uwa_core::Transport> = Arc::new(StubTransport);

    // --- MCP integration ---
    // 1. Connect external MCP clients (stdio subprocesses) and register them
    //    in the tool router before wrapping it in an Arc.
    let mut tool_router = ToolRouter::new();
    for mcp_cfg in &config.mcp_clients {
        let args: Vec<String> = mcp_cfg.args.clone();
        match StdioClient::spawn(&mcp_cfg.name, &mcp_cfg.command, &args).await {
            Ok(c) => {
                let c_arc = Arc::new(c);
                if let Err(e) = c_arc.initialize().await {
                    tracing::warn!(server = %mcp_cfg.name, "MCP initialize failed: {e}");
                } else {
                    tracing::info!(server = %mcp_cfg.name, "MCP client registered");
                    tool_router.register(Arc::new(McpClientProvider::new(c_arc)));
                }
            }
            Err(e) => tracing::warn!(server = %mcp_cfg.name, "MCP spawn failed: {e}"),
        }
    }
    let tool_router = Arc::new(tool_router);

    // 2. Optionally expose this bridge itself as an MCP server (stdio).
    if config.mcp_server.enabled {
        let providers_arc = providers.clone();
        let transport_for_server = transport.clone();
        tokio::spawn(async move {
            let provider_lookup: ProviderLookupFn =
                Arc::new(move |name: &str| providers_arc.get(name).ok());
            let mcp = McpServer::new("uwa")
                .register(Arc::new(WebChatHandler::new(
                    provider_lookup,
                    transport_for_server.clone(),
                )))
                .register(Arc::new(WebTabsHandler::new(transport_for_server)));
            if let Err(e) = mcp.serve_stdio().await {
                tracing::error!("MCP server exited: {e}");
            }
        });
        tracing::info!("MCP server enabled on stdio");
    }

    let state = AppState::minimal(config.clone(), providers.clone(), transport)
        .with_tool_router(tool_router);

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
