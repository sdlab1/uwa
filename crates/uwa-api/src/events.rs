//! In-memory ring of recent network events, plus a live fan-out channel
//! for the dashboard's Events page.
//!
//! The ring is bounded by `capacity` (default 1000 events). Bodies are
//! truncated at `max_body_bytes` so a long SSE response can't grow the
//! ring without bound; truncated entries carry `truncated: true` so the
//! UI can say so.
//!
//! The live channel is a `broadcast` — new subscribers see events from
//! the moment they subscribe. Replaying the ring is the caller's job
//! (the UI does one `GET /admin/events` then opens the SSE).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, Mutex};
use uwa_core::{NetworkEvent, TappedEvent};

const DEFAULT_CAPACITY: usize = 1000;
const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;
const LIVE_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub struct EventLogCfg {
    pub capacity: usize,
    pub max_body_bytes: usize,
}

impl Default for EventLogCfg {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// One stored event, ready for JSON serialization to the UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoggedEvent {
    /// Monotonic, per-process. Used by the UI as a stable key and by the
    /// SSE consumer to detect gaps.
    pub id: u64,
    /// Milliseconds since Unix epoch.
    pub at_ms: u64,
    /// `TabId` string.
    pub target: String,
    #[serde(flatten)]
    pub payload: LoggedPayload,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoggedPayload {
    ResponseBody {
        url: String,
        mime: String,
        body: String,
        size_bytes: u64,
        truncated: bool,
    },
    Finished {
        request_id: String,
    },
}

impl LoggedEvent {
    pub fn kind(&self) -> &'static str {
        match &self.payload {
            LoggedPayload::ResponseBody { .. } => "response_body",
            LoggedPayload::Finished { .. } => "finished",
        }
    }
}

struct Inner {
    entries: VecDeque<LoggedEvent>,
    by_target: HashMap<String, u64>,
    total: u64,
}

pub struct EventLog {
    cfg: EventLogCfg,
    inner: Arc<Mutex<Inner>>,
    next_id: AtomicU64,
    live: broadcast::Sender<LoggedEvent>,
}

