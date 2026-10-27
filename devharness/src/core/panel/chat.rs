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
    static CACHE: OnceLock<Mutex<Option<(u64, HashMap<String, HashSet<String>>)>>> =
        OnceLock::new();
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
    let unread = messages
        .iter()
        .filter(|m| is_unread(&m.id, &unread_map))
        .count();
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
        let unread = if is_unread(&msg.id, &unread_map) {
            1
        } else {
            0
        };
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

// ─────────────────────────── WRITE-КОНТУР (T-631) ───────────────────────────
//
// Панель по умолчанию — наблюдатель. Запись включается ТОЛЬКО явно:
//   PANEL_WRITE=1            — флаг записи (строго «1»);
//   PANEL_TOKEN / PANEL_TOKEN_FILE — токен оператора (сравнение constant-time).
//
// Отправка идёт КАНОНИЧЕСКИМ путём: дочерний процесс `devh msg send`.
// Панель НИКОГДА не пишет в файлы почты сама и не подделывает отправителя:
// `--from` не передаётся, адрес определяет `devh msg send` (safe_from, T-486).
// Каждое письмо попадает в журнал прав `.harness/orch/settings_ledger.jsonl`.

/// Максимальный размер тела письма (64 КБ).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Ключ конфига: флаг записи (`PANEL_WRITE=1`).
pub const CFG_WRITE: &str = "PANEL_WRITE";
/// Ключ конфига: токен оператора (в `.harness/config`).
pub const CFG_TOKEN: &str = "PANEL_TOKEN";
/// Ключ конфига: путь к файлу с токеном оператора (предпочтительнее конфига).
pub const CFG_TOKEN_FILE: &str = "PANEL_TOKEN_FILE";

/// TTL кэша имени отправителя (секунды).
const IDENTITY_TTL_SECS: u64 = 30;

/// Режим записи панели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteGate {
    /// Запись выключена: `PANEL_WRITE` не равен `1`.
    Disabled,
    /// Запись включена, но токен оператора не задан — писать нельзя.
    NoToken,
    /// Запись включена, токен есть.
    Enabled,
}

/// Значение ключа из `.harness/config` (кавычки снимаются).
pub fn config_value(root: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(root.join(".harness").join("config")).ok()?;
    config_value_in(&text, key)
}

/// Значение ключа из текста конфига (кавычки снимаются).
pub fn config_value_in(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            .unwrap_or(v);
        return Some(v.to_string());
    }
    None
}

