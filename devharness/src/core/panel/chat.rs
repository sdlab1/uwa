//! Чат-слой панели: треды почты и ОКНО последних сообщений (T-630).
//!
//! Источник — каноническая почта devh (`$DEVH_MSG_HOME/threads/<тред>.log`):
//! каждый блок письма завершается разделителем `SEP`, тред хранит историю
//! целиком и только дописывается.
//!
//! Ключевое свойство — **tail-read**: окно последних N сообщений читается
//! С КОНЦА файла (чанками с конца), а не «всю историю в память». Полный
//! размер треда нужен только как счётчик (потоковый подсчёт разделителей
//! без аллокации истории) — он показывается как «выше ещё N сообщений».
//!
//! Модуль ТОЛЬКО читает почту: никаких записей (запись из панели —
//! отдельная задача T-631, через канонический `devh msg send`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Сколько сообщений показываем по умолчанию (окно внизу треда).
pub const WINDOW_DEFAULT: usize = 20;

/// Размер чанка при чтении с конца.
const CHUNK: usize = 8192;

/// TTL кэша списка тредов (секунды).
const LIST_TTL_SECS: u64 = 5;

/// Письмо в окне треда.
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub tags: Vec<String>,
    pub stamp: String,
    pub body: String,
    pub place: Option<String>,
}

/// Строка списка тредов.
#[derive(Debug, Clone, Default)]
pub struct ThreadRow {
    pub id: String,
    pub participants: String,
    pub subject: String,
    pub last_stamp: String,
    pub preview: String,
    pub mtime: u64,
    /// Непрочитанные (последнее письмо треда ещё в чьём-то ящике).
    pub unread: usize,
}

/// Окно треда: сообщения + счётчик всей истории.
#[derive(Debug, Clone, Default)]
pub struct ChatWindow {
    pub thread: String,
    pub messages: Vec<ChatMessage>,
    /// Всего сообщений в треде.
    pub total: usize,
    /// Есть ли сообщения выше показанного окна.
    pub has_more: bool,
    /// Непрочитанные письма в показанном окне.
    pub unread: usize,
}

/// Корень почты (`$DEVH_MSG_HOME`).
pub fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Каталог тредов.
fn threads_dir() -> PathBuf {
    msg_home().join("threads")
}

/// Путь файла треда.
pub fn thread_path(thread: &str) -> PathBuf {
    threads_dir().join(format!("{}.log", crate::shell::msg::safe_name(thread)))
}

/// Разделитель блоков письма (`SEP` + перевод строки).
fn marker() -> String {
    format!("{}\n", crate::shell::msg::SEP)
}

/// Кэш непрочитанных ID по ящикам: (время, агент → ID).
fn unread_cache() -> &'static Mutex<Option<(u64, HashMap<String, HashSet<String>>)>> {
    static CACHE: OnceLock<Mutex<Option<(u64, HashMap<String, HashSet<String>>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Непрочитанные письма по ящикам (входящие, ещё не съеденные `msg read`).
///
/// Канон: `devh msg read` переписывает ящик, поэтому ID, ещё лежащие в
/// `inbox/<агент>.log`, — это непрочитанные письма.
fn unread_by_agent() -> HashMap<String, HashSet<String>> {
    let now = super::now_secs();
    if let Ok(map) = unread_cache().lock() {
        if let Some((ts, data)) = map.as_ref() {
            if now.saturating_sub(*ts) < LIST_TTL_SECS {
                return data.clone();
            }
        }
    }
    let mut data: HashMap<String, HashSet<String>> = HashMap::new();
    let dir = msg_home().join("inbox");
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("log") {
                continue;
            }
            let agent = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let ids: HashSet<String> = text
                .lines()
                .filter_map(|l| l.strip_prefix("ID: ").map(|v| v.trim().to_string()))
                .filter(|v| !v.is_empty())
                .collect();
            if !ids.is_empty() {
                data.insert(agent, ids);
            }
        }
    }
    if let Ok(mut map) = unread_cache().lock() {
        *map = Some((now, data.clone()));
    }
    data
}

