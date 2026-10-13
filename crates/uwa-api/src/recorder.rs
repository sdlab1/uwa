//! HTTP-layer recorder state.
//!
//! One recorder at a time. It runs a background task that polls the
//! target tab via `uwa_browser::recorder`, accumulates events, and stops
//! on command. The API surface is a thin wrapper — start, status, stop.
//!
//! The poller re-injects the observer on every tick. Installation is
//! idempotent, so a mid-recording navigation costs one extra eval and
//! nothing else.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use uwa_core::workflow::{Trace, TraceEvent};
use uwa_core::{Result, TabId};

use crate::state::AppState;

/// How often the poller reads the page's event buffer. 500 ms is the
/// sweet spot between latency (the UI's event counter should tick while
/// the user watches) and CDP chatter.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Hard ceiling. A user who walks away from a recording shouldn't be
/// able to make the daemon accumulate events forever.
const MAX_DURATION: Duration = Duration::from_secs(15 * 60);

/// Cap on retained events. Beyond this the oldest are dropped — a
/// recording long enough to hit this is almost certainly stuck.
const MAX_EVENTS: usize = 5_000;

pub struct Recorder {
    pub id: String,
    pub provider: String,
    pub tab: TabId,
    pub started_at: SystemTime,
    state: Arc<Mutex<RecState>>,
    stop: Arc<Notify>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

struct RecState {
    events: Vec<TraceEvent>,
    last_seq: u64,
    last_poll: SystemTime,
    installed: bool,
    poll_errors: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecorderStatus {
    pub active: bool,
    pub id: String,
    pub provider: String,
    pub tab: String,
    pub started_at_ms: u64,
    pub elapsed_ms: u64,
    pub events: usize,
    pub installed: bool,
    pub poll_errors: u32,
}

impl Recorder {
    pub async fn start(state: AppState, provider: String, tab: TabId) -> Result<Arc<Self>> {
        // Prime the target: reset any leftover buffer, install the observer.
        {
            let page = state.transport.page(&tab).await?;
            uwa_browser::recorder::reset(page.as_ref()).await.ok();
            uwa_browser::recorder::install(page.as_ref()).await?;
        }

        let inner = Arc::new(Mutex::new(RecState {
            events: Vec::new(),
            last_seq: 0,
            last_poll: SystemTime::now(),
            installed: true,
            poll_errors: 0,
        }));
        let stop = Arc::new(Notify::new());

        let id = format!("rec_{}", uuid::Uuid::new_v4().simple());
        let handle = {
            let inner = inner.clone();
            let stop = stop.clone();
            let transport = state.transport.clone();
            let tab = tab.clone();
            tokio::spawn(async move {
                let started = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = stop.notified() => break,
                        _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    }
                    if started.elapsed() >= MAX_DURATION {
                        break;
                    }
                    let Ok(page) = transport.page(&tab).await else {
                        continue;
                    };
                    // Idempotent install; survives navigation.
                    let _ = uwa_browser::recorder::install(page.as_ref()).await;

                    let last_seq = inner.lock().await.last_seq;
                    match uwa_browser::recorder::poll(page.as_ref(), last_seq).await {
                        Ok((installed, events)) => {
                            let mut g = inner.lock().await;
                            g.last_poll = SystemTime::now();
                            g.installed = installed;
                            g.poll_errors = 0;
                            for e in events {
                                g.last_seq = g.last_seq.max(e.seq());
                                g.events.push(e);
                            }
                            if g.events.len() > MAX_EVENTS {
                                let drop = g.events.len() - MAX_EVENTS;
                                g.events.drain(0..drop);
                            }
                        }
                        Err(e) => {
                            let mut g = inner.lock().await;
                            g.poll_errors = g.poll_errors.saturating_add(1);
                            tracing::debug!("recorder poll: {e}");
                        }
                    }
                }
                tracing::debug!("recorder poller stopped");
            })
        };

        Ok(Arc::new(Self {
            id,
            provider,
            tab,
            started_at: SystemTime::now(),
            state: inner,
            stop,
            handle: Mutex::new(Some(handle)),
        }))
    }

    /// Signal stop, join the poller, return the accumulated trace.
    pub async fn stop(&self) -> Trace {
        self.stop.notify_waiters();
        if let Some(h) = self.handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
        }
        let g = self.state.lock().await;
        let started_ms = to_ms(self.started_at);
        let ended_ms = to_ms(SystemTime::now());
        Trace {
            started_at_ms: started_ms,
            ended_at_ms: ended_ms,
            events: g.events.clone(),
        }
    }

    pub async fn status(&self) -> RecorderStatus {
        let g = self.state.lock().await;
        RecorderStatus {
            active: true,
            id: self.id.clone(),
            provider: self.provider.clone(),
            tab: self.tab.to_string(),
            started_at_ms: to_ms(self.started_at),
            elapsed_ms: SystemTime::now()
                .duration_since(self.started_at)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            events: g.events.len(),
            installed: g.installed,
            poll_errors: g.poll_errors,
        }
    }
}

fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
