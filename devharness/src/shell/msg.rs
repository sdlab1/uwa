//! Канал сообщений между агентами: `devh msg`.
//!
//! Модель: **send → event to read → once read deleted**.
//! Сообщение адресовано конкретному агенту (или «всем»); прочитанное удаляется.
//! Каналов нет: тема = тэг (несколько тэгов на сообщение), «новая тема» =
//! `forward` (новое сообщение с новым тэгом/адресатом).
//!
//! Хранилище (`$DEVH_MSG_HOME`, по умолчанию `/tmp/devh-msg`):
//!   `inbox/<agent>.log`  — входящие, непрочитанные (consume-on-read)
//!   `outbox/<agent>.log` — мои отправленные (архив; история не теряется)
//!
//! Команды:
//!   devh msg send --to <агент|all> --from <агент> --subject <текст>
//!                    [--body <текст> | --body-file <файл>] [--tag <тэг> ...]
//!   devh msg read [--agent <мне>] [--peek] [--tag <тэг>]
//!   devh msg watch [--agent <мне>] [--interval <сек>] [--consume]
//!   devh msg list
//!   devh msg sent [--agent <мне>]
//!   devh msg find <тэг|префикс-ID>
//!   devh msg forward --id <id> --to <агент|all> [--tag <тэг> ...] [--from <агент>]
//!
//! Формат сообщения:
//!   [ЧЧ:ММ] ОТ: <агент> — КОМУ: <кому>
//!   ID: <хэш 8 hex>
//!   ТЕМА: <тема>
//!   ТЭГИ: <тэг1>, <тэг2>      (необязательно)
//!   <тело>

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub const SEP: &str =
    "================================================================================";
const NOTIFY_TIMEOUT_MS: u64 = 20_000;
const DEDUPE_PATH: &str = "/tmp/messages.notified";

pub fn home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

fn inbox_dir() -> PathBuf {
    home().join("inbox")
}

fn outbox_dir() -> PathBuf {
    home().join("outbox")
}

/// Треды (обсуждения) — сохраняются целиком, в отличие от входящих.
fn threads_dir() -> PathBuf {
    home().join("threads")
}

fn thread_path(thread: &str) -> PathBuf {
    threads_dir().join(format!("{}.log", safe_name(thread)))
}

/// Присутствие агентов: presence/<agent>.txt — кто на месте и когда был последний раз.
fn presence_dir() -> PathBuf {
    home().join("presence")
}

fn presence_path(agent: &str) -> PathBuf {
    // на пару (агент, сессия): heartbeat watcher'а и «последнее действие»
    // другой сессии не должны затирать друг друга
    presence_dir().join(format!("{}.{}.txt", safe_name(agent), safe_name(&me(None))))
}

/// Записать «я жив, делал X» (heartbeat).
fn touch_presence(agent: &str, action: &str) {
    let path = presence_path(agent);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "local".to_string());
    let body = format!(
        "agent={agent}\nsession={}\npid={}\nhost={host}\naction={action}\nts={}\n",
        me(None),
        std::process::id(),
        unix_now()
    );
    let _ = fs::write(&path, body);
}

/// Прочие присутствия: агент -> (действие, ts, pid).
fn presence_rows() -> Vec<(String, String, u64, Option<i32>)> {
    let mut out: Vec<(String, String, u64, Option<i32>)> = Vec::new();
    if let Ok(dir) = fs::read_dir(presence_dir()) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(base) = name.strip_suffix(".txt") else {
                continue;
            };
            let Ok(raw) = fs::read_to_string(e.path()) else {
                continue;
            };
            // Имя агента берём из тела файла (agent=...), а не из имени файла:
            // имена с точками («a.b.c») ломали rsplit_once('.').
            // Fallback — старый разбор имени файла.
            let agent = raw
                .lines()
                .find_map(|l| l.strip_prefix("agent=").map(|v| v.trim().to_string()))
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| {
                    base.rsplit_once('.')
                        .map(|(a, _)| a.to_string())
                        .unwrap_or_else(|| base.to_string())
                });
            let mut action = String::new();
            let mut ts: u64 = 0;
            let mut pid: Option<i32> = None;
            for line in raw.lines() {
                if let Some(v) = line.strip_prefix("action=") {
                    action = v.to_string();
                } else if let Some(v) = line.strip_prefix("ts=") {
                    ts = v.parse::<u64>().unwrap_or(0);
                } else if let Some(v) = line.strip_prefix("pid=") {
                    pid = v.parse::<i32>().ok();
                }
            }
            // Синонимы — один агент: сливаем строки по каноническому имени.
            let canon = crate::core::identity::canonical(&agent);
            match out.iter_mut().find(|(n, _, _, _)| *n == canon) {
                Some(existing) => {
                    if ts >= existing.2 {
                        existing.1 = action;
                        existing.2 = ts;
                        existing.3 = pid;
                    }
                }
                None => out.push((canon, action, ts, pid)),
            }
        }
    }
    out.sort();
    out
}

/// «N секунд/минут/часов назад».
fn ago(ts: u64) -> String {
    let now = unix_now();
    let d = now.saturating_sub(ts);
    if d < 60 {
        format!("{d} с назад")
    } else if d < 3600 {
        format!("{} мин назад", d / 60)
    } else if d < 86_400 {
        format!("{} ч назад", d / 3600)
    } else {
        format!("{} дн назад", d / 86_400)
    }
}

/// Процесс жив? (владелец heartbeat мог уйти)
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).is_dir()
}

/// Поток подтверждений агента: «доставлено <id>», «прочитано <id>».
fn acks_dir() -> PathBuf {
    home().join("acks")
}

fn acks_path(agent: &str) -> PathBuf {
    acks_dir().join(format!("{}.log", safe_name(agent)))
}

/// Записать подтверждение (доставлено/прочитано) адресату.
fn ack(agent: &str, id: &str, what: &str) {
    if id.is_empty() || agent.is_empty() {
        return;
    }
    let stamp = now_hm();
    let line = format!("[{stamp}] {what} {id} (агент {agent})\n");
    let path = acks_path(agent);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Отправитель письма (из строки заголовка «[..] ОТ: a — КОМУ: b»).
fn block_sender(block: &str) -> String {
    let header = block.lines().next().unwrap_or("");
    match header.split_once("] ОТ: ") {
        Some((_, tail)) => tail
            .split(" — КОМУ: ")
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
        None => String::new(),
    }
}

/// Адресаты письма (из строки заголовка «... — КОМУ: a, b»).
fn block_recipients(block: &str) -> String {
    let header = block.lines().next().unwrap_or("");
    match header.split_once(" — КОМУ: ") {
        Some((_, to)) => to.trim().to_string(),
        None => String::new(),
    }
}

/// Письмо адресовано только самому отправителю (по канону, с учётом синонимов)?
fn addressed_only_to_self(block: &str, canon: &str) -> bool {
    let to = block_recipients(block);
    if to.is_empty() {
        return false;
    }
    let mut any = false;
    for raw in to.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        any = true;
        if crate::core::identity::canonical(name) != canon {
            return false;
        }
    }
    any
}

/// Голоса за закрытие треда: по строке на агента.
fn votes_path(thread: &str) -> PathBuf {
    threads_dir().join(format!("{}.votes", safe_name(thread)))
}

/// Указание на место: "путь" или "путь:строка" (строка опциональна).
fn parse_place(raw: &str) -> (String, Option<usize>) {
    match raw.rsplit_once(':') {
        Some((path, num)) => match num.parse::<usize>() {
            Ok(n) => (path.to_string(), Some(n)),
            Err(_) => (raw.to_string(), None),
        },
        None => (raw.to_string(), None),
    }
}

/// Регион кода вокруг строки: (путь, контекст БЕЗ номеров строк, разметка).
///
/// Привязка сообщения к коду — по СОДЕРЖИМОМУ (контекст), а не по номеру
/// строки: строки сдвигаются, контекст — нет. Как в unified diff.
fn code_region(path: &str, line: Option<usize>) -> Option<(String, String, String)> {
    let candidates = [
        path.to_string(),
        match crate::shell::git::root_from_cwd() {
            Ok(root) => root.join(path).to_string_lossy().to_string(),
            Err(_) => path.to_string(),
        },
    ];
    let mut content: Option<String> = None;
    for c in candidates.iter() {
        if let Ok(t) = fs::read_to_string(c) {
            content = Some(t);
            break;
        }
    }
    let content = content?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let center = line.unwrap_or(1).clamp(1, lines.len());
    let from = center.saturating_sub(3).max(1);
    let to = center.saturating_add(3).min(lines.len());
    let mut plain = String::new();
    let mut marked = String::new();
    for n in from..=to {
        let text = lines.get(n.saturating_sub(1)).copied().unwrap_or("");
        plain.push_str(text);
        plain.push('\n');
        let mark = if n == center { "»" } else { " " };
        marked.push_str(&format!("{mark}{n:>5} | {text}\n"));
    }
    Some((path.to_string(), plain, marked))
}

/// Unified diff, приложенный к письму: (путь, контекст для ключа, сам diff).
fn read_diff(file: &str) -> Option<(String, String, String)> {
    let raw = fs::read_to_string(file).ok()?;
    let mut path = String::new();
    let mut context = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let p = rest.strip_prefix("b/").unwrap_or(rest);
            if p != "/dev/null" {
                path = p.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            let p = rest.strip_prefix("a/").unwrap_or(rest);
            if p != "/dev/null" && path.is_empty() {
                path = p.to_string();
            }
        } else if line.starts_with(' ') {
            context.push_str(line.get(1..).unwrap_or(""));
            context.push('\n');
        }
    }
    if path.is_empty() {
        return None;
    }
    Some((path, context, raw))
}

/// Участники треда: отправители + адресаты (все, кого это касается).
fn thread_participants(blocks: &[String]) -> Vec<String> {
    let mut who: Vec<String> = Vec::new();
    for b in blocks.iter() {
        let header = b.lines().next().unwrap_or("");
        let Some((_, tail)) = header.split_once("] ОТ: ") else {
            continue;
        };
        let from = tail.split(" — КОМУ: ").next().unwrap_or("");
        if !from.trim().is_empty() && !who.iter().any(|x| x == from.trim()) {
            who.push(from.trim().to_string());
        }
        if let Some((_, to)) = tail.split_once(" — КОМУ: ") {
            for t in to.split(',') {
                let name = t.trim().to_string();
                if !name.is_empty() && !who.iter().any(|x| x == &name) {
                    who.push(name);
                }
            }
        }
    }
    who
}

fn inbox_path(agent: &str) -> PathBuf {
    inbox_dir().join(format!("{agent}.log"))
}

/// Свой ящик агента.
fn own_inbox_path(agent: &str) -> PathBuf {
    inbox_path(&safe_name(agent))
}

