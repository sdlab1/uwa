//! Broadcast bus: one channel per group key (target ID or root frame ID)
//! so every subscriber on that group sees the same network events.
//!
//! The key is the target ID for the current implementation; when OOPIF
//! root-frame routing is fully implemented, the key will become the root
//! frame ID. Both `sender_for` and `sender_for_root_frame` (and their
//! corresponding subscribe methods) share the same channel map so that
//! a subscription by target ID receives events routed by root frame ID
//! when they match (which is the case for same-process pages).

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use uwa_core::NetworkEvent;

const CAPACITY: usize = 512;

#[derive(Default, Clone)]
pub struct NetBus {
    /// Keyed by group key (target ID == root frame ID for same-process pages).
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<NetworkEvent>>>>,
}

impl NetBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (creating if needed) the sender for `target_id`.
    ///
    /// Shares the same channel map as `sender_for_root_frame`, so events
    /// sent via either method with the same key are received by the same
    /// subscribers.
    pub async fn sender_for(&self, target_id: String) -> broadcast::Sender<NetworkEvent> {
        let mut g = self.inner.lock().await;
        g.entry(target_id)
            .or_insert_with(|| broadcast::channel(CAPACITY).0)
            .clone()
    }

    /// Get (creating if needed) the sender for `root_frame_id`.
    ///
    /// Alias for `sender_for` — they share the same underlying channel map.
    pub async fn sender_for_root_frame(
        &self,
        root_frame_id: String,
    ) -> broadcast::Sender<NetworkEvent> {
        self.sender_for(root_frame_id).await
    }

    /// Subscribe to a group's events, creating the channel on demand.
    pub async fn subscribe(&self, target_id: &str) -> broadcast::Receiver<NetworkEvent> {
        self.sender_for(target_id.to_string()).await.subscribe()
    }

    /// Subscribe to events grouped by root frame ID.
    ///
    /// Alias for `subscribe` — they share the same underlying channel map.
    pub async fn subscribe_to_root_frame(
        &self,
        root_frame_id: &str,
    ) -> broadcast::Receiver<NetworkEvent> {
        self.subscribe(root_frame_id).await
    }

    pub async fn remove(&self, target_id: &str) {
        self.inner.lock().await.remove(target_id);
    }

    /// Alias for `remove`.
    pub async fn remove_root_frame(&self, root_frame_id: &str) {
        self.remove(root_frame_id).await;
    }

    pub async fn targets(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }

    /// Alias for `targets`.
    pub async fn root_frames(&self) -> Vec<String> {
        self.targets().await
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

    #[tokio::test]
    async fn sender_for_and_sender_for_root_frame_share_channel() {
        let bus = NetBus::new();
        // Subscribe by target ID...
        let mut rx = bus.subscribe("key1").await;
        // ...but send by root frame ID. They must share the same channel.
        let tx = bus.sender_for_root_frame("key1".into()).await;
        tx.send(NetworkEvent::Finished {
            request_id: "shared".into(),
        })
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("event should arrive via shared channel")
            .unwrap();
        assert!(matches!(ev, NetworkEvent::Finished { ref request_id } if request_id == "shared"));
    }

    #[tokio::test]
    async fn subscribe_to_root_frame_receives_from_sender_for() {
        let bus = NetBus::new();
        // Subscribe by root frame ID...
        let mut rx = bus.subscribe_to_root_frame("key2").await;
        // ...but send by target ID. They must share the same channel.
        let tx = bus.sender_for("key2".into()).await;
        tx.send(NetworkEvent::Finished {
            request_id: "cross".into(),
        })
        .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("event should arrive via shared channel")
            .unwrap();
        assert!(matches!(ev, NetworkEvent::Finished { ref request_id } if request_id == "cross"));
    }

    #[tokio::test]
    async fn remove_root_frame_removes_from_targets() {
        let bus = NetBus::new();
        bus.sender_for("k1".into()).await;
        bus.sender_for("k2".into()).await;
        assert_eq!(bus.targets().await.len(), 2);
        bus.remove_root_frame("k1").await;
        assert_eq!(bus.targets().await.len(), 1);
        assert_eq!(bus.root_frames().await, vec!["k2".to_string()]);
    }

    #[tokio::test]
    async fn many_subscribers_all_receive() {
        let bus = NetBus::new();
        let tx = bus.sender_for("fanout".into()).await;
        let mut r1 = bus.subscribe("fanout").await;
        let mut r2 = bus.subscribe("fanout").await;
        let mut r3 = bus.subscribe_to_root_frame("fanout").await;
        tx.send(NetworkEvent::Finished {
            request_id: "broadcast".into(),
        })
        .unwrap();
        for rx in [&mut r1, &mut r2, &mut r3] {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("every subscriber must receive")
                .unwrap();
            assert!(
                matches!(ev, NetworkEvent::Finished { ref request_id } if request_id == "broadcast")
            );
        }
    }
}
