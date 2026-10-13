//! Session table: conversation -> leased tab, with idle/capacity eviction and
//! automatic re-pinning when a tab dies.
//!
//! Invariants worth knowing before touching this file:
//!
//! * One conversation is served by at most one [`SessionHandle`] at a time
//!   (per-entry async mutex). Waiters queue on that mutex.
//! * "Busy" is an explicit counter (`leases`: live handle + queued waiters),
//!   never `Arc::strong_count` — snapshots taken by sweeps also clone the
//!   `Arc` and used to be miscounted as holders.
//! * The counter is bumped while the map shard is locked, and eviction uses
//!   `remove_if` under the same shard lock, so "check parked, then remove"
//!   cannot race with a concurrent `acquire`.
//! * A tab that stopped answering is replaced inside `acquire` (the conversation
//!   is re-pinned to a healthy tab); the pinning store notices the tab change
//!   and falls back to a full replay.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uwa_core::{ConversationId, Result, TabId, Transport, UwaError};

/// How many times `acquire` re-resolves an entry that was removed while the
/// caller was queued on it. Only reachable under absurd churn.
const MAX_ACQUIRE_ATTEMPTS: u32 = 8;

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
    /// Only ever changed while `access` is held (see `ensure_live_tab`).
    tab: StdMutex<TabId>,
    created: Instant,
    /// Milliseconds since `created`. Lock-free so that `Drop` can always
    /// record the release (the old `try_lock` silently skipped it).
    last_used_ms: AtomicU64,
    access: Arc<Mutex<()>>,
    /// Bumped on every release and whenever the tab is swapped.
    generation: AtomicU64,
    /// Live handle + queued waiters. 0 = parked.
    leases: AtomicUsize,
}

impl SessionEntry {
    fn new(tab: TabId) -> Self {
        Self {
            tab: StdMutex::new(tab),
            created: Instant::now(),
            last_used_ms: AtomicU64::new(0),
            access: Arc::new(Mutex::new(())),
            generation: AtomicU64::new(0),
            leases: AtomicUsize::new(0),
        }
    }

    fn tab(&self) -> TabId {
        self.tab
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_tab(&self, tab: TabId) {
        *self.tab.lock().unwrap_or_else(PoisonError::into_inner) = tab;
    }

    fn touch(&self) {
        let ms = u64::try_from(self.created.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_used_ms.fetch_max(ms, Ordering::AcqRel);
    }

    fn last_used(&self) -> Instant {
        let ms = self.last_used_ms.load(Ordering::Acquire);
        self.created
            .checked_add(Duration::from_millis(ms))
            .unwrap_or(self.created)
    }

    fn idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_used())
    }

    fn holders(&self) -> usize {
        self.leases.load(Ordering::Acquire)
    }

    fn is_parked(&self) -> bool {
        self.holders() == 0
    }

    fn is_evictable(&self, ttl: Duration) -> bool {
        self.is_parked() && self.idle(Instant::now()) > ttl
    }
}

/// Counts one interested party (waiter or holder) on an entry; released on
/// drop, so a cancelled `acquire` future cannot leak a lease.
struct Lease(Arc<SessionEntry>);

impl Lease {
    fn take(entry: &Arc<SessionEntry>) -> Self {
        entry.leases.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(entry))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.leases.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Immutable snapshot of one entry, safe to serialize outside the map.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub conversation: ConversationId,
    pub tab: TabId,
    pub created: Instant,
    pub last_used: Instant,
    pub idle: Duration,
    /// Number of live handles plus queued waiters (0 = parked).
    pub holders: usize,
    /// Changes on every release and on every tab swap.
    pub generation: u64,
}

pub struct SessionManager {
    entries: dashmap::DashMap<ConversationId, Arc<SessionEntry>>,
    cfg: SessionCfg,
    /// Round-robin cursor for tab picking on fresh sessions.
    rr: AtomicU64,
}

/// RAII lease on a conversation's tab. Serializes work per conversation.
pub struct SessionHandle {
    pub conversation: ConversationId,
    /// The tab to use for this turn — guaranteed to have answered a health
    /// probe at acquire time.
    pub tab: TabId,
    pub generation: u64,
    // Field order matters: the access guard is released before the lease.
    _guard: OwnedMutexGuard<()>,
    lease: Lease,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.lease.0.touch();
        self.lease.0.generation.fetch_add(1, Ordering::AcqRel);
    }
}

