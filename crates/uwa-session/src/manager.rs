//! Session table: conversation -> leased tab, with idle/capacity eviction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uwa_core::{ConversationId, Result, TabId, Transport, UwaError};

#[derive(Debug, Clone)]
pub struct SessionCfg {
    pub idle_ttl: Duration,
    pub sweep_interval: Duration,
    pub max_sessions: usize,
}

impl Default for SessionCfg {
    fn default() -> Self {
        Self {
            idle_ttl: Duration::from_secs(30 * 60),
            sweep_interval: Duration::from_secs(60),
            max_sessions: 64,
        }
    }
}

struct SessionEntry {
    tab: TabId,
    created: Instant,
    last_used: Mutex<Instant>,
    access: Arc<Mutex<()>>,
    generation: AtomicU64,
}

/// Immutable snapshot of one entry, safe to serialize outside the map.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub conversation: ConversationId,
    pub tab: TabId,
    pub created: Instant,
    pub last_used: Instant,
    pub idle: Duration,
    /// Number of live handles (0 = parked).
    pub holders: usize,
}

pub struct SessionManager {
    entries: dashmap::DashMap<ConversationId, Arc<SessionEntry>>,
    cfg: SessionCfg,
}

/// RAII lease on a conversation's tab. Serializes work per conversation.
pub struct SessionHandle {
    pub conversation: ConversationId,
    pub tab: TabId,
    pub generation: u64,
    _guard: OwnedMutexGuard<()>,
    entry: Arc<SessionEntry>,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        if let Ok(mut g) = self.entry.last_used.try_lock() {
            *g = Instant::now();
        }
        self.entry.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl SessionHandle {
    /// Bump `last_used` — a long tool-loop must not be evicted mid-flight.
    pub async fn touch(&self) {
        *self.entry.last_used.lock().await = Instant::now();
    }
}

impl SessionManager {
    pub fn new(cfg: SessionCfg) -> Self {
        Self {
            entries: dashmap::DashMap::new(),
            cfg,
        }
    }

    pub fn cfg(&self) -> &SessionCfg {
        &self.cfg
    }

    pub async fn acquire(
        &self,
        cid: &ConversationId,
        transport: &dyn Transport,
    ) -> Result<SessionHandle> {
        if let Some(entry) = self.entries.get(cid).map(|e| e.clone()) {
            let guard = entry.access.clone().lock_owned().await;
            *entry.last_used.lock().await = Instant::now();
            let generation = entry.generation.load(Ordering::Relaxed);
            return Ok(SessionHandle {
                conversation: cid.clone(),
                tab: entry.tab.clone(),
                generation,
                _guard: guard,
                entry,
            });
        }

        self.evict_if_over_capacity().await;
        let tabs = transport.list_tabs().await?;
        let tab = tabs
            .into_iter()
            .next()
            .ok_or_else(|| UwaError::Unavailable("no browser tabs available".into()))?;

        let fresh = Arc::new(SessionEntry {
            tab: tab.clone(),
            created: Instant::now(),
            last_used: Mutex::new(Instant::now()),
            access: Arc::new(Mutex::new(())),
            generation: AtomicU64::new(0),
        });
        let entry = match self.entries.entry(cid.clone()) {
            dashmap::mapref::entry::Entry::Occupied(o) => o.get().clone(),
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(fresh.clone());
                fresh
            }
        };
        let guard = entry.access.clone().lock_owned().await;
        let generation = entry.generation.load(Ordering::Relaxed);
        Ok(SessionHandle {
            conversation: cid.clone(),
            tab: entry.tab.clone(),
            generation,
            _guard: guard,
            entry,
        })
    }

    /// Idle eviction — **no DashMap guard held across `.await`** (audit B2).
    pub async fn evict_idle(&self) -> Vec<ConversationId> {
        let mut candidates: Vec<(ConversationId, Arc<SessionEntry>, Instant)> = Vec::new();
        for e in self.entries.iter() {
            let cid = e.key().clone();
            let entry = e.value().clone();
            // Critical: the shard guard must not live through the await below.
            drop(e);
            let last = *entry.last_used.lock().await;
            candidates.push((cid, entry, last));
        }
        let now = Instant::now();
        // `candidates` itself holds one clone per entry, and the map holds the
        // original, so an untouched parked entry has a refcount of exactly 2.
        let stale: Vec<ConversationId> = candidates
            .into_iter()
            .filter(|(_, entry, last)| {
                now.duration_since(*last) > self.cfg.idle_ttl
                    && Arc::strong_count(entry) <= 2
            })
            .map(|(cid, _, _)| cid)
            .collect();
        for id in &stale {
            self.entries.remove(id);
        }
        stale
    }

    async fn evict_if_over_capacity(&self) {
        while self.entries.len() >= self.cfg.max_sessions {
            let snapshot: Vec<(ConversationId, Arc<SessionEntry>)> = self
                .entries
                .iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect();
            let mut ages: Vec<(ConversationId, Instant)> = Vec::new();
            for (cid, entry) in snapshot {
                ages.push((cid, *entry.last_used.lock().await));
            }
            ages.sort_by_key(|(_, t)| *t);
            match ages.into_iter().next() {
                Some((id, _)) => {
                    self.entries.remove(&id);
                }
                None => return,
            }
        }
    }

    /// Drop sessions whose tab no longer answers; returns the dead tab ids.
    pub async fn recover(&self, transport: &dyn Transport) -> Vec<TabId> {
        let snapshot: Vec<(ConversationId, TabId)> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), e.value().tab.clone()))
            .collect();
        let mut dead = Vec::new();
        for (cid, tab) in snapshot {
            if transport.health(&tab).await.is_err() {
                dead.push(tab);
                self.entries.remove(&cid);
            }
        }
        dead
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let snapshot: Vec<(ConversationId, Arc<SessionEntry>)> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let now = Instant::now();
        let mut out = Vec::with_capacity(snapshot.len());
        for (cid, entry) in snapshot {
            let last_used = *entry.last_used.lock().await;
            out.push(SessionInfo {
                conversation: cid,
                tab: entry.tab.clone(),
                created: entry.created,
                last_used,
                idle: now.duration_since(last_used),
                // Refs: the map entry + this function's snapshot clone.
                holders: Arc::strong_count(&entry).saturating_sub(2),
            });
        }
        out.sort_by_key(|s| s.idle);
        out
    }

    pub fn remove(&self, cid: &ConversationId) -> Option<TabId> {
        self.entries.remove(cid).map(|(_, e)| e.tab.clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests;
