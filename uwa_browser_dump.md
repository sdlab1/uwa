# Uwa_browser Dump

_Generated: 2026-10-04T16:48:33Z_

## Table of Contents

- [crates/uwa-browser/Cargo.toml](#crates-uwa-browser-cargo.toml)
- [crates/uwa-browser/src/lib.rs](#crates-uwa-browser-src-lib.rs)
- [crates/uwa-browser/src/page.rs](#crates-uwa-browser-src-page.rs)
- [crates/uwa-browser/src/tabpool.rs](#crates-uwa-browser-src-tabpool.rs)
- [crates/uwa-browser/src/transport.rs](#crates-uwa-browser-src-transport.rs)

## crates/uwa-browser/Cargo.toml

<a id="crates-uwa-browser-cargo.toml"></a>
```toml
[package]
name = "uwa-browser"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
uwa-core.workspace = true
async-trait.workspace = true
tokio = { workspace = true, features = ["process", "sync", "time", "rt", "net", "io-util"] }
dashmap.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
thiserror.workspace = true
governor = "0.6"
url.workspace = true
futures.workspace = true
tokio-tungstenite = { version = "0.24", default-features = false, features = ["connect", "handshake"] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
tokio-tungstenite = { version = "0.24", default-features = false, features = ["connect", "handshake"] }
```

