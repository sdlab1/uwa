//! Line-delimited JSON-RPC client over child process stdin/stdout.

use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use uwa_core::{Result, UwaError};

pub struct SidecarClient {
    _child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    event_rx: Mutex<Option<mpsc::Receiver<Value>>>,
}

impl SidecarClient {
    /// Spawn the Python sidecar and wait for it to answer `initialize`.
    pub async fn spawn(python: &str, script: &str) -> Result<Self> {
        let mut child = Command::new(python)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| UwaError::Transport(format!("spawn {python} {script}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| UwaError::Transport("no sidecar stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| UwaError::Transport("no sidecar stdout".into()))?;

        let (event_tx, event_rx) = mpsc::channel::<Value>(1024);
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Reader task: parse each stdout line; route responses by id,
        // forward notifications to the event channel.
        let pending_for_reader = pending.clone();
        let event_tx_for_reader = event_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = v.get("id").and_then(Value::as_i64) {
                    if let Some(tx) = pending_for_reader.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                } else if v.get("event").is_some() {
                    let _ = event_tx_for_reader.send(v).await;
                }
            }
        });

        let client = Self {
            _child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending,
            event_rx: Mutex::new(Some(event_rx)),
        };

        Ok(client)
    }

    /// Take the event receiver (one-shot; panics if called twice).
    pub fn take_event_receiver(&self) -> mpsc::Receiver<Value> {
        let mut guard = self
            .event_rx
            .try_lock()
            .expect("take_event_receiver: lock poisoned");
        guard.take().expect("take_event_receiver already taken")
    }

    /// Send a JSON-RPC request and await the response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&msg)
            .map_err(|e| UwaError::Internal(format!("serialize rpc: {e}")))?;
        line.push(b'\n');
        {
            let mut g = self.stdin.lock().await;
            g.write_all(&line)
                .await
                .map_err(|e| UwaError::Transport(format!("write to sidecar: {e}")))?;
            g.flush()
                .await
                .map_err(|e| UwaError::Transport(format!("flush sidecar: {e}")))?;
        }

        let resp = tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .map_err(|_| UwaError::Timeout(Duration::from_secs(60)))?
            .map_err(|_| UwaError::Transport("sidecar closed".into()))?;

        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(UwaError::Transport(format!("sidecar `{method}`: {msg}")));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}
