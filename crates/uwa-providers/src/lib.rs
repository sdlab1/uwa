//! # uwa-providers
//!
//! Site adapters: one generic, config-driven implementation plus the DOM
//! helpers it is built from. Site-specific knowledge lives in TOML
//! (`config.example.toml`), never in code — adding a site means adding a
//! `[providers.<name>]` table.
//!
//! ## Passport (public API)
//! - [`input`] — robust DOM probes: `inject_text`, `exists`, `is_enabled`,
//!   `click_js`, `is_empty`, `count`, `wait_exists`
//! - [`GenericProvider`] — the [`uwa_core::SiteProvider`] everything runs on
//! - [`build_providers`] — `Config` -> `name -> Arc<dyn SiteProvider>`
//!
//! ## Snapshot binary
//! `cargo run -p uwa-providers --features snapshot --bin uwa-snapshot -- \
//!   --cdp http://127.0.0.1:9222 --url https://example.com/chat` dumps the
//! rendered HTML used to record extraction fixtures. It only *connects* to an
//! already-running Chromium (see `UWA_CDP_URL`).

pub mod generic;
pub mod input;
pub mod registry;

pub use generic::GenericProvider;
pub use registry::build_providers;