/// Legacy-ящик «default» (письма старой схемы, где имя не задавали).
fn legacy_inbox_path() -> PathBuf {
    inbox_path("default")
}

/// Ящики входящих агента: свой + legacy «default».
///
/// Legacy нужен для перехода: письма, отправленные старой схемой (без
/// явного имени), лежат в `default.log` — агент с авто-именем обязан их
/// видеть, иначе они потерялись бы навсегда.
/// Миграция синонимных ящиков в канонический (один раз, дедуп по ID).
fn migrate_synonym_inboxes(canon: &str) {
    let own = own_inbox_path(canon);
    let aliases = crate::core::identity::aliases_of(canon);
    for alias in &aliases {
        let path = inbox_path(&safe_name(alias));
        if path == own || !path.exists() {
            continue;
        }
        // Читаем синонимный ящик.
        let text = fs::read_to_string(&path).unwrap_or_default();
        let (_header, blocks) = split_log(&text);
        if blocks.is_empty() {
            let _ = fs::remove_file(&path);
            continue;
        }
        // Собираем ID уже в каноническом ящике.
        let own_text = fs::read_to_string(&own).unwrap_or_default();
        let (_, own_blocks) = split_log(&own_text);
        let mut own_ids: Vec<String> = own_blocks
            .iter()
            .map(|b| block_field(b, "ID: ").unwrap_or_default())
            .collect();
        // Дописываем только новые (дедуп по ID).
        for b in blocks.iter() {
            let id = block_field(b, "ID: ").unwrap_or_default();
            if !id.is_empty() && own_ids.iter().any(|x| x == &id) {
                continue;
            }
            let _ = append_message(&own, b);
            if !id.is_empty() {
                own_ids.push(id);
            }
        }
        // Удаляем синонимный ящик после миграции.
        let _ = fs::remove_file(&path);
    }
}

fn inbox_files(agent: &str) -> Vec<PathBuf> {
    let canon = crate::core::identity::canonical(agent);
    // Миграция синонимных ящиков в канонический (один раз).
    migrate_synonym_inboxes(&canon);
    let own = own_inbox_path(&canon);
    let mut files = vec![own.clone()];
    let legacy = legacy_inbox_path();
    if own != legacy && legacy.exists() {
        files.push(legacy);
    }
    files
}

/// Письма из одного файла-ящика.
fn blocks_in_file(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let (_header, blocks) = split_log(&text);
    blocks
}

/// Письма из СВОЕГО ящика (их можно съедать).
fn own_blocks(agent: &str) -> Vec<String> {
    blocks_in_file(&own_inbox_path(agent))
}

/// Письма из legacy-ящика (общие для всех старых сессий — только читать).
fn legacy_blocks() -> Vec<String> {
    blocks_in_file(&legacy_inbox_path())
}

/// Непрочитанные письма агента (из всех его ящиков).
fn inbox_blocks(agent: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for path in inbox_files(agent) {
        out.extend(blocks_in_file(&path));
    }
    out
}

fn outbox_path(agent: &str) -> PathBuf {
    outbox_dir().join(format!("{agent}.log"))
}

/// Имя агента → безопасное имя файла (без разделителей пути).
/// Единая точка правды: `shell::msg_path_guard` (T-111).
pub fn safe_name(agent: &str) -> String {
    crate::shell::msg_path_guard::safe_agent(agent)
}

/// Кто я в канале. Имя — авто: контекст сессии (core::identity), а не
/// ручной `DEVH_AGENT=...`; без него все агенты были «default».
fn me(explicit: Option<&str>) -> String {
    crate::core::identity::resolve(explicit).name
}

fn now_hm() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "??:??".to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn short_id(from: &str, subject: &str, body: &str, stamp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from.as_bytes());
    hasher.update(b"|");
    hasher.update(subject.as_bytes());
    hasher.update(b"|");
    hasher.update(body.as_bytes());
    hasher.update(b"|");
    hasher.update(stamp.as_bytes());
    hasher.update(b"|");
    // nonce: два сообщения с одинаковыми полями в одну минуту должны иметь
    // разные ID (иначе forward даёт ID оригинала).
    hasher.update(unix_nanos().to_string().as_bytes());
    digest_hex(&hasher.finalize())
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Файл сообщений → (шапка, блоки). Блок = текст без разделителя.
fn split_log(text: &str) -> (String, Vec<String>) {
    let sep = format!("{SEP}\n");
    let mut parts = text.split(sep.as_str());
    let first = parts.next().unwrap_or("").to_string();
    let mut blocks: Vec<String> = parts.map(str::to_string).collect();
    let header = if first.contains("ТЕМА: ") {
        blocks.insert(0, first);
        String::new()
    } else {
        first
    };
    // хвостовой пустой блок (файл кончается разделителем) — не сообщение
    blocks.retain(|b| !b.trim().is_empty());
    (header, blocks)
}

fn join_log(header: &str, blocks: &[String]) -> String {
    let mut out = String::new();
    out.push_str(header);
    out.push_str(SEP);
    out.push('\n');
    for b in blocks {
        out.push_str(b);
        out.push_str(SEP);
        out.push('\n');
    }
    out
}

fn block_field(block: &str, prefix: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.starts_with(prefix))
        .and_then(|l| l.get(prefix.len()..))
        .map(str::trim)
        .map(str::to_string)
}

fn block_tags(block: &str) -> Vec<String> {
    match block_field(block, "ТЭГИ: ") {
        None => Vec::new(),
        Some(s) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
    }
}

/// Разобранное письмо — для красивого вывода и уведомлений.
#[derive(Debug, Clone, Default)]
pub struct MsgView {
    pub stamp: String,
    pub sender: String,
    pub to: String,
    pub id: String,
    pub thread: String,
    pub subject: String,
    pub place: Option<String>,
    pub tags: Vec<String>,
    pub body: String,
}

/// Разобрать блок письма в структуру.
///
/// Формат (см. `build_message`):
///   [ЧЧ:ММ] ОТ: <отправитель> — КОМУ: <кому>
///   ID: <id>
///   ТРЕД: <тред>
///   ТЕМА: <тема>
///   МЕСТО: <путь>              (необязательно)
///   ВЫРЕЗКА: <код>            (необязательно)
///   ТЭГИ: <тэг1>, <тэг2>      (необязательно)
///   ТЕЛО:                     (маркер тела; в старых письмах его нет)
///   <текст>
///
/// Старые письма (без маркера `ТЕЛО:`) разбираются по остальным заголовкам:
/// тело — всё, что не является заголовком.
pub fn parse_view(block: &str) -> MsgView {
    let mut v = MsgView::default();
    let mut body: Vec<String> = Vec::new();
    let mut snippet: Vec<String> = Vec::new();
    let mut in_snippet = false;
    let mut in_body = false;
    let mut body_marker = false;
    let mut first = true;
    for line in block.lines() {
        if in_body {
            body.push(line.to_string());
            continue;
        }
        if in_snippet {
            // вырезка кода заканчивается на следующем заголовке
            if line.starts_with("ТЭГИ: ") {
                in_snippet = false;
            } else if line.starts_with("ТЕЛО:") {
                in_snippet = false;
                in_body = true;
                body_marker = true;
                continue;
            } else {
                snippet.push(line.to_string());
                continue;
            }
        }
        if line.starts_with("ТЕЛО:") {
            in_body = true;
            body_marker = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("ID: ") {
            v.id = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("ТРЕД: ") {
            v.thread = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("ТЕМА: ") {
            v.subject = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("МЕСТО: ") {
            v.place = Some(rest.trim().to_string());
            continue;
        }
        if line.starts_with("ТЭГИ: ") {
            v.tags = line
                .trim_start_matches("ТЭГИ: ")
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            continue;
        }
        if line.starts_with("ВЫРЕЗКА:") {
            in_snippet = true;
            continue;
        }
        if first {
            first = false;
            v.stamp = line
                .strip_prefix('[')
                .and_then(|s| s.split_once(']'))
                .map(|(a, _)| a.trim().to_string())
                .unwrap_or_default();
            v.sender = block_sender(block);
            v.to = block_recipients(block);
            continue;
        }
        // строка без заголовка — начало тела (старый формат)
        in_body = true;
        body.push(line.to_string());
    }
    // Старый формат (без маркера `ТЕЛО:`): вырезку не выбрасываем, а
    // возвращаем в тело — иначе текст письма терялся целиком.
    if !body_marker && !snippet.is_empty() {
        let mut merged = snippet;
        merged.extend(body);
        body = merged;
    }
    v.body = body.join("\n").trim().to_string();
    v
}

/// Ширина рамки письма.
const MSG_WIDTH: usize = 76;

/// Красивое представление письма: рамка, «отправитель → адресат», тема,
/// тред/тэги/место и тело. Цвета — если терминал (см. `shell::color`).
pub fn render_block(block: &str) -> String {
    let v = parse_view(block);
    let c = crate::shell::color::colors();
    let col_c = c.c;
    let col_b = c.b;
    let col_d = c.d;
    let col_z = c.z;
    let mut out = String::new();
    let rule = "━".repeat(MSG_WIDTH);
    let thin = "─".repeat(MSG_WIDTH);
    out.push_str(&format!("{col_c}{rule}{col_z}\n"));

    let sender = if v.sender.is_empty() {
        "?"
    } else {
        v.sender.as_str()
    };
    let to = if v.to.is_empty() { "?" } else { v.to.as_str() };
    let title = format!("✉  {sender} → {to}");
    let mut right_parts: Vec<String> = Vec::new();
    if !v.stamp.is_empty() {
        right_parts.push(v.stamp.clone());
    }
    if !v.id.is_empty() {
        right_parts.push(format!("id {}", v.id));
    }
    let right = right_parts.join(" · ");
    let used = title
        .chars()
        .count()
        .saturating_add(right.chars().count())
        .saturating_add(4);
    let pad = MSG_WIDTH.saturating_sub(used);
    out.push_str(&format!("{col_b}  {title:pad$}{col_d}  {right}{col_z}\n"));

    if !v.subject.is_empty() {
        out.push_str(&format!("{col_b}   Тема:  {}{col_z}\n", v.subject));
    }
    let mut meta: Vec<String> = Vec::new();
    if !v.thread.is_empty() {
        meta.push(format!("Тред: {}", v.thread));
    }
    if !v.tags.is_empty() {
        meta.push(format!("Тэги: {}", v.tags.join(", ")));
    }
    if !meta.is_empty() {
        out.push_str(&format!("{col_d}   {}{col_z}\n", meta.join("  ·  ")));
    }
    if let Some(place) = v.place.as_ref() {
        out.push_str(&format!("{col_d}   Место: {place}{col_z}\n"));
    }
    out.push_str(&format!("{col_d}{thin}{col_z}\n"));
    if !v.body.is_empty() {
        for line in v.body.lines() {
            out.push_str(&format!("   {line}\n"));
        }
    }
    out.push_str(&format!("{col_c}{rule}{col_z}\n"));
    out
}

/// Ростер агентов: все, у кого есть входящие или исходящие.
fn roster() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in [inbox_dir(), outbox_dir()] {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(base) = name.strip_suffix(".log") {
                    // канон: синонимы не плодят «двойных» соседей в ростере
                    let canon = crate::core::identity::canonical(base);
                    if !names.iter().any(|n| *n == canon) {
                        names.push(canon);
                    }
                }
            }
        }
    }
    names.sort();
    names
}

/// Сообщение → файл (append). Канал создаётся при первом сообщении.
fn append_message(path: &Path, block: &str) -> bool {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Атомарность при параллельных отправителях: advisory lock (fs2) +
    // один open handle + flush. Без этого два `devh msg send` могли
    // интерливить запись или потерять данные.
    let mut f = match fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if fs2::FileExt::lock_exclusive(&f).is_err() {
        return false;
    }
    let ok = writeln!(f, "{block}{SEP}").is_ok();
    let _ = f.flush();
    let _ = fs2::FileExt::unlock(&f);
    ok
}

/// Файл «уже показанных» уведомлений (на агента).
fn notified_path(agent: &str) -> PathBuf {
    home()
        .join("notified")
        .join(format!("{}.log", safe_name(agent)))
}

/// ID уже показанных уведомлений агента.
fn notified_ids(agent: &str) -> Vec<String> {
    let text = fs::read_to_string(notified_path(agent)).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .collect()
}

/// Запомнить показанные уведомления (чтобы не повторяться).
fn mark_notified(agent: &str, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let path = notified_path(agent);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        for id in ids {
            let _ = writeln!(f, "{id} {}", unix_now());
        }
    }
    prune_notified(&path);
}

