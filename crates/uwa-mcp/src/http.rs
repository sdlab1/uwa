//! Optional HTTP + SSE transport for the MCP server (feature `mcp-http`).
//!
//! - `POST /mcp`      — one JSON-RPC message in, one JSON-RPC message out.
//! - `GET  /mcp/sse`  — announces the message endpoint; traffic stays on `/mcp`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::trace::TraceLayer;

use crate::protocol::JsonRpcRequest;
use crate::server::McpServer;

pub fn router(server: Arc<McpServer>) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp))
        .route("/mcp/sse", get(sse))
        .layer(TraceLayer::new_for_http())
        .with_state(server)
}

/// A JSON-RPC message in, a JSON-RPC message out. Notifications carry no id
/// and are accepted with `202 Accepted` and an empty body.
async fn post_mcp(
    State(server): State<Arc<McpServer>>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let resp = server.dispatch_public(req).await;
    Json(resp).into_response()
}

/// Announce where messages should be POSTed. The stream stays open until the
/// client goes away.
async fn sse(
    State(server): State<Arc<McpServer>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(4);
    let endpoint = format!("/mcp?sessionId={}", server.name());
    let _ = tx
        .send(Ok(Event::default().event("endpoint").data(&endpoint)))
        .await;
    drop(tx);
    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}