/// Токен оператора: из файла (`PANEL_TOKEN_FILE`) или из конфига.
pub fn operator_token(root: &Path) -> Option<String> {
    if let Some(p) = config_value(root, CFG_TOKEN_FILE) {
        let p = p.trim();
        if !p.is_empty() {
            if let Ok(t) = fs::read_to_string(p) {
                let t = t.trim().to_string();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    config_value(root, CFG_TOKEN)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Режим записи панели по конфигу.
pub fn write_gate(root: &Path) -> WriteGate {
    let on = config_value(root, CFG_WRITE)
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if !on {
        return WriteGate::Disabled;
    }
    if operator_token(root).is_none() {
        return WriteGate::NoToken;
    }
    WriteGate::Enabled
}

/// Сравнение строк за постоянное время (секрет не утекает по времени).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut diff = (ab.len() ^ bb.len()) as u8;
    let n = ab.len().max(bb.len());
    for i in 0..n {
        let x = ab.get(i).copied().unwrap_or(0);
        let y = bb.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// Композер письма (то, что оператор набрал в панели).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    /// Получатель: канонический адрес агента или `all`.
    pub to: String,
    /// Тема.
    pub subject: String,
    /// Тело письма.
    pub body: String,
    /// Тред (пусто — новый тред).
    pub thread: String,
    /// Токен оператора (из формы).
    pub token: String,
}

/// Разобрать `application/x-www-form-urlencoded` в композер.
pub fn parse_form(body: &str) -> Composer {
    let mut c = Composer::default();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = url_decode(k);
        let val = url_decode(v);
        match key.as_str() {
            "to" => c.to = val,
            "subject" => c.subject = val,
            "body" => c.body = val,
            "thread" => c.thread = val,
            "token" => c.token = val,
            _ => {}
        }
    }
    c
}

/// Раскодировать percent-encoding (`+` → пробел).
pub fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes.get(i).copied().unwrap_or(0);
        match b {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Результат отправки письма.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendReport {
    /// ID письма.
    pub id: String,
    /// Тред (ответ агента придёт в него же).
    pub thread: String,
    /// Получатели (как их перечислил `devh msg send`).
    pub recipients: String,
    /// Фактический отправитель (по тред-логу).
    pub sender: String,
}

/// Путь к бинарю `devh` (сосед по target/, иначе — PATH).
fn devh_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("devh");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from("devh")
}

/// Отправить письмо каноническим путём (`devh msg send`, дочерний процесс).
pub fn send_letter(root: &Path, c: &Composer) -> Result<SendReport, String> {
    // 1) гейт записи: панель read-only по умолчанию.
    match write_gate(root) {
        WriteGate::Disabled => {
            return Err("панель read-only: запись включается только PANEL_WRITE=1".to_string())
        }
        WriteGate::NoToken => {
            return Err("панель read-only: не задан токен оператора (PANEL_TOKEN)".to_string())
        }
        WriteGate::Enabled => {}
    }
    // 2) токен оператора (constant-time).
    let expected = operator_token(root).unwrap_or_default();
    if !constant_time_eq(c.token.trim(), &expected) {
        return Err("токен неверен".to_string());
    }
    // 3) поля письма.
    let to = c.to.trim().to_string();
    if to.is_empty() {
        return Err("не указан получатель".to_string());
    }
    let subject = c.subject.trim().to_string();
    if subject.is_empty() {
        return Err("не указана тема".to_string());
    }
    if c.body.trim().is_empty() {
        return Err("пустое сообщение".to_string());
    }
    if c.body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "сообщение слишком велико: {} > {} байт",
            c.body.len(),
            MAX_BODY_BYTES
        ));
    }
    // 4) тело — во временный файл (в argv не светится).
    let body_path = std::env::temp_dir().join(format!(
        "devh-panel-msg-{}-{}",
        std::process::id(),
        super::now_secs()
    ));
    fs::write(&body_path, &c.body).map_err(|e| format!("не записать временный файл: {e}"))?;
    let mut cmd = std::process::Command::new(devh_binary());
    cmd.arg("msg")
        .arg("send")
        .arg("--to")
        .arg(&to)
        .arg("--subject")
        .arg(&subject)
        .arg("--body-file")
        .arg(&body_path);
    let thread = c.thread.trim().to_string();
    if !thread.is_empty() {
        cmd.arg("--thread").arg(&thread);
    }
    cmd.current_dir(root);
    let out = cmd
        .output()
        .map_err(|e| format!("не запустить devh msg send: {e}"))?;
    let _ = fs::remove_file(&body_path);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        let msg = stderr.trim().to_string();
        return Err(if msg.is_empty() {
            format!("devh msg send: код {}", out.status)
        } else {
            msg
        });
    }
    let (id, thread_id, recipients) = parse_sent(&stdout)
        .ok_or_else(|| format!("не разобран ответ devh msg send: {}", stdout.trim()))?;
    let sender = sender_of_thread(&thread_id, &id).unwrap_or_default();
    let report = SendReport {
        id,
        thread: thread_id,
        recipients,
        sender,
    };
    // 5) аудит: каждое письмо — в журнал прав.
    let _ = audit_letter(
        root,
        &report.sender,
        &to,
        &subject,
        &report.thread,
        &report.id,
    );
    Ok(report)
}

