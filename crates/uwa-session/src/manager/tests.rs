use super::*;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use uwa_core::Result;

#[derive(Default)]
struct MockTransport {
    tabs: Vec<TabId>,
    unhealthy: HashSet<String>,
    health_calls: AtomicUsize,
    concurrent: AtomicUsize,
    peak: AtomicUsize,
    rr: AtomicUsize,
}

impl MockTransport {
    fn with_tabs(n: usize) -> Self {
        Self {
            tabs: (0..n)
                .map(|i| TabId::from_raw(format!("tab_{i}")))
                .collect(),
            ..Default::default()
        }
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn uwa_core::Page>> {
        Err(UwaError::TabNotFound(tab.to_string()))
    }
    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        // Round-robin so successive leases land on distinct tabs, like a real
        // pool would once the first tab is busy.
        if self.tabs.is_empty() {
            return Ok(Vec::new());
        }
        let start = self.rr.fetch_add(1, Ordering::SeqCst) % self.tabs.len();
        let mut out = Vec::with_capacity(self.tabs.len());
        out.extend(self.tabs[start..].iter().cloned());
        out.extend(self.tabs[..start].iter().cloned());
        Ok(out)
    }
    async fn health(&self, tab: &TabId) -> Result<()> {
        self.health_calls.fetch_add(1, Ordering::SeqCst);
        if self.unhealthy.contains(tab.as_str()) {
            Err(UwaError::TabNotFound(tab.to_string()))
        } else {
            Ok(())
        }
    }
}

fn cfg(idle_ms: u64, max: usize) -> SessionCfg {
    SessionCfg {
        idle_ttl: Duration::from_millis(idle_ms),
        sweep_interval: Duration::from_millis(10),
        max_sessions: max,
    }
}

fn cid(s: &str) -> ConversationId {
    ConversationId::from_raw(s)
}

#[tokio::test]
async fn same_conversation_same_tab() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = MockTransport::with_tabs(3);
    let a = sm.acquire(&cid("c1"), &t).await.unwrap();
    let tab = a.tab.clone();
    drop(a);
    let b = sm.acquire(&cid("c1"), &t).await.unwrap();
    assert_eq!(b.tab, tab);
    assert_eq!(sm.len(), 1);
}

#[tokio::test]
async fn concurrent_same_cid_serialize() {
    let sm = Arc::new(SessionManager::new(cfg(60_000, 8)));
    let t = Arc::new(MockTransport::with_tabs(1));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let sm = sm.clone();
        let t = t.clone();
        handles.push(tokio::spawn(async move {
            let h = sm.acquire(&cid("shared"), t.as_ref()).await.unwrap();
            t.concurrent.fetch_add(1, Ordering::SeqCst);
            let peak = t.concurrent.load(Ordering::SeqCst);
            t.peak.fetch_max(peak, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            t.concurrent.fetch_sub(1, Ordering::SeqCst);
            h.tab.clone()
        }));
    }
    let first = handles.pop().unwrap().await.unwrap();
    for h in handles {
        assert_eq!(h.await.unwrap(), first);
    }
    assert_eq!(sm.len(), 1);
    assert_eq!(
        t.peak.load(Ordering::SeqCst),
        1,
        "same conversation must serialize"
    );
}

#[tokio::test]
async fn evict_idle_removes_old() {
    let sm = SessionManager::new(cfg(20, 8));
    let t = MockTransport::with_tabs(1);
    let h = sm.acquire(&cid("old"), &t).await.unwrap();
    drop(h);
    assert_eq!(sm.len(), 1);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let evicted = sm.evict_idle().await;
    assert_eq!(evicted, vec![cid("old")]);
    assert!(sm.is_empty());
}

#[tokio::test]
async fn evict_idle_keeps_leased_session() {
    let sm = SessionManager::new(cfg(20, 8));
    let t = MockTransport::with_tabs(1);
    let h = sm.acquire(&cid("busy"), &t).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let evicted = sm.evict_idle().await;
    assert!(evicted.is_empty());
    drop(h);
    assert_eq!(sm.len(), 1);
}

#[tokio::test]
async fn recover_drops_unhealthy() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let mut t = MockTransport::with_tabs(2);
    let h1 = sm.acquire(&cid("good"), &t).await.unwrap();
    let h2 = sm.acquire(&cid("bad"), &t).await.unwrap();
    t.unhealthy.insert(h2.tab.as_str().to_string());
    let dead = sm.recover(&t).await;
    assert_eq!(dead, vec![h2.tab.clone()]);
    assert_eq!(sm.len(), 1);
    assert!(sm.entries.contains_key(&cid("good")));
    drop(h1);
    drop(h2);
}

#[tokio::test]
async fn max_sessions_evicts_oldest() {
    let sm = SessionManager::new(cfg(60_000, 2));
    let t = MockTransport::with_tabs(4);
    let a = sm.acquire(&cid("a"), &t).await.unwrap();
    drop(a);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = sm.acquire(&cid("b"), &t).await.unwrap();
    drop(b);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let c = sm.acquire(&cid("c"), &t).await.unwrap();
    drop(c);
    assert!(sm.len() <= 2);
    assert!(!sm.entries.contains_key(&cid("a")));
    assert!(sm.entries.contains_key(&cid("c")));
}

#[tokio::test]
async fn list_reports_sessions() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = MockTransport::with_tabs(1);
    let h = sm.acquire(&cid("x"), &t).await.unwrap();
    let list = sm.list().await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].conversation, cid("x"));
    assert_eq!(list[0].tab, h.tab);
    assert_eq!(list[0].holders, 1);
    drop(h);
    let list = sm.list().await;
    assert_eq!(list[0].holders, 0);
}

#[tokio::test]
async fn remove_returns_tab() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = MockTransport::with_tabs(1);
    let h = sm.acquire(&cid("gone"), &t).await.unwrap();
    let tab = h.tab.clone();
    drop(h);
    assert_eq!(sm.remove(&cid("gone")), Some(tab));
    assert!(sm.remove(&cid("gone")).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eviction_does_not_hang_under_contention() {
    // Regression test for audit B2: DashMap shard guards must never be held
    // across an await point inside `evict_idle`.
    let sm = Arc::new(SessionManager::new(cfg(1_000, 64)));
    let t = Arc::new(MockTransport::with_tabs(4));

    for i in 0..16 {
        let h = sm
            .acquire(&cid(&format!("c{i}")), t.as_ref())
            .await
            .unwrap();
        drop(h);
    }

    let mut workers = Vec::new();
    for i in 0..4 {
        let sm = sm.clone();
        let t = t.clone();
        workers.push(tokio::spawn(async move {
            for j in 0..16 {
                let id = cid(&format!("c{}", (i * 16 + j) % 16));
                let h = sm.acquire(&id, t.as_ref()).await.unwrap();
                h.touch().await;
                drop(h);
                let _ = sm.evict_idle().await;
            }
        }));
    }
    for w in workers {
        w.await.unwrap();
    }
    assert!(!sm.is_empty());
}
