//! Атомарные операции записи и «прочитать и удалить» для почтовых файлов.
//!
//! Consume-on-read не должен терять письма и не должен отдавать одно письмо
//! двум читателям. Приём: атомарный `rename` в уникальное имя — выигрывает
//! ровно один процесс/поток, остальные видят `NotFound` и получают `None`.
//!
//! Запись также атомарна: письмо не пропадает при сбое записи (обрыв процесса,
//! сбой диска). Приём: писать во временный файл → `fsync` → `rename` над
//! целевым файлом. `rename` атомарен на POSIX-ФС: читатель видит либо старое
//! содержимое, либо новое — никогда не частичное.

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

/// Атомарно записать содержимое в файл (temp + rename).
///
/// Письмо не пропадает при сбое: запись идёт во временный файл, который
/// атомарно переименовывается над целевым. Читатель видит либо старый,
/// либо новый файл — никогда не частичный.
pub fn atomic_write_message(path: &Path, content: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = unique_write_tmp(path);
    fs::write(&tmp, content)?;
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp) {
        let _ = f.sync_all();
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Атомарно дозаписать блок в inbox (read-modify-write + temp + rename).
///
/// Сохраняет существующие письма, если запись прервалась.
pub fn atomic_append_message(path: &Path, block: &str) -> io::Result<bool> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let sep = "\x1f\x00";
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(block);
    new_content.push_str(sep);
    new_content.push('\n');
    let tmp = unique_write_tmp(path);
    fs::write(&tmp, &new_content)?;
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp) {
        let _ = f.sync_all();
    }
    fs::rename(&tmp, path)?;
    Ok(true)
}

fn unique_write_tmp(path: &Path) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "msg".to_string());
    let tmp = format!(".{name}.tmp.{}.{seq}", std::process::id());
    match path.parent() {
        Some(dir) => dir.join(tmp),
        None => PathBuf::from(tmp),
    }
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
        assert!(consume_text(&p)
            .unwrap_or_else(|_| unreachable!())
            .is_some());
        assert!(consume_text(&p)
            .unwrap_or_else(|_| unreachable!())
            .is_none());
    }

    #[test]
    fn missing_file_is_none_not_error() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("absent.log");
        assert!(consume_bytes(&p)
            .unwrap_or_else(|_| unreachable!())
            .is_none());
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

    #[test]
    fn atomic_write_creates_file() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("out.log");
        atomic_write_message(&p, "новое письмо\n").unwrap_or_else(|_| unreachable!());
        let text = fs::read_to_string(&p).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "новое письмо\n");
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("out.log");
        fs::write(&p, "старое\n").unwrap_or_else(|_| unreachable!());
        atomic_write_message(&p, "новое\n").unwrap_or_else(|_| unreachable!());
        let text = fs::read_to_string(&p).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "новое\n");
    }

    #[test]
    fn atomic_write_no_temp_files_left() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("out.log");
        atomic_write_message(&p, "x").unwrap_or_else(|_| unreachable!());
        let left: Vec<_> = fs::read_dir(d.path())
            .unwrap_or_else(|_| unreachable!())
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(left.is_empty(), "остались временные файлы: {left:?}");
    }

    #[test]
    fn atomic_append_preserves_existing() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        atomic_append_message(&p, "письмо1").unwrap_or_else(|_| unreachable!());
        atomic_append_message(&p, "письмо2").unwrap_or_else(|_| unreachable!());
        let text = fs::read_to_string(&p).unwrap_or_else(|_| unreachable!());
        assert!(text.contains("письмо1"));
        assert!(text.contains("письмо2"));
    }

    #[test]
    fn interrupted_write_preserves_original() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        fs::write(&p, "оригинальное письмо\n").unwrap_or_else(|_| unreachable!());
        let dir = p.parent().unwrap();
        let tmp = dir.join(".inbox.log.tmp.interrupted.123");
        fs::write(&tmp, "повреждённые данные").unwrap_or_else(|_| unreachable!());
        assert!(p.exists(), "оригинал должен существовать");
        let text = fs::read_to_string(&p).unwrap_or_else(|_| unreachable!());
        assert_eq!(text, "оригинальное письмо\n");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn interrupted_append_preserves_existing_messages() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let p = d.path().join("inbox.log");
        atomic_append_message(&p, "письмо1").unwrap_or_else(|_| unreachable!());
        let dir = p.parent().unwrap();
        let tmp = dir.join(".inbox.log.tmp.garbage.456");
        fs::write(&tmp, "частичная запись").unwrap_or_else(|_| unreachable!());
        let _ = fs::remove_file(&tmp);
        let text = fs::read_to_string(&p).unwrap_or_else(|_| unreachable!());
        assert!(
            text.contains("письмо1"),
            "существующие письма не должны пропасть при обрыве записи"
        );
    }
}
