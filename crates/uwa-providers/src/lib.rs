//! # uwa-providers
//!
//! Site adapters as `SiteProvider` impls.
//!
//! ## Design
//!
//! There is exactly **one** `SiteProvider` implementation:
//! [`generic::GenericProvider`]. Everything site-specific lives in
//! `ProviderCfg` (TOML): URL patterns, selectors, extraction strategy,
//! finisher tuning, network rules.
//!
//! Adding a new site is a TOML edit + a fixture HTML. No code changes.
//!
//! ## Passport (public API)
//! - [`input`] — robust JS-based text injection primitives
//! - [`generic::GenericProvider`] — the one and only `SiteProvider` impl
//! - [`registry::build_providers`] — `Config` → `HashMap<name, Arc<dyn SiteProvider>>`

pub mod generic;
pub mod input;
pub mod registry;

#[cfg(test)]
pub(crate) mod test_support;

pub use generic::GenericProvider;
pub use registry::build_providers;
