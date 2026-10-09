//! Preset System: multiple configurations per provider.
//!
//! A provider can carry multiple named presets (e.g. `flash`, `pro`,
//! `ultra`), each with its own selectors / net rules / capabilities.
//! The effective config is: `top-level defaults` <- overlaid by
//! `selected preset`.
//!
//! Routing:
//! * `/url/{domain}/v1/...`              -> `default_preset`
//! * `/url/{domain}/{preset}/v1/...`     -> that preset
//! * `X-UWA-Preset: pro`                 -> header override

use serde::{Deserialize, Serialize};

use crate::{ProviderCfg, Selectors};
use uwa_core::{Capabilities, ExtractionStrategy, FinisherTuning, NetRules, UwaError};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetCfg {
    #[serde(default, skip_serializing_if = "is_default_selectors")]
    pub selectors: Selectors,
    #[serde(default, skip_serializing_if = "is_default_extraction")]
    pub extraction: ExtractionStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<NetRules>,
    #[serde(default, skip_serializing_if = "is_default_finisher")]
    pub finisher: FinisherTuning,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    /// Free-form label, shown in `/admin/providers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

fn is_default_selectors(s: &Selectors) -> bool {
    s.input.is_none()
        && s.send_button.is_none()
        && s.stop_button.is_none()
        && s.assistant_message.is_none()
        && s.conversation_root.is_none()
}

fn is_default_extraction(e: &ExtractionStrategy) -> bool {
    matches!(e, ExtractionStrategy::NetworkFirst)
}

fn is_default_finisher(f: &FinisherTuning) -> bool {
    f.dom_stable_ms == 700 && f.poll_ms == 150 && f.min_wait_ms == 500 && f.max_wait_ms == 120_000
}

/// A `ProviderCfg` resolved against a specific preset.
///
/// This is a **view** -- cheap to construct, borrowed from the source.
#[derive(Debug, Clone)]
pub struct EffectiveProviderCfg {
    pub name: String,
    pub preset_name: Option<String>,
    pub url_patterns: Vec<String>,
    pub capabilities: Capabilities,
    pub selectors: Selectors,
    pub extraction: ExtractionStrategy,
    pub net: Option<NetRules>,
    pub finisher: FinisherTuning,
    pub selectors_version: Option<String>,
    pub backend: Option<crate::BackendKind>,
    pub file_paste: crate::FilePasteCfg,
    pub prompt_padding: crate::PromptPaddingCfg,
    pub stealth: bool,
}

/// Resolve the effective configuration for a preset.
///
/// * If `preset` is `None` -- uses `default_preset` (or top-level fields
///   if no presets are defined).
/// * If `preset` is `Some("x")` and "x" exists -- overrides.
/// * If `preset` is `Some("x")` and "x" does not exist -- `Err`.
pub fn effective_provider(
    provider: &ProviderCfg,
    preset: Option<&str>,
) -> Result<EffectiveProviderCfg, UwaError> {
    let preset_name = match preset {
        Some(p) => Some(p.to_string()),
        None => provider.default_preset.clone(),
    };

    let preset_data = match &preset_name {
        Some(p) => provider.presets.get(p).ok_or_else(|| {
            UwaError::BadRequest(format!(
                "provider `{}` has no preset `{p}`; available: {:?}",
                provider.name,
                provider.presets.keys().collect::<Vec<_>>()
            ))
        })?,
        None => {
            // No preset requested and no default -> return top-level as-is.
            return Ok(EffectiveProviderCfg {
                name: provider.name.clone(),
                preset_name: None,
                url_patterns: provider.url_patterns.clone(),
                capabilities: provider.capabilities.clone(),
                selectors: provider.selectors.clone(),
                extraction: provider.extraction,
                net: provider.net.clone(),
                finisher: provider.finisher.clone(),
                selectors_version: provider.selectors_version.clone(),
                backend: provider.backend,
                file_paste: provider.file_paste.clone(),
                prompt_padding: provider.prompt_padding.clone(),
                stealth: provider.stealth,
            });
        }
    };

    // Overlay preset on top of top-level.
    Ok(EffectiveProviderCfg {
        name: provider.name.clone(),
        preset_name,
        url_patterns: provider.url_patterns.clone(),
        capabilities: preset_data
            .capabilities
            .clone()
            .unwrap_or_else(|| provider.capabilities.clone()),
        selectors: merge_selectors(&provider.selectors, &preset_data.selectors),
        extraction: if matches!(preset_data.extraction, ExtractionStrategy::NetworkFirst)
            && !matches!(provider.extraction, ExtractionStrategy::DomOnly)
        {
            provider.extraction
        } else {
            preset_data.extraction
        },
        net: preset_data.net.clone().or_else(|| provider.net.clone()),
        finisher: if is_default_finisher(&preset_data.finisher) {
            provider.finisher.clone()
        } else {
            preset_data.finisher.clone()
        },
        selectors_version: provider.selectors_version.clone(),
        backend: provider.backend,
        file_paste: provider.file_paste.clone(),
        prompt_padding: provider.prompt_padding.clone(),
        stealth: provider.stealth,
    })
}

fn merge_selectors(top: &Selectors, preset: &Selectors) -> Selectors {
    Selectors {
        input: preset.input.clone().or_else(|| top.input.clone()),
        send_button: preset
            .send_button
            .clone()
            .or_else(|| top.send_button.clone()),
        stop_button: preset
            .stop_button
            .clone()
            .or_else(|| top.send_button.clone()),
        assistant_message: preset
            .assistant_message
            .clone()
            .or_else(|| top.assistant_message.clone()),
        conversation_root: preset
            .conversation_root
            .clone()
            .or_else(|| top.conversation_root.clone()),
    }
}

#[cfg(test)]
mod tests {
    use crate::Config;

    // Helper function to create a string starting with #
    fn h(s: &str) -> String {
        format!("#{}", s)
    }

    #[test]
    fn no_presets_no_default_returns_top_level() {
        let cfg = Config::load_from_str(
            r####"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [providers.x]
            name = "x"
            url_patterns = ["https://x/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.x.selectors]
            input = "#i"
            send_button = "#s"
            assistant_message = "#a"
            "####,
        )
        .unwrap();
        let p = &cfg.providers["x"];
        let eff = p.effective(None).unwrap();
        assert_eq!(eff.selectors.input.as_deref(), Some(h("i").as_str()));
        assert!(eff.preset_name.is_none());
    }

    #[test]
    fn preset_overrides_selectors() {
        let cfg = Config::load_from_str(
            r####"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [providers.x]
            name = "x"
            url_patterns = ["https://x/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            default_preset = "pro"
            [providers.x.selectors]
            input = "#top"
            send_button = "#s"
            assistant_message = "#a"
            [providers.x.presets.pro.selectors]
            input = "#pro"
            "####,
        )
        .unwrap();
        let p = &cfg.providers["x"];
        let eff = p.effective(None).unwrap();
        assert_eq!(eff.selectors.input.as_deref(), Some("#pro"));
        // Non-overridden fields come from top-level.
        assert_eq!(eff.selectors.send_button.as_deref(), Some("#s"));
        assert_eq!(eff.preset_name.as_deref(), Some("pro"));
    }

    #[test]
    fn unknown_preset_errors() {
        let cfg = Config::load_from_str(
            r####"
            [server]
            bind = "127.0.0.1"
            port = 8080
            [providers.x]
            name = "x"
            url_patterns = ["https://x/*"]
            capabilities = { streams = true, tool_calls = false, vision = false }
            [providers.x.presets.pro.selectors]
            input = "#i"
            "####,
        )
        .unwrap();
        let p = &cfg.providers["x"];
        assert!(p.effective(Some("bogus")).is_err());
        assert!(p.effective(Some("pro")).is_ok());
    }
}
