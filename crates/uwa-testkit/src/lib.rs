//! # uwa-testkit
//!
//! Shared test doubles for the whole workspace. Tests import from here
//! instead of rolling their own `MockPage`/`MockTransport`/`FakeProvider`.
//!
//! ## Passport (public API)
//! - [`page::MockPage`] — programmable [`Page`](uwa_core::Page): script
//!   matching + HTML + network events + an eval log
//! - [`transport::MockTransport`] — [`Transport`](uwa_core::Transport) with
//!   controllable tabs
//! - [`provider::MockProvider`] — [`SiteProvider`](uwa_core::SiteProvider)
//!   with a programmable answer and optional delay
//! - [`server::AppBuilder`], [`server::TestApp`] — one-shot
//!   [`AppState`](uwa_api::AppState) + `axum_test` server
//! - [`config::default_config`], [`config::config_with_key`] — canned
//!   [`Config`](uwa_config::Config)s

pub mod chromium;
pub mod config;
pub mod page;
pub mod provider;
pub mod server;
pub mod transport;

pub use chromium::{chromium_ws_url, normalize_cdp_url, CANONICAL_CDP};
pub use config::{config_with_key, default_config, TEST_API_KEY, TEST_AUTH_HEADER};
pub use page::MockPage;
pub use provider::MockProvider;
pub use server::{test_server, AppBuilder, TestApp};
pub use transport::MockTransport;
