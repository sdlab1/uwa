//! Shared application state: configuration, browser, providers and the
//! runtime services assembled at startup (sessions, semaphores, circuit
//! breakers, history).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use uwa_config::Config;
use uwa_core::{Result, SiteProvider, Transport, UwaError};
use uwa_resilience::circuit::{CircuitBreaker, CircuitCfg};
use uwa_resilience::rate_limit::ProviderRateLimiters;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_session::SessionManager;

/// Registry of site providers by name.
#[derive(Clone, Default)]
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
        self.by_name.get(name).cloned().ok_or_else(|| {
            UwaError::UnknownModel(uwa_core::ModelNotFound::new(name.to_string(), vec![]))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// State handed to every route. Cheap to clone: everything behind it is
/// behind an `Arc`.
#[derive(Clone)]
pub struct AppState {
    /// Hot-reloadable config. Use `cfg()` to read, `reload()` to swap.
    pub config: Arc<ArcSwap<Config>>,
    /// Hot-reloadable provider registry. Use `registry()` to read.
    pub providers: Arc<ArcSwap<ProviderRegistry>>,
    /// Path to the config file used for `/admin/config/reload`. `None`
    /// means reload is not available.
    pub config_path: Option<PathBuf>,
    pub transport: Arc<dyn Transport>,
    pub runtime: Arc<RuntimeServices>,
}

impl AppState {
    /// Read a snapshot of the current config (cloned from the `ArcSwap`).
    pub fn cfg(&self) -> Arc<Config> {
        self.config.load_full()
    }

    /// Read a snapshot of the current provider registry.
    pub fn registry(&self) -> Arc<ProviderRegistry> {
        self.providers.load_full()
    }

    /// Atomically swap in a new config. Callers that hold `Arc<AppState>`
    //  will see the new value on the next `cfg()` call.
    pub fn reload(&self, new_config: Config) {
        self.config.store(Arc::new(new_config));
    }

    /// Atomically swap in a new provider registry.
    pub fn reload_providers(&self, new_registry: ProviderRegistry) {
        self.providers.store(Arc::new(new_registry));
    }
}

/// The services the pipeline reaches for on every request. It is replaced
/// as a whole by the `with_*` builders instead of being mutated in place,
/// so every route can keep a plain `Clone`.
#[derive(Clone)]
pub struct RuntimeServices {
    pub sessions: Option<Arc<SessionManager>>,
    pub semaphores: Arc<ProviderSemaphores>,
    pub breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    pub history: Option<Arc<uwa_history::HistoryStore>>,
    /// Conversation pinning state. `Some` by default: it's a safe
    /// optimization that also disables cleanly per-provider via config.
    pub pinning: Option<Arc<uwa_session::PinningStore>>,
    /// Single in-flight recording, when the user started one from the
    /// dashboard. Shared across requests so the status endpoint and the
    /// stop endpoint see the same recorder.
    pub recorder: Arc<tokio::sync::Mutex<Option<Arc<crate::recorder::Recorder>>>>,
    /// Ring of recent network events. `None` when the transport has no
    /// tap; the UI shows a hint in that case.
    pub events: Option<Arc<crate::events::EventLog>>,
    /// Per-provider request-rate budgets. Providers without a `rate_limit`
    /// block get an unlimited limiter from the registry.
    pub rate_limiters: Arc<ProviderRateLimiters>,
    /// Response cache. `None` when `[cache].enabled = false`. When
    /// present, only requests with `temperature = 0` consult it.
    pub cache: Option<Arc<crate::cache::ResponseCache>>,
}

impl Default for RuntimeServices {
    fn default() -> Self {
        Self {
            sessions: None,
            semaphores: Arc::new(ProviderSemaphores::new(4)),
            breakers: Arc::new(DashMap::new()),
            history: None,
            recorder: Arc::new(tokio::sync::Mutex::new(None)),
            pinning: Some(Arc::new(uwa_session::PinningStore::new(
                std::time::Duration::from_secs(30 * 60),
            ))),
            events: None,
            rate_limiters: Arc::new(ProviderRateLimiters::new()),
            cache: None,
        }
    }
}

impl AppState {
    /// State with the default runtime services (no sessions).
    pub fn minimal(
        config: Arc<Config>,
        providers: Arc<ProviderRegistry>,
        transport: Arc<dyn Transport>,
        config_path: Option<PathBuf>,
    ) -> Self {
        let config_swap: ArcSwap<Config> = ArcSwap::new(config);
        let providers_swap: ArcSwap<ProviderRegistry> = ArcSwap::new(providers);
        Self {
            config: Arc::new(config_swap),
            providers: Arc::new(providers_swap),
            config_path,
            transport,
            runtime: Arc::new(RuntimeServices::default()),
        }
    }

    /// Reload config from the stored config path, if any.
    pub fn reload_from_file(&self) -> Result<(), uwa_core::UwaError> {
        let Some(path) = &self.config_path else {
            return Err(UwaError::Config(
                "no config path configured for reload".into(),
            ));
        };
        let new_cfg = Config::load_from_path(path)
            .map_err(|e| UwaError::Config(format!("config reload failed: {e}")))?;
        self.reload(new_cfg);
        Ok(())
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

    pub fn with_pinning(mut self, p: Arc<uwa_session::PinningStore>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.pinning = Some(p);
        self.runtime = Arc::new(rt);
        self
    }

    pub fn without_pinning(mut self) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.pinning = None;
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_events(mut self, log: Arc<crate::events::EventLog>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.events = Some(log);
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_rate_limiters(
        mut self,
        rl: Arc<ProviderRateLimiters>,
    ) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.rate_limiters = rl;
        self.runtime = Arc::new(rt);
        self
    }

    pub fn with_cache(mut self, cache: Arc<crate::cache::ResponseCache>) -> Self {
        let mut rt = (*self.runtime).clone();
        rt.cache = Some(cache);
        self.runtime = Arc::new(rt);
        self
    }

    /// Per-provider breaker, created on first use and shared by all routes.
    pub fn breaker(&self, provider: &str) -> Arc<CircuitBreaker> {
        self.runtime
            .breakers
            .entry(provider.to_string())
            .or_insert_with(|| {
                Arc::new(CircuitBreaker::new(
                    provider.to_string(),
                    CircuitCfg::default(),
                ))
            })
            .clone()
    }
}