impl SessionHandle {
    /// Bump `last_used` — a long tool-loop must not be evicted mid-flight.
    /// (Eviction already skips leased sessions; this keeps the idle clock
    /// honest for the moment the lease ends.)
    pub async fn touch(&self) {
        self.lease.0.touch();
    }
}

/// Rotate `items` left by `offset` (clamped), without slicing or modulo.
fn rotate_by<T>(items: &mut [T], offset: usize) {
    let by = offset.min(items.len());
    items.rotate_left(by);
}

impl SessionManager {
    pub fn new(cfg: SessionCfg) -> Self {
        Self {
            entries: dashmap::DashMap::new(),
            cfg,
            rr: AtomicU64::new(0),
        }
    }

    pub fn cfg(&self) -> &SessionCfg {
        &self.cfg
    }

    /// Lease the conversation's tab, creating the session on first use.
    ///
    /// If the pinned tab no longer answers, the conversation is moved to a
    /// healthy tab instead of failing every request until idle eviction.
    pub async fn acquire(
        &self,
        cid: &ConversationId,
        transport: &dyn Transport,
    ) -> Result<SessionHandle> {
        for _ in 0..MAX_ACQUIRE_ATTEMPTS {
            let lease = match self.lease_existing(cid) {
                Some(l) => l,
                None => self.create(cid, transport).await?,
            };
            let guard = Arc::clone(&lease.0.access).lock_owned().await;
            // Explicitly removed (admin drop / recover) while we were queued:
            // our entry is orphaned, so a later caller would get a *second*
            // entry and run concurrently on another tab. Start over.
            if !self.is_current(cid, &lease.0) {
                continue;
            }
            let tab = self.ensure_live_tab(cid, &lease.0, transport).await?;
            lease.0.touch();
            let generation = lease.0.generation.load(Ordering::Acquire);
            return Ok(SessionHandle {
                conversation: cid.clone(),
                tab,
                generation,
                _guard: guard,
                lease,
            });
        }
        Err(UwaError::Unavailable(
            "session table changed too quickly to lease a tab".into(),
        ))
    }

    /// Take a lease on an existing entry. The counter is bumped while the
    /// shard read lock is held, which is what makes `remove_if` eviction safe.
    fn lease_existing(&self, cid: &ConversationId) -> Option<Lease> {
        self.entries.get(cid).map(|r| Lease::take(r.value()))
    }

    fn is_current(&self, cid: &ConversationId, entry: &Arc<SessionEntry>) -> bool {
        self.entries
            .get(cid)
            .is_some_and(|r| Arc::ptr_eq(r.value(), entry))
    }

    async fn create(&self, cid: &ConversationId, transport: &dyn Transport) -> Result<Lease> {
        self.evict_if_over_capacity();
        let tab = self.pick_tab(transport, None).await?;
        // Another task may have created the entry while we were probing
        // tabs; then its tab wins and ours is simply dropped.
        Ok(match self.entries.entry(cid.clone()) {
            dashmap::mapref::entry::Entry::Occupied(o) => Lease::take(o.get()),
            dashmap::mapref::entry::Entry::Vacant(v) => {
                let entry = Arc::new(SessionEntry::new(tab));
                let lease = Lease::take(&entry);
                v.insert(entry);
                lease
            }
        })
    }

    /// Return the entry's tab if it answers; otherwise re-pin the entry to a
    /// healthy tab. Called with the entry's access lock held.
    async fn ensure_live_tab(
        &self,
        cid: &ConversationId,
        entry: &SessionEntry,
        transport: &dyn Transport,
    ) -> Result<TabId> {
        let current = entry.tab();
        let Err(cause) = transport.health(&current).await else {
            return Ok(current);
        };
        let fresh = self.pick_tab(transport, Some(&current)).await?;
        tracing::warn!(
            conversation = %cid,
            dead_tab = %current,
            new_tab = %fresh,
            %cause,
            "pinned tab stopped answering; re-pinning conversation"
        );
        entry.set_tab(fresh.clone());
        entry.generation.fetch_add(1, Ordering::AcqRel);
        Ok(fresh)
    }

