use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uwa_testkit::MockTransport;

/// Tabs come out rotated, so two fresh sessions land on different tabs the
/// way they would against a real pool.
fn tabs(n: usize) -> MockTransport {
    MockTransport::with_n_tabs(n).with_round_robin()
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
    let t = tabs(3);
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
    let t = Arc::new(tabs(1));
    // How many holders are inside the critical section right now, and the
    // highest that number ever got.
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let sm = sm.clone();
        let t = t.clone();
        let inside = inside.clone();
        let peak = peak.clone();
        handles.push(tokio::spawn(async move {
            let h = sm.acquire(&cid("shared"), t.as_ref()).await.unwrap();
            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            inside.fetch_sub(1, Ordering::SeqCst);
            h.tab.clone()
        }));
    }
    let first = handles.pop().unwrap().await.unwrap();
    for h in handles {
        assert_eq!(h.await.unwrap(), first);
    }
    assert_eq!(sm.len(), 1);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "same conversation must serialize"
    );
}

#[tokio::test]
async fn evict_idle_removes_old() {
    let sm = SessionManager::new(cfg(20, 8));
    let t = tabs(1);
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
    let t = tabs(1);
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
    let t = tabs(2);
    let h1 = sm.acquire(&cid("good"), &t).await.unwrap();
    let h2 = sm.acquire(&cid("bad"), &t).await.unwrap();
    t.mark_unhealthy(&h2.tab);
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
    let t = tabs(4);
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
async fn distinct_conversations_spread_across_tabs() {
    // Regression: fresh sessions used to always take tabs.iter().next(),
    // funneling every conversation onto the browser's first tab.
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let a = sm.acquire(&cid("c1"), &t).await.unwrap();
    let b = sm.acquire(&cid("c2"), &t).await.unwrap();
    assert_ne!(
        a.tab, b.tab,
        "two idle pools of conversations must not share the only two tabs"
    );
    drop((a, b));
}

#[tokio::test]
async fn over_capacity_never_evicts_a_busy_session() {
    // Regression: capacity eviction used to remove entries with live
    // handles, silently forking a mid-flight conversation onto another tab.
    let sm = SessionManager::new(cfg(60_000, 1));
    let t = tabs(2);
    let busy = sm.acquire(&cid("busy"), &t).await.unwrap();
    let tab_of_busy = busy.tab.clone();

    // At capacity with the only session busy: the manager must grow past
    // the cap rather than evicting the live lease.
    let other = sm.acquire(&cid("other"), &t).await.unwrap();
    assert!(sm.entries.contains_key(&cid("busy")));
    assert!(sm.entries.contains_key(&cid("other")));
    assert_eq!(busy.tab, tab_of_busy, "the live lease keeps its tab");

    drop(busy);
    drop(other);
}

#[tokio::test]
async fn list_reports_sessions() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(1);
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
    let t = tabs(1);
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
    let t = Arc::new(tabs(4));

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

#[tokio::test]
async fn recover_drops_parked_sessions_on_dead_tabs() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let h1 = sm.acquire(&cid("good"), &t).await.unwrap();
    let h2 = sm.acquire(&cid("bad"), &t).await.unwrap();
    let dead_tab = h2.tab.clone();
    drop(h2);
    t.mark_unhealthy(&dead_tab);
    let dead = sm.recover(&t).await;
    assert_eq!(dead, vec![dead_tab]);
    assert_eq!(sm.len(), 1);
    assert!(sm.entries.contains_key(&cid("good")));
    drop(h1);
}

#[tokio::test]
async fn recover_keeps_leased_session_but_reports_its_dead_tab() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let h = sm.acquire(&cid("busy"), &t).await.unwrap();
    let dead_tab = h.tab.clone();
    t.mark_unhealthy(&dead_tab);

    assert_eq!(sm.recover(&t).await, vec![dead_tab.clone()]);
    assert_eq!(sm.len(), 1, "leased session must survive recover");

    drop(h);
    assert_eq!(sm.recover(&t).await, vec![dead_tab]);
    assert!(sm.is_empty(), "parked session on a dead tab is dropped");
}

#[tokio::test]
async fn recover_reports_each_dead_tab_once() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(1);
    for name in ["a", "b", "c"] {
        drop(sm.acquire(&cid(name), &t).await.unwrap());
    }
    let only = t.tab_ids().remove(0);
    t.mark_unhealthy(&only);
    let dead = sm.recover(&t).await;
    assert_eq!(dead, vec![only], "three sessions, one dead tab, one report");
    assert!(sm.is_empty());
}

