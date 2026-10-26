//! ПУ — панель управления оркестратором: отдельная точка входа.
//!
//! ```text
//! cargo run -p devh --bin devh-panel -- serve --addr 127.0.0.1 --port 8787
//! ```
//!
//! Сервер только читает состояние оркестратора (реестр, почта, git, `docs/`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(devh::commands::dashboard::run(&args));
}
