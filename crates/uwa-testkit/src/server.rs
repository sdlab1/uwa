//! Fluent builder for `AppState` + an `axum_test::TestServer`.

use std::sync::Arc;
use uwa_api::{router, AppState, ProviderRegistry};
use uwa_config::Config;
use uwa_core::{SiteProvider, Transport};
use uwa_mcp::ToolRouter;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_session::SessionManager;

use crate::config::{config_with_key, default_config, TEST_AUTH_HEADER};
use crate::transport::MockTransport;

/// Wrap an [`AppState`] in an [`axum_test::TestServer`].
///
/// Panics if the router itself cannot be built — that is a wiring bug, not
/// a test outcome.
pub fn test_server(state: AppState) -> axum_test::TestServer {
    axum_test::TestServer::new(router(state)).expect("router builds")
}

/// Builds an [`AppState`] out of pieces; every `with_*` is optional.
///
/// Defaults are deliberately usable: a `chatgpt` config, one healthy tab
/// (the pipeline needs a tab to type into) and default runtime services.
pub struct AppBuilder {
    config: Arc<Config>,
    providers: Vec<Arc<dyn SiteProvider>>,
    transport: Arc<dyn Transport>,
    tool_router: Option<Arc<ToolRouter>>,
    sessions: Option<Arc<SessionManager>>,
    semaphores: Option<Arc<ProviderSemaphores>>,
    history: Option<Arc<uwa_history::HistoryStore>>,
}

impl AppBuilder {
    pub fn new() -> Self {
        Self {
            config: default_config(),
            providers: Vec::new(),
            transport: Arc::new(MockTransport::with_n_tabs(1)),
            tool_router: None,
            sessions: None,
            semaphores: None,
            history: None,
        }
    }

    /// Use [`config_with_key`]: requests must then carry `Authorization:
    /// Bearer k` (see [`TestApp::auth`]).
    pub fn with_key_auth(mut self) -> Self {
        self.config = config_with_key();
        self
    }

    pub fn with_config(mut self, c: Arc<Config>) -> Self {
        self.config = c;
        self
    }

    pub fn with_transport(mut self, t: Arc<dyn Transport>) -> Self {
        self.transport = t;
        self
    }

    pub fn with_provider(mut self, p: Arc<dyn SiteProvider>) -> Self {
        self.providers.push(p);
        self
    }

    pub fn with_tool_router(mut self, tr: Arc<ToolRouter>) -> Self {
        self.tool_router = Some(tr);
        self
    }

    pub fn with_sessions(mut self, sm: Arc<SessionManager>) -> Self {
        self.sessions = Some(sm);
        self
    }

    pub fn with_semaphores(mut self, s: Arc<ProviderSemaphores>) -> Self {
        self.semaphores = Some(s);
        self
    }

    pub fn with_history(mut self, h: Arc<uwa_history::HistoryStore>) -> Self {
        self.history = Some(h);
        self
    }

    pub fn build_state(self) -> AppState {
        let mut registry = ProviderRegistry::new();
        for p in self.providers {
            registry.register(p);
        }
        let mut state = AppState::minimal(self.config, Arc::new(registry), self.transport);
        if let Some(tr) = self.tool_router {
            state = state.with_tool_router(tr);
        }
        if let Some(sm) = self.sessions {
            state = state.with_sessions(sm);
        }
        if let Some(s) = self.semaphores {
            state = state.with_semaphores(s);
        }
        if let Some(h) = self.history {
            state = state.with_history(h);
        }
        state
    }

    pub fn build(self) -> TestApp {
        let state = self.build_state();
        let server = test_server(state.clone());
        TestApp { server, state }
    }
}

impl Default for AppBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A running app: the HTTP server plus the state it was built from.
pub struct TestApp {
    pub server: axum_test::TestServer,
    pub state: AppState,
}

impl TestApp {
    /// `("Authorization", "Bearer k")` for configs built with [`config_with_key`].
    pub fn auth(&self) -> (&'static str, &'static str) {
        ("Authorization", TEST_AUTH_HEADER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockProvider;
    use serde_json::json;

    #[tokio::test]
    async fn test_server_answers_health_and_models() {
        let app = AppBuilder::new()
            .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
            .build();
        app.server.get("/healthz").await.assert_status_ok();
        let resp = app.server.get("/v1/models").await;
        resp.assert_status_ok();
        let models: serde_json::Value = resp.json();
        assert_eq!(models["data"][0]["id"].as_str(), Some("gpt-4o"));
        assert_eq!(models["data"][0]["owned_by"].as_str(), Some("chatgpt"));
    }

    #[tokio::test]
    async fn keyed_config_rejects_anonymous_requests() {
        let app = AppBuilder::new()
            .with_key_auth()
            .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
            .build();
        let anon = app
            .server
            .post("/v1/chat/completions")
            .json(&json!({"model": "gpt-4o", "messages": []}));
        assert_eq!(anon.await.status_code(), 401);

        let (name, value) = app.auth();
        let ok = app
            .server
            .post("/v1/chat/completions")
            .add_header(name, value)
            .json(&json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "x"}]}));
        ok.await.assert_status_ok();
    }
}
