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
        let mut out = String::with_capacity(body.len().saturating_add(512));
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    // ---- response media extraction ----
    /// Scan the last assistant message for images, canvas snapshots,
    /// audio, and video. Returned to the client as `uwa_media`.
    #[serde(default)]
    pub extract_from_response: bool,
    /// Skip `extract_from_response` for the very first response of a
    /// session — the request has no assistant container yet, so the scan
    /// would find the site's own chrome (avatars, icons).
    #[serde(default = "default_skip_first")]
    pub skip_first_response: bool,
    /// Save inline `data:` assets to disk. When `false`, the `data:`
    /// URI is returned verbatim.
    #[serde(default = "default_save_data_uris")]
    pub save_data_uris: bool,
    /// Download `http(s):` sources to disk. Off by default — most
    /// clients render remote URLs fine, and downloading is a
    /// bandwidth cost that isn't always wanted.
    #[serde(default)]
    pub download_remote: bool,
    /// Fetch `blob:` URLs from inside the page and save the bytes.
    /// Blob URLs are unreachable from outside the tab, so when this is
    /// `false`, blobs are dropped from the response.
    #[serde(default = "default_fetch_blobs")]
    pub fetch_blobs: bool,
    /// Save `<audio>` assets. Off means audio URLs pass through
    /// untouched, no bytes written.
    #[serde(default = "default_save_audio")]
    pub save_audio: bool,
    /// Upper bound for a single asset. Assets larger than this are left
    /// as URLs, not saved.
    #[serde(default = "default_max_asset_bytes")]
    pub max_asset_bytes: u64,
    /// Cap on assets per response. The first N in DOM order win.
    #[serde(default = "default_max_assets")]
    pub max_assets: usize,
}

fn default_audio_max_wait() -> u64 {
    12
}
fn default_skip_first() -> bool {
    true
}
fn default_save_data_uris() -> bool {
    true
}
fn default_fetch_blobs() -> bool {
    true
}
fn default_save_audio() -> bool {
    true
}
fn default_max_asset_bytes() -> u64 {
    20 * 1024 * 1024
}
fn default_max_assets() -> usize {
    8
}

impl Default for MediaCfg {
    fn default() -> Self {
        Self {
            audio_capture_enabled: false,
            audio_max_wait_secs: default_audio_max_wait(),
            attach_audio_to_response: false,
            video_detection_enabled: false,
            extract_from_response: false,
            skip_first_response: default_skip_first(),
            save_data_uris: default_save_data_uris(),
            download_remote: false,
            fetch_blobs: default_fetch_blobs(),
            save_audio: default_save_audio(),
            max_asset_bytes: default_max_asset_bytes(),
            max_assets: default_max_assets(),
        }
    }
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
        assert_eq!(args.first().unwrap_or_else(|| unreachable!()), "--proxy-server=socks5://127.0.0.1:1080");
        assert_eq!(args.get(1).unwrap_or_else(|| unreachable!()), "--proxy-bypass-list=localhost,127.0.0.1");
    }

    #[test]
    fn file_paste_defaults() {
        let c = FilePasteCfg::default();
        assert!(!c.enabled);
        assert_eq!(c.threshold_bytes, 130_000);
    }

    #[test]
    fn media_response_extraction_defaults_off() {
        let c = MediaCfg::default();
        assert!(!c.extract_from_response);
        assert!(c.skip_first_response);
        assert_eq!(c.max_assets, 8);
        assert_eq!(c.max_asset_bytes, 20 * 1024 * 1024);
    }

    #[test]
    fn media_response_extraction_can_be_enabled() {
        let toml = r#"
            extract_from_response = true
            download_remote = true
            max_assets = 3
        "#;
        let c: MediaCfg = toml::from_str(toml).unwrap_or_else(|_| unreachable!());
        assert!(c.extract_from_response);
        assert!(c.download_remote);
        assert_eq!(c.max_assets, 3);
    }
}
