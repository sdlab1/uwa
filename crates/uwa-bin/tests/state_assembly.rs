//! Wiring smoke test — everything except the real Chromium connect.
//!
//! No MCP: UWA is a bridge; tool execution lives in the client.

use std::sync::Arc;
use uwa_api::{AppState, ProviderRegistry};
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_testkit::{MockProvider, MockTransport};

#[tokio::test]
async fn appstate_assembles_with_all_pieces() {
    let cfg = uwa_testkit::config::default_config();
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(MockProvider::new("chatgpt").with_answer("hi")));

    let mut sem = ProviderSemaphores::new(4);
    sem = sem.with_limit("chatgpt", 2);

    let sessions = Arc::new(uwa_session::SessionManager::new(Default::default()));
    let history = Arc::new(
        uwa_history::HistoryStore::new(uwa_history::HistoryCfg::default(), false)
            .await
            .unwrap(),
    );

    let state = AppState::minimal(
        cfg,
        Arc::new(registry),
        Arc::new(MockTransport::with_n_tabs(1)),
    )
    .with_sessions(sessions.clone())
    .with_semaphores(Arc::new(sem))
    .with_history(history);

    assert!(state.runtime.sessions.is_some());
    assert!(state.runtime.history.is_some());
    assert_eq!(state.runtime.semaphores.available("chatgpt"), 2);
    // Breaker lazily created on first use.
    let b = state.breaker("chatgpt");
    assert_eq!(b.state(), uwa_resilience::CircuitState::Closed);
}
