//! Cloudflare challenge detection and human-in-the-loop recovery.
//!
//! This module only detects a challenge. It deliberately does not attempt to
//! solve it, forge clearance state, or alter Cloudflare's decision.

use serde_json::Value;
use tokio::sync::broadcast;
use uwa_core::{NetworkEvent, Page, TabId, UwaError};

const DOM_PROBE: &str = r#"(() => {
  const title = (document.title || '').toLowerCase();
  const body = (document.body?.innerText || '').slice(0, 5000).toLowerCase();
  return {
    challengeForm: !!document.querySelector('#challenge-form'),
    challengeToken: !!document.querySelector('[id^="cf-chl-"], [name^="__cf_chl_"]'),
    challengeStage: !!document.querySelector('#challenge-stage, #cf-challenge-running, .cf-browser-verification'),
    challengeText: /just a moment|checking your browser|verify you are human|performing security verification|security check/.test(title) &&
      /cloudflare|cf-ray/.test(body)
  };
})"#;

#[derive(Debug, Clone)]
pub struct PageVerification {
    pub url: String,
    pub challenged: bool,
    pub signals: Vec<String>,
}

/// Challenge categories for the verification store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    Cloudflare,
    Generic,
}

impl ChallengeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::Generic => "generic",
        }
    }
}

/// Structured info about a detected challenge, stored per-tab for the dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChallengeInfo {
    pub kind: ChallengeKind,
    pub url: String,
    pub tab: String,
    pub reason: String,
    /// Milliseconds since Unix epoch.
    pub detected_at_ms: u64,
}

