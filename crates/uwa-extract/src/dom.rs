//! DOM-side extraction: CSS selectors → text, with a stability-based finisher.

use async_trait::async_trait;
use scraper::{Html, Selector};
use std::time::{Duration, Instant};
use uwa_core::{Page, Result, UwaError};

/// A selector-driven extractor. All configuration comes from the caller.
#[derive(Debug, Clone)]
pub struct DomExtractor {
    /// Selector for the container of the *last* assistant message.
    pub assistant_message: String,
    /// Optional: selector that must be absent for the answer to be considered complete.
    pub stop_button: Option<String>,
    /// DOM stable this long ⇒ consider generation done.
    pub dom_stable_for: Duration,
    /// Hard upper bound.
    pub max_wait: Duration,
    /// How often to poll.
    pub poll_interval: Duration,
}

impl DomExtractor {
    /// Extract text from a raw HTML document. Pure, no I/O. Testable in isolation.
    pub fn extract_from_html(&self, html: &str) -> Result<String> {
        let doc = Html::parse_document(html);
        let sel = Selector::parse(&self.assistant_message).map_err(|e| {
            UwaError::Extraction(format!("bad selector `{}`: {e}", self.assistant_message))
        })?;
        let last = doc
            .select(&sel)
            .last()
            .ok_or_else(|| UwaError::Extraction("assistant message element not found".into()))?;
        Ok(last.text().collect::<Vec<_>>().join("").trim().to_string())
    }

    /// Poll the page until the answer stabilizes. Returns the extracted text.
    pub async fn wait_and_extract(&self, page: &dyn Page) -> Result<String> {
        let start = Instant::now();
        let mut last_html_hash: Option<u64> = None;
        let mut stable_since: Option<Instant> = None;

        loop {
            if start.elapsed() > self.max_wait {
                // Whatever we have, we return — but signal it via last text.
                return self.extract_from_html(&page.html().await?);
            }

            // 1. Stop button gone? Quick finish signal.
            if let Some(stop_sel) = &self.stop_button {
                let stop_present: bool = page
                    .eval(&format!(
                        "!!document.querySelector({})",
                        serde_json::to_string(stop_sel).unwrap()
                    ))
                    .await
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !stop_present {
                    return self.extract_from_html(&page.html().await?);
                }
            }

            // 2. DOM stability.
            let html = page.html().await?;
            let h = hash(&html);
            match last_html_hash {
                Some(prev) if prev == h => {
                    let since = stable_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= self.dom_stable_for {
                        return self.extract_from_html(&html);
                    }
                }
                _ => {
                    last_html_hash = Some(h);
                    stable_since = None;
                }
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Trait object wrapper for dynamic dispatch across crate boundaries.
#[async_trait]
pub trait DomLike: Send + Sync {
    async fn wait_and_extract(&self, page: &dyn Page) -> Result<String>;
}

#[async_trait]
impl DomLike for DomExtractor {
    async fn wait_and_extract(&self, page: &dyn Page) -> Result<String> {
        DomExtractor::wait_and_extract(self, page).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk() -> DomExtractor {
        DomExtractor {
            assistant_message: "[data-message-author-role=assistant]".into(),
            stop_button: None,
            dom_stable_for: Duration::from_millis(50),
            max_wait: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
        }
    }

    #[test]
    fn extracts_last_assistant_message() {
        let html = r#"
            <html><body>
              <div data-message-author-role="user">hi</div>
              <div data-message-author-role="assistant">first</div>
              <div data-message-author-role="user">again</div>
              <div data-message-author-role="assistant">second</div>
            </body></html>
        "#;
        assert_eq!(mk().extract_from_html(html).unwrap(), "second");
    }

    #[test]
    fn missing_selector_errors() {
        let html = "<html><body></body></html>";
        let err = mk().extract_from_html(html).unwrap_err();
        assert!(matches!(err, UwaError::Extraction(_)));
    }

    #[test]
    fn bad_selector_errors() {
        let mut m = mk();
        m.assistant_message = ">>>bad".into();
        let err = m.extract_from_html("<html/>").unwrap_err();
        assert!(matches!(err, UwaError::Extraction(_)));
    }
}
