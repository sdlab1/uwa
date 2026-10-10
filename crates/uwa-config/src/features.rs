//! Feature configs: file paste, prompt padding, proxy.

use serde::{Deserialize, Serialize};

// ---------- File Paste ----------

/// When the prompt exceeds `threshold_bytes`, write it to a temp file and
/// attach it instead of typing it inline. Works around sites that choke on
/// very long inline prompts (>100KB).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePasteCfg {
    #[serde(default)]
    pub enabled: bool,
    /// Byte size above which we switch to file paste.
    #[serde(default = "default_threshold")]
    pub threshold_bytes: usize,
    #[serde(default = "default_hint")]
    pub hint_text: String,
    /// Re-acquire the input selector after upload — some sites reset the DOM.
    #[serde(default)]
    pub reacquire_input_after_upload: bool,
}

fn default_threshold() -> usize {
    130_000
}
fn default_hint() -> String {
    "The attached file contains context for your reply. Answer based on the file.".into()
}

impl Default for FilePasteCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_bytes: default_threshold(),
            hint_text: default_hint(),
            reacquire_input_after_upload: false,
        }
    }
}

// ---------- Prompt Padding ----------

/// Insert filler segments around the real prompt. Only useful for evading
/// naive rate-limiters that count tokens/characters.
///
/// Default off — it degrades LLM quality (adds noise).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPaddingCfg {
    #[serde(default)]
    pub enabled: bool,
    /// Filler marker repeated `segments_per_side` times before and after
    /// the real prompt.
    #[serde(default = "default_marker")]
    pub marker_text: String,
    #[serde(default = "default_segments")]
    pub segments_per_side: usize,
    /// Randomize segment length by appending 1–N random chars.
    #[serde(default)]
    pub randomize: bool,
    #[serde(default = "default_random_chars")]
    pub random_chars: String,
}

fn default_marker() -> String {
    "───".into()
}
fn default_segments() -> usize {
    6
}
fn default_random_chars() -> String {
    "·".into()
}

impl Default for PromptPaddingCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            marker_text: default_marker(),
            segments_per_side: default_segments(),
            randomize: false,
            random_chars: default_random_chars(),
        }
    }
}

impl PromptPaddingCfg {
    /// Wrap `body` with padding. Returns `body` unchanged if disabled.
    pub fn apply(&self, body: &str) -> String {
        if !self.enabled || self.segments_per_side == 0 {
            return body.to_string();
        }
        let mut out = String::with_capacity(body.len() + 512);
        for _ in 0..self.segments_per_side {
            out.push_str(&self.marker_text);
            if self.randomize {
                out.push_str(&self.random_chars);
            }
            out.push('\n');
        }
        out.push_str(body);
        out.push('\n');
        for _ in 0..self.segments_per_side {
            out.push_str(&self.marker_text);
            if self.randomize {
                out.push_str(&self.random_chars);
            }
            out.push('\n');
        }
        out
    }
}

// ---------- Media ----------

/// Audio capture + video detection knobs. Everything off by default —
/// capture costs a Web Audio hook and a Chrome autoplay flag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaCfg {
    /// Hook `HTMLMediaElement.prototype.play` and record whatever plays
    /// during the response into a webm temp file.
    #[serde(default)]
    pub audio_capture_enabled: bool,
    /// Max seconds to keep recording after the last chunk arrives.
    #[serde(default = "default_audio_max_wait")]
    pub audio_max_wait_secs: u64,
    /// Auto-attach captured audio to the response (MVP: writes the file and
    /// logs the path; full multimodal-response DTO is a follow-up).
    #[serde(default)]
    pub attach_audio_to_response: bool,
    /// Detect `<video>` elements in the DOM after each response. Never
    /// auto-downloads.
    #[serde(default)]
    pub video_detection_enabled: bool,
}

fn default_audio_max_wait() -> u64 {
    12
}

// ---------- Proxy ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyCfg {
    #[serde(default)]
    pub enabled: bool,
    /// e.g. `socks5://127.0.0.1:1080` or `http://proxy.example.com:8080`.
    #[serde(default)]
    pub address: String,
    /// Comma-separated list of hosts to bypass.
    #[serde(default)]
    pub bypass: String,
}

impl ProxyCfg {
    /// Returns Chrome launch flags for this proxy config, if enabled.
    pub fn chrome_args(&self) -> Vec<String> {
        if !self.enabled || self.address.is_empty() {
            return Vec::new();
        }
        let mut args = vec![format!("--proxy-server={}", self.address)];
        if !self.bypass.is_empty() {
            args.push(format!("--proxy-bypass-list={}", self.bypass));
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_disabled_noop() {
        let c = PromptPaddingCfg::default();
        assert_eq!(c.apply("hi"), "hi");
    }

    #[test]
    fn padding_wraps() {
        let c = PromptPaddingCfg {
            enabled: true,
            marker_text: "X".into(),
            segments_per_side: 2,
            randomize: false,
            random_chars: String::new(),
        };
        let out = c.apply("real");
        assert!(out.starts_with("X\nX\n"));
        assert!(out.ends_with("X\nX\n"));
        assert!(out.contains("real"));
    }

    #[test]
    fn proxy_disabled_no_args() {
        let c = ProxyCfg::default();
        assert!(c.chrome_args().is_empty());
    }

    #[test]
    fn proxy_enabled_with_bypass() {
        let c = ProxyCfg {
            enabled: true,
            address: "socks5://127.0.0.1:1080".into(),
            bypass: "localhost,127.0.0.1".into(),
        };
        let args = c.chrome_args();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--proxy-server=socks5://127.0.0.1:1080");
        assert_eq!(args[1], "--proxy-bypass-list=localhost,127.0.0.1");
    }

    #[test]
    fn file_paste_defaults() {
        let c = FilePasteCfg::default();
        assert!(!c.enabled);
        assert_eq!(c.threshold_bytes, 130_000);
    }
}
