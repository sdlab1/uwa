//! Config validation: the single gate every loaded TOML must pass.
//!
//! These are the *negative* cases — each declares one structural mistake
//! and asserts that `Config::load_from_str` refuses it with a message a
//! human can act on.

use uwa_config::Config;

fn load(toml: &str) -> Result<Config, String> {
    Config::load_from_str(toml).map_err(|e| e.to_string())
}

const BASE: &str = r#"
    [server]
    bind = "127.0.0.1"
    port = 8080
"#;

#[test]
fn loopback_no_key_ok() {
    assert!(load(BASE).is_ok());
}

#[test]
fn non_loopback_requires_key() {
    let toml = r#"
        [server]
        bind = "0.0.0.0"
        port = 8080
    "#;
    let e = load(toml).unwrap_err();
    assert!(e.contains("api_key"), "got: {e}");
}

#[test]
fn non_loopback_with_key_ok() {
    let toml = r#"
        [server]
        bind = "0.0.0.0"
        port = 8080
        api_key = "x"
    "#;
    assert!(load(toml).is_ok());
}

#[test]
fn dangling_model_alias_errors() {
    let toml = format!(
        r#"{BASE}
        [model_aliases]
        "gpt-4o" = "nonexistent"
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("nonexistent"), "got: {e}");
}

#[test]
fn provider_key_mismatch_errors() {
    let toml = format!(
        r##"{BASE}
        [providers.wrong]
        name = "chatgpt"
        url_patterns = ["https://x/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        [providers.wrong.selectors]
        input = "#i"
        send_button = "#s"
        assistant_message = "#a"
        "##
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("wrong") && e.contains("chatgpt"), "got: {e}");
}

#[test]
fn missing_selectors_error() {
    let toml = format!(
        r##"{BASE}
        [providers.x]
        name = "x"
        url_patterns = ["https://x/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        [providers.x.selectors]
        send_button = "#s"
        assistant_message = "#a"
        "##
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("input"), "got: {e}");
}

#[test]
fn empty_url_patterns_error() {
    let toml = format!(
        r##"{BASE}
        [providers.x]
        name = "x"
        url_patterns = []
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        [providers.x.selectors]
        input = "#i"
        send_button = "#s"
        assistant_message = "#a"
        "##
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("url_patterns"), "got: {e}");
}

#[test]
fn default_preset_must_exist() {
    let toml = format!(
        r##"{BASE}
        [providers.x]
        name = "x"
        url_patterns = ["https://x/*"]
        capabilities = {{ streams = true, tool_calls = false, vision = false }}
        default_preset = "ghost"
        [providers.x.selectors]
        input = "#i"
        send_button = "#s"
        assistant_message = "#a"
        "##
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("ghost"), "got: {e}");
}

#[test]
fn group_references_missing_provider() {
    let toml = format!(
        r#"{BASE}
        [groups.g]
        strategy = "round_robin"
        members = [{{ provider = "missing" }}]
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("missing"), "got: {e}");
}

#[test]
fn scheduled_restart_at_must_parse() {
    let toml = format!(
        r#"{BASE}
        [scheduled_restart]
        enabled = true
        at = "25:99"
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("25:99"), "got: {e}");
}

#[test]
fn scheduled_restart_valid_at_ok() {
    let toml = format!(
        r#"{BASE}
        [scheduled_restart]
        enabled = true
        at = "04:30"
        "#
    );
    assert!(load(&toml).is_ok());
}

#[test]
fn proxy_enabled_requires_address() {
    let toml = format!(
        r#"{BASE}
        [proxy]
        enabled = true
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("address"), "got: {e}");
}

#[test]
fn proxy_enabled_with_parseable_address_ok() {
    let toml = format!(
        r#"{BASE}
        [proxy]
        enabled = true
        address = "socks5://127.0.0.1:1080"
        "#
    );
    assert!(load(&toml).is_ok());
}

#[test]
fn cdp_backend_ws_url_must_parse() {
    let toml = format!(
        r#"{BASE}
        [backend]
        kind = "cdp"
        [backend.cdp]
        ws_url = "not a url"
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("ws_url"), "got: {e}");
}

/// CI lesson #3, now enforced at config time: a bare
/// `ws://host/devtools/browser` (no browser GUID) is a 404 at connect.
#[test]
fn cdp_backend_bare_browser_ws_url_is_rejected() {
    let toml = format!(
        r#"{BASE}
        [backend]
        kind = "cdp"
        [backend.cdp]
        ws_url = "ws://127.0.0.1:9222/devtools/browser"
        "#
    );
    let e = load(&toml).unwrap_err();
    assert!(e.contains("GUID"), "got: {e}");
}

/// …while the resolvable `http://` form passes validation.
#[test]
fn cdp_backend_http_url_ok() {
    let toml = format!(
        r#"{BASE}
        [backend]
        kind = "cdp"
        [backend.cdp]
        ws_url = "http://127.0.0.1:9222"
        "#
    );
    assert!(load(&toml).is_ok());
}
