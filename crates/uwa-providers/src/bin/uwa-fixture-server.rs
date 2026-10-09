//! Serves synthetic provider HTML fixtures over localhost so integration
//! tests can exercise the full pipeline against a real browser without
//! touching the real sites.
//!
//! Each fixture provides:
//! * a fake input field with a stable id,
//! * a send button,
//! * a scripted "assistant" element that fills in with a canned reply.
//!
//! ```bash
//! cargo run -p uwa-providers --features fixture-server --bin uwa-fixture-server -- \
//!   --addr 127.0.0.1:0   # binds an ephemeral port, prints URL=... to stdout
//! ```

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use clap::Parser;

#[derive(Parser)]
struct Cli {
    /// Listen address. Use port 0 for ephemeral.
    #[arg(long, default_value = "127.0.0.1:0")]
    addr: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let app = router();

    let listener = tokio::net::TcpListener::bind(&cli.addr).await?;
    let local = listener.local_addr()?;
    println!("PORT={}", local.port());
    println!("URL=http://{}", local);

    axum::serve(listener, app).await?;
    Ok(())
}

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/fake/chat", get(fake_chat))
        .route("/fake/claude", get(fake_claude))
        .route("/fake/gemini", get(fake_gemini))
}

async fn index() -> Html<&'static str> {
    Html(
        "<html><body><h1>uwa fixture server</h1><ul>\
         <li><a href='/fake/chat'>/fake/chat</a></li>\
         <li><a href='/fake/claude'>/fake/claude</a></li>\
         <li><a href='/fake/gemini'>/fake/gemini</a></li>\
         </ul></body></html>",
    )
}

async fn fake_chat() -> Html<&'static str> {
    Html(include_str!("../../tests/fixtures/serve/chat.html"))
}

async fn fake_claude() -> Html<&'static str> {
    Html(include_str!("../../tests/fixtures/serve/claude.html"))
}

async fn fake_gemini() -> Html<&'static str> {
    Html(include_str!("../../tests/fixtures/serve/gemini.html"))
}
