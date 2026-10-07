//! Every piece of the daemon, in start-up order:
//! config → pid file → shutdown → providers → transport → MCP clients →
//! sessions → semaphores → MCP server → HTTP → graceful drain.
//!
//! `main.rs` only parses the CLI and calls [`run`].

use anyhow::Context;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uwa_api::{router, AppState, ProviderRegistry};
use uwa_config::Config;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage};
use uwa_core::types::Role;
use uwa_core::Transport;
use uwa_mcp::{
    DispatcherFn, McpClientProvider, McpServer, StdioClient, ToolRouter, WebChatHandler,
    WebPromptHandler, WebTabsHandler,
};

/// Endpoint of a Chromium started with `--remote-debugging-port=9222`.
/// `CdpTransport::connect` resolves `http(s)` URLs through `/json/version`,
/// so a plain HTTP endpoint is the friendliest default.
const DEFAULT_CDP: &str = "http://127.0.0.1:9222";

/// Start the daemon from a config file. The CDP endpoint comes from
/// `UWA_CHROMIUM_WS` and falls back to [`DEFAULT_CDP`].
pub async fn run(config_path: PathBuf) -> anyhow::Result<()> {
    let cdp = std::env::var("UWA_CHROMIUM_WS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CDP.into());
    run_with(config_path, &cdp).await
}

/// [`run`] with an explicit CDP endpoint — tests point it at a dead port.
pub async fn run_with(config_path: PathBuf, cdp_url: &str) -> anyhow::Result<()> {
    let config = Arc::new(
        Config::load_from_path(&config_path)
            .with_context(|| format!("loading {}", config_path.display()))?,
    );

    let _pid = uwa_lifecycle::PidFile::acquire(
        config
            .server
            .pid_file
            .clone()
            .unwrap_or_else(|| "uwa.pid".into()),
    )?;

    let shutdown = uwa_lifecycle::Shutdown::new();
    {
        let s = shutdown.clone();
        tokio::spawn(async move {
            s.wait_signals().await;
        });
    }

    // Providers.
    let mut registry = ProviderRegistry::new();
    for (name, p) in uwa_providers::build_providers(&config)? {
        tracing::debug!(provider = %name, "registered");
        registry.register(p);
    }

    // Real transport — the daemon is useless without a browser.
    let transport: Arc<dyn Transport> = Arc::new(
        uwa_browser::CdpTransport::connect(
            cdp_url,
            Duration::from_secs(1800),
            Some(uwa_stealth::builtin::default_pack()),
        )
        .await
        .with_context(|| {
            format!(
                "connect Chromium at {cdp_url} — start it with \
                 --remote-debugging-port=9222 or set UWA_CHROMIUM_WS"
            )
        })?,
    );

    // MCP clients.
    let mut tool_router = ToolRouter::new();
    for client_cfg in &config.mcp_clients {
        let args: Vec<String> = client_cfg.args.clone();
        match StdioClient::spawn(&client_cfg.name, &client_cfg.command, &args).await {
            Ok(client) => {
                let client = Arc::new(client);
                if let Err(e) = client.initialize().await {
                    tracing::warn!(server = %client_cfg.name, "MCP initialize failed: {e}");
                } else {
                    tracing::info!(server = %client_cfg.name, "MCP client registered");
                    tool_router.register(Arc::new(McpClientProvider::new(client)));
                }
            }
            Err(e) => tracing::warn!(server = %client_cfg.name, "MCP spawn failed: {e}"),
        }
    }
    let tool_router = Arc::new(tool_router);

    // Sessions + sweeper.
    let sessions = Arc::new(uwa_session::SessionManager::new(
        uwa_session::SessionCfg::default(),
    ));
    {
        let sm = sessions.clone();
        let stop = shutdown.clone();
        tokio::spawn(async move {
            let interval = sm.cfg().sweep_interval;
            loop {
                tokio::select! {
                    _ = stop.wait() => break,
                    _ = tokio::time::sleep(interval) => {
                        let n = sm.evict_idle().await.len();
                        if n > 0 {
                            tracing::info!(n, "evicted idle sessions");
                        }
                        uwa_api::metrics::active_sessions(sm.len());
                    }
                }
            }
        });
    }

    // Semaphores: two slots per configured provider, four by default.
    let mut sem = uwa_resilience::semaphore::ProviderSemaphores::new(4);
    for name in config.providers.keys() {
        sem = sem.with_limit(name, 2);
    }

    let state = AppState::minimal(config.clone(), Arc::new(registry), transport.clone())
        .with_sessions(sessions.clone())
        .with_semaphores(Arc::new(sem))
        .with_tool_router(tool_router);

    // MCP server (stdio)
    let mut mcp_handle = None;
    if config.mcp_server.enabled {
        let mcp = build_mcp_server(&state);

        let mcp_stdio = mcp;
        mcp_handle = Some(tokio::spawn(async move {
            if let Err(e) = mcp_stdio.serve_stdio().await {
                tracing::error!("MCP stdio: {e}");
            }
        }));
        tracing::info!("MCP server enabled on stdio");
    }

    // HTTP API.
    let addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "uwa listening");
    let app = router(state);
    let s = shutdown.clone();
    let http = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { s.wait().await })
            .await
            .map_err(anyhow::Error::from)
    });

    shutdown.wait().await;
    tracing::info!("draining");
    match tokio::time::timeout(Duration::from_secs(15), http).await {
        Ok(Ok(Ok(()))) => tracing::info!("http drained"),
        Ok(Ok(Err(e))) => tracing::warn!("http server: {e}"),
        Ok(Err(_)) | Err(_) => tracing::warn!("http did not drain within 15s"),
    }
    if let Some(handle) = mcp_handle {
        handle.abort();
    }

    // Give in-flight conversations a moment before the tabs go away.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !sessions.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tracing::info!("bye");
    Ok(())
}

