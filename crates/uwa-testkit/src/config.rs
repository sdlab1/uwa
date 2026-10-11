//! Canonical configs for tests.

use std::sync::Arc;
use uwa_config::Config;

/// API key used by [`config_with_key`].
pub const TEST_API_KEY: &str = "k";
/// `Authorization` header value for [`config_with_key`].
pub const TEST_AUTH_HEADER: &str = "Bearer k";

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

/// Same as [`default_config`] but requires `Authorization: Bearer k`.
/// Same as [`config_with_key`] but bound off-loopback, so the key-gate
/// middleware actually enforces the key (loopback binds accept any key).
pub fn config_with_key_lan() -> Arc<Config> {
    let cfg = config_with_key();
    let mut inner = Arc::try_unwrap(cfg).unwrap_or_else(|_| unreachable!("fresh Arc"));
    inner.server.bind = "192.168.1.5".to_string();
    Arc::new(inner)
}

pub fn config_with_key() -> Arc<Config> {
    Arc::new(
        Config::load_from_str(
            r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

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
        .expect("keyed test config must parse"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_with_the_chatgpt_provider() {
        let cfg = default_config();
        assert!(cfg.providers.contains_key("chatgpt"));
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.server.api_key.is_none());
        assert_eq!(
            cfg.model_aliases.get("gpt-4o").map(String::as_str),
            Some("chatgpt")
        );
    }

    #[test]
    fn keyed_config_requires_the_bearer_header() {
        let cfg = config_with_key();
        assert_eq!(cfg.server.api_key.as_deref(), Some(TEST_API_KEY));
        assert_eq!(TEST_AUTH_HEADER, "Bearer k");
    }
}