/// Файл «уже показанных» не должен расти бесконечно: оставляем хвост.
fn prune_notified(path: &Path) {
    const MAX_LINES: usize = 4000;
    const MAX_BYTES: u64 = 256 * 1024;
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_LINES && meta.len() <= MAX_BYTES {
        return;
    }
    let now = unix_now();
    // Не резать ID, которые молодые (now - ts < 300): они нужны для дедупа.
    // Удаляем только старые строки, сверх лимита.
    let mut keep: Vec<&str> = Vec::new();
    // Сначала собираем все молодые строки.
    for line in lines.iter() {
        let mut it = line.split_whitespace();
        if let (Some(_k), Some(ts)) = (it.next(), it.next()) {
            if let Ok(t) = ts.parse::<u64>() {
                if now.saturating_sub(t) < 300 {
                    keep.push(line);
                    continue;
                }
            }
        }
    }
    // Затем добавляем старые, пока не достигнем лимита.
    for line in lines.iter() {
        if keep.len() >= MAX_LINES {
            break;
        }
        let mut it = line.split_whitespace();
        let is_young = if let (Some(_k), Some(ts)) = (it.next(), it.next()) {
            ts.parse::<u64>()
                .map(|t| now.saturating_sub(t) < 300)
                .unwrap_or(false)
        } else {
            false
        };
        if !is_young && !keep.contains(line) {
            keep.push(line);
        }
    }
    let _ = fs::write(path, format!("{}\n", keep.join("\n")));
}

/// Авто-уведомление: новые входящие показываются красиво (в stderr) при
/// ЛЮБОМ вызове devh — агенту не нужно самому помнить про `devh msg read`.
///
/// Письма НЕ съедаются: consume-on-read остаётся за `devh msg read`.
/// Показ дедуплицируется по ID (файл `notified/<агент>.log`).
pub fn auto_notify(agent: &str) -> usize {
    // Проверка ВНУТРИ функции: вызывающий код не должен был бы помнить
    // про гаситель (иначе другой путь вызова снова «забыл» бы его).
    if !auto_notify_enabled() {
        return 0;
    }
    let blocks = inbox_blocks(agent);
    if blocks.is_empty() {
        return 0;
    }
    let seen = notified_ids(agent);
    let mut fresh: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for b in blocks.iter() {
        let id = block_field(b, "ID: ").unwrap_or_default();
        if id.is_empty() || seen.iter().any(|s| s == &id) {
            continue;
        }
        fresh.push(b.clone());
        ids.push(id);
    }
    if fresh.is_empty() {
        return 0;
    }
    // Flood protection: не больше 5 писем за раз.
    let (shown, rest) = if fresh.len() > 5 {
        (5usize, fresh.len() - 5)
    } else {
        (fresh.len(), 0usize)
    };
    let shown_ids: Vec<String> = ids.iter().take(shown).cloned().collect();
    let shown_blocks: Vec<String> = fresh.iter().take(shown).cloned().collect();
    let c = crate::shell::color::colors();
    let col_y = c.y;
    let col_z = c.z;
    eprintln!(
        "\n{col_y}📬 {} новых сообщений для {} — прочитать: devh msg read{col_z}\n",
        fresh.len(),
        agent
    );
    for b in shown_blocks.iter() {
        eprintln!("{}", render_block(b));
    }
    if rest > 0 {
        eprintln!("… ещё {} (devh msg read)", rest);
    }
    // Один popup на всю пачку (не N popups).
    notify_summary(&shown_ids, &shown_blocks);
    mark_notified(agent, &ids);
    fresh.len()
}

/// Краткая сводка входящих для промпта агента (без ANSI, коротко).
pub fn unread_brief(agent: &str) -> String {
    // Считаем только consumable (свои + алиасные) блоки; legacy — отдельно.
    let canon = crate::core::identity::canonical(agent);
    let mut consumable: Vec<String> = Vec::new();
    let own = own_inbox_path(&canon);
    if own.exists() {
        consumable.extend(blocks_in_file(&own));
    }
    for alias in crate::core::identity::aliases_of(&canon) {
        let path = inbox_path(&safe_name(&alias));
        if path != own && path.exists() {
            consumable.extend(blocks_in_file(&path));
        }
    }
    let legacy_count = if legacy_inbox_path().exists() && own != legacy_inbox_path() {
        blocks_in_file(&legacy_inbox_path()).len()
    } else {
        0
    };
    if consumable.is_empty() && legacy_count == 0 {
        return String::new();
    }
    let mut out = String::new();
    if !consumable.is_empty() {
        out.push_str(&format!(
            "ВХОДЯЩИЕ СООБЩЕНИЯ (канал devh msg): {} непрочитанных — прочитать: devh msg read\n",
            consumable.len()
        ));
        for b in consumable.iter().take(5) {
            let v = parse_view(b);
            out.push_str(&format!(
                "  • [{}] {} → {} | тема: {} | тред: {} | id: {}\n",
                v.stamp, v.sender, v.to, v.subject, v.thread, v.id
            ));
            let first = v
                .body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            if !first.is_empty() {
                out.push_str(&format!("    {first}\n"));
            }
        }
        if consumable.len() > 5 {
            out.push_str(&format!("  … ещё {}\n", consumable.len().saturating_sub(5)));
        }
    }
    if legacy_count > 0 {
        out.push_str(&format!("legacy: {} (общий ящик)\n", legacy_count));
    }
    out
}

/// Сколько непрочитанных писем у агента (свой ящик + legacy).
///
/// Инструмент ведущего: «кто не прочитал мои письма» — без разбора логов.
pub fn unread_count(agent: &str) -> usize {
    let canon = crate::core::identity::canonical(agent);
    let mut n = 0usize;
    for f in inbox_files(&canon) {
        n = n.saturating_add(blocks_in_file(&f).len());
    }
    n
}

/// Когда агент последний раз проявлял активность (секунд назад).
///
/// `None` — присутствия не было вовсе.
pub fn presence_age_secs(agent: &str) -> Option<u64> {
    let canon = crate::core::identity::canonical(agent);
    let ts = presence_rows()
        .into_iter()
        .filter(|(a, _, _, _)| crate::core::identity::canonical(a) == canon)
        .map(|(_, _, ts, _)| ts)
        .max()?;
    Some(unix_now().saturating_sub(ts))
}

/// Уведомление о пачке сообщений: один popup на всю пачку.
fn notify_summary(ids: &[String], blocks: &[String]) {
    use std::io::{Read, Seek, SeekFrom};
    let path = match std::env::var("MSG_NOTIFY_DEDUPE") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(DEDUPE_PATH),
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    if fs2::FileExt::lock_exclusive(&file).is_err() {
        return;
    }
    let mut raw = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut raw);
    let now = unix_now();
    // Если ВСЕ id уже были показаны недавно — пропускаем popup.
    let mut all_seen = true;
    for id in ids {
        let mut found = false;
        for line in raw.lines() {
            let mut it = line.split_whitespace();
            if let (Some(k), Some(ts)) = (it.next(), it.next()) {
                if k == id {
                    if let Ok(t) = ts.parse::<u64>() {
                        if now.saturating_sub(t) < 300 {
                            found = true;
                            break;
                        }
                    }
                }
            }
        }
        if !found {
            all_seen = false;
            break;
        }
    }
    if all_seen && !ids.is_empty() {
        let _ = fs2::FileExt::unlock(&file);
        return;
    }
    let _ = file.seek(SeekFrom::End(0));
    for id in ids {
        let _ = writeln!(file, "{id} {now}");
    }
    let _ = fs2::FileExt::unlock(&file);

    let mut parts: Vec<String> = Vec::new();
    for b in blocks.iter() {
        let v = parse_view(b);
        let sender = if v.sender.is_empty() {
            "агент"
        } else {
            v.sender.as_str()
        };
        let subject = if v.subject.is_empty() {
            "новое сообщение".to_string()
        } else {
            v.subject.clone()
        };
        parts.push(format!("{sender}: {subject}"));
    }
    let summary = if parts.len() == 1 {
        parts[0].clone()
    } else {
        format!("{} новых сообщений", parts.len())
    };
    if env_flag_opt("DEVH_MSG_POPUP").unwrap_or_else(has_display) {
        let _ = Command::new("notify-send")
            .arg("-t")
            .arg(NOTIFY_TIMEOUT_MS.to_string())
            .arg(format!("devh msg — {} новых", ids.len()))
            .arg(summary.chars().take(220).collect::<String>())
            .output();
    }
    if env_flag("DEVH_MSG_SOUND") {
        let _ = Command::new("paplay")
            .arg("/usr/share/sounds/freedesktop/stereo/message.oga")
            .output();
    }
}