/// Письмо ещё не прочитано (есть в чьём-то ящике).
fn is_unread(id: &str, unread: &HashMap<String, HashSet<String>>) -> bool {
    if id.is_empty() {
        return false;
    }
    unread.values().any(|set| set.contains(id))
}

/// Кэш списка тредов: (время, строки).
fn list_cache() -> &'static Mutex<Option<(u64, Vec<ThreadRow>)>> {
    static CACHE: OnceLock<Mutex<Option<(u64, Vec<ThreadRow>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Счётчик вхождений подстроки.
fn count_matches(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Потоковый подсчёт сообщений в треде (без загрузки истории в память).
fn count_blocks(path: &Path) -> usize {
    let Ok(mut f) = fs::File::open(path) else {
        return 0;
    };
    let mark = marker().into_bytes();
    let mut buf = vec![0u8; CHUNK];
    let mut count = 0usize;
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        let mut chunk = std::mem::take(&mut carry);
        chunk.extend_from_slice(&buf[..n]);
        count += count_matches(&chunk, &mark);
        let keep = mark.len().saturating_sub(1).min(chunk.len());
        carry = chunk[chunk.len() - keep..].to_vec();
    }
    count + count_matches(&carry, &mark)
}

/// Последние `want` блоков файла, прочитанные С КОНЦА.
///
/// Блоки возвращаются в хронологическом порядке (старый → новый).
fn tail_blocks(path: &Path, want: usize) -> (Vec<String>, bool) {
    let Ok(mut f) = fs::File::open(path) else {
        return (Vec::new(), true);
    };
    let Ok(meta) = f.metadata() else {
        return (Vec::new(), true);
    };
    let mark = marker();
    let mark_b = mark.as_bytes();
    let mut pos = meta.len();
    let mut buf: Vec<u8> = Vec::new();
    let mut found = 0usize;
    while pos > 0 && found < want {
        let take = CHUNK.min(pos as usize);
        pos -= take as u64;
        if f.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut chunk = vec![0u8; take];
        if f.read_exact(&mut chunk).is_err() {
            break;
        }
        chunk.extend_from_slice(&buf);
        buf = chunk;
        found = count_matches(&buf, mark_b);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut parts: Vec<String> = text.split(mark.as_str()).map(|s| s.to_string()).collect();
    // Хвостовой пустой блок (файл кончается разделителем) — не сообщение.
    if parts.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
        parts.pop();
    }
    // Первый кусок может быть обрезан (не дочитали до начала файла).
    let complete = pos == 0;
    if !complete && !parts.is_empty() {
        parts.remove(0);
    }
    (parts, complete)
}

/// Разобрать блок письма в сообщение.
fn parse_message(block: &str) -> ChatMessage {
    let v = crate::shell::msg::parse_view(block);
    ChatMessage {
        id: v.id,
        from: v.sender,
        to: v.to,
        subject: v.subject,
        tags: v.tags,
        stamp: v.stamp,
        body: v.body,
        place: v.place,
    }
}

/// Окно треда: последние `limit` сообщений, пропустив `before` самых новых.
///
/// `before` — смещение от хвоста: 0 = самые новые, 20 = «ещё на 20 выше».
pub fn window(thread: &str, limit: usize, before: usize) -> ChatWindow {
    let path = thread_path(thread);
    let total = count_blocks(&path);
    if total == 0 {
        return ChatWindow {
            thread: thread.to_string(),
            messages: Vec::new(),
            total: 0,
            has_more: false,
            unread: 0,
        };
    }
    let limit = limit.clamp(1, 200);
    // +1 блок нужен, чтобы понять: есть ли ещё сообщения выше окна.
    let want = limit.saturating_add(before).saturating_add(1);
    let (parts, _complete) = tail_blocks(&path, want);
    let end = parts.len().saturating_sub(before);
    let start = end.saturating_sub(limit);
    let messages: Vec<ChatMessage> = parts[start..end].iter().map(|b| parse_message(b)).collect();
    let unread_map = unread_by_agent();
    let unread = messages.iter().filter(|m| is_unread(&m.id, &unread_map)).count();
    ChatWindow {
        thread: thread.to_string(),
        messages,
        total,
        // Есть ли ещё сообщения выше окна — по общему счётчику истории
        // (streaming-счётчик надёжнее, чем длина прочитанного хвоста).
        has_more: total > before.saturating_add(limit),
        unread,
    }
}

/// Список тредов (свежие сверху). Кэш 5 с — список меняется медленно.
pub fn threads() -> Vec<ThreadRow> {
    let now = super::now_secs();
    if let Ok(map) = list_cache().lock() {
        if let Some((ts, rows)) = map.as_ref() {
            if now.saturating_sub(*ts) < LIST_TTL_SECS {
                return rows.clone();
            }
        }
    }
    let rows = collect_threads();
    if let Ok(mut map) = list_cache().lock() {
        *map = Some((now, rows.clone()));
    }
    rows
}

/// Собрать список тредов с диска.
fn collect_threads() -> Vec<ThreadRow> {
    let dir = threads_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut rows: Vec<ThreadRow> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let mtime = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (parts, _complete) = tail_blocks(&path, 1);
        let Some(last) = parts.last() else {
            continue;
        };
        let msg = parse_message(last);
        let preview = msg
            .body
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(90)
            .collect::<String>();
        let unread_map = unread_by_agent();
        let unread = if is_unread(&msg.id, &unread_map) { 1 } else { 0 };
        rows.push(ThreadRow {
            id: id.to_string(),
            participants: if msg.to.is_empty() {
                msg.from.clone()
            } else {
                format!("{} → {}", msg.from, msg.to)
            },
            subject: msg.subject.clone(),
            last_stamp: msg.stamp.clone(),
            preview,
            mtime,
            unread,
        });
    }
    rows.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    rows.truncate(100);
    rows
}

