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

const SEP: &str =
    "================================================================================";
const NOTIFY_TIMEOUT_MS: u64 = 20_000;
const DEDUPE_PATH: &str = "/tmp/messages.notified";

fn home() -> PathBuf {
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
    presence_dir().join(format!("{}.txt", safe_name(agent)))
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
            out.push((base.to_string(), action, ts, pid));
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

fn outbox_path(agent: &str) -> PathBuf {
    outbox_dir().join(format!("{agent}.log"))
}

/// Имя агента → безопасное имя файла (без разделителей пути).
fn safe_name(agent: &str) -> String {
    let cleaned: String = agent
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned
    }
}

fn me(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("DEVH_AGENT").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "default".to_string())
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

/// Ростер агентов: все, у кого есть входящие или исходящие.
fn roster() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in [inbox_dir(), outbox_dir()] {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(base) = name.strip_suffix(".log") {
                    if !names.iter().any(|n| n == base) {
                        names.push(base.to_string());
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
    let mut ok = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .is_ok();
    if ok {
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(path) {
            ok = writeln!(f, "{block}{SEP}").is_ok();
        }
    }
    ok
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
                    if now.saturating_sub(t) < 90 {
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

    let subject = block_field(text, "ТЕМА: ").unwrap_or_else(|| "новое сообщение".to_string());
    let sender = block_field(text, "ОТ: ").unwrap_or_default();
    let first_body = text
        .lines()
        .filter(|l| {
            !l.starts_with('[')
                && !l.starts_with("ID:")
                && !l.starts_with("ТЕМА:")
                && !l.starts_with("ТЭГИ:")
        })
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    let summary = format!("{sender}: {subject} — {}", first_body.trim());
    let _ = Command::new("notify-send")
        .arg("-t")
        .arg(NOTIFY_TIMEOUT_MS.to_string())
        .arg("Сессии: сообщение")
        .arg(summary.chars().take(220).collect::<String>())
        .output();
    let _ = Command::new("paplay")
        .arg("/usr/share/sounds/freedesktop/stereo/message.oga")
        .output();
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
    block.push_str(&format!("\n{}\n", body.trim_end()));
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
    let mut unique: Vec<String> = Vec::new();
    for t in targets.iter() {
        if !unique.iter().any(|x| x == t) {
            unique.push(t.clone());
        }
    }
    let msg_id = block_field(block, "ID: ").unwrap_or_default();
    let mut done: Vec<String> = Vec::new();
    for t in unique.iter() {
        let path = inbox_path(&safe_name(t));
        if append_message(&path, block) {
            done.push(t.clone());
            ack(&safe_name(from), &msg_id, "доставлено");
        }
    }
    let _ = append_message(&thread_path(thread), block);
    let _ = append_message(&outbox_path(&safe_name(from)), block);
    done
}

fn send(args: &[String]) -> i32 {
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
                return 2;
            }
            let text = fs::read_to_string(thread_path(&id)).unwrap_or_default();
            let (_h, blocks) = split_log(&text);
            if blocks.is_empty() {
                eprintln!("тред {id} не найден");
                return 1;
            }
            for b in blocks.iter() {
                println!("{b}{SEP}");
            }
            0
        }
        "read" => {
            let agent = me(m.value("agent").as_deref());
            let peek = m.flag("peek");
            let filter = m.value("tag");
            let path = inbox_path(&safe_name(&agent));
            let text = fs::read_to_string(&path).unwrap_or_default();
            let (header, blocks) = split_log(&text);
            let mut kept: Vec<String> = Vec::new();
            let mut shown = 0usize;
            for b in blocks.iter() {
                let matches = match filter.as_ref() {
                    None => true,
                    Some(t) => block_tags(b).iter().any(|x| x == t),
                };
                if matches {
                    println!("{b}{SEP}");
                    shown = shown.saturating_add(1);
                    if !peek {
                        let id = block_field(b, "ID: ").unwrap_or_default();
                        let sender = block_sender(b);
                        ack(&safe_name(&sender), &id, "прочитано");
                    }
                } else {
                    kept.push(b.clone());
                }
            }
            if !peek && shown > 0 {
                let _ = fs::write(&path, join_log(&header, &kept));
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
            let path = inbox_path(&safe_name(&agent));
            let mut seen: Vec<String> = Vec::new();
            loop {
                // heartbeat: «я на месте» — по нему соседи видят, кто онлайн
                touch_presence(&agent, "watch");
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
                    for b in fresh.iter() {
                        let id = block_field(b, "ID: ").unwrap_or_default();
                        if !id.is_empty() {
                            seen.push(id.clone());
                        }
                        println!("{b}{SEP}");
                        notify(&id, b);
                    }
                    if consume {
                        let keep: Vec<String> = blocks
                            .iter()
                            .filter(|b| {
                                let id = block_field(b, "ID: ").unwrap_or_default();
                                !seen.iter().any(|s| s == &id)
                            })
                            .cloned()
                            .collect();
                        let _ = fs::write(&path, join_log(&header, &keep));
                    }
                }
                if once {
                    break;
                }
                std::thread::sleep(Duration::from_secs(interval));
            }
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
            let sent_text = fs::read_to_string(outbox_path(&safe_name(&agent))).unwrap_or_default();
            let (_h, sent_blocks) = split_log(&sent_text);
            let acks_text = fs::read_to_string(acks_path(&safe_name(&agent))).unwrap_or_default();
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
                let state = if fresh && alive {
                    "ОНЛАЙН"
                } else {
                    "не в сети"
                };
                println!(
                    "{agent}	{state}	{}	{action}	pid={}",
                    ago(*ts),
                    pid.map(|p| p.to_string()).unwrap_or_else(|| "?".to_string())
                );
            }
            0
        }
        "list" => {
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
                return 2;
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
                return 2;
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
                "канал агентов: send | reply | read | watch | thread | threads | close | list | sent | find | acks | pending | presence\n\
                 письмо с одним или несколькими адресатами; прочитанное удаляется (consume-on-read);\n\
                 обсуждение (тред) сохраняется целиком, треды закрываются консенсусом;\n\
                 указание на код: --at путь[:строка] (регион с контекстом) или --diff файл (unified diff);\n\
                 привязка к коду — по содержимому (контексту), номер строки может сдвинуться"
            );
            2
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

    fn block_of(to: &str, subject: &str, tags: &[&str], body: &str) -> String {
        let mut b = format!("[10:00] ОТ: sender — КОМУ: {to}\nID: abcd1234\nТРЕД: 11112222\nТЕМА: {subject}\n");
        if !tags.is_empty() {
            b.push_str(&format!("ТЭГИ: {}\n", tags.join(", ")));
        }
        b.push_str(body);
        b
    }

    #[test]
    fn split_log_keeps_blocks_and_header() {
        let text = format!("# шапка\n{SEP}\n{}{SEP}\n", block_of("a", "тема", &[], "тело"));
        let (header, blocks) = split_log(&text);
        assert!(header.contains("шапка"), "шапка сохраняется: {header}");
        assert_eq!(blocks.len(), 1, "один блок: {blocks:?}");
        assert!(blocks[0].contains("ТЕМА: тема"));
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
        let blocks = vec![block_of("a", "t1", &["x"], "b1"), block_of("b", "t2", &[], "b2")];
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
        let path = dir.path().join("f.rs");
        let body = (1..=12)
            .map(|n| format!("line{n}"))
            .collect::<Vec<String>>()
            .join("\n");
        std::fs::write(&path, body).unwrap_or_default();
        let (p, plain, marked) = code_region(&path.to_string_lossy(), Some(6)).unwrap_or_default();
        assert!(p.ends_with("f.rs"), "путь: {p}");
        assert!(plain.contains("line5") && plain.contains("line6"), "контекст: {plain}");
        assert!(marked.contains('»'), "маркер указанной строки: {marked}");
        assert!(marked.contains("line5"), "контекст в вырезке: {marked}");
    }

    #[test]
    fn code_region_without_line_returns_first_lines() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
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
        assert!(msg.contains("МЕСТО: devharness/src/core/load.rs"), "место: {msg}");
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
    fn safe_name_strips_path_separators() {
        assert_eq!(safe_name("../evil"), ".._evil");
        assert_eq!(safe_name(""), "agent");
        assert_eq!(safe_name("ok-name_1"), "ok-name_1");
    }
}
