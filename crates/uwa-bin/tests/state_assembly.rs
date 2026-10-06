//! Wiring smoke test — everything except the real Chromium connect.

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
    let tool_router = Arc::new(uwa_mcp::ToolRouter::new());

    let state = AppState::minimal(
        cfg,
        Arc::new(registry),
        Arc::new(MockTransport::with_n_tabs(1)),
    )
    .with_sessions(sessions.clone())
    .with_semaphores(Arc::new(sem))
    .with_tool_router(tool_router);

    assert!(state.runtime.sessions.is_some());
    assert!(state.runtime.tool_router.is_some());
    assert_eq!(state.runtime.semaphores.available("chatgpt"), 2);
    // Breaker lazily created on first use.
    let b = state.breaker("chatgpt");
    assert_eq!(b.state(), uwa_resilience::CircuitState::Closed);
}
