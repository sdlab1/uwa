//! Every piece of the daemon, in start-up order:
//! config → pid file → shutdown → providers → transport → sessions →
//! history → semaphores → HTTP → graceful drain.
//!
//! `main.rs` only parses the CLI and calls [`run`].
//!
//! No MCP here — UWA is a bridge. MCP servers live in the client (agent).

use anyhow::Context;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uwa_api::{router, AppState, ProviderRegistry};
use uwa_config::Config;
use uwa_core::Transport;

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

    // Transport: prefer nodriver if any provider uses it, else CDP.
    let use_nodriver = config
        .providers
        .values()
        .any(|p| matches!(p.backend, Some(uwa_config::BackendKind::Nodriver)))
        || matches!(config.backend.kind, uwa_config::BackendKind::Nodriver);

    let transport: Arc<dyn Transport> = if use_nodriver {
        // Merge proxy args into nodriver config.
        let mut nd_cfg = config.backend.nodriver.clone();
        for arg in config.proxy.chrome_args() {
            if !nd_cfg.extra_args.contains(&arg) {
                nd_cfg.extra_args.push(arg);
            }
        }
        let transport = uwa_browser::NodriverTransport::spawn(&nd_cfg)
            .await
            .with_context(|| "spawn nodriver sidecar")?;
        tracing::info!(
            "using nodriver transport with proxy={:?}",
            config.proxy.enabled
        );
        Arc::new(transport)
    } else {
        let transport = uwa_browser::CdpTransport::connect(
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
        })?;

        if !config.proxy.chrome_args().is_empty() {
            tracing::warn!(
                "proxy configured but CDP backend attaches to a running Chrome — \
                 restart Chrome with the proxy args from `/admin/config` output"
            );
        }
        Arc::new(transport)
    };

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

    // History (JSONL persistence on).
    let history =
        Arc::new(uwa_history::HistoryStore::new(uwa_history::HistoryCfg::default(), true).await?);

    // Semaphores: two slots per configured provider, four by default.
    let mut sem = uwa_resilience::semaphore::ProviderSemaphores::new(4);
    for name in config.providers.keys() {
        sem = sem.with_limit(name, 2);
    }

    let state = AppState::minimal(config.clone(), Arc::new(registry), transport.clone())
        .with_sessions(sessions.clone())
        .with_semaphores(Arc::new(sem))
        .with_history(history);

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

    // Give in-flight conversations a moment before the tabs go away.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !sessions.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tracing::info!("bye");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn state_assembles_without_mcp() {
        // Bridge semantics: the state carries no MCP anything.
        let state = state_without_browser();
        assert!(state.providers.is_empty());
        assert!(state.runtime.history.is_none());
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
                "[server]\nbind = \"127.0.0.1\"\nport = 59999\npid_file = \"{}\"\n[backend]\nkind = \"cdp\"\n",
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
