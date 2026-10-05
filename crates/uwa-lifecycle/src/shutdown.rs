//! Graceful shutdown: one trigger fans out to every waiter.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Cheap-to-clone shutdown signal.
#[derive(Clone, Default)]
pub struct Shutdown {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    notify: Notify,
    fired: AtomicBool,
}

impl std::fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shutdown")
            .field("fired", &self.inner.fired.load(Ordering::SeqCst))
            .finish()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve once `trigger()` has been called (immediately if it already was).
    pub async fn wait(&self) {
        loop {
            if self.inner.fired.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.inner.notify.notified();
            if self.inner.fired.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub fn trigger(&self) {
        self.inner.fired.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_triggered(&self) -> bool {
        self.inner.fired.load(Ordering::SeqCst)
    }

    /// Block until SIGINT or SIGTERM, then trigger. No-op on unsupported OSes.
    pub async fn wait_signals(&self) {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("install SIGTERM handler: {e}");
                    return;
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tracing::info!("signal received, shutting down");
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        self.trigger();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trigger_wakes_waiters() {
        let s = Shutdown::new();
        let a = s.clone();
        let b = s.clone();
        let h1 = tokio::spawn(async move { a.wait().await });
        let h2 = tokio::spawn(async move { b.wait().await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!s.is_triggered());
        s.trigger();
        tokio::time::timeout(std::time::Duration::from_secs(1), h1)
            .await
            .expect("waiter 1 hung")
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), h2)
            .await
            .expect("waiter 2 hung")
            .unwrap();
        assert!(s.is_triggered());
    }

    #[tokio::test]
    async fn wait_returns_immediately_after_trigger() {
        let s = Shutdown::new();
        s.trigger();
        tokio::time::timeout(std::time::Duration::from_millis(100), s.wait())
            .await
            .expect("already-triggered wait must not block");
    }
}
