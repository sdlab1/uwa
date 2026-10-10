//! # uwa-extract
//!
//! Extraction layer: turns CDP network events + DOM state into a normalized
//! assistant text stream. Two backends behind one pipeline:
//!
//! * **Network-first** — SSE / JSON bodies intercepted via CDP. Produces real
//!   token deltas; preferred when the provider config declares it.
//! * **DOM-fallback** — `scraper` + CSS selectors; produces the final message.
//!   Used when network paths are encrypted or absent.
//!
//! ## Passport (public API)
//! - [`pipeline::ExtractionPipeline`] — the orchestrator
//! - [`pipeline::ExtractionOutcome`] — `{text, source}`
//! - [`pipeline::ExtractionSource`] — `Network | Dom`
//! - [`net::SseParser`] — pure SSE frame parser (no I/O)
//! - [`net::json_path_str`] — minimal JSON-path extractor for JSON bodies
//! - [`net::NetExtractor`], [`net::NetDelta`], [`net::NetRules`], [`net::NetDecoder`]
//! - [`dom::DomExtractor`] — selector-driven text extraction
//! - [`finisher::Finisher`], [`finisher::FinisherCfg`], [`finisher::FinishSignal`]
//!
//! The crate is transport-agnostic: it consumes [`uwa_core::NetworkEvent`] and
//! drives a `&dyn uwa_core::Page`. No CDP, no Chromium, no HTTP deps.

pub mod dom;
pub mod finisher;
pub mod net;
pub mod parsers;
pub mod pipeline;

pub use dom::DomExtractor;
pub use finisher::{FinishSignal, Finisher, FinisherCfg};
pub use net::{json_path_str, NetDecoder, NetDelta, NetExtractor, NetRules, SseFrame, SseParser};
pub use pipeline::{ExtractionOutcome, ExtractionPipeline, ExtractionSource, PipelineCfg};
