//! Builds the `name -> SiteProvider` table from [`uwa_config::Config`].
//!
//! All structural checks (key == name, required selectors, presets, groups)
//! happen once in [`uwa_config::Config::validate`], which every
//! `load_from_str`/`load_from_path` runs. The builder trusts that contract.

use std::collections::HashMap;
use std::sync::Arc;

use uwa_config::Config;
use uwa_core::{Result, SiteProvider};

use uwa_extract::ExtractionPipeline;

use crate::GenericProvider;

/// Instantiate every provider declared in the config.
pub fn build_providers(config: &Config) -> Result<HashMap<String, Arc<dyn SiteProvider>>> {
    // NOTE: validation happens in `Config::validate()` — see its doc comment.
    let mut map = HashMap::new();
    for (name, cfg) in &config.providers {
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