/// Convert a `PageVerification` (signal list) into a structured `ChallengeInfo`.
pub fn info_from_signals(pv: &PageVerification, tab: &str) -> ChallengeInfo {
    let is_cloudflare = pv.signals.iter().any(|s| {
        s.contains("challenge-platform")
            || s.contains("challenge-form")
            || s.contains("cf-browser-verification")
            || s.contains("cf-chl")
    });
    let kind = if is_cloudflare {
        ChallengeKind::Cloudflare
    } else {
        ChallengeKind::Generic
    };
    ChallengeInfo {
        kind,
        tab: tab.to_string(),
        url: pv.url.clone(),
        reason: pv.signals.join(", "),
        detected_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

/// Inspect the current top-level page without interacting with or solving any
/// challenge. DOM and URL markers are kept specific to interstitial challenges
/// to avoid mistaking regular Turnstile widgets or JavaScript Detections for a block.
pub async fn inspect_page(page: &dyn Page) -> uwa_core::Result<PageVerification> {
    let url = page.url().await?.to_string();
    let mut signals = url_signals(&url);

    // Do not classify embedded Turnstile widgets or JavaScript Detection
    // scripts as an interstitial challenge. They can appear on otherwise
    // healthy provider pages. Only the top-level challenge URL and explicit
    // challenge DOM markers are decisive here.
    match page.eval(DOM_PROBE).await {
        Ok(value) => append_dom_signals(&value, &mut signals),
        Err(_) => {
            // Older browser transports may not support eval consistently.
            // HTML is a best-effort fallback and is never treated as proof
            // unless a Cloudflare-specific marker is present.
            let html = page.html().await.unwrap_or_default();
            append_html_signals(&html, &mut signals);
        }
    }

    Ok(PageVerification {
        url,
        challenged: !signals.is_empty(),
        signals,
    })
}

/// Per-request monitor. Its broadcast receiver is created before
/// submitting the prompt so it can observe `cf-mitigated: challenge`
/// responses even when the top-level document URL and DOM have not
/// changed (for example, an XHR challenge). URL/DOM inspection
/// remains the fallback for interstitial pages.
pub struct ChallengeMonitor {
    events: Option<broadcast::Receiver<NetworkEvent>>,
}

impl ChallengeMonitor {
    pub async fn new(page: &dyn Page) -> Self {
        Self {
            events: page.network_events().await.ok(),
        }
    }

    /// Abort the current provider turn when either the response-header signal
    /// or a visible-page signal indicates a Cloudflare challenge.
    pub async fn ensure_clear(
        &mut self,
        page: &dyn Page,
        provider: &str,
        tab: &TabId,
    ) -> uwa_core::Result<()> {
        if let Some(events) = self.events.as_mut() {
            loop {
                match events.try_recv() {
                    Ok(NetworkEvent::ResponseBody {
                        url,
                        cf_mitigated_challenge: true,
                        ..
                    }) => {
                        let url = if url.is_empty() {
                            page.url()
                                .await
                                .map(|url| url.to_string())
                                .unwrap_or_default()
                        } else {
                            url
                        };
                        return Err(challenge_error(provider, tab, url));
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                        tracing::debug!(skipped, "challenge monitor lagged behind browser events");
                        // Keep draining retained events: a challenge response may
                        // still be queued after the skipped older entries.
                        continue;
                    }
                    Err(broadcast::error::TryRecvError::Empty)
                    | Err(broadcast::error::TryRecvError::Closed) => break,
                }
            }
        }

        let Ok(status) = inspect_page(page).await else {
            // Preserve the provider's original error if browser inspection is
            // unavailable. This diagnostic path must not hide other failures.
            return Ok(());
        };
        if status.challenged {
            return Err(challenge_error(provider, tab, status.url));
        }
        Ok(())
    }
}

fn challenge_error(provider: &str, tab: &TabId, url: String) -> UwaError {
    tracing::warn!(
        provider,
        tab_id = %tab.as_str(),
        url = %url,
        "Cloudflare challenge detected; waiting for manual browser verification"
    );
    UwaError::HumanVerificationRequired {
        provider: provider.to_string(),
        tab_id: tab.as_str().to_string(),
        url,
    }
}

fn url_signals(raw_url: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(url) = url::Url::parse(raw_url) else {
        return found;
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    let is_challenge_platform = path.contains("/cdn-cgi/challenge-platform/");
    // JavaScript Detections loads from /scripts/jsd/, and Turnstile may use
    // the same host/platform path. Neither is an interstitial page by itself.
    let is_non_interstitial = path.contains("/scripts/jsd/") || path.contains("/turnstile/");
    if is_challenge_platform && !is_non_interstitial && !host.is_empty() {
        found.push("url:challenge-platform".to_string());
    }
    found
}

fn append_dom_signals(value: &Value, signals: &mut Vec<String>) {
    for (key, signal) in [
        ("challengeForm", "dom:challenge-form"),
        ("challengeToken", "dom:cf-challenge-token"),
        ("challengeStage", "dom:challenge-stage"),
        ("challengeText", "dom:challenge-title-and-text"),
    ] {
        if value.get(key).and_then(Value::as_bool) == Some(true) {
            push_unique(signals, signal);
        }
    }
}

fn append_html_signals(html: &str, signals: &mut Vec<String>) {
    let html = html.to_ascii_lowercase();
    if html.contains("id=\"challenge-form\"") || html.contains("id='challenge-form'") {
        push_unique(signals, "dom:challenge-form");
    }
    if html.contains("__cf_chl_") || html.contains("id=\"cf-chl-") || html.contains("id='cf-chl-") {
        push_unique(signals, "dom:cf-challenge-token");
    }
    if html.contains("id=\"challenge-stage\"")
        || html.contains("id='challenge-stage'")
        || html.contains("id=\"cf-challenge-running\"")
    {
        push_unique(signals, "dom:challenge-stage");
    }
    if (html.contains("just a moment")
        || html.contains("checking your browser")
        || html.contains("verify you are human")
        || html.contains("performing security verification")
        || html.contains("security check"))
        && (html.contains("cloudflare") || html.contains("cf-ray"))
    {
        push_unique(signals, "dom:challenge-title-and-text");
    }
}

fn push_unique(signals: &mut Vec<String>, signal: &str) {
    if !signals.iter().any(|existing| existing == signal) {
        signals.push(signal.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_top_level_challenge_platform_url() {
        assert_eq!(
            url_signals(
                "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/g/orchestrate"
            ),
            vec!["url:challenge-platform".to_string()]
        );
        assert_eq!(
            url_signals("https://chat.deepseek.com/cdn-cgi/challenge-platform/h/g/orchestrate"),
            vec!["url:challenge-platform".to_string()]
        );
    }

    #[test]
    fn does_not_flag_regular_cloudflare_fronted_site_by_url_alone() {
        assert!(url_signals("https://chat.deepseek.com/").is_empty());
        assert!(url_signals("https://example.com/cdn-cgi/trace").is_empty());
        assert!(
            url_signals("https://example.com/cdn-cgi/challenge-platform/scripts/jsd/api.js")
                .is_empty()
        );
        assert!(url_signals(
            "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/g/turnstile/frame"
        )
        .is_empty());
    }

    #[test]
    fn html_fallback_requires_challenge_specific_markers() {
        let mut signals = Vec::new();
        append_html_signals("<footer>Protected by Cloudflare</footer><script src=\"/cdn-cgi/challenge-platform/scripts/jsd/api.js\"></script>", &mut signals);
        assert!(signals.is_empty());

        append_html_signals(
            "<title>Just a moment...</title><form id=\"challenge-form\"></form>",
            &mut signals,
        );
        assert!(signals.contains(&"dom:challenge-form".to_string()));
    }

    #[tokio::test]
    async fn response_header_challenge_is_detected_without_dom_redirect() {
        use uwa_core::{NetworkEvent, TabId};
        use uwa_testkit::MockPage;

        let page = MockPage::new().with_url("https://chat.deepseek.com/");
        let mut monitor = ChallengeMonitor::new(&page).await;
        page.network_sender()
            .send(NetworkEvent::ResponseBody {
                request_id: "req_1".into(),
                url: "https://chat.deepseek.com/api/conversation".into(),
                body: "<!doctype html>challenge".into(),
                mime: "text/html".into(),
                cf_mitigated_challenge: true,
            })
            .expect("monitor subscribes before the response is sent");

        let err = monitor
            .ensure_clear(&page, "deepseek", &TabId::from_raw("tab_1"))
            .await
            .expect_err("challenge response must stop the request");
        assert!(matches!(
            err,
            UwaError::HumanVerificationRequired { ref provider, .. } if provider == "deepseek"
        ));
    }
}

// ---------------------------------------------------------------------------
// Verification store
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// A detected challenge page, recorded for the dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingVerification {
    pub info: crate::verification::ChallengeInfo,
    pub resolved: bool,
    pub resolved_at_ms: Option<u64>,
}

/// Per-tab record of "this tab is on a challenge page right now".
/// The store is a convenience for the UI, not a control-flow gate.
/// The pipeline decides what to do by probing the page (a fresh probe
/// finds no challenge the moment the user solves it).
pub struct VerificationStore {
    inner: RwLock<HashMap<String, PendingVerification>>,
    ttl: Duration,
}

impl VerificationStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    pub async fn record(&self, info: crate::verification::ChallengeInfo) {
        let mut g = self.inner.write().await;
        g.insert(
            info.tab.clone(),
            PendingVerification {
                info,
                resolved: false,
                resolved_at_ms: None,
            },
        );
    }

    pub async fn resolve(&self, tab: &str) -> bool {
        let mut g = self.inner.write().await;
        if let Some(e) = g.get_mut(tab) {
            e.resolved = true;
            e.resolved_at_ms = Some(now_ms());
            true
        } else {
            false
        }
    }

    pub async fn clear(&self) -> usize {
        let mut g = self.inner.write().await;
        let n = g.len();
        g.clear();
        n
    }

    pub async fn remove(&self, tab: &str) -> bool {
        self.inner.write().await.remove(tab).is_some()
    }

    pub async fn list(&self) -> Vec<PendingVerification> {
        let g = self.inner.read().await;
        let mut out: Vec<PendingVerification> = g.values().cloned().collect();
        out.sort_by(|a, b| b.info.detected_at_ms.cmp(&a.info.detected_at_ms));
        out
    }

    pub async fn sweep(&self) -> usize {
        let now = now_ms();
        let ttl_ms = self.ttl.as_millis() as u64;
        let mut g = self.inner.write().await;
        let before = g.len();
        g.retain(|_, e| now.saturating_sub(e.info.detected_at_ms) < ttl_ms);
        before - g.len()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::verification::ChallengeKind;

    fn info(tab: &str, at: u64) -> crate::verification::ChallengeInfo {
        crate::verification::ChallengeInfo {
            kind: ChallengeKind::Generic,
            url: "https://x/".into(),
            tab: tab.into(),
            reason: "test".into(),
            detected_at_ms: at,
        }
    }

    #[tokio::test]
    async fn record_and_list() {
        let s = VerificationStore::new(Duration::from_secs(60));
        s.record(info("a", 100)).await;
        s.record(info("b", 200)).await;
        let list = s.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].info.tab, "b");
    }

    #[tokio::test]
    async fn resolve_sets_flag() {
        let s = VerificationStore::new(Duration::from_secs(60));
        s.record(info("a", 100)).await;
        assert!(s.resolve("a").await);
        assert!(!s.resolve("missing").await);
        let list = s.list().await;
        assert!(list[0].resolved);
        assert!(list[0].resolved_at_ms.is_some());
    }

    #[tokio::test]
    async fn remove_and_clear() {
        let s = VerificationStore::new(Duration::from_secs(60));
        s.record(info("a", 0)).await;
        s.record(info("b", 0)).await;
        assert!(s.remove("a").await);
        assert_eq!(s.len().await, 1);
        assert_eq!(s.clear().await, 1);
        assert!(s.is_empty().await);
    }

    #[tokio::test]
    async fn sweep_drops_old_entries() {
        let s = VerificationStore::new(Duration::from_millis(10));
        s.record(info("old", 1)).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(s.sweep().await, 1);
        assert!(s.is_empty().await);
    }
}
