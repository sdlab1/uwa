//! Атомарное «прочитать и удалить» для почтовых файлов.
//!
//! Consume-on-read не должен терять письма и не должен отдавать одно письмо
//! двум читателям. Приём: атомарный `rename` в уникальное имя — выигрывает
//! ровно один процесс/поток, остальные видят `NotFound` и получают `None`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_sibling(path: &Path) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "msg".to_string());
    let tmp = format!(".{name}.consumed.{}.{seq}", std::process::id());
    match path.parent() {
        Some(dir) => dir.join(tmp),
        None => PathBuf::from(tmp),
    }
}

/// Прочитать и удалить файл атомарно. `None` — файла уже нет (кто-то забрал).
pub fn consume_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let tmp = unique_sibling(path);
    match fs::rename(path, &tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let data = fs::read(&tmp);
    let _ = fs::remove_file(&tmp);
    data.map(Some)
}

/// То же, но как UTF-8 (потерянные байты заменяются — почта текстовая).
pub fn consume_text(path: &Path) -> io::Result<Option<String>> {
    Ok(consume_bytes(path)?.map(|b| String::from_utf8_lossy(&b).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn consume_returns_content_and_removes_file() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        fs::write(&p, "письмо\n").unwrap_or_else(|_| unreachable!());
        assert_eq!(
            consume_text(&p).unwrap_or_else(|_| unreachable!()),
            Some("письмо\n".to_string())
        );
        assert!(!p.exists(), "файл должен исчезнуть");
    }

    #[test]
    fn second_consume_is_none() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        fs::write(&p, "x").unwrap_or_else(|_| unreachable!());
        assert!(consume_text(&p).unwrap_or_else(|_| unreachable!()).is_some());
        assert!(consume_text(&p).unwrap_or_else(|_| unreachable!()).is_none());
    }

    #[test]
    fn missing_file_is_none_not_error() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("absent.log");
        assert!(consume_bytes(&p).unwrap_or_else(|_| unreachable!()).is_none());
    }

    #[test]
    fn empty_file_is_consumed_once() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("empty.log");
        fs::write(&p, "").unwrap_or_else(|_| unreachable!());
        assert_eq!(
            consume_text(&p).unwrap_or_else(|_| unreachable!()),
            Some(String::new())
        );
        assert!(!p.exists());
    }

    #[test]
    fn binary_content_is_preserved() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("bin.log");
        let bytes = vec![0u8, 159, 146, 150, b'\n'];
        fs::write(&p, &bytes).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            consume_bytes(&p).unwrap_or_else(|_| unreachable!()),
            Some(bytes)
        );
    }

    #[test]
    fn no_temp_files_left_behind() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        fs::write(&p, "x").unwrap_or_else(|_| unreachable!());
        let _ = consume_text(&p).unwrap_or_else(|_| unreachable!());
        let left: Vec<_> = fs::read_dir(d.path())
            .unwrap_or_else(|_| unreachable!())
            .flatten()
            .collect();
        assert!(left.is_empty(), "остались файлы: {left:?}");
    }

    #[test]
    fn concurrent_consumers_exactly_one_wins() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = Arc::new(d.path().join("inbox.log"));
        fs::write(&*p, "единственное письмо\n").unwrap_or_else(|_| unreachable!());
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::new();
        for _ in 0..n {
            let p = Arc::clone(&p);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                consume_text(&p).unwrap_or_else(|_| unreachable!())
            }));
        }
        let got: Vec<Option<String>> = handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| unreachable!()))
            .collect();
        let winners: Vec<_> = got.iter().filter(|r| r.is_some()).collect();
        assert_eq!(winners.len(), 1, "письмо получил ровно один: {got:?}");
        assert_eq!(winners[0].as_deref(), Some("единственное письмо\n"));
        assert!(!p.exists());
    }
}
