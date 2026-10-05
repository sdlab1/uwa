//! Pure network/extraction config shared by uwa-config, uwa-extract, uwa-providers.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionStrategy {
    #[default]
    NetworkFirst,
    DomOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetRules {
    pub url_contains: Vec<String>,
    pub mime_contains: Vec<String>,
    pub decoder: NetDecoder,
    #[serde(default = "default_idle")]
    pub idle_timeout_ms: u64,
}

fn default_idle() -> u64 {
    30_000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetDecoder {
    Sse { json_path: String },
    Json { json_path: String },
}

impl NetRules {
    pub fn matches(&self, url: &str, mime: &str) -> bool {
        self.url_contains.iter().all(|p| url.contains(p.as_str()))
            && self.mime_contains.iter().any(|p| mime.contains(p.as_str()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FinisherTuning {
    #[serde(default = "d_stable")]
    pub dom_stable_ms: u64,
    #[serde(default = "d_poll")]
    pub poll_ms: u64,
    #[serde(default = "d_min")]
    pub min_wait_ms: u64,
    #[serde(default = "d_max")]
    pub max_wait_ms: u64,
}

fn d_stable() -> u64 {
    700
}
fn d_poll() -> u64 {
    150
}
fn d_min() -> u64 {
    500
}
fn d_max() -> u64 {
    120_000
}

impl Default for FinisherTuning {
    fn default() -> Self {
        Self {
            dom_stable_ms: 700,
            poll_ms: 150,
            min_wait_ms: 500,
            max_wait_ms: 120_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> NetRules {
        NetRules {
            url_contains: vec!["/backend-api/conversation".into()],
            mime_contains: vec!["text/event-stream".into()],
            decoder: NetDecoder::Sse {
                json_path: "__raw__".into(),
            },
            idle_timeout_ms: 1_000,
        }
    }

    #[test]
    fn matches_requires_all_url_patterns_and_any_mime() {
        let r = rules();
        assert!(r.matches(
            "https://chatgpt.com/backend-api/conversation/abc",
            "text/event-stream"
        ));
        assert!(!r.matches(
            "https://chatgpt.com/backend-api/conversation/abc",
            "application/json"
        ));
        assert!(!r.matches("https://chatgpt.com/", "text/event-stream"));
    }
}