/// The MCP server the daemon exposes: chat via the pipeline, tabs as a tool
/// and a resource, and the `ask` prompt.
fn build_mcp_server(state: &AppState) -> Arc<McpServer> {
    let providers_list: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
        let cfg = state.config.clone();
        Arc::new(move || {
            let mut names: Vec<String> = cfg.providers.keys().cloned().collect();
            names.sort();
            names
        })
    };
    Arc::new(
        McpServer::new("uwa")
            .register(Arc::new(WebChatHandler::new(chat_dispatcher(
                state.clone(),
            ))))
            .register(Arc::new(WebTabsHandler::new(state.transport.clone())))
            .register(Arc::new(WebPromptHandler::new(providers_list))),
    )
}

/// `web__chat(provider, message)` — an MCP client asks the pipeline for one
/// answer, with no local or remote tools in play.
fn chat_dispatcher(state: AppState) -> DispatcherFn {
    Arc::new(move |provider, message| {
        let st = state.clone();
        Box::pin(async move {
            let req = ChatCompletionRequest {
                model: provider,
                messages: vec![ChatMessage::text(Role::User, message)],
                stream: Some(false),
                temperature: None,
                max_tokens: None,
                tools: None,
                tool_choice: Some(serde_json::json!("none")),
                user: None,
            };
            let (text, _, _) = uwa_api::routes::chat::run_pipeline(&st, &req, &[], None).await?;
            Ok(text)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use uwa_mcp::JsonRpcRequest;
    use uwa_testkit::MockTransport;

    fn state_without_browser() -> AppState {
        let config: Arc<Config> = Arc::new(
            Config::load_from_str(
                r#"
            [server]
            bind = "127.0.0.1"
            port = 59999
            "#,
            )
            .unwrap(),
        );
        AppState::minimal(
            config,
            Arc::new(ProviderRegistry::new()),
            Arc::new(MockTransport::new()),
        )
    }

    async fn call(server: &McpServer, method: &str) -> Value {
        let resp = server
            .dispatch_public(JsonRpcRequest::new(1, method, Some(json!({}))))
            .await;
        match (resp.result, resp.error) {
            (Some(v), _) => v,
            (None, Some(e)) => panic!("{method} failed: {e:?}"),
            (None, None) => panic!("{method} returned nothing"),
        }
    }

    #[tokio::test]
    async fn build_mcp_server_serves_tools_resources_and_prompts() {
        let mcp = build_mcp_server(&state_without_browser());

        let tools = call(&mcp, "tools/list").await;
        let names: Vec<&str> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"web__chat"), "{names:?}");
        assert!(names.contains(&"web__list_tabs"), "{names:?}");

        let resources = call(&mcp, "resources/list").await;
        assert_eq!(resources["resources"][0]["uri"], "uwa://web/tabs");

        let prompts = call(&mcp, "prompts/list").await;
        assert_eq!(prompts["prompts"][0]["name"], "ask");

        let init = call(&mcp, "initialize").await;
        assert!(init["capabilities"]["tools"].is_object());
        assert!(init["capabilities"]["resources"].is_object());
        assert!(init["capabilities"]["prompts"].is_object());
    }

    #[tokio::test]
    async fn run_fails_fast_without_a_browser_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("uwa-wiring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_path = dir.join("uwa.pid");
        let config_path = dir.join("uwa.toml");
        std::fs::write(
            &config_path,
            format!(
                "[server]\nbind = \"127.0.0.1\"\nport = 59999\npid_file = \"{}\"\n",
                pid_path.display()
            ),
        )
        .unwrap();

        // Port 9 is closed on any sane machine, so the connect fails instead
        // of hanging: the daemon must refuse to start, not stall.
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            run_with(config_path, "http://127.0.0.1:9"),
        )
        .await
        .expect("run_with must not hang");

        let err = outcome.expect_err("no browser => no daemon");
        let msg = format!("{err:#}");
        assert!(msg.contains("connect Chromium"), "{msg}");

        assert!(!pid_path.exists(), "pid file must be removed on exit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
