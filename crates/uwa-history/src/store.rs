//! Append-only JSONL store with in-memory ring buffer.

use crate::record::{RequestRecord, RequestStatus};
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

/// One page request. All fields are optional except `limit`.
///
/// `before_id` is a cursor: the id of the newest record the caller has
/// already seen. Results start strictly *after* that record in the
/// newest-first ordering — i.e. strictly older. `None` starts from the
/// newest record in the ring.
///
/// If the cursor record has already been evicted from the ring (a
/// bounded buffer, so this happens after a while), the query returns an
/// empty page with `next_before_id: None`. That's the honest answer:
/// the caller asked for records older than something that no longer
/// exists, and we can't guess where to resume.
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery<'a> {
    pub limit: usize,
    pub before_id: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub status: Option<RequestStatus>,
    /// `"default"` matches records with no `profile` set.
    pub profile: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// One page of results plus enough info for the caller to know whether
/// to ask for another.
#[derive(Debug, Clone)]
pub struct HistoryPage {
    pub records: Vec<RequestRecord>,
    /// Total matching records in the scanned range (from the cursor
    /// backwards through the whole ring). This is what a UI shows as
    /// "N of M" — it's a **scan count**, not a promise that N more are
    /// reachable via the cursor if the ring evicts between pages.
    pub total_matching: usize,
    /// Pass as `before_id` to fetch the next page. `None` means there is
    /// no next page (we ran out of matches, or the ring is exhausted
    /// relative to the filters).
    pub next_before_id: Option<String>,
}

