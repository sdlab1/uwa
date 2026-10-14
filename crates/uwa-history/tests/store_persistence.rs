use std::sync::Arc;
use tempfile::tempdir;
use tokio::time::{sleep, Duration};
use uwa_history::record::{RequestSnapshot, RequestStatus};
use uwa_history::{HistoryCfg, HistoryStore, RequestRecord};

fn rec(id: &str) -> RequestRecord {
    RequestRecord::new(
        id,
        "gpt-4o",
        "chatgpt",
        RequestSnapshot {
            model: "gpt-4o".into(),
            user_preview: "x".into(),
            messages: 1,
            tools: 0,
            stream: false,
        },
    )
}

#[tokio::test]
async fn writes_jsonl_to_disk() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let cfg = HistoryCfg {
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(HistoryStore::new(cfg, true).await?);
    store.append(rec("r1")).await;
    store.append(rec("r2")).await;
    // Writer task is async; give it a moment.
    sleep(Duration::from_millis(300)).await;
    // Drop the Arc to signal the writer channel close, then flush.
    drop(store);
    sleep(Duration::from_millis(200)).await;

    let entries: Vec<_> = std::fs::read_dir(dir.path())?
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "expected one jsonl file");
    let content = std::fs::read_to_string(&entries[0].path())?;
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "two lines, got: {content}");
    let first_line = lines.first().ok_or("no first line")?;
    let second_line = lines.get(1).ok_or("no second line")?;
    assert!(first_line.contains("r1"));
    assert!(second_line.contains("r2"));
    Ok(())
}

#[tokio::test]
async fn no_disk_when_disabled() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let cfg = HistoryCfg {
        data_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let store = HistoryStore::new(cfg, false).await?;
    store.append(rec("r1")).await;
    sleep(Duration::from_millis(100)).await;
    let entries: Vec<_> = std::fs::read_dir(dir.path())?
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty(), "should not write when persist=false");
    Ok(())
}

#[tokio::test]
async fn status_filter_works() -> Result<(), Box<dyn std::error::Error>> {
    let store = HistoryStore::new(HistoryCfg::default(), false).await?;
    let mut err = rec("bad");
    err.mark_error("x");
    store.append(err).await;
    store.append(rec("good")).await;
    let errs = store.by_status(RequestStatus::Error, 10).await;
    assert_eq!(errs.len(), 1);
    let first_err = errs.first().ok_or("no error entry")?;
    assert_eq!(first_err.id, "bad");
    Ok(())
}
