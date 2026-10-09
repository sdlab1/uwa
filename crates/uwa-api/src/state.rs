//! Shared application state: configuration, browser, providers and the
//! runtime services assembled at startup (tool router, sessions, semaphores,
//! circuit breakers).

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use uwa_config::Config;
use uwa_core::{Result, SiteProvider, Transport, UwaError};
use uwa_mcp::ToolRouter;
use uwa_resilience::circuit::{CircuitBreaker, CircuitCfg};
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_session::SessionManager;

/// State handed to every route. Cheap to clone: everything behind it is
/// behind an `Arc`.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub providers: Arc<ProviderRegistry>,
    pub transport: Arc<dyn Transport>,
    pub runtime: Arc<RuntimeServices>,
}

/// The services the pipeline reaches for on every request. It is replaced
/// as a whole by the `with_*` builders instead of being mutated in place,
/// so every route can keep a plain `Clone`.
#[derive(Clone)]
pub struct RuntimeServices {
    pub tool_router: Option<Arc<ToolRouter>>,
    pub sessions: Option<Arc<SessionManager>>,
    pub semaphores: Arc<ProviderSemaphores>,
    pub breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    pub history: Option<Arc<uwa_history::HistoryStore>>,
}

impl Default for RuntimeServices {
    fn default() -> Self {
        Self {
            tool_router: None,
            sessions: None,
            semaphores: Arc::new(ProviderSemaphores::new(4)),
            breakers: Arc::new(DashMap::new()),
            history: None,
        }
    }
}

impl AppState {
    /// State with the default runtime services (no MCP router, no sessions).
    pub fn minimal(
        config: Arc<Config>,
        providers: Arc<ProviderRegistry>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            config,
            providers,
            transport,
            runtime: Arc::new(RuntimeServices::default()),
        }
    }

    pub fn with_tool_router(mut self, tr: Arc<ToolRouter>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.tool_router = Some(tr);
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_sessions(mut self, sm: Arc<SessionManager>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.sessions = Some(sm);
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_semaphores(mut self, s: Arc<ProviderSemaphores>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.semaphores = s;
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_history(mut self, h: Arc<uwa_history::HistoryStore>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.history = Some(h);
        self.runtime = Arc::new(rt);
        self
    }

    /// Per-provider breaker, created on first use and shared by all routes.
    pub fn breaker(&self, provider: &str) -> Arc<CircuitBreaker> {
        self.runtime
            .breakers
            .entry(provider.into())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(provider, CircuitCfg::default())))
            .clone()
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    by_name: HashMap<String, Arc<dyn SiteProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, p: Arc<dyn SiteProvider>) {
        self.by_name.insert(p.name().to_string(), p);
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn SiteProvider>> {
        self.by_name
            .get(name)
            .cloned()
            .ok_or_else(|| UwaError::UnknownModel(name.to_string()))
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}
