//! Per-provider concurrency limits.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uwa_core::{Result, UwaError};

/// Named semaphores with a shared default limit.
pub struct ProviderSemaphores {
    map: dashmap::DashMap<String, Arc<Semaphore>>,
    default_limit: usize,
}

impl ProviderSemaphores {
    pub fn new(n: usize) -> Self {
        Self {
            map: dashmap::DashMap::new(),
            default_limit: n.max(1),
        }
    }

    pub fn with_limit(self, provider: &str, n: usize) -> Self {
        self.map
            .insert(provider.to_string(), Arc::new(Semaphore::new(n.max(1))));
        self
    }

    pub fn default_limit(&self) -> usize {
        self.default_limit
    }

    pub fn available(&self, provider: &str) -> usize {
        self.map
            .get(provider)
            .map(|e| e.available_permits())
            .unwrap_or(self.default_limit)
    }

    pub async fn acquire(&self, provider: &str) -> Result<OwnedSemaphorePermit> {
        let sem: Arc<Semaphore> = {
            let entry = self
                .map
                .entry(provider.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.default_limit)));
            // Explicit `.value()`: the `RefMut` guard is dropped at the end of
            // this block, so it is never held across an `.await`.
            entry.value().clone()
        };
        sem.acquire_owned()
            .await
            .map_err(|_| UwaError::Unavailable(format!("provider `{provider}` semaphore closed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn default_limit_creates_semaphore_on_demand() {
        let sems = ProviderSemaphores::new(1);
        assert_eq!(sems.default_limit(), 1);
        let a = sems.acquire("chatgpt").await.unwrap();
        assert_eq!(
            sems.map.get("chatgpt").map(|e| e.available_permits()),
            Some(0)
        );
        drop(a);
        let b = sems.acquire("chatgpt").await.unwrap();
        drop(b);
    }

    /// Acquire with a deadline: a wrong limit must fail the test instead of
    /// parking it forever on the semaphore.
    async fn acquire_within(
        sems: &ProviderSemaphores,
        provider: &str,
        limit: usize,
    ) -> Vec<OwnedSemaphorePermit> {
        let mut held = Vec::new();
        for i in 0..limit {
            let permit = tokio::time::timeout(Duration::from_millis(200), sems.acquire(provider))
                .await
                .unwrap_or_else(|_| panic!("acquire #{i} for `{provider}` blocked past the limit"))
                .expect("acquire succeeds");
            held.push(permit);
        }
        held
    }

    #[tokio::test]
    async fn with_limit_sets_custom_semaphore() {
        let sems = ProviderSemaphores::new(2)
            .with_limit("provider-a", 3)
            .with_limit("provider-b", 5);
        assert_eq!(sems.default_limit(), 2);
        assert_eq!(
            acquire_within(&sems, "provider-a", 3).await.len(),
            3,
            "provider-a honours its own limit of 3, not the default 2"
        );
        assert_eq!(acquire_within(&sems, "provider-b", 5).await.len(), 5);
    }

    #[tokio::test]
    async fn default_limit_is_as_given() {
        let sems = ProviderSemaphores::new(0);
        assert_eq!(sems.default_limit(), 1); // max(1, n)
        let sems = ProviderSemaphores::new(5);
        assert_eq!(sems.default_limit(), 5);
    }

    #[tokio::test]
    async fn a_full_semaphore_queues_the_next_caller() {
        let sems = ProviderSemaphores::new(1);
        let held = sems.acquire("x").await.unwrap();
        // The limit is spent, so a second acquire must wait rather than succeed.
        let queued = tokio::time::timeout(Duration::from_millis(150), sems.acquire("x")).await;
        assert!(queued.is_err(), "second acquire must not jump the queue");
        drop(held);
        // Freeing the permit lets the waiter through.
        let permit = tokio::time::timeout(Duration::from_millis(200), sems.acquire("x"))
            .await
            .expect("waiter is released")
            .expect("acquire succeeds");
        drop(permit);
    }

    #[tokio::test]
    async fn acquire_releases_permit() {
        let sems = ProviderSemaphores::new(3);
        let a = sems.acquire("x").await.unwrap();
        assert_eq!(sems.map.get("x").map(|e| e.available_permits()), Some(2));
        let b = sems.acquire("x").await.unwrap();
        assert_eq!(sems.map.get("x").map(|e| e.available_permits()), Some(1));
        drop(a);
        assert_eq!(sems.map.get("x").map(|e| e.available_permits()), Some(2));
        drop(b);
        assert_eq!(sems.map.get("x").map(|e| e.available_permits()), Some(3));
    }
}
