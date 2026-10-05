//! [`Transport`] mock with tabs the test controls.

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uwa_core::{Page, Result, TabId, Transport, UwaError};

use crate::page::MockPage;

/// A transport that hands out [`MockPage`]s and remembers tab health.
pub struct MockTransport {
    tabs: Vec<TabId>,
    healthy: Mutex<HashSet<TabId>>,
    pages: HashMap<TabId, Arc<MockPage>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            healthy: Mutex::new(HashSet::new()),
            pages: HashMap::new(),
        }
    }

    /// Convenience: `n` healthy tabs with default pages.
    pub fn with_n_tabs(n: usize) -> Self {
        let mut t = Self::new();
        for _ in 0..n {
            t = t.add_default_tab();
        }
        t
    }

    pub fn add_default_tab(self) -> Self {
        self.add_tab(Arc::new(MockPage::new()))
    }

    /// Register a tab backed by a specific page.
    pub fn add_tab(mut self, page: Arc<MockPage>) -> Self {
        let id = TabId::new();
        self.tabs.push(id.clone());
        self.healthy
            .lock()
            .expect("healthy lock")
            .insert(id.clone());
        self.pages.insert(id, page);
        self
    }

    /// Same, but for a tab id the test already holds.
    pub fn with_page(mut self, id: TabId, page: Arc<MockPage>) -> Self {
        self.tabs.push(id.clone());
        self.healthy
            .lock()
            .expect("healthy lock")
            .insert(id.clone());
        self.pages.insert(id, page);
        self
    }

    pub fn mark_unhealthy(&self, id: &TabId) {
        self.healthy.lock().expect("healthy lock").remove(id);
    }

    pub fn mark_healthy(&self, id: &TabId) {
        self.healthy
            .lock()
            .expect("healthy lock")
            .insert(id.clone());
    }

    pub fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.clone()
    }

    /// The mock page behind a tab, for direct assertions.
    pub fn mock_page(&self, id: &TabId) -> Option<Arc<MockPage>> {
        self.pages.get(id).cloned()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn page(&self, tab: &TabId) -> Result<Box<dyn Page>> {
        match self.pages.get(tab) {
            Some(p) => Ok(Box::new(p.as_ref().clone())),
            None => Err(UwaError::TabNotFound(tab.to_string())),
        }
    }

    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        Ok(self.tabs.clone())
    }

    async fn health(&self, tab: &TabId) -> Result<()> {
        if self.healthy.lock().expect("healthy lock").contains(tab) {
            Ok(())
        } else {
            Err(UwaError::TabNotFound(tab.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_toggles() {
        let t = MockTransport::with_n_tabs(1);
        let id = t.tab_ids()[0].clone();
        assert!(t.health(&id).await.is_ok());
        t.mark_unhealthy(&id);
        assert!(t.health(&id).await.is_err());
        t.mark_healthy(&id);
        assert!(t.health(&id).await.is_ok());
    }

    #[tokio::test]
    async fn page_for_known_tab_is_the_one_registered() {
        let page = Arc::new(MockPage::new().with_html("<b>hi</b>"));
        let id = TabId::new();
        let t = MockTransport::new().with_page(id.clone(), page);
        let got = t.page(&id).await.expect("known tab");
        assert_eq!(got.html().await.expect("html"), "<b>hi</b>");
        assert_eq!(t.list_tabs().await.expect("tabs"), vec![id]);
    }

    #[tokio::test]
    async fn page_for_unknown_tab_errors() {
        let t = MockTransport::new();
        let id = TabId::new();
        match t.page(&id).await {
            Err(e) => assert!(matches!(e, UwaError::TabNotFound(_)), "{e:?}"),
            Ok(_) => panic!("unknown tab must not produce a page"),
        }
    }
}