impl EventLog {
    pub fn new(cfg: EventLogCfg) -> Self {
        let (live, _) = broadcast::channel(LIVE_CAPACITY);
        let cap = cfg.capacity.min(4096);
        Self {
            cfg,
            inner: Arc::new(Mutex::new(Inner {
                entries: VecDeque::with_capacity(cap),
                by_target: HashMap::new(),
                total: 0,
            })),
            next_id: AtomicU64::new(1),
            live,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LoggedEvent> {
        self.live.subscribe()
    }

    /// Ingest one tapped event. Truncates the body if needed, appends to
    /// the ring, and fans out to live subscribers.
    pub async fn ingest(&self, tapped: TappedEvent) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let payload = match tapped.event {
            NetworkEvent::ResponseBody {
                url, body, mime, ..
            } => {
                let full_len = body.len() as u64;
                let (body, truncated) = if body.len() > self.cfg.max_body_bytes {
                    let mut cut = self.cfg.max_body_bytes;
                    while cut > 0 && !body.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    (body[..cut].to_string(), true)
                } else {
                    (body, false)
                };
                LoggedPayload::ResponseBody {
                    url,
                    mime,
                    body,
                    size_bytes: full_len,
                    truncated,
                }
            }
            NetworkEvent::Finished { request_id } => LoggedPayload::Finished { request_id },
        };

        let ev = LoggedEvent {
            id,
            at_ms,
            target: tapped.target.clone(),
            payload,
        };

        {
            let mut g = self.inner.lock().await;
            if g.entries.len() >= self.cfg.capacity {
                g.entries.pop_front();
            }
            *g.by_target.entry(tapped.target.clone()).or_insert(0) += 1;
            g.total += 1;
            g.entries.push_back(ev.clone());
        }

        // Best-effort; a slow subscriber just lags.
        let _ = self.live.send(ev);
    }

    /// Recent events, newest first. Filters are ANDed.
    pub async fn recent(
        &self,
        limit: usize,
        target: Option<&str>,
        kind: Option<&str>,
        url_contains: Option<&str>,
    ) -> Vec<LoggedEvent> {
        let g = self.inner.lock().await;
        let limit = limit.clamp(1, 1000);
        let mut out = Vec::with_capacity(limit);
        // Iterate newest-first.
        for ev in g.entries.iter().rev() {
            if let Some(t) = target {
                if ev.target != t {
                    continue;
                }
            }
            if let Some(k) = kind {
                if ev.kind() != k {
                    continue;
                }
            }
            if let Some(sub) = url_contains {
                if !sub.is_empty() {
                    let url = match &ev.payload {
                        LoggedPayload::ResponseBody { url, .. } => url.as_str(),
                        LoggedPayload::Finished { request_id } => request_id.as_str(),
                    };
                    if !url.contains(sub) {
                        continue;
                    }
                }
            }
            out.push(ev.clone());
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    /// Snapshot of the counters and per-target tallies.
    pub async fn summary(&self) -> EventSummary {
        let g = self.inner.lock().await;
        let mut targets: Vec<TargetCount> = g
            .by_target
            .iter()
            .map(|(t, n)| TargetCount {
                target: t.clone(),
                count: *n,
            })
            .collect();
        targets.sort_by_key(|b| std::cmp::Reverse(b.count));
        EventSummary {
            capacity: self.cfg.capacity as u64,
            retained: g.entries.len() as u64,
            total: g.total,
            targets,
        }
    }

    pub async fn clear(&self) {
        let mut g = self.inner.lock().await;
        g.entries.clear();
        g.by_target.clear();
        // Don't reset `total` — it's a lifetime counter the UI uses to
        // show throughput over the session.
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EventSummary {
    pub capacity: u64,
    pub retained: u64,
    pub total: u64,
    pub targets: Vec<TargetCount>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TargetCount {
    pub target: String,
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(target: &str, body: &str) -> TappedEvent {
        TappedEvent {
            target: target.into(),
            event: NetworkEvent::ResponseBody {
                request_id: "r1".into(),
                url: "https://example.com/sse".into(),
                body: body.into(),
                mime: "text/event-stream".into(),
                cf_mitigated_challenge: false,
            },
        }
    }

    #[tokio::test]
    async fn ring_respects_capacity() {
        let log = EventLog::new(EventLogCfg {
            capacity: 3,
            ..Default::default()
        });
        for i in 0..5 {
            log.ingest(ev("t", &format!("body-{i}"))).await;
        }
        let recent = log.recent(10, None, None, None).await;
        assert_eq!(recent.len(), 3);
        // Newest first.
        match &recent[0].payload {
            LoggedPayload::ResponseBody { body, .. } => assert_eq!(body, "body-4"),
            _ => panic!(),
        }
        match &recent[2].payload {
            LoggedPayload::ResponseBody { body, .. } => assert_eq!(body, "body-2"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn body_is_truncated_and_flagged() {
        let log = EventLog::new(EventLogCfg {
            capacity: 10,
            max_body_bytes: 8,
        });
        log.ingest(ev("t", "0123456789abcdef")).await;
        let recent = log.recent(1, None, None, None).await;
        match &recent[0].payload {
            LoggedPayload::ResponseBody {
                body,
                size_bytes,
                truncated,
                ..
            } => {
                assert_eq!(body.len(), 8);
                assert_eq!(*size_bytes, 16);
                assert!(*truncated);
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn filters_compose() {
        let log = EventLog::new(EventLogCfg::default());
        log.ingest(TappedEvent {
            target: "t1".into(),
            event: NetworkEvent::ResponseBody {
                request_id: "r1".into(),
                url: "https://a/x".into(),
                body: "a".into(),
                mime: "text/event-stream".into(),
                cf_mitigated_challenge: false,
            },
        })
        .await;
        log.ingest(TappedEvent {
            target: "t2".into(),
            event: NetworkEvent::ResponseBody {
                request_id: "r2".into(),
                url: "https://b/y".into(),
                body: "b".into(),
                mime: "text/event-stream".into(),
                cf_mitigated_challenge: false,
            },
        })
        .await;
        log.ingest(TappedEvent {
            target: "t2".into(),
            event: NetworkEvent::Finished {
                request_id: "req-42".into(),
            },
        })
        .await;

        assert_eq!(log.recent(10, Some("t2"), None, None).await.len(), 2);
        assert_eq!(
            log.recent(10, None, Some("response_body"), None)
                .await
                .len(),
            2
        );
        assert_eq!(log.recent(10, None, None, Some("b/y")).await.len(), 1);
        assert_eq!(
            log.recent(10, Some("t2"), Some("finished"), None)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn subscribe_sees_live_events() {
        let log = EventLog::new(EventLogCfg::default());
        let mut rx = log.subscribe();
        log.ingest(ev("t", "x")).await;
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("event within 1s")
            .unwrap();
        assert_eq!(got.target, "t");
    }
}
