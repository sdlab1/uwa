//! Append-only JSONL store with in-memory ring buffer.

use crate::record::RequestRecord;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, RwLock};
use uwa_core::{Result, UwaError};

#[derive(Debug, Clone)]
pub struct HistoryCfg {
    pub data_dir: PathBuf,
    /// How many recent records to keep in memory for queries.
    pub buffer_size: usize,
    /// Delete JSONL files older than this many days.
    pub retention_days: u32,
    /// Truncate long strings in snapshots.
    pub max_preview_bytes: usize,
}

impl Default for HistoryCfg {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data/logs"),
            buffer_size: 1000,
            retention_days: 14,
            max_preview_bytes: 1024,
        }
    }
}

/// History store. Cloneable — inner state is `Arc`.
#[derive(Clone)]
pub struct HistoryStore {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: HistoryCfg,
    buffer: RwLock<VecDeque<RequestRecord>>,
    /// Sender for async disk writes. `None` if disabled.
    writer_tx: RwLock<Option<mpsc::Sender<RequestRecord>>>,
}

impl HistoryStore {
    /// Create the store and, if `persist` is true, spawn the writer task.
    pub async fn new(cfg: HistoryCfg, persist: bool) -> Result<Self> {
        let inner = Arc::new(Inner {
            cfg: cfg.clone(),
            buffer: RwLock::new(VecDeque::with_capacity(cfg.buffer_size)),
            writer_tx: RwLock::new(None),
        });

        if persist {
            tokio::fs::create_dir_all(&cfg.data_dir)
                .await
                .map_err(|e| UwaError::Config(format!("mkdir {}: {e}", cfg.data_dir.display())))?;

            let (tx, mut rx) = mpsc::channel::<RequestRecord>(1024);
            *inner.writer_tx.write().await = Some(tx);

            let data_dir = cfg.data_dir.clone();
            tokio::spawn(async move {
                let mut current_date = String::new();
                let mut file: Option<tokio::fs::File> = None;

                while let Some(rec) = rx.recv().await {
                    let date = date_key(&rec.started_at);
                    if date != current_date {
                        if let Some(mut f) = file.take() {
                            let _ = f.flush().await;
                        }
                        let path = data_dir.join(format!("{date}.jsonl"));
                        file = tokio::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                            .await
                            .ok();
                        current_date = date;
                    }
                    if let Some(f) = file.as_mut() {
                        if let Ok(mut line) = serde_json::to_vec(&rec) {
                            line.push(b'\n');
                            let _ = f.write_all(&line).await;
                        }
                    }
                }

                if let Some(mut f) = file {
                    let _ = f.flush().await;
                }
            });

            // Retention sweeper.
            let data_dir = cfg.data_dir.clone();
            let retention_days = cfg.retention_days;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
                loop {
                    tick.tick().await;
                    let _ = sweep_old(&data_dir, retention_days).await;
                }
            });
        }

        Ok(Self { inner })
    }

    /// Append a record. Updates the in-memory buffer and queues a disk
    /// write (non-blocking).
    pub async fn append(&self, mut rec: RequestRecord) {
        // Truncate long previews.
        let cap = self.inner.cfg.max_preview_bytes;
        truncate_preview(&mut rec.request.user_preview, cap);
        if let Some(r) = &mut rec.response {
            truncate_preview(&mut r.text_preview, cap);
        }

        // Ring buffer.
        {
            let mut buf = self.inner.buffer.write().await;
            if buf.len() >= self.inner.cfg.buffer_size {
                buf.pop_front();
            }
            buf.push_back(rec.clone());
        }

        // Disk.
        let tx = self.inner.writer_tx.read().await.clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(rec);
        }
    }

    /// Return the most recent `limit` records, newest first.
    pub async fn recent(&self, limit: usize) -> Vec<RequestRecord> {
        let buf = self.inner.buffer.read().await;
        buf.iter().rev().take(limit).cloned().collect()
    }

    /// Newest-first records matching *every* given filter.
    ///
    /// This method applies **all** filters before `take(limit)`, so combining
    /// `provider` and `status` here is correct. Do **not** reimplement this by
    /// chaining `by_provider` followed by `by_status`: each of those methods
    /// applies its own `take(limit)` first, which would silently drop matches
    /// that pass the second filter.
    pub async fn query(
        &self,
        provider: Option<&str>,
        status: Option<crate::record::RequestStatus>,
        limit: usize,
    ) -> Vec<RequestRecord> {
        let buf = self.inner.buffer.read().await;
        buf.iter()
            .rev()
            .filter(|r| provider.map_or(true, |p| r.provider == p))
            .filter(|r| status.map_or(true, |s| r.status == s))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Filter by provider.
    pub async fn by_provider(&self, provider: &str, limit: usize) -> Vec<RequestRecord> {
        let buf = self.inner.buffer.read().await;
        buf.iter()
            .rev()
            .filter(|r| r.provider == provider)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Filter by status.
    pub async fn by_status(
        &self,
        status: crate::record::RequestStatus,
        limit: usize,
    ) -> Vec<RequestRecord> {
        let buf = self.inner.buffer.read().await;
        buf.iter()
            .rev()
            .filter(|r| r.status == status)
            .take(limit)
            .cloned()
            .collect()
    }

    pub async fn len(&self) -> usize {
        self.inner.buffer.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.buffer.read().await.is_empty()
    }
}

fn date_key(t: &std::time::SystemTime) -> String {
    use chrono::{DateTime, Utc};
    let dt: DateTime<Utc> = (*t).into();
    dt.format("%Y-%m-%d").to_string()
}

async fn sweep_old(dir: &std::path::Path, retention_days: u32) -> Result<()> {
    use tokio::fs;
    let mut entries = fs::read_dir(dir)
        .await
        .map_err(|e| UwaError::Config(format!("read_dir: {e}")))?;
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            (retention_days as u64).saturating_mul(86_400),
        ))
        .unwrap_or(std::time::UNIX_EPOCH);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        // Never delete today's file, even if its mtime is odd (restored
        // backup, clock skew). The writer holds it open; sweeping it out
        // from under the appender is the only failure mode here.
        let today = date_key(&std::time::SystemTime::now());
        if path.file_stem().and_then(|s| s.to_str()) == Some(today.as_str()) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            let _ = fs::remove_file(&path).await;
            tracing::info!(file = %path.display(), "removed old history file");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{RequestSnapshot, RequestStatus};

    fn rec(id: &str, provider: &str) -> RequestRecord {
        RequestRecord::new(
            id,
            "gpt-4o",
            provider,
            RequestSnapshot {
                model: "gpt-4o".into(),
                user_preview: "hi".into(),
                messages: 1,
                tools: 0,
                stream: false,
            },
        )
    }

    #[tokio::test]
    async fn query_applies_all_filters_before_the_limit() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = HistoryStore::new(HistoryCfg::default(), false).await?;
        // Oldest -> newest. Only "a1" and "a3" are (chatgpt, Error).
        for (id, prov, st) in [
            ("a1", "chatgpt", RequestStatus::Error),
            ("b1", "claude", RequestStatus::Error),
            ("a2", "chatgpt", RequestStatus::Success),
            ("a3", "chatgpt", RequestStatus::Error),
            ("a4", "chatgpt", RequestStatus::Success),
        ] {
            let mut r = rec(id, prov);
            r.status = st;
            store.append(r).await;
        }
        let ids = |v: Vec<RequestRecord>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();

        // Both filters, newest first, and the limit counts *matches*.
        assert_eq!(
            ids(store
                .query(Some("chatgpt"), Some(RequestStatus::Error), 10)
                .await),
            ["a3", "a1"]
        );
        assert_eq!(
            ids(store
                .query(Some("chatgpt"), Some(RequestStatus::Error), 1)
                .await),
            ["a3"],
            "limit must apply after filtering"
        );
        // Single filters and no filter behave like the dedicated methods.
        assert_eq!(ids(store.query(Some("claude"), None, 10).await), ["b1"]);
        assert_eq!(ids(store.query(None, None, 2).await), ["a4", "a3"]);
        assert!(store
            .query(Some("nope"), Some(RequestStatus::Success), 10)
            .await
            .is_empty());
        assert!(store.query(None, None, 0).await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_ring_buffer() -> Result<(), Box<dyn std::error::Error>> {
        let store = HistoryStore::new(
            HistoryCfg {
                buffer_size: 3,
                ..Default::default()
            },
            false,
        )
        .await?;

        for i in 0..5 {
            store.append(rec(&format!("r{i}"), "chatgpt")).await;
        }
        let recent = store.recent(10).await;
        assert_eq!(recent.len(), 3);
        assert_eq!(recent.first().map(|r| r.id.as_str()), Some("r4"));
        assert_eq!(recent.get(2).map(|r| r.id.as_str()), Some("r2"));
        Ok(())
    }

    #[tokio::test]
    async fn filter_by_provider() -> Result<(), Box<dyn std::error::Error>> {
        let store = HistoryStore::new(HistoryCfg::default(), false).await?;
        store.append(rec("a", "chatgpt")).await;
        store.append(rec("b", "claude")).await;
        store.append(rec("c", "chatgpt")).await;
        let only = store.by_provider("chatgpt", 10).await;
        assert_eq!(only.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn filter_by_status() -> Result<(), Box<dyn std::error::Error>> {
        let store = HistoryStore::new(HistoryCfg::default(), false).await?;
        let mut r = rec("a", "chatgpt");
        r.status = RequestStatus::Error;
        store.append(r).await;
        store.append(rec("b", "chatgpt")).await;
        let errs = store.by_status(RequestStatus::Error, 10).await;
        assert_eq!(errs.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn truncates_long_previews() -> Result<(), Box<dyn std::error::Error>> {
        let store = HistoryStore::new(
            HistoryCfg {
                max_preview_bytes: 8,
                ..Default::default()
            },
            false,
        )
        .await?;
        let mut r = rec("a", "chatgpt");
        r.request.user_preview = "x".repeat(100);
        store.append(r).await;
        let recent = store.recent(1).await;
        let rec = recent.first().ok_or("expected one record")?;
        assert!(rec.request.user_preview.len() < 100);
        assert!(rec.request.user_preview.ends_with('…'));
        Ok(())
    }
}

/// Truncate `s` to at most `cap` bytes without splitting a UTF-8 character,
/// appending an ellipsis when anything was cut.
fn truncate_preview(s: &mut String, cap: usize) {
    if s.len() <= cap {
        return;
    }
    let mut cut = cap;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    s.truncate(cut);
    s.push('…');
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_preview;

    #[test]
    fn does_not_split_multibyte_chars() {
        let mut s = "é".repeat(600); // 2 bytes each
        truncate_preview(&mut s, 1023); // odd cap lands mid-char
        assert!(s.ends_with('…'));
        assert!(s.len() <= 1023 + '…'.len_utf8());
    }

    #[test]
    fn short_strings_untouched() {
        let mut s = String::from("hi");
        truncate_preview(&mut s, 10);
        assert_eq!(s, "hi");
    }
}
