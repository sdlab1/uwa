#[test]
fn example_config_loads_and_builds_providers() {
    let raw = include_str!("../../../crates/uwa-bin/config.example.toml");
    let cfg = uwa_config::Config::load_from_str(raw).expect("example parses");
    let map = uwa_providers::build_providers(&cfg).expect("all providers valid");
    assert!(map.contains_key("chatgpt"));
    assert!(map.contains_key("claude"));
    assert!(map.contains_key("gemini"));
    assert!(map.contains_key("deepseek"));
}