/// Уведомление: один popup на сообщение (дедуп по ID под advisory-локом).
fn notify(id: &str, text: &str) {
    use std::io::{Read, Seek, SeekFrom};
    let path = match std::env::var("MSG_NOTIFY_DEDUPE") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(DEDUPE_PATH),
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    if fs2::FileExt::lock_exclusive(&file).is_err() {
        return;
    }
    let mut raw = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut raw);
    let now = unix_now();
    for line in raw.lines() {
        let mut it = line.split_whitespace();
        if let (Some(k), Some(ts)) = (it.next(), it.next()) {
            if k == id {
                if let Ok(t) = ts.parse::<u64>() {
                    if now.saturating_sub(t) < 300 {
                        let _ = fs2::FileExt::unlock(&file);
                        return;
                    }
                }
            }
        }
    }
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.set_len(0);
    let _ = writeln!(file, "{id} {now}");
    let _ = fs2::FileExt::unlock(&file);

    let v = parse_view(text);
    let subject = if v.subject.is_empty() {
        "новое сообщение".to_string()
    } else {
        v.subject.clone()
    };
    let sender = if v.sender.is_empty() {
        "агент"
    } else {
        v.sender.as_str()
    };
    let first_body = v
        .body
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let summary = format!("{sender}: {subject} — {first_body}");
    if env_flag_opt("DEVH_MSG_POPUP").unwrap_or_else(has_display) {
        let _ = Command::new("notify-send")
            .arg("-t")
            .arg(NOTIFY_TIMEOUT_MS.to_string())
            .arg(format!("devh msg — {sender}"))
            .arg(summary.chars().take(220).collect::<String>())
            .output();
    }
    if env_flag("DEVH_MSG_SOUND") {
        let _ = Command::new("paplay")
            .arg("/usr/share/sounds/freedesktop/stereo/message.oga")
            .output();
    }
}

/// Флаг окружения «включено»: по умолчанию да; выключает 0/false/no/off.
fn env_flag(name: &str) -> bool {
    env_flag_opt(name).unwrap_or(true)
}

/// Флаг окружения с умолчанием: `Some(вкл/выкл)` — явное решение,
/// `None` — переменной нет (тогда действует умолчание).
fn env_flag_opt(name: &str) -> Option<bool> {
    match std::env::var(name) {
        Ok(v) => Some(!matches!(v.trim(), "0" | "false" | "no" | "off")),
        Err(_) => None,
    }
}

/// Есть ли графическая сессия (popup без неё бессмыслен — notify-send не
/// откроется, только потратит время на запуск процесса).
fn has_display() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|v| {
        std::env::var(v)
            .map(|x| !x.trim().is_empty())
            .unwrap_or(false)
    })
}

/// Авто-уведомления включены?
///
/// В сьюте/тестах — выключены: иначе каждый запуск `devh` внутри тестов
/// читал бы РЕАЛЬНЫЙ ящик `/tmp/devh-msg`, печатал реальные письма в
/// stderr, запускал notify-send/paplay и помечал реальные уведомления
/// показанными. Гасится `DEVH_MSG_NO_AUTO=1` или `HARNESS_TEST=1`.
pub fn auto_notify_enabled() -> bool {
    for var in ["DEVH_MSG_NO_AUTO", "HARNESS_TEST"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if matches!(v, "1" | "true" | "yes" | "on") {
                return false;
            }
        }
    }
    true
}

/// Разбор аргументов вручную (KISS: «проход как есть»).
struct Args {
    sub: String,
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, v)| k == name && v.is_none())
    }
    fn value(&self, name: &str) -> Option<String> {
        self.flags
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.clone())
    }
    fn values(&self, name: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(k, _)| k == name)
            .filter_map(|(_, v)| v.clone())
            .collect()
    }
    fn first(&self) -> String {
        self.positional.first().cloned().unwrap_or_default()
    }
}

fn parse_args(args: &[String]) -> Args {
    let mut sub = String::new();
    let mut positional: Vec<String> = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut it = args.iter();
    if let Some(first) = it.next() {
        sub = first.clone();
    }
    let rest: Vec<String> = it.cloned().collect();
    let mut i = 0usize;
    while i < rest.len() {
        let Some(a) = rest.get(i).map(|s| s.as_str()) else {
            break;
        };
        if let Some(name) = a.strip_prefix("--") {
            if let Some((k, v)) = name.split_once('=') {
                flags.push((k.to_string(), Some(v.to_string())));
            } else if rest
                .get(i.saturating_add(1))
                .map(|n| !n.starts_with("--"))
                .unwrap_or(false)
            {
                let Some(v) = rest.get(i.saturating_add(1)).cloned() else {
                    break;
                };
                flags.push((name.to_string(), Some(v)));
                i = i.saturating_add(1);
            } else {
                flags.push((name.to_string(), None));
            }
        } else {
            positional.push(a.to_string());
        }
        i = i.saturating_add(1);
    }
    Args {
        sub,
        positional,
        flags,
    }
}

/// Собрать сообщение (тело + заголовки).
/// Привязка к коду: место (путь) + вырезка (контекст/diff).
struct Attachment {
    place: Option<String>,
    snippet: Option<String>,
}

impl Attachment {
    fn empty() -> Self {
        Attachment {
            place: None,
            snippet: None,
        }
    }
}

fn build_message(
    from: &str,
    to: &[String],
    subject: &str,
    tags: &[String],
    body: &str,
    thread: &str,
    att: &Attachment,
) -> String {
    let stamp = now_hm();
    let id = short_id(from, subject, body, &stamp);
    let mut block = format!(
        "[{stamp}] ОТ: {from} — КОМУ: {}\nID: {id}\nТРЕД: {thread}\nТЕМА: {subject}",
        to.join(", ")
    );
    if let Some(pl) = att.place.as_ref() {
        block.push_str(&format!("\nМЕСТО: {pl}"));
    }
    if let Some(sn) = att.snippet.as_ref() {
        block.push_str(&format!("\nВЫРЕЗКА:\n{sn}"));
    }
    if !tags.is_empty() {
        block.push_str(&format!("\nТЭГИ: {}", tags.join(", ")));
    }
    // Явный маркер тела: без него вырезка кода (ВЫРЕЗКА:) неотличима от текста.
    block.push_str(&format!("\nТЕЛО:\n{}\n", body.trim_end()));
    block
}

/// Раздать письмо: каждому адресату (или всем в ростере) + в тред (обсуждение)
/// + копия в мои исходящие. Тред сохраняется целиком — обсуждение не теряется.
fn deliver(from: &str, to: &[String], block: &str, thread: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for t in to.iter() {
        if t == "all" {
            let all = roster();
            if all.is_empty() {
                targets.push(from.to_string());
            } else {
                targets.extend(all);
            }
        } else {
            targets.push(t.clone());
        }
    }
    // Канонизация ДО дедупа: два синонима (opencode, deepseek) → один канон.
    let mut unique: Vec<String> = Vec::new();
    for t in targets.iter() {
        let canon = crate::core::identity::canonical(t);
        if !unique.iter().any(|x| x == &canon) {
            unique.push(canon);
        }
    }
    let msg_id = block_field(block, "ID: ").unwrap_or_default();
    let mut done: Vec<String> = Vec::new();
    for target in unique.iter() {
        let path = inbox_path(&safe_name(target));
        if append_message(&path, block) {
            done.push(target.clone());
            ack(
                &safe_name(&crate::core::identity::canonical(from)),
                &msg_id,
                "доставлено",
            );
        }
    }
    let _ = append_message(&thread_path(thread), block);
    let _ = append_message(&outbox_path(&safe_name(from)), block);
    done
}

