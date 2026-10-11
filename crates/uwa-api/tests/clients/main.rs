//! Wire-shape compatibility tests.
//!
//! Each module below reproduces the request bytes a specific client sends
//! and asserts the response deserializes back through that client's
//! expectations. When a client changes its wire shape and our tests still
//! pass, we know we're compatible; when they fail, we know what to fix.
//!
//! The tests use the same `MockProvider` and `MockTransport` as the rest
//! of the suite — no browser, no network.

mod aider;
mod claude_code;
mod codex_cli;
mod continue_vscode;
mod cursor;
mod openai_sdk;
