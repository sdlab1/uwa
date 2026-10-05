//! Programmable [`SiteProvider`].

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;
use uwa_core::{Capabilities, Page, Result, SiteProvider, UwaError};

/// A provider that answers with a scripted string, optional artificial
/// delay, and — like a real site — remembers what was typed into it.
#[derive(Clone)]
pub struct MockProvider {
    name: String,
    answer: Arc<Mutex<String>>,
    error: Arc<Mutex<Option<String>>>,
    delay: Arc<Mutex<Option<Duration>>>,
    capabilities: Capabilities,
    matches: Arc<Mutex<bool>>,
    /// Everything `send_message` received, in order.
    sent: Arc<Mutex<Vec<String>>>,
}

impl MockProvider {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            answer: Arc::new(Mutex::new(String::new())),
            error: Arc::new(Mutex::new(None)),
            delay: Arc::new(Mutex::new(None)),
            capabilities: Capabilities {
                streams: true,
                tool_calls: true,
                vision: false,
                max_context_tokens: None,
            },
            matches: Arc::new(Mutex::new(true)),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_answer(self, a: &str) -> Self {
        *self.answer.lock().expect("answer lock") = a.into();
        self
    }

    pub fn with_capabilities(mut self, c: Capabilities) -> Self {
        self.capabilities = c;
        self
    }

    /// Make [`wait_response`](SiteProvider::wait_response) fail instead of
    /// answering — what a broken provider looks like to the pipeline.
    pub fn with_error(self, msg: &str) -> Self {
        *self.error.lock().expect("error lock") = Some(msg.into());
        self
    }

    /// How long [`wait_response`](SiteProvider::wait_response) sleeps first.
    pub fn with_delay(self, d: Duration) -> Self {
        *self.delay.lock().expect("delay lock") = Some(d);
        self
    }

    /// Flip [`matches`](SiteProvider::matches) at runtime.
    pub fn set_matches(&self, v: bool) {
        *self.matches.lock().expect("matches lock") = v;
    }

    /// Replace the answer at runtime (e.g. between two turns).
    pub fn set_answer(&self, a: &str) {
        *self.answer.lock().expect("answer lock") = a.into();
    }

    /// Everything typed into the provider, oldest first.
    pub fn sent(&self) -> Vec<String> {
        self.sent.lock().expect("sent lock").clone()
    }

    /// The last prompt that reached the provider, if any.
    pub fn sent_last(&self) -> String {
        self.sent
            .lock()
            .expect("sent lock")
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl SiteProvider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn matches(&self, _url: &Url) -> bool {
        *self.matches.lock().expect("matches lock")
    }

    async fn send_message(&self, _page: &dyn Page, text: &str) -> Result<()> {
        self.sent.lock().expect("sent lock").push(text.to_string());
        Ok(())
    }

    async fn wait_response(&self, _page: &dyn Page) -> Result<String> {
        if let Some(msg) = self.error.lock().expect("error lock").clone() {
            return Err(UwaError::Unavailable(msg));
        }
        let delay = *self.delay.lock().expect("delay lock");
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        Ok(self.answer.lock().expect("answer lock").clone())
    }

    async fn cancel(&self, _page: &dyn Page) -> Result<()> {
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::MockPage;

    #[tokio::test]
    async fn answers_with_the_scripted_text() {
        let p = MockProvider::new("chatgpt").with_answer("hello");
        let answer = p.wait_response(&MockPage::new()).await.expect("answer");
        assert_eq!(answer, "hello");
        assert_eq!(p.name(), "chatgpt");
        assert!(p.capabilities().streams);
    }

    #[tokio::test]
    async fn records_what_was_sent() {
        let p = MockProvider::new("chatgpt");
        p.send_message(&MockPage::new(), "first")
            .await
            .expect("send");
        p.send_message(&MockPage::new(), "second")
            .await
            .expect("send");
        assert_eq!(p.sent(), vec!["first".to_string(), "second".to_string()]);
        assert_eq!(p.sent_last(), "second");
    }

    #[tokio::test]
    async fn scripted_error_replaces_the_answer() {
        let p = MockProvider::new("chatgpt")
            .with_answer("never seen")
            .with_error("provider exploded");
        let err = p.wait_response(&MockPage::new()).await.expect_err("fails");
        assert!(matches!(err, UwaError::Unavailable(m) if m == "provider exploded"));
    }

    #[tokio::test]
    async fn delay_slows_the_answer_down() {
        let p = MockProvider::new("slow").with_delay(Duration::from_millis(50));
        let started = std::time::Instant::now();
        let _ = p.wait_response(&MockPage::new()).await.expect("answer");
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn matches_can_be_flipped() {
        let p = MockProvider::new("x");
        let url = Url::parse("https://chatgpt.com/").expect("url");
        assert!(p.matches(&url));
        p.set_matches(false);
        assert!(!p.matches(&url));
    }
}