pub fn send(args: &[String]) -> i32 {
    let m = parse_args(args);
    match m.sub.as_str() {
        "send" | "reply" => {
            let from = m
                .value("from")
                .unwrap_or_else(|| me(m.value("agent").as_deref()));
            let mut to = m.values("to");
            if to.is_empty() {
                to.push("all".to_string());
            }
            let subject = m.value("subject").unwrap_or_default();
            let tags = m.values("tag");
            let body = match m.value("body-file") {
                Some(p) => fs::read_to_string(&p).unwrap_or_default(),
                None => m.value("body").unwrap_or_default(),
            };
            // Указание на код: --diff <файл> (unified diff, привязка по
            // содержимому) или --at путь[:строка] (регион с контекстом).
            let mut place_label: Option<String> = None;
            let mut snippet: Option<String> = None;
            let mut ctx_key: Option<(String, String)> = None;
            if let Some(diff_file) = m.value("diff") {
                if let Some((p, ctx, raw)) = read_diff(&diff_file) {
                    place_label = Some(p.clone());
                    snippet = Some(raw);
                    ctx_key = Some((p, ctx));
                }
            }
            if let Some(at) = m.value("at") {
                let (path, line) = parse_place(&at);
                if let Some((p, plain, marked)) = code_region(&path, line) {
                    place_label = Some(p.clone());
                    snippet = Some(marked);
                    ctx_key = Some((p, plain));
                }
            }
            let block = build_message(
                &from,
                &to,
                &subject,
                &tags,
                &body,
                "x",
                &Attachment::empty(),
            );
            let id = block_field(&block, "ID: ").unwrap_or_default();
            // тред: явный --thread, иначе ключ по КОНТЕКСТУ кода (устойчив к
            // сдвигу строк), иначе новое обсуждение
            let thread = match (m.value("thread"), ctx_key.as_ref()) {
                (Some(t), _) => t,
                (None, Some((p, ctx))) => short_id("ctx", p, ctx, ""),
                (None, None) => id.clone(),
            };
            let att = Attachment {
                place: place_label,
                snippet,
            };
            let block = build_message(&from, &to, &subject, &tags, &body, &thread, &att);
            let id = block_field(&block, "ID: ").unwrap_or_default();
            touch_presence(&from, "send");
            let done = deliver(&from, &to, &block, &thread);
            if done.is_empty() {
                eprintln!("не доставлено никому");
                return 1;
            }
            println!("отправлено (ID {id}, тред {thread}) -> {}", done.join(", "));
            0
        }
        "thread" => {
            let id = m.first();
            if id.is_empty() {
                eprintln!("нужен ID треда");
                return 1;
            }
            let text = fs::read_to_string(thread_path(&id)).unwrap_or_default();
            let (_h, blocks) = split_log(&text);
            if blocks.is_empty() {
                eprintln!("тред {id} не найден");
                return 1;
            }
            for b in blocks.iter() {
                println!("{}", render_block(b));
            }
            0
        }
        "read" => {
            let agent = me(m.value("agent").as_deref());
            let peek = m.flag("peek");
            let filter = m.value("tag");
            let mut shown = 0usize;
            // Сначала СЪЕДАЕМ (переписываем ящик), потом печатаем.
            // Наоборот — письмо терялось, если вывод обрывался пайпом
            // (например `devh msg read | head`): процесс умирал по SIGPIPE
            // ДО записи, и агент видел одно и то же письмо снова и снова.
            let mut to_print: Vec<String> = Vec::new();
            let mut rewrites: Vec<(PathBuf, String)> = Vec::new();
            for path in inbox_files(&agent) {
                // legacy-ящик общий для всех старых сессий: его только читаем,
                // но НЕ съедаем — иначе одно письмо забирал себе кто-то один.
                let legacy = path == legacy_inbox_path() && path != own_inbox_path(&agent);
                let text = fs::read_to_string(&path).unwrap_or_default();
                let (header, blocks) = split_log(&text);
                let mut kept: Vec<String> = Vec::new();
                let mut matched_here = 0usize;
                for b in blocks.iter() {
                    let matches = match filter.as_ref() {
                        None => true,
                        Some(t) => block_tags(b).iter().any(|x| x == t),
                    };
                    if matches {
                        to_print.push(render_block(b));
                        if legacy {
                            to_print
                                .push("(legacy-ящик — письмо остаётся в общем ящике)".to_string());
                        }
                        shown = shown.saturating_add(1);
                        matched_here = matched_here.saturating_add(1);
                        if !peek && !legacy {
                            let id = block_field(b, "ID: ").unwrap_or_default();
                            let sender = crate::core::identity::canonical(&block_sender(b));
                            ack(&safe_name(&sender), &id, "прочитано");
                        }
                    } else {
                        kept.push(b.clone());
                    }
                }
                if !peek && matched_here > 0 && !legacy {
                    rewrites.push((path.clone(), join_log(&header, &kept)));
                }
            }
            for (path, body) in rewrites.iter() {
                if let Err(e) = fs::write(path, body) {
                    eprintln!("не записать ящик {}: {e}", path.display());
                }
            }
            for line in to_print.iter() {
                println!("{line}");
            }
            touch_presence(&agent, "read");
            if shown == 0 {
                println!("(входящих нет)");
            }
            0
        }
        "watch" => {
            let agent = me(m.value("agent").as_deref());
            let interval: u64 = m
                .value("interval")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3);
            let consume = m.flag("consume");
            let once = m.flag("once");
            let from_start = m.flag("from-start");
            let mut seen: Vec<String> = Vec::new();
            // По умолчанию при старте НЕ вываливаем весь бэклог: помечаем
            // существующие письма как seen. --from-start включает дамп.
            if !from_start {
                for path in inbox_files(&agent) {
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    let (_header, blocks) = split_log(&text);
                    for b in blocks.iter() {
                        let id = block_field(b, "ID: ").unwrap_or_default();
                        if !id.is_empty() && !seen.iter().any(|s| s == &id) {
                            seen.push(id);
                        }
                    }
                }
            }
            println!(
                "канал: {agent} (идентификация автоматическая; закрепить имя: devh msg claim <имя>)"
            );
            loop {
                // heartbeat: «я на месте» — по нему соседи видят, кто онлайн
                touch_presence(&agent, "watch");
                for path in inbox_files(&agent) {
                    // legacy-ящик общий: его не consume-on-read (см. «read»).
                    let legacy = path == legacy_inbox_path() && path != own_inbox_path(&agent);
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    let (header, blocks) = split_log(&text);
                    let fresh: Vec<String> = blocks
                        .iter()
                        .filter(|b| {
                            let id = block_field(b, "ID: ").unwrap_or_default();
                            !seen.iter().any(|s| s == &id)
                        })
                        .cloned()
                        .collect();
                    if !fresh.is_empty() {
                        // Сначала съедаем (если --consume), потом печатаем:
                        // обрыв вывода (пайп/SIGPIPE) не должен оставлять
                        // письмо в ящике — иначе оно всплывёт снова.
                        // Потребляем только если агент — текущий (свой ящик).
                        // Сторож чужого агента не должен красть письма.
                        let is_self = crate::core::identity::canonical(&agent)
                            == crate::core::identity::canonical(
                                &crate::core::identity::current().name,
                            );
                        if consume && !legacy && is_self {
                            // Съедаем свежие (fresh) письма: удаляем их из ящика.
                            // Раньше фильтр был инвертирован — свежие оставались.
                            let fresh_ids: Vec<String> = fresh
                                .iter()
                                .map(|b| block_field(b, "ID: ").unwrap_or_default())
                                .collect();
                            let keep: Vec<String> = blocks
                                .iter()
                                .filter(|b| {
                                    let id = block_field(b, "ID: ").unwrap_or_default();
                                    !fresh_ids.iter().any(|s| s == &id)
                                })
                                .cloned()
                                .collect();
                            if let Err(e) = fs::write(&path, join_log(&header, &keep)) {
                                eprintln!("не записать ящик {}: {e}", path.display());
                            }
                        }
                        for b in fresh.iter() {
                            let id = block_field(b, "ID: ").unwrap_or_default();
                            if !id.is_empty() {
                                seen.push(id.clone());
                            }
                            println!("{}", render_block(b));
                            notify(&id, b);
                        }
                    }
                }
                if once {
                    break;
                }
                std::thread::sleep(Duration::from_secs(interval));
            }
            0
        }
        "alias" => {
            // Синонимы имён: один агент — один адрес в канале.
            if m.flag("list") {
                let map = crate::core::identity::alias_map();
                if map.is_empty() {
                    println!("алиасов нет");
                } else {
                    for (a, c) in map {
                        println!("{a} → {c}");
                    }
                }
                0
            } else if let Some(a) = m.value("remove") {
                match crate::core::identity::remove_alias(&a) {
                    Ok(true) => {
                        println!("синоним {a} убран");
                        0
                    }
                    Ok(false) => {
                        eprintln!("синоним {a} не был задан");
                        1
                    }
                    Err(e) => {
                        eprintln!("devh msg alias: {e}");
                        1
                    }
                }
            } else {
                let alias = m.first();
                if alias.is_empty() {
                    eprintln!(
                        "нужно: devh msg alias <синоним> [--for <канон>] | --list | --remove <синоним>"
                    );
                    return 1;
                }
                let canon = m.value("for").unwrap_or_else(|| me(None));
                match crate::core::identity::set_alias(&alias, &canon) {
                    Ok(()) => {
                        println!("синоним {alias} → {canon}");
                        0
                    }
                    Err(e) => {
                        eprintln!("devh msg alias: {e}");
                        1
                    }
                }
            }
        }
        "claim" => {
            // Закрепить имя за текущей сессией: дальше devh сам знает, кто я.
            let raw = m.first();
            if raw.is_empty() {
                eprintln!("нужно имя: devh msg claim <имя> [--takeover]");
                return 1;
            }
            let takeover = m.flag("takeover");
            match crate::core::identity::claim(&raw, takeover) {
                Ok(id) => {
                    println!("идентификация закреплена: {}", id.name);
                    println!("ключ сессии: {}", id.key);
                    if takeover {
                        println!("⚠ Использован флаг --takeover (авторизация ведущим)");
                    }
                    // Мини-описание сразу после claim: агент отвечает на вопрос
                    // «какая ты модель / что у тебя в system prompt» (может быть
                    // приблизительно — но работает в большинстве случаев).
                    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    crate::commands::about::print_about(&root);
                    0
                }
                Err(e) => {
                    eprintln!("devh msg claim: {e}");
                    1
                }
            }
        }
        "claim-authorize" => {
            // Авторизовать takeover имени ведущим: devh msg claim-authorize <имя> --for <ключ>
            let raw = m.first();
            if raw.is_empty() {
                eprintln!("нужно имя: devh msg claim-authorize <имя> --for <ключ>");
                return 1;
            }
            let for_key = m.value("for");
            if for_key.is_none() {
                eprintln!("нужен ключ сессии: --for <ключ>");
                return 1;
            }
            // Проверка: вызывающий должен быть ведущим (lead).
            let caller = crate::core::identity::current().name;
            let canon_caller = crate::core::identity::canonical(&caller);
            let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            // Ищем .harness/orch/registry.json от текущей директории вверх.
            let mut harness_path: Option<PathBuf> = None;
            let mut cur = root.clone();
            loop {
                let candidate = cur.join(".harness/orch/registry.json");
                if candidate.exists() {
                    harness_path = Some(cur.clone());
                    break;
                }
                if !cur.pop() {
                    break;
                }
            }
            // Worktree агента: реестра в нём нет — он в ГЛАВНОМ дереве.
            if harness_path.is_none() {
                if let Ok(main_root) = crate::shell::git::main_root_from_cwd(&root) {
                    if main_root.join(".harness/orch/registry.json").exists() {
                        harness_path = Some(main_root);
                    }
                }
            }
            if let Some(repo_root) = harness_path {
                let reg = crate::commands::orch::load(&repo_root);
                let (lead, _) = crate::commands::orch::lead_of(&reg);
                if let Some(lead_addr) = lead {
                    let canon_lead = crate::core::identity::canonical(&lead_addr);
                    if canon_caller != canon_lead {
                        eprintln!("devh msg claim-authorize: только ведущий ({}) может авторизовать takeover", canon_lead);
                        return 1;
                    }
                } else {
                    eprintln!("devh msg claim-authorize: ведущий не назначен (devh orch lead --set <адрес>)");
                    return 1;
                }
            } else {
                eprintln!("devh msg claim-authorize: не найден .harness/orch/registry.json (не в репозитории devh)");
                return 1;
            }
            match crate::core::identity::authorize_takeover(&raw, &for_key.as_ref().unwrap()) {
                Ok(_) => {
                    println!(
                        "takeover авторизован для {} (ключ: {})",
                        raw,
                        for_key.as_ref().unwrap()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("devh msg claim-authorize: {e}");
                    1
                }
            }
        }
        "about" => {
            // Мини-описание агента: адрес, модель, рантайм, сводка system prompt.
            let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            crate::commands::about::print_about(&root);
            0
        }
        "whoami" => {
            let id = crate::core::identity::current();
            println!("имя: {}", id.name);
            println!("источник: {}", id.source.as_str());
            println!("ключ: {}", id.key);
            println!("непрочитанных (своих): {}", own_blocks(&id.name).len());
            if !id.aliases.is_empty() {
                println!("синонимы (тот же агент): {}", id.aliases.join(", "));
            }
            println!("legacy (общий старый ящик): {}", legacy_blocks().len());
            if id.source == crate::core::identity::Source::Cwd {
                println!(
                    "подсказка: ключ по каталогу не различает соседей — закрепите имя: devh msg claim <имя>"
                );
            }
            // Модель/рантайм/роль — коротко (полный блок: devh msg about).
            let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let about = crate::commands::about::collect(&root);
            println!("{}", crate::commands::about::short(&about));
            0
        }
        "threads" => {
            let agent = me(m.value("agent").as_deref());
            let mut rows: Vec<(String, String, usize, Vec<String>)> = Vec::new();
            if let Ok(dir) = fs::read_dir(threads_dir()) {
                for e in dir.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let Some(tid) = name.strip_suffix(".log") else {
                        continue;
                    };
                    let text = fs::read_to_string(e.path()).unwrap_or_default();
                    let (_h, blocks) = split_log(&text);
                    if blocks.is_empty() {
                        continue;
                    }
                    let who = thread_participants(&blocks);
                    if !who.iter().any(|w| w == &agent) {
                        continue;
                    }
                    let mut tags: Vec<String> = Vec::new();
                    for b in blocks.iter() {
                        for t in block_tags(b) {
                            if !tags.contains(&t) {
                                tags.push(t);
                            }
                        }
                    }
                    let first = blocks.first().cloned().unwrap_or_default();
                    let subject = block_field(&first, "ТЕМА: ").unwrap_or_default();
                    let place = block_field(&first, "МЕСТО: ").unwrap_or_default();
                    let label = if place.is_empty() {
                        subject
                    } else {
                        format!("{subject} @ {place}")
                    };
                    rows.push((tid.to_string(), label, blocks.len(), tags));
                }
            }
            rows.sort();
            if rows.is_empty() {
                println!("открытых тредов с твоим участием нет");
            }
            for (tid, label, n, tags) in rows {
                println!(
                    "тред {tid}: сообщений={n} тэги=[{}] {label}",
                    tags.join(", ")
                );
            }
            0
        }
        "acks" => {
            let agent = me(m.value("agent").as_deref());
            let path = acks_path(&safe_name(&agent));
            let text = fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<String> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect();
            if lines.is_empty() {
                println!("(подтверждений нет)");
            }
            for l in lines.iter() {
                println!("{l}");
            }
            // поток подтверждений НЕ уничтожается по умолчанию: по нему работает
            // `pending` (что не прочитано). Очистить — флагом --consume.
            if m.flag("consume") && !lines.is_empty() {
                let _ = fs::write(&path, "");
            }
            0
        }
        "pending" => {
            // Что из отправленного ещё не прочитано адресатами.
            let agent = me(m.value("agent").as_deref());
            let canon = crate::core::identity::canonical(&agent);
            // Синнонимные ящики — в канонический (дедуп по ID): иначе они
            // считаются отдельными письмами и раздувают счёт (R-8).
            migrate_synonym_inboxes(&canon);
            let sent_text = fs::read_to_string(outbox_path(&safe_name(&agent))).unwrap_or_default();
            let (_h, sent_blocks) = split_log(&sent_text);
            // Читаем acks из канонического ящика + всех синонимов + legacy имён.
            let mut acks_text = String::new();
            // Читаем ВСЕ acks-файлы: старые acks могли быть записаны
            // под синонимом/legacy-именем отправителя.
            if let Ok(dir) = fs::read_dir(acks_dir()) {
                for e in dir.flatten() {
                    if let Ok(t) = fs::read_to_string(e.path()) {
                        acks_text.push_str(&t);
                        acks_text.push('\n');
                    }
                }
            }
            let read_ids: Vec<String> = acks_text
                .lines()
                .filter(|l| l.contains("прочитано"))
                .filter_map(|l| l.split_whitespace().nth(2).map(str::to_string))
                .collect();
            let mut rows: Vec<(String, String)> = Vec::new();
            for b in sent_blocks.iter() {
                let id = block_field(b, "ID: ").unwrap_or_default();
                if id.is_empty() || read_ids.iter().any(|r| r == &id) {
                    continue;
                }
                // Письмо самому себе (в т.ч. по синониму) не ждёт чужого
                // прочтения: оно лежит в своём ящике и читается через
                // `devh msg read`. Считать его «ожидающим» нельзя — иначе
                // счёт пухнет от писем самому себе (R-8).
                if addressed_only_to_self(b, &canon) {
                    continue;
                }
                let to = block_recipients(b);
                let subject = block_field(b, "ТЕМА: ").unwrap_or_default();
                rows.push((id, format!("-> {to}: {subject}")));
            }
            if rows.is_empty() {
                println!("всё отправленное прочитано");
            }
            for (id, what) in rows {
                println!("не прочитано {id} {what}");
            }
            0
        }
        "presence" => {
            let window: u64 = m
                .value("window")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(120);
            let rows = presence_rows();
            if rows.is_empty() {
                println!("никто не отмечался (presence пуст)");
            }
            let now = unix_now();
            for (agent, action, ts, pid) in rows.iter() {
                let fresh = now.saturating_sub(*ts) <= window;
                let alive = pid.map(pid_alive).unwrap_or(false);
                let state = if action == "watch" {
                    // Сессия с живым watcher'ом: только он и есть «онлайн».
                    if fresh && alive {
                        "ОНЛАЙН"
                    } else {
                        "не в сети"
                    }
                } else {
                    // Одноразовое действие (send/read): онлайн-статус по нему
                    // не судим — это «последняя активность», а не присутствие.
                    "активность"
                };
                println!(
                    "{agent}	{state}	{}	{action}	pid={}",
                    ago(*ts),
                    pid.map(|p| p.to_string())
                        .unwrap_or_else(|| "?".to_string())
                );
            }
            0
        }
        "list" => {
            // Уведомление о новых входящих: один popup на пачку, без повторов
            // (дедуп по notified/<агент>.log). Раньше auto_notify пропускался
            // для всех `cmd msg`, поэтому агент, запустивший `msg list`,
            // терял уведомление, и письма «висели» молча (R-4).
            {
                let who = me(None);
                auto_notify(&who);
            }
            let mut rows: Vec<(String, usize)> = Vec::new();
            if let Ok(dir) = fs::read_dir(inbox_dir()) {
                for e in dir.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Some(base) = name.strip_suffix(".log") {
                        let text = fs::read_to_string(e.path()).unwrap_or_default();
                        let (_h, blocks) = split_log(&text);
                        rows.push((base.to_string(), blocks.len()));
                    }
                }
            }
            rows.sort();
            if rows.is_empty() {
                println!("входящих нет ({})", inbox_dir().display());
            }
            for (agent, n) in rows {
                println!("{agent}: непрочитанных={n}");
            }
            let mut threads: Vec<(String, usize)> = Vec::new();
            if let Ok(dir) = fs::read_dir(threads_dir()) {
                for e in dir.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Some(base) = name.strip_suffix(".log") {
                        let text = fs::read_to_string(e.path()).unwrap_or_default();
                        let (_h, blocks) = split_log(&text);
                        threads.push((base.to_string(), blocks.len()));
                    }
                }
            }
            threads.sort();
            for (t, n) in threads {
                println!("тред {t}: сообщений={n}");
            }
            0
        }
        "sent" => {
            let agent = me(m.value("agent").as_deref());
            let text = fs::read_to_string(outbox_path(&safe_name(&agent))).unwrap_or_default();
            let (_h, blocks) = split_log(&text);
            for b in blocks.iter() {
                println!("{b}{SEP}");
            }
            0
        }
        "find" => {
            let needle = m.first();
            if needle.is_empty() {
                eprintln!("нужен тэг или префикс ID");
                return 1;
            }
            let mut hits = 0usize;
            let mut files: Vec<PathBuf> = Vec::new();
            for dir in [inbox_dir(), outbox_dir(), threads_dir()] {
                if let Ok(entries) = fs::read_dir(dir) {
                    for e in entries.flatten() {
                        files.push(e.path());
                    }
                }
            }
            files.sort();
            for path in files {
                let text = fs::read_to_string(&path).unwrap_or_default();
                let (_h, blocks) = split_log(&text);
                for b in blocks.iter() {
                    let tags = block_tags(b);
                    let id = block_field(b, "ID: ").unwrap_or_default();
                    if tags.iter().any(|t| t == &needle) || id.starts_with(&needle) {
                        let who = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        println!(
                            "{who} ID {id} тэги [{}]: {}",
                            tags.join(", "),
                            block_field(b, "ТЕМА: ").unwrap_or_default()
                        );
                        hits = hits.saturating_add(1);
                    }
                }
            }
            if hits == 0 {
                println!("не найдено: {needle}");
            }
            0
        }
        "close" => {
            let thread = m.first();
            if thread.is_empty() {
                eprintln!("нужен ID треда");
                return 1;
            }
            let actor = m
                .value("by")
                .unwrap_or_else(|| me(m.value("agent").as_deref()));
            let text = fs::read_to_string(thread_path(&thread)).unwrap_or_default();
            let (_h, blocks) = split_log(&text);
            if blocks.is_empty() {
                eprintln!("тред {thread} не найден");
                return 1;
            }
            let participants = thread_participants(&blocks);
            let mut votes: Vec<String> = Vec::new();
            if let Ok(raw) = fs::read_to_string(votes_path(&thread)) {
                for line in raw.lines() {
                    if let Some((a, _)) = line.split_once(' ') {
                        votes.push(a.to_string());
                    }
                }
            }
            if !votes.iter().any(|v| v == &actor) {
                votes.push(actor.clone());
            }
            let voted: Vec<String> = participants
                .iter()
                .filter(|p| votes.iter().any(|v| v == *p))
                .cloned()
                .collect();
            let missing: Vec<String> = participants
                .iter()
                .filter(|p| !votes.iter().any(|v| v == *p))
                .cloned()
                .collect();
            let force = m.flag("force");
            if missing.is_empty() || force {
                let _ = fs::remove_file(thread_path(&thread));
                let _ = fs::remove_file(votes_path(&thread));
                // непрочитанные копии этого треда — тоже вон (вопрос решён)
                for agent in roster() {
                    let path = inbox_path(&safe_name(&agent));
                    let raw = fs::read_to_string(&path).unwrap_or_default();
                    let (header, blks) = split_log(&raw);
                    let kept: Vec<String> = blks
                        .iter()
                        .filter(|b| block_field(b, "ТРЕД: ").as_deref() != Some(thread.as_str()))
                        .cloned()
                        .collect();
                    if kept.len() != blks.len() {
                        let _ = fs::write(&path, join_log(&header, &kept));
                    }
                }
                let stamp = now_hm();
                let how = if force {
                    "форс"
                } else {
                    "консенсус"
                };
                let tomb = format!(
                    "[{stamp}] ОТ: {actor} — КОМУ: все\nТРЕД: {thread}\nТЕМА: тред закрыт ({how})\n"
                );
                let _ = append_message(&outbox_path(&safe_name(&actor)), &tomb);
                println!(
                    "тред {thread} закрыт ({how}): голоса {}/{}",
                    voted.len(),
                    participants.len()
                );
                return 0;
            }
            let mut lines: Vec<String> = Vec::new();
            for v in voted.iter() {
                lines.push(format!("{v} {}", unix_now()));
            }
            if let Some(parent) = votes_path(&thread).parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(votes_path(&thread), lines.join("\n") + "\n");
            println!(
                "голос за закрытие треда {thread} принят ({}/{}); ждём: {}",
                voted.len(),
                participants.len(),
                missing.join(", ")
            );
            0
        }
        _ => {
            println!(
                "канал агентов: send | reply | read | watch | thread | threads | close | list | sent | find | acks | pending | presence | alias | claim | whoami\n\
                 письмо с одним или несколькими адресатами; прочитанное удаляется (consume-on-read);\n\
                 обсуждение (тред) сохраняется целиком, треды закрываются консенсусом;\n\
                 указание на код: --at путь[:строка] (регион с контекстом) или --diff файл (unified diff);\n\
                 привязка к коду — по содержимому (контексту), номер строки может сдвинуться"
            );
            1
        }
    }
}

