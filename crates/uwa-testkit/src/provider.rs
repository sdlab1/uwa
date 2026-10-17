//! Programmable [`SiteProvider`].

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Scripted answers, one per `wait_response` (the last one repeats).
    script: Arc<Mutex<VecDeque<String>>>,
    /// When set, `wait_response_stream` yields these chunks one by one.
    chunks: Arc<Mutex<Option<Vec<String>>>>,
    chunk_delay: Arc<Mutex<Duration>>,
    /// How many chunks the pipeline actually pulled from the stream.
    polled: Arc<AtomicUsize>,
    /// How many times `cancel` ("click Stop") was called.
    cancels: Arc<AtomicUsize>,
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
            script: Arc::new(Mutex::new(VecDeque::new())),
            chunks: Arc::new(Mutex::new(None)),
            chunk_delay: Arc::new(Mutex::new(Duration::ZERO)),
            polled: Arc::new(AtomicUsize::new(0)),
            cancels: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Answer each successive `wait_response` with the next item; the last
    /// item repeats. Models a browser LLM that corrects itself on retry.
    pub fn with_answers<I, S>(self, answers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        *crate::lock(&self.script) = answers.into_iter().map(Into::into).collect();
        self
    }

    /// Stream the answer as these chunks, `delay` apart.
    pub fn with_chunks<I, S>(self, chunks: I, delay: Duration) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        *crate::lock(&self.chunks) = Some(chunks.into_iter().map(Into::into).collect());
        *crate::lock(&self.chunk_delay) = delay;
        self
    }

    /// Chunks pulled from the response stream so far.
    pub fn polled_chunks(&self) -> usize {
        self.polled.load(Ordering::SeqCst)
    }

    /// Number of `cancel` calls so far.
    pub fn cancel_count(&self) -> usize {
        self.cancels.load(Ordering::SeqCst)
    }

    pub fn with_answer(self, a: &str) -> Self {
        *crate::lock(&self.answer) = a.into();
        self
    }

    pub fn with_capabilities(mut self, c: Capabilities) -> Self {
        self.capabilities = c;
        self
    }

    /// Make [`wait_response`](SiteProvider::wait_response) fail instead of
    /// answering — what a broken provider looks like to the pipeline.
    pub fn with_error(self, msg: &str) -> Self {
        *crate::lock(&self.error) = Some(msg.into());
        self
    }

    /// How long [`wait_response`](SiteProvider::wait_response) sleeps first.
    pub fn with_delay(self, d: Duration) -> Self {
        *crate::lock(&self.delay) = Some(d);
        self
    }

    /// Flip [`matches`](SiteProvider::matches) at runtime.
    pub fn set_matches(&self, v: bool) {
        *crate::lock(&self.matches) = v;
    }

    /// Replace the answer at runtime (e.g. between two turns).
    pub fn set_answer(&self, a: &str) {
        *crate::lock(&self.answer) = a.into();
    }

    /// Everything typed into the provider, oldest first.
    pub fn sent(&self) -> Vec<String> {
        crate::lock(&self.sent).clone()
    }

    /// The last prompt that reached the provider, if any.
    pub fn sent_last(&self) -> String {
        crate::lock(&self.sent).last().cloned().unwrap_or_default()
    }
}

#[async_trait]
impl SiteProvider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn matches(&self, _url: &Url) -> bool {
        *crate::lock(&self.matches)
    }

    async fn send_message(&self, _page: &dyn Page, text: &str) -> Result<()> {
        crate::lock(&self.sent).push(text.to_string());
        Ok(())
    }

    async fn wait_response(&self, _page: &dyn Page) -> Result<String> {
        if let Some(msg) = crate::lock(&self.error).clone() {
            return Err(UwaError::Unavailable(msg));
        }
        let delay = *crate::lock(&self.delay);
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        {
            // Scripted answer sequence (added by the conformance work):
            // pop until one is left, then keep returning it.
            let mut script = crate::lock(&self.script);
            if script.len() > 1 {
                if let Some(next) = script.pop_front() {
                    return Ok(next);
                }
            } else if let Some(only) = script.front() {
                return Ok(only.clone());
            }
        }
        Ok(crate::lock(&self.answer).clone())
    }

    async fn wait_response_stream(&self, page: &dyn Page) -> Result<uwa_core::ResponseStream> {
        let chunks = crate::lock(&self.chunks).clone();
        let Some(chunks) = chunks else {
            let text = self.wait_response(page).await?;
            return Ok(Box::pin(futures::stream::iter(vec![Ok(text)])));
        };
        let delay = *crate::lock(&self.chunk_delay);
        let polled = self.polled.clone();
        Ok(Box::pin(futures::stream::unfold(
            (chunks.into_iter(), polled),
            move |(mut it, polled)| async move {
                let next = it.next()?;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                polled.fetch_add(1, Ordering::SeqCst);
                Some((Ok(next), (it, polled)))
            },
        )))
    }

    async fn cancel(&self, _page: &dyn Page) -> Result<()> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
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
        let answer = p
            .wait_response(&MockPage::new())
            .await
            .unwrap_or_else(|_| unreachable!("answer"));
        assert_eq!(answer, "hello");
        assert_eq!(p.name(), "chatgpt");
        assert!(p.capabilities().streams);
    }

    #[tokio::test]
    async fn records_what_was_sent() {
        let p = MockProvider::new("chatgpt");
        p.send_message(&MockPage::new(), "first")
            .await
            .unwrap_or_else(|_| unreachable!("send"));
        p.send_message(&MockPage::new(), "second")
            .await
            .unwrap_or_else(|_| unreachable!("send"));
        assert_eq!(p.sent(), vec!["first".to_string(), "second".to_string()]);
        assert_eq!(p.sent_last(), "second");
    }

    #[tokio::test]
    async fn scripted_error_replaces_the_answer() {
        let p = MockProvider::new("chatgpt")
            .with_answer("never seen")
            .with_error("provider exploded");
        let err = match p.wait_response(&MockPage::new()).await {
            Err(e) => e,
            Ok(_) => unreachable!("fails"),
        };
        assert!(matches!(err, UwaError::Unavailable(m) if m == "provider exploded"));
    }

    #[tokio::test]
    async fn delay_slows_the_answer_down() {
        let p = MockProvider::new("slow").with_delay(Duration::from_millis(50));
        let started = std::time::Instant::now();
        let _ = p
            .wait_response(&MockPage::new())
            .await
            .unwrap_or_else(|_| unreachable!("answer"));
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn matches_can_be_flipped() {
        let p = MockProvider::new("x");
        let url = Url::parse("https://chatgpt.com/").unwrap_or_else(|_| unreachable!("url"));
        assert!(p.matches(&url));
        p.set_matches(false);
        assert!(!p.matches(&url));
    }
}
