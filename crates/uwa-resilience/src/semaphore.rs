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
}