    /// Least-leased healthy tab (spreads conversations across the pool).
    /// The scan order rotates so ties don't funnel everyone onto tab #0.
    async fn pick_tab(&self, transport: &dyn Transport, avoid: Option<&TabId>) -> Result<TabId> {
        let tabs = transport.list_tabs().await?;
        if tabs.is_empty() {
            return Err(UwaError::Unavailable("no browser tabs available".into()));
        }

        let mut load: HashMap<TabId, usize> = HashMap::new();
        for e in self.entries.iter() {
            let n = load.entry(e.value().tab()).or_default();
            *n = n.saturating_add(1);
        }

        let mut candidates: Vec<TabId> = tabs.into_iter().filter(|t| Some(t) != avoid).collect();
        let cursor = self.rr.fetch_add(1, Ordering::Relaxed);
        let len = u64::try_from(candidates.len()).unwrap_or(u64::MAX);
        let offset = cursor
            .checked_rem(len)
            .and_then(|o| usize::try_from(o).ok())
            .unwrap_or(0);
        rotate_by(&mut candidates, offset);
        // Stable sort: equal load keeps the rotated order.
        candidates.sort_by_key(|t| load.get(t).copied().unwrap_or(0));

        for tab in candidates {
            if transport.health(&tab).await.is_ok() {
                return Ok(tab);
            }
        }
        Err(UwaError::Unavailable(
            "no healthy browser tabs available".into(),
        ))
    }

    /// Idle eviction. Never touches a leased session, and re-checks idleness
    /// under the shard lock so a session used since the scan survives.
    pub async fn evict_idle(&self) -> Vec<ConversationId> {
        let ttl = self.cfg.idle_ttl;
        let candidates: Vec<(ConversationId, Arc<SessionEntry>)> = self
            .entries
            .iter()
            .filter(|e| e.value().is_evictable(ttl))
            .map(|e| (e.key().clone(), Arc::clone(e.value())))
            .collect();
        candidates
            .into_iter()
            .filter(|(cid, cand)| {
                self.entries
                    .remove_if(cid, |_, cur| {
                        Arc::ptr_eq(cur, cand) && cur.is_evictable(ttl)
                    })
                    .is_some()
            })
            .map(|(cid, _)| cid)
            .collect()
    }

    /// Make room for one more session by dropping the least recently used
    /// *parked* one. If everything is busy, grow past the cap instead of
    /// forking a mid-flight conversation onto another tab.
    fn evict_if_over_capacity(&self) {
        let mut budget = self.entries.len();
        while self.entries.len() >= self.cfg.max_sessions && budget > 0 {
            budget = budget.saturating_sub(1);
            let oldest = self
                .entries
                .iter()
                .filter(|e| e.value().is_parked())
                .min_by_key(|e| e.value().last_used())
                .map(|e| (e.key().clone(), Arc::clone(e.value())));
            let Some((cid, cand)) = oldest else {
                return;
            };
            self.entries
                .remove_if(&cid, |_, cur| Arc::ptr_eq(cur, &cand) && cur.is_parked());
        }
    }

    /// Probe every distinct pinned tab once and drop the *parked* sessions
    /// whose tab no longer answers. Returns the dead tabs (deduplicated).
    ///
    /// Sessions that are in use are kept: their holder either finishes or
    /// fails, and the next `acquire` re-pins them. Removing a leased entry
    /// would let a second request run the same conversation concurrently.
    pub async fn recover(&self, transport: &dyn Transport) -> Vec<TabId> {
        let snapshot: Vec<(ConversationId, Arc<SessionEntry>)> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), Arc::clone(e.value())))
            .collect();
        let mut alive: HashMap<TabId, bool> = HashMap::new();
        let mut dead = Vec::new();
        for (cid, entry) in snapshot {
            let tab = entry.tab();
            let ok = match alive.get(&tab) {
                Some(v) => *v,
                None => {
                    let v = transport.health(&tab).await.is_ok();
                    alive.insert(tab.clone(), v);
                    if !v {
                        dead.push(tab.clone());
                    }
                    v
                }
            };
            if !ok {
                self.entries.remove_if(&cid, |_, cur| {
                    Arc::ptr_eq(cur, &entry) && cur.is_parked() && cur.tab() == tab
                });
            }
        }
        dead
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let now = Instant::now();
        let mut out: Vec<SessionInfo> = self
            .entries
            .iter()
            .map(|e| {
                let entry = e.value();
                SessionInfo {
                    conversation: e.key().clone(),
                    tab: entry.tab(),
                    created: entry.created,
                    last_used: entry.last_used(),
                    idle: entry.idle(now),
                    holders: entry.holders(),
                    generation: entry.generation.load(Ordering::Acquire),
                }
            })
            .collect();
        out.sort_by_key(|s| s.idle);
        out
    }

    /// Forget a conversation unconditionally (admin). A handle that is still
    /// alive keeps working on its tab; the next request starts a new session.
    pub fn remove(&self, cid: &ConversationId) -> Option<TabId> {
        self.entries.remove(cid).map(|(_, e)| e.tab())
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