/// Точка входа: `devh msg ...` (args — всё после «msg»).
pub fn run(args: &[String]) -> i32 {
    let owned: Vec<String> = args.to_vec();
    send(&owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сериализация тестов, меняющих DEVH_MSG_HOME (env общий для процесса).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_msg_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        // Отравление lock'а не должно «убивать» остальные тесты: забираем
        // его и при отравленном (poison recovery) — иначе один упавший тест
        // превращался в каскад «entered unreachable code».
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var("DEVH_MSG_HOME", home);
        std::env::set_var("MSG_NOTIFY_DEDUPE", home.join("notified.dedupe"));
        std::env::set_var("DEVH_MSG_POPUP", "0");
        std::env::set_var("DEVH_MSG_SOUND", "0");
        let out = f();
        std::env::remove_var("DEVH_MSG_HOME");
        std::env::remove_var("MSG_NOTIFY_DEDUPE");
        std::env::remove_var("DEVH_MSG_POPUP");
        std::env::remove_var("DEVH_MSG_SOUND");
        drop(guard);
        out
    }

    fn write_inbox(home: &Path, agent: &str, block: &str) {
        let inbox = home.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap_or_default();
        let text = format!("{block}{SEP}\n");
        std::fs::write(inbox.join(format!("{agent}.log")), text).unwrap_or_default();
    }

    /// Письмо самому себе (в т.ч. по синониму) не ждёт чужого прочтения —
    /// иначе счёт `pending` пухнет от писем самому себе (R-8).
    #[test]
    fn addressed_only_to_self_detects_self_letters() {
        let home = std::env::temp_dir().join(format!("devh-test-self-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap_or_default();
        std::fs::write(
            home.join("aliases.json"),
            r#"{"aliases": {"deepseek": "beam"}}"#,
        )
        .unwrap_or_default();
        with_msg_home(&home, || {
            let to_self = "[10:00] ОТ: beam — КОМУ: deepseek\nID: x1\nТЕМА: t";
            assert!(
                addressed_only_to_self(to_self, "beam"),
                "письмо по синониму — себе"
            );
            let to_self_direct = "[10:00] ОТ: beam — КОМУ: beam\nID: x2\nТЕМА: t";
            assert!(addressed_only_to_self(to_self_direct, "beam"));
            let to_other = "[10:00] ОТ: beam — КОМУ: target\nID: x3\nТЕМА: t";
            assert!(
                !addressed_only_to_self(to_other, "beam"),
                "письмо другому — не себе"
            );
            let to_mixed = "[10:00] ОТ: beam — КОМУ: beam, target\nID: x4\nТЕМА: t";
            assert!(
                !addressed_only_to_self(to_mixed, "beam"),
                "письмо себе и другому — ждёт другого"
            );
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn parse_view_reads_headers_and_body() {
        let b = "[10:24] ОТ: deepseek — КОМУ: minimax\n\
ID: abcd1234\n\
ТРЕД: thr-1\n\
ТЕМА: F-2 — FREEZE\n\
МЕСТО: devharness/src/commands/run.rs\n\
ТЭГИ: f2, freeze\n\
ТЕЛО:\n\
строка тела\n\
вторая строка";
        let v = parse_view(b);
        assert_eq!(v.stamp, "10:24");
        assert_eq!(v.sender, "deepseek");
        assert_eq!(v.to, "minimax");
        assert_eq!(v.id, "abcd1234");
        assert_eq!(v.thread, "thr-1");
        assert_eq!(v.subject, "F-2 — FREEZE");
        assert_eq!(v.place.as_deref(), Some("devharness/src/commands/run.rs"));
        assert_eq!(v.tags.len(), 2, "тэги: {v:?}");
        assert_eq!(v.body, "строка тела\nвторая строка");
    }

    #[test]
    fn parse_view_reads_old_format_without_body_marker() {
        let b = "[10:00] ОТ: sender — КОМУ: a\nID: x\nТЕМА: t\nтело старого письма";
        let v = parse_view(b);
        assert_eq!(v.subject, "t");
        assert_eq!(v.body, "тело старого письма");
    }

    #[test]
    fn parse_view_keeps_body_of_old_letter_with_snippet() {
        // Старый формат: вырезка и тело не разделены маркером ТЕЛО:.
        // Раньше такой блок терялся целиком (вырезка «съедала» тело).
        let b = "[10:00] ОТ: sender — КОМУ: a\nID: x\nТЕМА: t\n\
ВЫРЕЗКА:\n\
  42 | let x = 1;\n\
тело старого письма";
        let v = parse_view(b);
        assert!(
            v.body.contains("тело старого письма"),
            "тело не потеряно: {v:?}"
        );
    }

    #[test]
    fn parse_view_skips_code_snippet() {
        let b = "[10:00] ОТ: sender — КОМУ: a\nID: x\n\
ВЫРЕЗКА:\n\
  42 | let x = 1;\n\
ТЭГИ: code\n\
ТЕЛО:\n\
текст";
        let v = parse_view(b);
        assert_eq!(v.body, "текст", "вырезка не попала в тело: {v:?}");
        assert_eq!(v.tags.len(), 1);
    }

    #[test]
    fn render_block_is_readable() {
        let b = "[10:24] ОТ: deepseek — КОМУ: minimax\nID: abcd1234\n\
ТРЕД: thr-1\nТЕМА: Тема письма\nТЕЛО:\nтело";
        let out = render_block(b);
        assert!(out.contains("deepseek"), "{out}");
        assert!(out.contains("minimax"), "{out}");
        assert!(out.contains("Тема письма"), "{out}");
        assert!(out.contains("thr-1"), "{out}");
        assert!(out.contains("тело"), "{out}");
        assert!(out.contains('━'), "рамка: {out}");
    }

    #[test]
    fn auto_notify_shows_new_message_once() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "test-agent",
            "[10:24] ОТ: deepseek — КОМУ: test-agent\nID: aa11bb22\n\
ТРЕД: t1\nТЕМА: Письмо\nТЕЛО:\nтело\n",
        );
        let (first, second) = with_msg_home(home, || {
            (auto_notify("test-agent"), auto_notify("test-agent"))
        });
        assert_eq!(first, 1, "первый вызов показал письмо");
        assert_eq!(second, 0, "повтор не дублирует (дедуп по ID)");
    }

    #[test]
    fn unread_brief_lists_pending_messages() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "test-agent",
            "[10:24] ОТ: deepseek — КОМУ: test-agent\nID: aa11bb22\n\
ТРЕД: t1\nТЕМА: Письмо\nТЕЛО:\nтело письма\n",
        );
        let brief = with_msg_home(home, || unread_brief("test-agent"));
        assert!(brief.contains("ВХОДЯЩИЕ СООБЩЕНИЯ"), "{brief}");
        assert!(brief.contains("Письмо"), "{brief}");
        assert!(brief.contains("тело письма"), "{brief}");
    }

    #[test]
    fn legacy_inbox_is_not_consumed() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // legacy-ящик: письмо старой схемы (адресат «default»).
        write_inbox(
            home,
            "default",
            "[10:24] ОТ: deepseek — КОМУ: default\nID: bb22cc33\n\
ТРЕД: t1\nТЕМА: Старое письмо\nТЕЛО:\nтекст\n",
        );
        let (shown, left) = with_msg_home(home, || {
            let blocks = own_blocks("test-agent");
            let legacy = legacy_blocks();
            // читаем «legacy» — но не едим
            let legacy_len = legacy.len();
            let _ = blocks;
            (legacy_len, blocks_in_file(&legacy_inbox_path()).len())
        });
        assert_eq!(shown, 1, "legacy-письмо видно");
        assert_eq!(left, 1, "legacy-письмо НЕ съедено другим агентом");
    }

    #[test]
    fn own_and_legacy_inboxes_are_separate() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "test-agent",
            "[10:24] ОТ: deepseek — КОМУ: test-agent\nID: aa11bb22\n\
ТРЕД: t1\nТЕМА: Своё\nТЕЛО:\nсвоё тело\n",
        );
        write_inbox(
            home,
            "default",
            "[10:24] ОТ: deepseek — КОМУ: default\nID: bb22cc33\n\
ТРЕД: t1\nТЕМА: Старое\nТЕЛО:\nстарое тело\n",
        );
        with_msg_home(home, || {
            assert_eq!(own_blocks("test-agent").len(), 1, "свой ящик");
            assert_eq!(legacy_blocks().len(), 1, "legacy-ящик");
            assert_eq!(inbox_blocks("test-agent").len(), 2, "видимы оба");
        });
    }

    #[test]
    fn auto_notify_can_be_disabled() {
        // Гасится DEVH_MSG_NO_AUTO / HARNESS_TEST (сьют не должен трогать
        // реальный ящик агентов).
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "test-agent",
            "[10:24] ОТ: deepseek — КОМУ: test-agent\nID: aa11bb22\n\
ТРЕД: t1\nТЕМА: Письмо\nТЕЛО:\nтело\n",
        );
        with_msg_home(home, || {
            std::env::set_var("DEVH_MSG_NO_AUTO", "1");
            assert!(!auto_notify_enabled(), "NO_AUTO=1 выключает");
            std::env::remove_var("DEVH_MSG_NO_AUTO");
            std::env::set_var("HARNESS_TEST", "1");
            assert!(!auto_notify_enabled(), "HARNESS_TEST=1 выключает");
            std::env::remove_var("HARNESS_TEST");
            assert!(auto_notify_enabled(), "по умолчанию включено");
        });
    }

    fn block_of(to: &str, subject: &str, tags: &[&str], body: &str) -> String {
        let mut b = format!(
            "[10:00] ОТ: sender — КОМУ: {to}\nID: abcd1234\nТРЕД: 11112222\nТЕМА: {subject}\n"
        );
        if !tags.is_empty() {
            b.push_str(&format!("ТЭГИ: {}\n", tags.join(", ")));
        }
        b.push_str(body);
        b
    }

    #[test]
    fn split_log_keeps_blocks_and_header() {
        let text = format!(
            "# шапка\n{SEP}\n{}{SEP}\n",
            block_of("a", "тема", &[], "тело")
        );
        let (header, blocks) = split_log(&text);
        assert!(header.contains("шапка"), "шапка сохраняется: {header}");
        assert_eq!(blocks.len(), 1, "один блок: {blocks:?}");
        assert!(
            blocks.first().is_some_and(|b| b.contains("ТЕМА: тема")),
            "тема в блоке: {blocks:?}"
        );
    }

    #[test]
    fn split_log_tolerates_file_starting_with_block() {
        // канал, созданный promote'ом: файл начинается с блока, без шапки
        let text = format!("{}{SEP}\n", block_of("a", "тема", &[], "тело"));
        let (_header, blocks) = split_log(&text);
        assert_eq!(blocks.len(), 1, "блок без шапки не теряется: {blocks:?}");
    }

    #[test]
    fn join_log_roundtrip() {
        let blocks = vec![
            block_of("a", "t1", &["x"], "b1"),
            block_of("b", "t2", &[], "b2"),
        ];
        let text = join_log("# h\n", &blocks);
        let (_h, back) = split_log(&text);
        assert_eq!(back.len(), 2, "оба блока на месте: {back:?}");
    }

    #[test]
    fn parse_place_splits_path_and_line() {
        assert_eq!(
            parse_place("devharness/src/core/load.rs:42"),
            ("devharness/src/core/load.rs".to_string(), Some(42))
        );
        assert_eq!(
            parse_place("devharness/src/core/load.rs"),
            ("devharness/src/core/load.rs".to_string(), None)
        );
    }

    #[test]
    fn block_sender_and_recipients_parse_header() {
        let b = block_of("a, b", "тема", &[], "тело");
        assert_eq!(block_sender(&b), "sender");
        assert_eq!(block_recipients(&b), "a, b");
    }

    #[test]
    fn block_tags_reads_several() {
        let b = block_of("a", "тема", &["ошибка", "ревью"], "тело");
        let tags = block_tags(&b);
        assert_eq!(tags.len(), 2, "оба тэга: {tags:?}");
        assert!(tags.iter().any(|t| t == "ошибка"));
        assert!(tags.iter().any(|t| t == "ревью"));
    }

    #[test]
    fn thread_participants_cover_sender_and_recipients() {
        let blocks = vec![block_of("a, b", "тема", &[], "тело")];
        let who = thread_participants(&blocks);
        assert!(who.iter().any(|w| w == "sender"), "отправитель: {who:?}");
        assert!(who.iter().any(|w| w == "a"), "адресат a: {who:?}");
        assert!(who.iter().any(|w| w == "b"), "адресат b: {who:?}");
    }

    #[test]
    fn code_region_marks_pointed_line_and_keeps_context() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        // (tempdir без unwrap — репо запрещает expect/unwrap в проде)
        let path = dir.path().join("f.rs");
        let body = (1..=12)
            .map(|n| format!("line{n}"))
            .collect::<Vec<String>>()
            .join("\n");
        std::fs::write(&path, body).unwrap_or_default();
        let (p, plain, marked) = code_region(&path.to_string_lossy(), Some(6)).unwrap_or_default();
        assert!(p.ends_with("f.rs"), "путь: {p}");
        assert!(
            plain.contains("line5") && plain.contains("line6"),
            "контекст: {plain}"
        );
        assert!(marked.contains('»'), "маркер указанной строки: {marked}");
        assert!(marked.contains("line5"), "контекст в вырезке: {marked}");
    }

    #[test]
    fn code_region_without_line_returns_first_lines() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        // (tempdir без unwrap — репо запрещает expect/unwrap в проде)
        let path = dir.path().join("g.rs");
        std::fs::write(&path, "a\nb\nc\n").unwrap_or_default();
        let (_p, plain, _marked) = code_region(&path.to_string_lossy(), None).unwrap_or_default();
        assert!(plain.contains('a'), "первые строки: {plain}");
    }

    #[test]
    fn build_message_carries_thread_place_and_tags() {
        let att = Attachment {
            place: Some("devharness/src/core/load.rs".to_string()),
            snippet: Some("  42 | let x = 1;\n".to_string()),
        };
        let msg = build_message(
            "me",
            &["peer".to_string()],
            "ошибка",
            &["ошибка".to_string()],
            "тело",
            "thr",
            &att,
        );
        assert!(msg.contains("ОТ: me"), "отправитель: {msg}");
        assert!(msg.contains("КОМУ: peer"), "адресат: {msg}");
        assert!(msg.contains("ТРЕД: thr"), "тред: {msg}");
        assert!(
            msg.contains("МЕСТО: devharness/src/core/load.rs"),
            "место: {msg}"
        );
        assert!(msg.contains("ВЫРЕЗКА:"), "вырезка: {msg}");
        assert!(msg.contains("ТЭГИ: ошибка"), "тэги: {msg}");
    }

    #[test]
    fn short_id_differs_for_same_fields() {
        // одинаковые поля в одну минуту -> разные ID (иначе forward даёт ID оригинала)
        let a = short_id("me", "s", "b", "10:00");
        let b = short_id("me", "s", "b", "10:00");
        assert_ne!(a, b, "ID должны различаться: {a} {b}");
    }

    #[test]
    fn ago_formats_relative_time() {
        let now = unix_now();
        assert_eq!(ago(now), "0 с назад");
        assert!(ago(now.saturating_sub(120)).ends_with("мин назад"));
        assert!(ago(now.saturating_sub(7200)).ends_with("ч назад"));
    }

    #[test]
    fn deliver_two_synonyms_one_message() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // Создаём два алиаса → beam
        crate::core::identity::set_alias_in(home, "opencode", "beam")
            .unwrap_or_else(|_| unreachable!());
        crate::core::identity::set_alias_in(home, "kilo", "beam")
            .unwrap_or_else(|_| unreachable!());
        // Устанавливаем DEVH_MSG_HOME на temp
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("DEVH_MSG_HOME", home);
        std::env::set_var("MSG_NOTIFY_DEDUPE", home.join("notified.dedupe"));
        std::env::set_var("DEVH_MSG_POPUP", "0");
        std::env::set_var("DEVH_MSG_SOUND", "0");
        let block = "[10:00] ОТ: sender — КОМУ: opencode, kilo\nID: test123\nТРЕД: t1\nТЕМА: test\nТЕЛО:\nbody\n";
        let done = deliver(
            "sender",
            &["opencode".to_string(), "kilo".to_string()],
            block,
            "t1",
        );
        // beam — канон для обоих, значит одно письмо
        assert_eq!(done.len(), 1, "одно письмо, не два: {done:?}");
        // Проверяем что в inbox/beam.log ровно один блок
        let text = std::fs::read_to_string(home.join("inbox").join("beam.log")).unwrap_or_default();
        let (_, blocks) = split_log(&text);
        assert_eq!(blocks.len(), 1, "один блок в inbox: {blocks:?}");
        std::env::remove_var("DEVH_MSG_HOME");
        std::env::remove_var("MSG_NOTIFY_DEDUPE");
        std::env::remove_var("DEVH_MSG_POPUP");
        std::env::remove_var("DEVH_MSG_SOUND");
        drop(_guard);
    }

    #[test]
    fn set_alias_race_both_survive() {
        use std::thread;
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path().to_path_buf();
        let h1 = home.clone();
        let h2 = home.clone();
        let t1 = thread::spawn(move || {
            crate::core::identity::set_alias_in(&h1, "alias-a", "canon-a").ok();
        });
        let t2 = thread::spawn(move || {
            crate::core::identity::set_alias_in(&h2, "alias-b", "canon-b").ok();
        });
        t1.join().unwrap();
        t2.join().unwrap();
        let map = crate::core::identity::alias_map_in(&home);
        assert!(map.contains_key("alias-a"), "alias-a должен быть: {map:?}");
        assert!(map.contains_key("alias-b"), "alias-b должен быть: {map:?}");
    }

    #[test]
    fn migrate_synonym_inboxes_no_duplicates() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("DEVH_MSG_HOME", home);
        std::env::set_var("MSG_NOTIFY_DEDUPE", home.join("notified.dedupe"));
        std::env::set_var("DEVH_MSG_POPUP", "0");
        std::env::set_var("DEVH_MSG_SOUND", "0");
        // Создаём алиас kilo → beam
        crate::core::identity::set_alias_in(home, "kilo", "beam")
            .unwrap_or_else(|_| unreachable!());
        // Пишем письмо в оба ящика с одинаковым ID
        let block =
            "[10:00] ОТ: sender — КОМУ: beam\nID: dup1\nТРЕД: t1\nТЕМА: test\nТЕЛО:\nbody\n";
        let inbox = home.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap_or_default();
        std::fs::write(inbox.join("beam.log"), format!("{block}{}\n", SEP)).unwrap_or_default();
        std::fs::write(inbox.join("kilo.log"), format!("{block}{}\n", SEP)).unwrap_or_default();
        // Миграция
        migrate_synonym_inboxes("beam");
        // kilo.log должен быть удалён
        assert!(
            !inbox.join("kilo.log").exists(),
            "kilo.log удалён после миграции"
        );
        // beam.log должен содержать ровно один блок (дедуп)
        let text = std::fs::read_to_string(inbox.join("beam.log")).unwrap_or_default();
        let (_, blocks) = split_log(&text);
        assert_eq!(blocks.len(), 1, "один блок после дедупа: {blocks:?}");
        std::env::remove_var("DEVH_MSG_HOME");
        std::env::remove_var("MSG_NOTIFY_DEDUPE");
        std::env::remove_var("DEVH_MSG_POPUP");
        std::env::remove_var("DEVH_MSG_SOUND");
        drop(_guard);
    }

    #[test]
    fn inbox_files_no_synonym_boxes_after_migration() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("DEVH_MSG_HOME", home);
        std::env::set_var("MSG_NOTIFY_DEDUPE", home.join("notified.dedupe"));
        std::env::set_var("DEVH_MSG_POPUP", "0");
        std::env::set_var("DEVH_MSG_SOUND", "0");
        crate::core::identity::set_alias_in(home, "kilo", "beam")
            .unwrap_or_else(|_| unreachable!());
        let files = inbox_files("beam");
        // После миграции только own + legacy (если есть)
        for f in &files {
            assert!(
                !f.to_string_lossy().contains("kilo.log"),
                "нет kilo.log в списке: {f:?}"
            );
        }
        std::env::remove_var("DEVH_MSG_HOME");
        std::env::remove_var("MSG_NOTIFY_DEDUPE");
        std::env::remove_var("DEVH_MSG_POPUP");
        std::env::remove_var("DEVH_MSG_SOUND");
        drop(_guard);
    }

    #[test]
    fn safe_name_strips_path_separators() {
        assert_eq!(safe_name("../evil"), ".._evil");
        assert_eq!(safe_name(""), "agent");
        assert_eq!(safe_name("ok-name_1"), "ok-name_1");
    }
}
