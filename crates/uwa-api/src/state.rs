//! Shared application state.

use std::collections::HashMap;
use std::sync::Arc;
use uwa_config::Config;
use uwa_core::{Result, SiteProvider, Transport, UwaError};
use uwa_mcp::ToolRouter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub providers: Arc<ProviderRegistry>,
    pub transport: Arc<dyn Transport>,
    pub tool_router: Option<Arc<ToolRouter>>,
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