/// Whether a record satisfies every filter in `q`. Extracted so the
/// counting pass and the collecting pass share one definition.
fn matches(q: &HistoryQuery<'_>, r: &RequestRecord) -> bool {
    if let Some(p) = q.provider {
        if r.provider != p {
            return false;
        }
    }
    if let Some(s) = q.status {
        if r.status != s {
            return false;
        }
    }
    if let Some(prof) = q.profile {
        let actual = r.profile.as_deref().unwrap_or("default");
        if actual != prof {
            return false;
        }
    }
    if let Some(m) = q.model {
        if r.model != m {
            return false;
        }
    }
    true
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

    /// Filtered, cursor-paginated query over the in-memory ring.
    ///
    /// Iterates newest-first, starting after `before_id` if set. Two
    /// passes over the same range: one to count matches, one to collect
    /// up to `limit`. On a ring of 1000 records this is a nanosecond-scale
    /// difference; a single pass with a "we hit the limit but don't know
    /// if there are more" heuristic would cost the UI a round-trip on
    /// every page boundary.
    pub async fn query(&self, q: HistoryQuery<'_>) -> HistoryPage {
        let buf = self.inner.buffer.read().await;
        let limit = q.limit.clamp(1, 500);

        // Locate the cursor. Records are FIFO (oldest at index 0,
        // newest at `len() - 1`), so "older than X" is the prefix
        // `[0 .. pos_of_X)`.
        let end = match q.before_id {
            None => buf.len(),
            Some(id) => match buf.iter().position(|r| r.id == id) {
                Some(pos) => pos,
                None => {
                    // Cursor evicted; can't honour the request.
                    return HistoryPage {
                        records: Vec::new(),
                        total_matching: 0,
                        next_before_id: None,
                    };
                }
            },
        };

        let mut total_matching: usize = 0;
        let mut collected: Vec<RequestRecord> = Vec::with_capacity(limit);

        // Newest-first iteration over the prefix.
        for r in buf.iter().take(end).rev() {
            if !matches(&q, r) {
                continue;
            }
            total_matching = total_matching.saturating_add(1);
            if collected.len() < limit {
                collected.push(r.clone());
            }
        }

        // There's a next page iff we returned a full page *and* there
        // was more than a page worth of matches.
        let next_before_id = if collected.len() == limit && total_matching > limit {
            collected.last().map(|r| r.id.clone())
        } else {
            None
        };

        HistoryPage {
            records: collected,
            total_matching,
            next_before_id,
        }
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

    fn rec_named(id: &str, provider: &str, profile: Option<&str>, model: &str) -> RequestRecord {
        let mut r = RequestRecord::new(
            id,
            model,
            provider,
            RequestSnapshot {
                model: model.into(),
                user_preview: String::new(),
                messages: 1,
                tools: 0,
                stream: false,
            },
        );
        r.profile = profile.map(str::to_string);
        r
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
        let ids = |page: HistoryPage| page.records.into_iter().map(|r| r.id).collect::<Vec<_>>();

        // Both filters, newest first, and the limit counts *matches*.
        assert_eq!(
            ids(store
                .query(HistoryQuery {
                    limit: 10,
                    provider: Some("chatgpt"),
                    status: Some(RequestStatus::Error),
                    ..Default::default()
                })
                .await),
            ["a3", "a1"]
        );
        assert_eq!(
            ids(store
                .query(HistoryQuery {
                    limit: 1,
                    provider: Some("chatgpt"),
                    status: Some(RequestStatus::Error),
                    ..Default::default()
                })
                .await),
            ["a3"],
            "limit must apply after filtering"
        );
        // Single filters and no filter behave like the dedicated methods.
        assert_eq!(
            ids(store
                .query(HistoryQuery {
                    limit: 10,
                    provider: Some("claude"),
                    ..Default::default()
                })
                .await),
            ["b1"]
        );
        assert_eq!(
            ids(store
                .query(HistoryQuery {
                    limit: 2,
                    ..Default::default()
                })
                .await),
            ["a4", "a3"]
        );
        assert!(store
            .query(HistoryQuery {
                limit: 10,
                provider: Some("nope"),
                status: Some(RequestStatus::Success),
                ..Default::default()
            })
            .await
            .records
            .is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn query_returns_pages_with_cursors() {
        let store = HistoryStore::new(
            HistoryCfg {
                buffer_size: 20,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_or_else(|e| unreachable!("store: {e}"));
        for i in 0..10 {
            store
                .append(rec_named(&format!("r{i}"), "p", None, "m"))
                .await;
        }

        // Page 1 — first 4, newest first.
        let page = store
            .query(HistoryQuery {
                limit: 4,
                ..Default::default()
            })
            .await;
        assert_eq!(page.records.len(), 4);
        assert_eq!(page.records.first().map(|r| r.id.as_str()), Some("r9"));
        assert_eq!(page.records.get(3).map(|r| r.id.as_str()), Some("r6"));
        assert_eq!(page.total_matching, 10);
        assert_eq!(page.next_before_id.as_deref(), Some("r6"));

        // Page 2 — continues from the cursor.
        let page = store
            .query(HistoryQuery {
                limit: 4,
                before_id: Some("r6"),
                ..Default::default()
            })
            .await;
        assert_eq!(page.records.first().map(|r| r.id.as_str()), Some("r5"));
        assert_eq!(page.records.get(3).map(|r| r.id.as_str()), Some("r2"));
        assert_eq!(page.next_before_id.as_deref(), Some("r2"));

        // Page 3 — last two, no cursor (nothing left).
        let page = store
            .query(HistoryQuery {
                limit: 4,
                before_id: Some("r2"),
                ..Default::default()
            })
            .await;
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records.first().map(|r| r.id.as_str()), Some("r1"));
        assert_eq!(page.records.get(1).map(|r| r.id.as_str()), Some("r0"));
        assert!(page.next_before_id.is_none());
    }

    #[tokio::test]
    async fn query_pages_do_not_overlap_or_repeat() {
        let store = HistoryStore::new(
            HistoryCfg {
                buffer_size: 30,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_or_else(|e| unreachable!("store: {e}"));
        for i in 0..13 {
            store
                .append(rec_named(&format!("r{i:02}"), "p", None, "m"))
                .await;
        }
        let mut all_ids: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .query(HistoryQuery {
                    limit: 5,
                    before_id: cursor.as_deref(),
                    ..Default::default()
                })
                .await;
            for r in &page.records {
                all_ids.push(r.id.clone());
            }
            match page.next_before_id {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(all_ids.len(), 13);
        let unique: std::collections::HashSet<_> = all_ids.iter().collect();
        assert_eq!(unique.len(), 13, "pagination repeated an id");
    }

    #[tokio::test]
    async fn query_filters_by_profile() {
        let store = HistoryStore::new(HistoryCfg::default(), false)
            .await
            .unwrap_or_else(|e| unreachable!("store: {e}"));
        store.append(rec_named("a", "p", None, "m")).await;
        store.append(rec_named("b", "p", Some("work"), "m")).await;
        store.append(rec_named("c", "p", Some("work"), "m")).await;

        let work = store
            .query(HistoryQuery {
                limit: 10,
                profile: Some("work"),
                ..Default::default()
            })
            .await;
        assert_eq!(work.records.len(), 2);
        assert_eq!(work.total_matching, 2);

        let default = store
            .query(HistoryQuery {
                limit: 10,
                profile: Some("default"),
                ..Default::default()
            })
            .await;
        assert_eq!(default.records.len(), 1);
        assert_eq!(
            default.records.first().map(|r| r.id.clone()),
            Some("a".into())
        );
    }

    #[tokio::test]
    async fn query_filters_by_model() {
        let store = HistoryStore::new(HistoryCfg::default(), false)
            .await
            .unwrap_or_else(|e| unreachable!("store: {e}"));
        store.append(rec_named("a", "p", None, "gpt-4o")).await;
        store
            .append(rec_named("b", "p", None, "claude-3-5-sonnet"))
            .await;

        let r = store
            .query(HistoryQuery {
                limit: 10,
                model: Some("claude-3-5-sonnet"),
                ..Default::default()
            })
            .await;
        assert_eq!(r.records.len(), 1);
        assert_eq!(r.records.first().map(|x| x.id.clone()), Some("b".into()));
    }

    #[tokio::test]
    async fn query_combines_filters_with_and() {
        let store = HistoryStore::new(HistoryCfg::default(), false)
            .await
            .unwrap_or_else(|e| unreachable!("store: {e}"));
        store
            .append(rec_named("a", "chatgpt", Some("work"), "gpt-4o"))
            .await;
        store
            .append(rec_named("b", "chatgpt", Some("work"), "claude"))
            .await;
        store
            .append(rec_named("c", "claude", Some("work"), "gpt-4o"))
            .await;

        let r = store
            .query(HistoryQuery {
                limit: 10,
                provider: Some("chatgpt"),
                profile: Some("work"),
                model: Some("gpt-4o"),
                ..Default::default()
            })
            .await;
        assert_eq!(r.records.len(), 1);
        assert_eq!(r.records.first().map(|x| x.id.clone()), Some("a".into()));
    }

    #[tokio::test]
    async fn query_returns_empty_when_cursor_is_evicted() {
        let store = HistoryStore::new(
            HistoryCfg {
                buffer_size: 3,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_or_else(|e| unreachable!("store: {e}"));
        for i in 0..5 {
            store
                .append(rec_named(&format!("r{i}"), "p", None, "m"))
                .await;
        }
        // r0 and r1 are evicted; cursor at r1 finds nothing.
        let page = store
            .query(HistoryQuery {
                limit: 10,
                before_id: Some("r1"),
                ..Default::default()
            })
            .await;
        assert!(page.records.is_empty());
        assert!(page.next_before_id.is_none());
        assert_eq!(page.total_matching, 0);
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
