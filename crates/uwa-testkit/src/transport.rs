//! [`Transport`] mock with tabs the test controls.

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use uwa_core::{Page, Result, TabId, Transport, UwaError};

use crate::page::MockPage;

/// A transport that hands out [`MockPage`]s and remembers tab health.
pub struct MockTransport {
    /// Statically-registered tabs (from `with_n_tabs` / `with_page`).
    tabs: Vec<TabId>,
    healthy: Mutex<HashSet<TabId>>,
    /// Tabs the "user" closed: gone from `list_tabs`, `page` and `health`.
    closed: Mutex<HashSet<TabId>>,
    pages: Mutex<HashMap<TabId, Arc<MockPage>>>,
    /// Tabs opened via `open_tab`, with their profile.
    dynamic: Mutex<Vec<(TabId, String)>>,
    /// Which profile each *static* tab belongs to. Missing = "default".
    tab_profiles: Mutex<HashMap<TabId, String>>,
    supports_profiles: bool,
    /// Rotate `list_tabs` on every call, like a tab pool handing out the
    /// next free tab.
    round_robin: bool,
    rr: AtomicUsize,
    next_dyn: AtomicUsize,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            healthy: Mutex::new(HashSet::new()),
            closed: Mutex::new(HashSet::new()),
            pages: Mutex::new(HashMap::new()),
            dynamic: Mutex::new(Vec::new()),
            tab_profiles: Mutex::new(HashMap::new()),
            supports_profiles: false,
            round_robin: false,
            rr: AtomicUsize::new(0),
            next_dyn: AtomicUsize::new(0),
        }
    }

    /// Enable multi-profile behaviour: `open_tab` creates new tabs, and
    /// `list_tabs_in` filters by profile.
    pub fn with_profiles(mut self) -> Self {
        self.supports_profiles = true;
        self
    }

    /// Hand tabs out in rotation instead of a fixed order.
    pub fn with_round_robin(mut self) -> Self {
        self.round_robin = true;
        self
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
    pub fn add_tab(self, page: Arc<MockPage>) -> Self {
        let id = TabId::new();
        println!("MockTransport: generated tab id: {}", id.as_str());
        self.with_page(id, page)
    }

    /// Same, but for a tab id the test already holds.
    pub fn with_page(self, id: TabId, page: Arc<MockPage>) -> Self {
        let mut this = self;
        this.tabs.push(id.clone());
        crate::lock(&this.healthy).insert(id.clone());
        crate::lock(&this.pages).insert(id, page);
        this
    }

    /// Register a static tab under a specific profile.
    pub fn with_page_in(self, profile: &str, id: TabId, page: Arc<MockPage>) -> Self {
        let this = self.with_page(id.clone(), page);
        if profile != "default" {
            crate::lock(&this.tab_profiles).insert(id, profile.to_string());
        }
        this
    }

    pub fn mark_unhealthy(&self, id: &TabId) {
        crate::lock(&self.healthy).remove(id);
    }

    pub fn mark_healthy(&self, id: &TabId) {
        crate::lock(&self.healthy).insert(id.clone());
    }

    /// Simulate the user closing the tab: it vanishes from `list_tabs`, and
    /// `page`/`health` report `TabNotFound` (what a real CDP target does).
    pub fn close_tab(&self, id: &TabId) {
        crate::lock(&self.closed).insert(id.clone());
    }

    fn is_closed(&self, id: &TabId) -> bool {
        crate::lock(&self.closed).contains(id)
    }

    pub fn tab_ids(&self) -> Vec<TabId> {
        let mut out = self.tabs.clone();
        for (t, _) in crate::lock(&self.dynamic).iter() {
            out.push(t.clone());
        }
        out
    }

    /// The mock page behind a tab, for direct assertions.
    pub fn mock_page(&self, id: &TabId) -> Option<Arc<MockPage>> {
        crate::lock(&self.pages).get(id).cloned()
    }

    fn profile_of(&self, id: &TabId) -> String {
        crate::lock(&self.tab_profiles)
            .get(id)
            .cloned()
            .unwrap_or_else(|| "default".into())
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
        if self.is_closed(tab) {
            return Err(UwaError::TabNotFound(tab.to_string()));
        }
        match crate::lock(&self.pages).get(tab) {
            Some(p) => Ok(Box::new(p.as_ref().clone())),
            None => Err(UwaError::TabNotFound(tab.to_string())),
        }
    }

    async fn list_tabs(&self) -> Result<Vec<TabId>> {
        let mut open: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|t| !self.is_closed(t))
            .cloned()
            .collect();
        for (t, _) in crate::lock(&self.dynamic).iter() {
            if !self.is_closed(t) {
                open.push(t.clone());
            }
        }
        if self.round_robin && !open.is_empty() {
            let start = self.rr.fetch_add(1, Ordering::SeqCst) % open.len();
            open.rotate_left(start);
        }
        Ok(open)
    }

    async fn health(&self, tab: &TabId) -> Result<()> {
        if !self.is_closed(tab) && crate::lock(&self.healthy).contains(tab) {
            Ok(())
        } else {
            Err(UwaError::TabNotFound(tab.to_string()))
        }
    }

    fn supports_profiles(&self) -> bool {
        self.supports_profiles
    }

    async fn tab_profile(&self, tab: &TabId) -> Result<Option<String>> {
        Ok(Some(self.profile_of(tab)))
    }

    /// Direct walk over both maps keeps dynamic tabs visible in one
    /// consistent pass (the trait default also works; this is cheaper).
    async fn list_tabs_in(&self, profile: &str) -> Result<Vec<TabId>> {
        if !self.supports_profiles {
            return if profile == "default" {
                self.list_tabs().await
            } else {
                Ok(Vec::new())
            };
        }
        let mut out = Vec::new();
        for t in self.tabs.iter() {
            if !self.is_closed(t) && self.profile_of(t) == profile {
                out.push(t.clone());
            }
        }
        for (t, p) in crate::lock(&self.dynamic).iter() {
            if !self.is_closed(t) && p == profile {
                out.push(t.clone());
            }
        }
        Ok(out)
    }

    async fn open_tab(&self, profile: &str, _url: &str) -> Result<TabId> {
        if !self.supports_profiles {
            return Err(UwaError::Config(
                "MockTransport was not built with .with_profiles()".into(),
            ));
        }
        let n = self.next_dyn.fetch_add(1, Ordering::SeqCst);
        let id = TabId::from_raw(format!("dyn_{n}"));
        crate::lock(&self.dynamic).push((id.clone(), profile.to_string()));
        crate::lock(&self.healthy).insert(id.clone());
        crate::lock(&self.pages).insert(id.clone(), Arc::new(MockPage::new()));
        Ok(id)
    }

    async fn close_profile(&self, profile: &str) -> Result<Vec<TabId>> {
        if profile == "default" {
            return Err(UwaError::Config("cannot close the default profile".into()));
        }
        let mut dropped = Vec::new();
        // Static tabs assigned to this profile.
        {
            let mut profs = crate::lock(&self.tab_profiles);
            let matching: Vec<TabId> = profs
                .iter()
                .filter(|(_, p)| p.as_str() == profile)
                .map(|(t, _)| t.clone())
                .collect();
            for t in matching {
                profs.remove(&t);
                dropped.push(t);
            }
        }
        // Dynamic tabs.
        {
            let mut dyn_tabs = crate::lock(&self.dynamic);
            dyn_tabs.retain(|(t, p)| {
                if p == profile {
                    dropped.push(t.clone());
                    false
                } else {
                    true
                }
            });
        }
        for t in &dropped {
            crate::lock(&self.healthy).remove(t);
            crate::lock(&self.pages).remove(t);
        }
        Ok(dropped)
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
        println!("MockTransport: generated tab id: {}", id.as_str());
        let t = MockTransport::new().with_page(id.clone(), page);
        let got = t.page(&id).await.expect("known tab");
        assert_eq!(got.html().await.expect("html"), "<b>hi</b>");
        assert_eq!(t.list_tabs().await.expect("tabs"), vec![id]);
    }

    #[tokio::test]
    async fn round_robin_rotates_the_first_tab() {
        let t = MockTransport::with_n_tabs(3).with_round_robin();
        let first: Vec<TabId> = vec![
            t.list_tabs().await.expect("1")[0].clone(),
            t.list_tabs().await.expect("2")[0].clone(),
            t.list_tabs().await.expect("3")[0].clone(),
        ];
        let distinct: std::collections::HashSet<_> = first.iter().collect();
        assert_eq!(
            distinct.len(),
            3,
            "each call must lead with a new tab: {first:?}"
        );
    }

    #[tokio::test]
    async fn page_for_unknown_tab_errors() {
        let t = MockTransport::new();
        let id = TabId::new();
        println!("MockTransport: generated tab id: {}", id.as_str());
        match t.page(&id).await {
            Err(e) => assert!(matches!(e, UwaError::TabNotFound(_)), "{e:?}"),
            Ok(_) => panic!("unknown tab must not produce a page"),
        }
    }

    #[tokio::test]
    async fn closed_tab_disappears_everywhere() {
        let t = MockTransport::with_n_tabs(2);
        let ids = t.tab_ids();
        let gone = ids.first().expect("tab 0").clone();
        let kept = ids.get(1).expect("tab 1").clone();
        t.close_tab(&gone);
        assert_eq!(t.list_tabs().await.expect("tabs"), vec![kept.clone()]);
        assert!(t.health(&gone).await.is_err());
        assert!(t.page(&gone).await.is_err());
        assert!(t.health(&kept).await.is_ok());
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[tokio::test]
    async fn open_tab_creates_a_tab_in_the_profile() {
        let t = MockTransport::new().with_profiles();
        let a = t.open_tab("work", "about:blank").await.unwrap();
        let b = t.open_tab("work", "about:blank").await.unwrap();
        assert_ne!(a, b);
        let work = t.list_tabs_in("work").await.unwrap_or_default();
        assert_eq!(work.len(), 2);
        assert!(t
            .list_tabs_in("personal")
            .await
            .unwrap_or_default()
            .is_empty());
    }

    #[tokio::test]
    async fn close_profile_drops_every_tab_in_it() {
        let t = MockTransport::with_n_tabs(1).with_profiles();
        let _ = t.open_tab("work", "about:blank").await;
        let _ = t.open_tab("work", "about:blank").await;
        let dropped = t.close_profile("work").await.unwrap_or_default();
        assert_eq!(dropped.len(), 2);
        assert!(t.list_tabs_in("work").await.unwrap_or_default().is_empty());
        // The default tab survives.
        assert_eq!(t.list_tabs().await.unwrap_or_default().len(), 1);
    }

    #[tokio::test]
    async fn close_default_profile_is_rejected() {
        let t = MockTransport::new().with_profiles();
        assert!(t.close_profile("default").await.is_err());
    }

    #[tokio::test]
    async fn open_tab_without_profiles_is_a_config_error() {
        let t = MockTransport::with_n_tabs(1);
        assert!(t.open_tab("work", "about:blank").await.is_err());
    }

    #[tokio::test]
    async fn static_tabs_default_to_the_default_profile() {
        let t = MockTransport::with_n_tabs(1).with_profiles();
        let tabs = t.list_tabs().await.unwrap_or_default();
        let id = tabs.first().cloned().unwrap_or_default();
        assert_eq!(
            t.tab_profile(&id).await.unwrap_or(None).as_deref(),
            Some("default")
        );
        // Static tab registered into a named profile.
        let t2 = MockTransport::new().with_profiles().with_page_in(
            "work",
            TabId::from_raw("s1"),
            Arc::new(MockPage::new()),
        );
        let id2 = TabId::from_raw("s1");
        assert_eq!(
            t2.tab_profile(&id2).await.unwrap_or(None).as_deref(),
            Some("work")
        );
        assert_eq!(t2.list_tabs_in("work").await.unwrap_or_default().len(), 1);
    }

    #[tokio::test]
    async fn closed_dynamic_tab_leaves_its_profile() {
        let t = MockTransport::new().with_profiles();
        let id = t.open_tab("work", "about:blank").await.unwrap();
        t.close_tab(&id);
        assert!(t.list_tabs_in("work").await.unwrap_or_default().is_empty());
    }
}
