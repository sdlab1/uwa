//! Tap forwarder shared by every transport that has a per-tab network
//! bus. One forwarder per (tab, source-bus) pair.
//!
//! The loop is written so that a `Lagged` error — a slow consumer
//! falling behind a burst — drops only the overflowed events, not the
//! forwarder itself. `broadcast::Receiver::recv` returns `Lagged(n)` and
//! continues to return subsequent events on the next call, so the
//! forwarder just logs and loops.
//!
//! Exits only when the source bus is closed (i.e. the tab is gone and
//! no sender holds a reference), which is the honest end-of-stream
//! signal.

use std::sync::Arc;
use tokio::sync::broadcast::{self, error::RecvError};
use uwa_core::{NetworkEvent, TappedEvent};

/// Mirror every event of a per-tab bus into the shared tap, tagged with
/// the tab. Returns the join handle so callers can await shutdown in
/// tests.
pub fn spawn_tap_forwarder(
    tap: Arc<broadcast::Sender<TappedEvent>>,
    tab: String,
    src: &broadcast::Sender<NetworkEvent>,
) -> tokio::task::JoinHandle<()> {
    // Subscribe before spawning: a sender that fires between the
    // `spawn` and the task's first poll would otherwise be lost.
    let mut rx = src.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Best-effort; a lagged consumer of the tap drops
                    // its own overflow, we keep forwarding.
                    let _ = tap.send(TappedEvent {
                        target: tab.clone(),
                        event,
                    });
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(tab = %tab, dropped = n, "tap forwarder lagged");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fin(id: &str) -> NetworkEvent {
        NetworkEvent::Finished {
            request_id: id.into(),
        }
    }

    #[tokio::test]
    async fn forwarder_mirrors_events() {
        let tap = Arc::new(broadcast::channel::<TappedEvent>(8).0);
        let mut sub = tap.subscribe();
        let (src, _keep) = broadcast::channel::<NetworkEvent>(8);
        spawn_tap_forwarder(tap.clone(), "tab_a".into(), &src);
        tokio::time::sleep(Duration::from_millis(20)).await;

        src.send(fin("r1")).unwrap_or_else(|e| unreachable!("{e}"));
        let ev = tokio::time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .unwrap_or_else(|_| unreachable!("event within 1s"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(ev.target, "tab_a");
        assert!(matches!(&ev.event, NetworkEvent::Finished { request_id } if request_id == "r1"));
    }

    #[tokio::test]
    async fn forwarder_survives_lag() {
        let tap = Arc::new(broadcast::channel::<TappedEvent>(64).0);
        let mut sub = tap.subscribe();
        let (src, _keep) = broadcast::channel::<NetworkEvent>(2);
        spawn_tap_forwarder(tap.clone(), "tab".into(), &src);
        tokio::time::sleep(Duration::from_millis(20)).await;

        for i in 0..10 {
            let _ = src.send(fin(&format!("burst{i}")));
        }
        tokio::task::yield_now().await;
        src.send(fin("after"))
            .unwrap_or_else(|e| unreachable!("{e}"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut seen_after = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(300), sub.recv()).await {
                Ok(Ok(ev)) => {
                    if matches!(&ev.event, NetworkEvent::Finished { request_id } if request_id == "after")
                    {
                        seen_after = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(seen_after, "forwarder must keep running after Lagged");
    }

    #[tokio::test]
    async fn forwarder_exits_when_source_closes() {
        let tap = Arc::new(broadcast::channel::<TappedEvent>(8).0);
        let (src, _) = broadcast::channel::<NetworkEvent>(8);
        let h = spawn_tap_forwarder(tap, "t".into(), &src);
        drop(src);
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .unwrap_or_else(|_| unreachable!("forwarder exits within 1s"))
            .unwrap_or_else(|e| unreachable!("task did not panic: {e}"));
    }
}
