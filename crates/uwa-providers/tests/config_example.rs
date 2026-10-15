#[test]
fn example_config_loads_and_builds_providers() -> Result<(), Box<dyn std::error::Error>> {
    let raw = include_str!("../../../crates/uwa-bin/config.example.toml");
    let cfg = uwa_config::Config::load_from_str(raw)?;
    let map = uwa_providers::build_providers(&cfg)?;
    assert!(map.contains_key("chatgpt"));
    assert!(map.contains_key("claude"));
    assert!(map.contains_key("gemini"));
    assert!(map.contains_key("deepseek"));
    Ok(())
}