/// Разобрать строку `отправлено (ID <id>, тред <тред>) -> <получатели>`.
fn parse_sent(out: &str) -> Option<(String, String, String)> {
    let start = out.find('(')?;
    let end = out.find(')')?;
    let inner = out.get(start + 1..end)?;
    let id = inner
        .split("ID ")
        .nth(1)?
        .split(',')
        .next()?
        .trim()
        .to_string();
    let thread = inner.split("тред ").nth(1)?.trim().to_string();
    let recipients = out
        .get(end + 1..)
        .unwrap_or_default()
        .trim()
        .trim_start_matches("->")
        .trim()
        .to_string();
    if id.is_empty() || thread.is_empty() {
        return None;
    }
    Some((id, thread, recipients))
}

/// Фактический отправитель письма (по тред-логу).
fn sender_of_thread(thread: &str, id: &str) -> Option<String> {
    let path = thread_path(thread);
    let (parts, _complete) = tail_blocks(&path, 8);
    for p in parts.iter().rev() {
        let m = parse_message(p);
        if m.id == id {
            return Some(m.from);
        }
    }
    None
}

/// Запись в журнал прав: кто, когда, кому, тема, тред.
pub fn audit_letter(
    root: &Path,
    sender: &str,
    to: &str,
    subject: &str,
    thread: &str,
    id: &str,
) -> Result<(), String> {
    let path = root
        .join(".harness")
        .join("orch")
        .join("settings_ledger.jsonl");
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("нет каталога журнала: {e}"))?;
    }
    let rec = serde_json::json!({
        "ts": super::now_secs(),
        "actor": sender,
        "actor_key": "",
        "actor_source": "panel",
        "actor_role": "operator",
        "target": to,
        "field": "panel_msg",
        "before": "",
        "after": subject,
        "reason": format!("тред {thread}"),
        "thread": thread,
        "message_id": id,
    });
    let line = format!("{rec}\n");
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("журнал недоступен: {e}"))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("журнал недоступен: {e}"))?;
    Ok(())
}

