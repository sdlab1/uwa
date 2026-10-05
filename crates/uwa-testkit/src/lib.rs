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
//! - [`tool_provider::MockToolProvider`] — `ToolProvider` for `ToolRouter`
//!   tests
//! - [`server::test_server`], [`server::default_config`] — an `axum_test`
//!   server over a canned [`Config`](uwa_config::Config)

pub mod page;
pub mod provider;
pub mod server;
pub mod tool_provider;
pub mod transport;

pub use page::MockPage;
pub use provider::MockProvider;
pub use server::{default_config, test_server};
pub use tool_provider::MockToolProvider;
pub use transport::MockTransport;
