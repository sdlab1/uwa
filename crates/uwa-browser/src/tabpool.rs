//! Tab pool: owns tabs, serializes access, health-checks, LRU evicts.
//!
//! ## Passport
//! - `TabPool::new(idle_ttl)` — configure
//! - `TabPool::add(tab)` / `remove(tab_id)`
//! - `TabPool::acquire() -> TabGuard` — round-robin, per-tab mutex
//! - `TabPool::evict_idle()` — LRU/TTL eviction
//! - `TabPool::list() -> Vec<TabId>`

use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uwa_core::{Result, TabId, UwaError};

#[derive(Debug)]
pub struct TabEntry {
    pub id: TabId,
    pub last_used: Mutex<Instant>,
    pub access: Arc<Mutex<()>>,
    pub healthy: AtomicBool,
}

pub struct TabPool {
    tabs: DashMap<TabId, Arc<TabEntry>>,
    rr: AtomicUsize,
    pub idle_ttl: Duration,
}

#[derive(Debug)]
pub struct TabGuard {
    pub tab: TabId,
    _guard: OwnedMutexGuard<()>,
    entry: Arc<TabEntry>,
}

impl TabPool {
    pub fn new(idle_ttl: Duration) -> Self {
        Self {
            tabs: DashMap::new(),
            rr: AtomicUsize::new(0),
            idle_ttl,
        }
    }

    pub fn add(&self, id: TabId) {
        let entry = Arc::new(TabEntry {
            id: id.clone(),
            last_used: Mutex::new(Instant::now()),
            access: Arc::new(Mutex::new(())),
            healthy: AtomicBool::new(true),
        });
        self.tabs.insert(id, entry);
    }

    pub fn remove(&self, id: &TabId) {
        self.tabs.remove(id);
    }

    pub fn list(&self) -> Vec<TabId> {
        self.tabs.iter().map(|e| e.key().clone()).collect()
    }

    pub async fn acquire(&self) -> Result<TabGuard> {
        let ids: Vec<TabId> = self.list();
        if ids.is_empty() {
            return Err(UwaError::Unavailable("no tabs in pool".into()));
        }
        let start = self.rr.fetch_add(1, Ordering::Relaxed) % ids.len();
        for offset in 0..ids.len() {
            let id = &ids[(start + offset) % ids.len()];
            let Some(entry) = self.tabs.get(id).map(|e| e.clone()) else {
                continue;
            };
            if !entry.healthy.load(Ordering::Relaxed) {
                continue;
            }
            if let Ok(guard) = entry.access.clone().try_lock_owned() {
                *entry.last_used.lock().await = Instant::now();
                return Ok(TabGuard {
                    tab: id.clone(),
                    _guard: guard,
                    entry,
                });
            }
        }
        Err(UwaError::Unavailable("all tabs busy".into()))
    }

    /// Drop tabs that haven't been used for `idle_ttl`. Called from a background task.
    pub async fn evict_idle(&self) -> Vec<TabId> {
        let now = Instant::now();
        let mut evicted = Vec::new();
        for e in self.tabs.iter() {
            let last = *e.value().last_used.lock().await;
            if now.duration_since(last) > self.idle_ttl {
                evicted.push(e.key().clone());
            }
        }
        for id in &evicted {
            self.tabs.remove(id);
        }
        evicted
    }

    pub async fn acquire_specific(&self, id: &TabId) -> Result<TabGuard> {
        let Some(entry) = self.tabs.get(id).map(|e| e.clone()) else {
            return Err(UwaError::TabNotFound(id.to_string()));
        };
        if !entry.healthy.load(Ordering::Relaxed) {
            return Err(UwaError::Unavailable(format!("tab {id} is unhealthy")));
        }
        let guard = entry
            .access
            .clone()
            .try_lock_owned()
            .map_err(|_| UwaError::Unavailable("tab is busy".into()))?;
        *entry.last_used.lock().await = Instant::now();
        Ok(TabGuard {
            tab: id.clone(),
            _guard: guard,
            entry,
        })
    }

    pub fn exists(&self, id: &TabId) -> bool {
        self.tabs.contains_key(id)
    }

    pub fn is_healthy(&self, id: &TabId) -> bool {
        self.tabs
            .get(id)
            .map(|e| e.healthy.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    pub fn mark_unhealthy(&self, id: &TabId) {
        if let Some(e) = self.tabs.get(id) {
            e.healthy.store(false, Ordering::Relaxed);
        }
    }

    pub fn mark_healthy(&self, id: &TabId) {
        if let Some(e) = self.tabs.get(id) {
            e.healthy.store(true, Ordering::Relaxed);
        }
    }

    pub fn healthy_count(&self) -> usize {
        self.tabs
            .iter()
            .filter(|e| e.value().healthy.load(Ordering::Relaxed))
            .count()
    }
}

impl Drop for TabGuard {
    fn drop(&mut self) {
        self.entry.healthy.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_round_robin() {
        let pool = TabPool::new(Duration::from_secs(60));
        pool.add(TabId::new());
        pool.add(TabId::new());
        pool.add(TabId::new());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..6 {
            let g = pool.acquire().await.unwrap();
            seen.insert(g.tab.clone());
        }
        assert_eq!(seen.len(), 3);
    }

    #[tokio::test]
    async fn acquire_fails_when_all_busy() {
        let pool = TabPool::new(Duration::from_secs(60));
        pool.add(TabId::new());
        let _g = pool.acquire().await.unwrap();
        let err = pool.acquire().await.unwrap_err();
        assert!(matches!(err, UwaError::Unavailable(_)));
    }

    #[tokio::test]
    async fn evict_idle_drops_old_tabs() {
        let pool = TabPool::new(Duration::from_millis(0));
        pool.add(TabId::new());
        tokio::time::sleep(Duration::from_millis(5)).await;
        let evicted = pool.evict_idle().await;
        assert_eq!(evicted.len(), 1);
        assert!(pool.list().is_empty());
    }

    #[tokio::test]
    async fn unhealthy_tab_is_skipped() {
        let pool = TabPool::new(Duration::from_secs(60));
        let a = TabId::new();
        let b = TabId::new();
        pool.add(a.clone());
        pool.add(b.clone());
        pool.mark_unhealthy(&a);
        for _ in 0..4 {
            let g = pool.acquire().await.unwrap();
            assert_eq!(g.tab, b);
        }
    }
}
