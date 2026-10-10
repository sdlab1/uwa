//! Registry: TOML config -> `name -> provider`, with the validation that
//! catches typos before a browser is ever opened.

use uwa_config::Config;
use uwa_providers::build_providers;

const HEAD: &str = r#"
    [server]
    bind = "127.0.0.1"
    port = 8080
"#;

const DEMO: &str = r##"
    [providers.demo]
    name = "demo"
    url_patterns = ["https://demo.test/*"]
    capabilities = { streams = true, tool_calls = false, vision = false }
    [providers.demo.selectors]
    input = "#prompt"
    send_button = "#send"
    assistant_message = ".assistant"
"##;

fn load(providers: &str) -> Config {
    Config::load_from_str(&format!("{HEAD}{providers}")).expect("valid config")
}

/// Structural mistakes must be caught at *load* time (see
/// `Config::validate`), long before a browser is ever opened.
fn load_err(providers: &str) -> String {
    Config::load_from_str(&format!("{HEAD}{providers}"))
        .expect_err("config must be rejected")
        .to_string()
}

#[test]
fn validates_provider_tables() {
    // Happy path: two providers, each reachable through its URL pattern.
    let cfg = load(&format!(
        "{DEMO}
        [providers.other]
        name = \"other\"
        url_patterns = [\"https://other.test/*\"]
        capabilities = {{ streams = false, tool_calls = false, vision = false }}
        [providers.other.selectors]
        input = \"#prompt\"
        send_button = \"#send\"
        assistant_message = \".assistant\"
        "
    ));
    let providers = build_providers(&cfg).expect("both providers build");
    assert_eq!(providers.len(), 2);
    let demo = &providers["demo"];
    assert_eq!(demo.name(), "demo");
    assert!(demo.matches(&"https://demo.test/chat".parse().unwrap()));
    assert!(!demo.matches(&"https://other.test/chat".parse().unwrap()));

    // A key that disagrees with `name` is a typo, not a second provider.
    let err = load_err(&DEMO.replace("name = \"demo\"", "name = \"deco\""));
    assert!(err.contains("deco"), "{err}");

    // The generic flow needs input + send + assistant selectors.
    let err = load_err(&DEMO.replace("send_button = \"#send\"\n", ""));
    assert!(err.contains("send_button"), "{err}");
}

#[test]
fn example_config_builds_every_provider() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../uwa-bin/config.example.toml");
    let cfg = Config::load_from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let providers = build_providers(&cfg).expect("the shipped example is valid");
    // 10+ providers in config.example.toml
    assert!(
        providers.len() >= 10,
        "expected 10+, got {}",
        providers.len()
    );
    for name in [
        "chatgpt",
        "claude",
        "gemini",
        "deepseek",
        "kimi",
        "qwen",
        "grok",
        "doubao",
        "ai-studio",
        "arena",
    ] {
        assert!(providers.contains_key(name), "missing provider `{name}`");
    }
}
