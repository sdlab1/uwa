//! # uwa-lifecycle
//!
//! Process-level concerns: pid file, graceful shutdown signalling.
//!
//! ## Passport (public API)
//! - [`PidFile`]
//! - [`Shutdown`]

pub mod pid;
pub mod shutdown;

pub use pid::PidFile;
pub use shutdown::Shutdown;
