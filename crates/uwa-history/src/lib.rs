//! # uwa-history
//!
//! Request history and statistics for `uwa`.
//!
//! ## Design
//!
//! * **Append-only JSONL files** under `<data_dir>/logs/YYYY-MM-DD.jsonl`.
//!   One JSON object per line, greppable with `jq`, no DB dependency.
//! * **In-memory ring buffer** (last N records, default 1000) for fast
//!   dashboard queries.
//! * **Retention** — delete files older than `retention_days`.
//! * **Statistics** — computed on demand from the ring buffer; per-provider
//!   error rates, timing percentiles, tab utilization.
//!
//! ## Passport (public API)
//! - [`record::RequestRecord`] — one full request/response
//! - [`record::RequestTiming`] — per-stage timings
//! - [`record::RequestStatus`] — Pending | Success | Error
//! - [`store::HistoryStore`] — append + query
//! - [`stats::Stats`] — aggregated metrics
//! - [`stats::ProviderStats`] — per-provider rollup

pub mod record;
pub mod stats;
pub mod store;

pub use record::{RequestRecord, RequestSnapshot, RequestStatus, RequestTiming, ResponseSnapshot};
pub use stats::{ProviderStats, Stats, StatsWindow};
pub use store::{HistoryCfg, HistoryStore};