/// Кэш имени отправителя: (время, имя).
fn identity_cache() -> &'static Mutex<Option<(u64, String)>> {
    static CACHE: OnceLock<Mutex<Option<(u64, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Под кем панель пишет (канонический `devh msg whoami`, кэш 30 с).
pub fn sender_identity(root: &Path) -> String {
    let now = super::now_secs();
    if let Ok(c) = identity_cache().lock() {
        if let Some((ts, name)) = c.as_ref() {
            if now.saturating_sub(*ts) < IDENTITY_TTL_SECS {
                return name.clone();
            }
        }
    }
    let name = whoami(root).unwrap_or_default();
    if let Ok(mut c) = identity_cache().lock() {
        *c = Some((now, name.clone()));
    }
    name
}

/// Имя текущей сессии — В ПРОЦЕССЕ (без дочернего процесса).
///
/// Дочерний `devh msg whoami` тут не годится: он читает реестр и может
/// встать на блокировку — HTTP-поток панели не должен ждать (бюджет 300 мс).
/// Идентичность панели = идентичность её процесса (`identity::current()`);
/// ровно её подхватит и дочерний `devh msg send` (тот же контекст).
fn whoami(_root: &Path) -> Option<String> {
    let id = crate::core::identity::current();
    let name = id.name.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
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

    // ──────────────────────────── WRITE-КОНТУР (T-631) ────────────────────────

    fn tmp_root(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("devh-chat-write-{tag}-{}", std::process::id()));
        p
    }

    fn root_with_config(tag: &str, config: &str) -> PathBuf {
        let root = tmp_root(tag);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".harness")).unwrap();
        fs::write(root.join(".harness/config"), config).unwrap();
        root
    }

    #[test]
    fn config_value_reads_quoted_values() {
        let text = "# комментарий\nPANEL_WRITE='1'\nPANEL_TOKEN=\"секрет\"\nOTHER=2\n";
        assert_eq!(config_value_in(text, "PANEL_WRITE").as_deref(), Some("1"));
        assert_eq!(
            config_value_in(text, "PANEL_TOKEN").as_deref(),
            Some("секрет")
        );
        assert_eq!(config_value_in(text, "OTHER").as_deref(), Some("2"));
        assert!(config_value_in(text, "НЕТ").is_none());
    }

    #[test]
    fn write_gate_is_off_by_default_and_needs_token() {
        let root = root_with_config("off", "PANEL_WRITE=0\n");
        assert_eq!(write_gate(&root), WriteGate::Disabled);
        let root = root_with_config("notoken", "PANEL_WRITE=1\n");
        assert_eq!(write_gate(&root), WriteGate::NoToken);
        let root = root_with_config("on", "PANEL_WRITE=1\nPANEL_TOKEN=секрет\n");
        assert_eq!(write_gate(&root), WriteGate::Enabled);
    }

    #[test]
    fn constant_time_eq_compares_content() {
        assert!(constant_time_eq("секрет", "секрет"));
        assert!(!constant_time_eq("секрет", "секрете"));
        assert!(!constant_time_eq("секрет", ""));
    }

    #[test]
    fn parse_form_decodes_fields() {
        let c = parse_form("to=minimax&subject=%D0%9F%D1%80%D0%B8%D0%B2%D0%B5%D1%82&body=hello+world&thread=abc123&token=t%2Dok");
        assert_eq!(c.to, "minimax");
        assert_eq!(c.subject, "Привет");
        assert_eq!(c.body, "hello world");
        assert_eq!(c.thread, "abc123");
        assert_eq!(c.token, "t-ok");
    }

    #[test]
    fn send_letter_rejects_when_panel_read_only() {
        let root = root_with_config("ro", "PANEL_WRITE=0\n");
        let c = Composer {
            to: "minimax".into(),
            subject: "тема".into(),
            body: "текст".into(),
            thread: String::new(),
            token: String::new(),
        };
        let err = send_letter(&root, &c).expect_err("read-only должен отказать");
        assert!(err.contains("read-only"), "текст ошибки: {err}");
    }

    #[test]
    fn send_letter_rejects_wrong_token_and_empty_body() {
        let root = root_with_config("token", "PANEL_WRITE=1\nPANEL_TOKEN=верный\n");
        let mut c = Composer {
            to: "minimax".into(),
            subject: "тема".into(),
            body: "текст".into(),
            thread: String::new(),
            token: "неверный".into(),
        };
        let err = send_letter(&root, &c).expect_err("неверный токен должен отказать");
        assert!(err.contains("токен"), "текст ошибки: {err}");
        c.token = "верный".into();
        c.body = "   ".into();
        let err = send_letter(&root, &c).expect_err("пустое тело должно отказать");
        assert!(err.contains("пустое"), "текст ошибки: {err}");
    }

    #[test]
    fn parse_sent_reads_id_thread_and_recipients() {
        let (id, thread, rec) =
            parse_sent("отправлено (ID 36212908, тред bb74bdbe) -> ag-cd51d769\n")
                .expect("разбор ответа");
        assert_eq!(id, "36212908");
        assert_eq!(thread, "bb74bdbe");
        assert_eq!(rec, "ag-cd51d769");
    }

    #[test]
    fn audit_letter_writes_jsonl_record() {
        let root = tmp_root("audit");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".harness/orch")).unwrap();
        audit_letter(&root, "оператор", "minimax", "тема", "тред-1", "id-1").unwrap();
        let text = fs::read_to_string(root.join(".harness/orch/settings_ledger.jsonl")).unwrap();
        let line = text.lines().next().expect("запись журнала");
        let v: serde_json::Value = serde_json::from_str(line).expect("валидный JSON");
        assert_eq!(v.get("actor").and_then(|x| x.as_str()), Some("оператор"));
        assert_eq!(v.get("target").and_then(|x| x.as_str()), Some("minimax"));
        assert_eq!(v.get("after").and_then(|x| x.as_str()), Some("тема"));
        assert_eq!(v.get("thread").and_then(|x| x.as_str()), Some("тред-1"));
        assert_eq!(v.get("message_id").and_then(|x| x.as_str()), Some("id-1"));
    }

    #[test]
    fn url_decode_handles_plus_and_percent() {
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("%2F"), "/");
        assert_eq!(url_decode("100%"), "100%");
    }
}