#[tokio::test]
async fn acquire_repins_conversation_whose_tab_was_closed() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let first = sm.acquire(&cid("c"), &t).await.unwrap();
    let old_tab = first.tab.clone();
    let old_gen = first.generation;
    drop(first);

    t.close_tab(&old_tab);
    let second = sm.acquire(&cid("c"), &t).await.unwrap();
    assert_ne!(second.tab, old_tab, "must move off the closed tab");
    assert!(t.page(&second.tab).await.is_ok(), "new tab must be usable");
    assert!(second.generation > old_gen, "a tab swap must be visible");
    assert_eq!(sm.len(), 1, "same conversation, same entry");
    drop(second);

    let third = sm.acquire(&cid("c"), &t).await.unwrap();
    assert_ne!(third.tab, old_tab);
}

#[tokio::test]
async fn acquire_repins_when_tab_turns_unhealthy() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let first = sm.acquire(&cid("c"), &t).await.unwrap();
    let old_tab = first.tab.clone();
    drop(first);
    t.mark_unhealthy(&old_tab);

    let second = sm.acquire(&cid("c"), &t).await.unwrap();
    assert_ne!(second.tab, old_tab);
    assert!(t.health(&second.tab).await.is_ok());
}

#[tokio::test]
async fn repin_invalidates_pinning_state() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    let first = sm.acquire(&cid("c"), &t).await.unwrap();
    let old_tab = first.tab.clone();
    drop(first);
    let state = crate::ConversationState {
        tab: old_tab.clone(),
        prefix_hash: crate::hash_messages(&[]),
        messages_seen: 0,
        system_hash: crate::hash_system(None),
        last_updated: Instant::now(),
    };
    t.close_tab(&old_tab);
    let second = sm.acquire(&cid("c"), &t).await.unwrap();
    assert_eq!(
        crate::decide(Some(&state), &second.tab, &[], state.system_hash),
        crate::PinDecision::Full
    );
}

#[tokio::test]
async fn no_healthy_tab_is_unavailable_and_keeps_the_session() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(1);
    let only = t.tab_ids().remove(0);
    drop(sm.acquire(&cid("c"), &t).await.unwrap());

    t.mark_unhealthy(&only);
    match sm.acquire(&cid("c"), &t).await {
        Err(UwaError::Unavailable(_)) => {}
        Err(other) => panic!("expected Unavailable, got {other:?}"),
        Ok(_) => panic!("a dead pool must not hand out a lease"),
    }
    assert_eq!(sm.len(), 1, "a transient outage must not drop the session");
    assert!(sm.list().await.first().map(|s| s.holders) == Some(0));

    t.mark_healthy(&only);
    let back = sm.acquire(&cid("c"), &t).await.unwrap();
    assert_eq!(back.tab, only);
}

#[tokio::test]
async fn fresh_sessions_never_land_on_unhealthy_tabs() {
    let sm = SessionManager::new(cfg(60_000, 64));
    let t = tabs(3);
    let ids = t.tab_ids();
    for dead in ids.iter().take(2) {
        t.mark_unhealthy(dead);
    }
    let alive = ids.get(2).unwrap().clone();
    for i in 0..9 {
        let h = sm.acquire(&cid(&format!("c{i}")), &t).await.unwrap();
        assert_eq!(h.tab, alive);
    }
}

#[tokio::test]
async fn fresh_sessions_skip_closed_tabs_in_the_pool() {
    let sm = SessionManager::new(cfg(60_000, 64));
    let t = tabs(3);
    let closed = t.tab_ids().first().unwrap().clone();
    t.close_tab(&closed);
    for i in 0..6 {
        let h = sm.acquire(&cid(&format!("c{i}")), &t).await.unwrap();
        assert_ne!(h.tab, closed);
    }
}

#[tokio::test]
async fn queued_waiter_counts_as_holder_and_blocks_eviction() {
    let sm = Arc::new(SessionManager::new(cfg(10, 8)));
    let t = Arc::new(tabs(1));
    let first = sm.acquire(&cid("c"), t.as_ref()).await.unwrap();

    let waiter = {
        let (sm, t) = (sm.clone(), t.clone());
        tokio::spawn(async move {
            sm.acquire(&cid("c"), t.as_ref())
                .await
                .map(|h| h.tab.clone())
        })
    };
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(sm.list().await.first().map(|s| s.holders), Some(2));
    assert!(sm.evict_idle().await.is_empty(), "leased + queued: keep");

    drop(first);
    waiter.await.unwrap().unwrap();
    assert_eq!(sm.list().await.first().map(|s| s.holders), Some(0));
}

#[tokio::test]
async fn cancelled_acquire_releases_its_lease() {
    let sm = SessionManager::new(cfg(10, 8));
    let t = tabs(1);
    let held = sm.acquire(&cid("c"), &t).await.unwrap();
    let timed_out =
        tokio::time::timeout(Duration::from_millis(20), sm.acquire(&cid("c"), &t)).await;
    assert!(timed_out.is_err(), "second acquire must be queued");
    assert_eq!(sm.list().await.first().map(|s| s.holders), Some(1));

    drop(held);
    assert_eq!(sm.list().await.first().map(|s| s.holders), Some(0));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(sm.evict_idle().await, vec![cid("c")]);
}