/// Найти последний (самый свежий) тред — открывается по умолчанию.
pub fn latest_thread() -> Option<String> {
    threads().first().map(|t| t.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("devh-chat-test-{}", std::process::id()));
        p
    }

    fn write_thread(id: &str, blocks: &[(&str, &str, &str)]) {
        let home = tmp_home();
        let dir = home.join("threads");
        fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for (from, subject, body) in blocks {
            text.push_str(&format!(
                "[12:00] ОТ: {from} — КОМУ: peer\nID: {subject}\nТРЕД: {id}\nТЕМА: {subject}\nТЕЛО:\n{body}\n{}\n",
                crate::shell::msg::SEP
            ));
        }
        fs::write(dir.join(format!("{id}.log")), text).unwrap();
    }

    #[test]
    fn window_returns_last_messages_only() {
        write_thread(
            "t-win",
            &[("a", "m1", "one"), ("a", "m2", "two"), ("a", "m3", "three")],
        );
        std::env::set_var("DEVH_MSG_HOME", tmp_home());
        let w = window("t-win", 2, 0);
        assert_eq!(w.messages.len(), 2, "окно ограничено limit");
        assert_eq!(w.total, 3, "счётчик всей истории");
        assert!(w.has_more, "есть сообщения выше окна");
        assert_eq!(w.messages[1].body.trim(), "three", "последнее снизу");
    }

    #[test]
    fn window_before_skips_newest() {
        write_thread(
            "t-before",
            &[("a", "m1", "one"), ("a", "m2", "two"), ("a", "m3", "three")],
        );
        std::env::set_var("DEVH_MSG_HOME", tmp_home());
        let w = window("t-before", 1, 1);
        assert_eq!(w.messages.len(), 1);
        assert_eq!(w.messages[0].body.trim(), "two", "пропущен один свежий");
    }

    #[test]
    fn threads_list_has_preview_and_participants() {
        write_thread("t-list", &[("lead", "отчёт", "всё сделано, P/F: P1/F0")]);
        std::env::set_var("DEVH_MSG_HOME", tmp_home());
        let rows = threads();
        assert!(rows.iter().any(|r| r.id == "t-list"), "тред в списке");
        let row = rows.iter().find(|r| r.id == "t-list").unwrap();
        assert!(
            row.preview.contains("всё сделано"),
            "превью: {}",
            row.preview
        );
        assert_eq!(row.participants, "lead → peer", "участники");
    }
}
