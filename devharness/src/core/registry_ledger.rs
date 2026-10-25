//! Журнал реестра: append-only ledger.jsonl.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub event: String,
    pub id: String,
    pub ts: u64,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupKey {
    pub event: String,
    pub id: String,
    pub ts: u64,
}

impl From<&LedgerEntry> for DedupKey {
    fn from(e: &LedgerEntry) -> Self {
        Self {
            event: e.event.clone(),
            id: e.id.clone(),
            ts: e.ts,
        }
    }
}

/// Прочитать все валидные записи (битые строки пропускаются).
pub fn read_all(path: &Path) -> Vec<LedgerEntry> {
    let Ok(f) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<LedgerEntry>(line) {
            out.push(e);
        }
    }
    out
}

/// Проверить дубликат по (event, id, ts).
pub fn is_duplicate(entries: &[LedgerEntry], e: &LedgerEntry) -> bool {
    entries
        .iter()
        .any(|x| x.event == e.event && x.id == e.id && x.ts == e.ts)
}

/// Добавить запись, если не дубликат. Возвращает true если добавлено.
pub fn append(path: &Path, entry: &LedgerEntry) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = read_all(path);
    if is_duplicate(&existing, entry) {
        return Ok(false);
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    writeln!(f, "{}", line)?;
    Ok(true)
}

/// Ограничить размер файла: если больше max_bytes, оставить последние `keep` строк.
pub fn rotate(path: &Path, max_bytes: u64, keep_lines: usize) -> std::io::Result<()> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= max_bytes {
        return Ok(());
    }
    let entries = read_all(path);
    let start = entries.len().saturating_sub(keep_lines);
    let tail = &entries[start..];
    let mut f = OpenOptions::new().write(true).truncate(true).open(path)?;
    for e in tail {
        let line = serde_json::to_string(e)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writeln!(f, "{}", line)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_and_read() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("ledger.jsonl");
        let e = LedgerEntry {
            event: "start".into(),
            id: "T-1".into(),
            ts: 10,
            detail: "x".into(),
        };
        assert!(append(&p, &e).unwrap());
        let r = read_all(&p);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], e);
    }

    #[test]
    fn dedup_skips_duplicate() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("ledger.jsonl");
        let e = LedgerEntry {
            event: "start".into(),
            id: "T-1".into(),
            ts: 10,
            detail: "".into(),
        };
        assert!(append(&p, &e).unwrap());
        assert!(!append(&p, &e).unwrap());
        assert_eq!(read_all(&p).len(), 1);
    }

    #[test]
    fn distinct_ts_both_kept() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("ledger.jsonl");
        let e1 = LedgerEntry {
            event: "start".into(),
            id: "T-1".into(),
            ts: 10,
            detail: "".into(),
        };
        let e2 = LedgerEntry {
            event: "start".into(),
            id: "T-1".into(),
            ts: 11,
            detail: "".into(),
        };
        assert!(append(&p, &e1).unwrap());
        assert!(append(&p, &e2).unwrap());
        assert_eq!(read_all(&p).len(), 2);
    }

    #[test]
    fn bad_lines_skipped() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("ledger.jsonl");
        std::fs::write(
            &p,
            "not json\n{\"event\":\"done\",\"id\":\"T-2\",\"ts\":5}\n",
        )
        .unwrap();
        let r = read_all(&p);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "T-2");
    }

    #[test]
    fn empty_file_read() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("nope.jsonl");
        assert!(read_all(&p).is_empty());
    }

    #[test]
    fn rotate_trims() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("ledger.jsonl");
        for i in 0..5 {
            let e = LedgerEntry {
                event: "e".into(),
                id: format!("T-{}", i),
                ts: i as u64,
                detail: "".into(),
            };
            append(&p, &e).unwrap();
        }
        // force rotate with small max_bytes
        rotate(&p, 1, 2).unwrap();
        let r = read_all(&p);
        assert!(r.len() <= 2);
    }
}
