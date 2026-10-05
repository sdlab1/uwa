//! Broadcast bus: one channel per CDP target so every subscriber on that tab
//! sees the same network events.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use uwa_core::NetworkEvent;

const CAPACITY: usize = 512;

#[derive(Default, Clone)]
pub struct NetBus {
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<NetworkEvent>>>>,
}

impl NetBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (creating if needed) the sender for `target_id`.
    pub async fn sender_for(&self, target_id: String) -> broadcast::Sender<NetworkEvent> {
        let mut g = self.inner.lock().await;
        g.entry(target_id)
            .or_insert_with(|| broadcast::channel(CAPACITY).0)
            .clone()
    }

    /// Subscribe to a target's events, creating the channel on demand.
    pub async fn subscribe(&self, target_id: &str) -> broadcast::Receiver<NetworkEvent> {
        self.sender_for(target_id.to_string()).await.subscribe()
    }

    pub async fn remove(&self, target_id: &str) {
        self.inner.lock().await.remove(target_id);
    }

    pub async fn targets(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_key_shares_one_channel() {
        let bus = NetBus::new();
        let a = bus.sender_for("t1".into()).await;
        let mut rx = bus.subscribe("t1").await;
        a.send(NetworkEvent::Finished {
            request_id: "r1".into(),
        })
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("event should arrive")
            .unwrap();
        assert!(matches!(ev, NetworkEvent::Finished { ref request_id } if request_id == "r1"));
    }

    #[tokio::test]
    async fn different_keys_are_isolated() {
        let bus = NetBus::new();
        let a = bus.sender_for("t1".into()).await;
        let mut rx1 = bus.subscribe("t1").await;
        let mut rx2 = bus.subscribe("t2").await;
        a.send(NetworkEvent::Finished {
            request_id: "only-t1".into(),
        })
        .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx2.recv())
                .await
                .is_err(),
            "t2 must not see t1 traffic"
        );
        assert!(
            matches!(
                rx1.recv().await,
                Ok(NetworkEvent::Finished { ref request_id }) if request_id == "only-t1"
            ),
            "t1 must see its own traffic"
        );
        assert_eq!(bus.targets().await.len(), 2);
        bus.remove("t2").await;
        assert_eq!(bus.targets().await, vec!["t1".to_string()]);
    }
}