#[tokio::test]
async fn entry_removed_while_queued_does_not_fork_the_conversation() {
    let sm = Arc::new(SessionManager::new(cfg(60_000, 8)));
    let t = Arc::new(tabs(2));
    let first = sm.acquire(&cid("c"), t.as_ref()).await.unwrap();

    let waiter = {
        let (sm, t) = (sm.clone(), t.clone());
        tokio::spawn(async move { sm.acquire(&cid("c"), t.as_ref()).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(sm.remove(&cid("c")).is_some());
    drop(first);

    let second = waiter.await.unwrap().unwrap();
    assert_eq!(sm.len(), 1, "waiter re-resolved onto a live entry");

    let third =
        tokio::time::timeout(Duration::from_millis(50), sm.acquire(&cid("c"), t.as_ref())).await;
    assert!(third.is_err(), "same conversation ran concurrently");
    drop(second);
}

#[tokio::test]
async fn released_long_request_is_not_immediately_evicted() {
    let sm = SessionManager::new(cfg(20, 8));
    let t = tabs(1);
    let h = sm.acquire(&cid("c"), &t).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    drop(h);
    assert!(sm.evict_idle().await.is_empty());
    assert_eq!(sm.len(), 1);
}

#[tokio::test]
async fn touch_keeps_a_leased_session_fresh_after_release() {
    let sm = SessionManager::new(cfg(40, 8));
    let t = tabs(1);
    let h = sm.acquire(&cid("c"), &t).await.unwrap();
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(15)).await;
        h.touch().await;
    }
    drop(h);
    assert!(sm.evict_idle().await.is_empty());
}

#[tokio::test]
async fn capacity_eviction_takes_oldest_parked_and_spares_busy() {
    let sm = SessionManager::new(cfg(60_000, 2));
    let t = tabs(4);
    let busy = sm.acquire(&cid("busy"), &t).await.unwrap(); // oldest, but leased
    tokio::time::sleep(Duration::from_millis(5)).await;
    drop(sm.acquire(&cid("parked"), &t).await.unwrap());
    tokio::time::sleep(Duration::from_millis(5)).await;

    let newcomer = sm.acquire(&cid("new"), &t).await.unwrap();
    assert!(sm.entries.contains_key(&cid("busy")), "leased entry spared");
    assert!(
        !sm.entries.contains_key(&cid("parked")),
        "parked LRU evicted"
    );
    assert!(sm.entries.contains_key(&cid("new")));
    drop((busy, newcomer));
}

#[tokio::test]
async fn list_reports_generation_bumps() {
    let sm = SessionManager::new(cfg(60_000, 8));
    let t = tabs(2);
    drop(sm.acquire(&cid("c"), &t).await.unwrap());
    let g1 = sm.list().await.first().map(|s| s.generation).unwrap();
    drop(sm.acquire(&cid("c"), &t).await.unwrap());
    let g2 = sm.list().await.first().map(|s| s.generation).unwrap();
    assert!(g2 > g1, "every release bumps the generation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn churn_never_runs_one_conversation_twice_at_once() {
    const CONVS: usize = 4;
    let sm = Arc::new(SessionManager::new(cfg(1, 3)));
    let t = Arc::new(tabs(3));
    let inside: Arc<Vec<AtomicUsize>> = Arc::new((0..CONVS).map(|_| AtomicUsize::new(0)).collect());
    let violations = Arc::new(AtomicUsize::new(0));

    let mut workers = Vec::new();
    for w in 0..6usize {
        let (sm, t, inside, violations) =
            (sm.clone(), t.clone(), inside.clone(), violations.clone());
        workers.push(tokio::spawn(async move {
            for i in 0..40usize {
                let c = (w + i) % CONVS;
                let h = sm
                    .acquire(&cid(&format!("c{c}")), t.as_ref())
                    .await
                    .unwrap();
                let slot = inside.get(c).unwrap();
                if slot.fetch_add(1, Ordering::SeqCst) != 0 {
                    violations.fetch_add(1, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
                h.touch().await;
                slot.fetch_sub(1, Ordering::SeqCst);
                drop(h);
                // Admin `remove` is deliberately absent: it drops leased
                // entries by design (covered by its own test).
                match i % 4 {
                    0 => {
                        let _ = sm.evict_idle().await;
                    }
                    1 => {
                        let _ = sm.list().await;
                    }
                    2 => {
                        let _ = sm.recover(t.as_ref()).await;
                    }
                    _ => {}
                }
            }
        }));
    }
    for w in workers {
        w.await.unwrap();
    }
    assert_eq!(violations.load(Ordering::SeqCst), 0, "conversation forked");
    for s in sm.list().await {
        assert_eq!(s.holders, 0, "lease leaked for {}", s.conversation);
    }
}
