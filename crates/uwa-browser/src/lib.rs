//! # uwa-browser
//!
//! Chromium/CDP layer: connect to an external browser, own a pool of tabs,
//! expose them as `uwa_core::{Transport, Page}`.
//!
//! ## Passport (public API)
//! - [`CdpTransport::connect`] — connect to a running Chromium (http or ws URL)
//! - [`CdpTransport::pool`] — the shared [`TabPool`]
//! - [`TabPool`] — round-robin, per-tab mutex, health, LRU/TTL eviction
//! - [`NetBus`] — per-target broadcast of [`uwa_core::NetworkEvent`]s
//! - [`tab_id_from_target`] / [`target_id_from_tab`]

pub mod attach;
pub mod bus;
pub mod cdp_cmd;
pub mod frame;
#[cfg(feature = "nodriver")]
pub mod nodriver;
pub mod oopif;
#[cfg(feature = "cdp")]
pub mod oopif_ws;
pub mod page;
pub mod tab_id;
pub mod tabpool;
pub mod transport;

pub use bus::NetBus;
pub use frame::FrameMap;
pub use page::CdpPageAdapter;
pub use tab_id::{tab_id_from_target, target_id_from_tab};
pub use tabpool::{TabGuard, TabPool};
pub use transport::CdpTransport;

#[cfg(feature = "nodriver")]
pub use nodriver::NodriverTransport;
