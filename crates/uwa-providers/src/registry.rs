//! Builds the `name -> SiteProvider` table from [`uwa_config::Config`].

use std::collections::HashMap;
use std::sync::Arc;

use uwa_config::Config;
use uwa_core::{Result, SiteProvider, UwaError};
use uwa_extract::ExtractionPipeline;

use crate::GenericProvider;

/// Instantiate every provider declared in the config.
///
/// Fails when a TOML key disagrees with the nested `name`, or when a provider
/// is missing one of the three selectors the generic flow cannot work without.
pub fn build_providers(config: &Config) -> Result<HashMap<String, Arc<dyn SiteProvider>>> {
    let mut map = HashMap::new();
    for (name, cfg) in &config.providers {
        if cfg.name != *name {
            return Err(UwaError::Config(format!(
                "key `{name}` != name `{}`",
                cfg.name
            )));
        }
        if cfg.selectors.input.is_none()
            || cfg.selectors.send_button.is_none()
            || cfg.selectors.assistant_message.is_none()
        {
            return Err(UwaError::Config(format!(
                "provider `{name}`: missing required selectors"
            )));
        }
        map.insert(
            name.clone(),
            Arc::new(GenericProvider::new(
                cfg.clone(),
                Arc::new(ExtractionPipeline::new()),
            )) as Arc<dyn SiteProvider>,
        );
    }
    Ok(map)
}
