//! Test-server helpers: one canned [`Config`] and an `axum_test` server
//! over [`uwa_api::router`].

use std::sync::Arc;
use uwa_config::Config;

/// Wrap an [`AppState`](uwa_api::AppState) in an [`axum_test::TestServer`].
///
/// Panics if the router itself cannot be built — that is a wiring bug, not
/// a test outcome.
pub fn test_server(state: uwa_api::AppState) -> axum_test::TestServer {
    axum_test::TestServer::new(uwa_api::router(state)).expect("router builds")
}

/// Config with a single `chatgpt` provider and no API key.
pub fn default_config() -> Arc<Config> {
    Arc::new(
        Config::load_from_str(
            r##"
            [server]
            bind = "127.0.0.1"
            port = 8080

            [model_aliases]
            "gpt-4o" = "chatgpt"

            [providers.chatgpt]
            name = "chatgpt"
            url_patterns = ["https://chatgpt.com/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            [providers.chatgpt.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"
            "##,
        )
        .expect("default test config must parse"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockProvider;
    use crate::transport::MockTransport;
    use uwa_api::{AppState, ProviderRegistry};

    fn default_state() -> AppState {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(MockProvider::new("chatgpt").with_answer("hi")));
        AppState::minimal(
            default_config(),
            Arc::new(registry),
            Arc::new(MockTransport::with_n_tabs(1)),
        )
    }

    #[test]
    fn default_config_parses_with_the_chatgpt_provider() {
        let cfg = default_config();
        assert!(cfg.providers.contains_key("chatgpt"));
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.server.api_key.is_none());
    }

    #[tokio::test]
    async fn test_server_answers_health_and_models() {
        let server = test_server(default_state());
        server.get("/healthz").await.assert_status_ok();
        let resp = server.get("/v1/models").await;
        resp.assert_status_ok();
        let models: serde_json::Value = resp.json();
        assert_eq!(models["data"][0]["id"].as_str(), Some("gpt-4o"));
        assert_eq!(models["data"][0]["owned_by"].as_str(), Some("chatgpt"));
    }
}
