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

const SEP: &str = "================================================================================";
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
        .filter(|l| !l.starts_with('[') && !l.starts_with("ID:") && !l.starts_with("ТЕМА:") && !l.starts_with("ТЭГИ:"))
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
fn build_message(
    from: &str,
    to: &str,
    subject: &str,
    tags: &[String],
    body: &str,
) -> String {
    let stamp = now_hm();
    let id = short_id(from, subject, body, &stamp);
    let mut block = format!("[{stamp}] ОТ: {from} — КОМУ: {to}\nID: {id}\nТЕМА: {subject}");
    if !tags.is_empty() {
        block.push_str(&format!("\nТЭГИ: {}", tags.join(", ")));
    }
    block.push_str(&format!("\n{}\n", body.trim_end()));
    block
}

/// Раздать сообщение: адресату (или всем в ростере) + копия в мой исходящий.
fn deliver(from: &str, to: &str, block: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    if to == "all" {
        targets = roster();
        if targets.is_empty() {
            targets.push(from.to_string());
        }
    } else {
        targets.push(to.to_string());
    }
    let mut done: Vec<String> = Vec::new();
    for t in targets.iter() {
        let path = inbox_path(&safe_name(t));
        if append_message(&path, block) {
            done.push(t.clone());
        }
    }
    let _ = append_message(&outbox_path(&safe_name(from)), block);
    done
}

fn send(args: &[String]) -> i32 {
    let m = parse_args(args);
    match m.sub.as_str() {
        "send" => {
            let from = m.value("from").unwrap_or_else(|| me(m.value("agent").as_deref()));
            let to = m.value("to").unwrap_or_else(|| "all".to_string());
            let subject = m.value("subject").unwrap_or_default();
            let tags = m.values("tag");
            let body = match m.value("body-file") {
                Some(p) => fs::read_to_string(&p).unwrap_or_default(),
                None => m.value("body").unwrap_or_default(),
            };
            let block = build_message(&from, &to, &subject, &tags, &body);
            let id = block_field(&block, "ID: ").unwrap_or_default();
            let done = deliver(&from, &to, &block);
            if done.is_empty() {
                eprintln!("не доставлено никому");
                return 1;
            }
            println!("отправлено (ID {id}) -> {}", done.join(", "));
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
                } else {
                    kept.push(b.clone());
                }
            }
            if !peek && shown > 0 {
                let _ = fs::write(&path, join_log(&header, &kept));
            }
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
            for dir in [inbox_dir(), outbox_dir()] {
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
        "forward" => {
            let id = m.value("id").unwrap_or_default();
            let to = m.value("to").unwrap_or_else(|| "all".to_string());
            let from = m.value("from").unwrap_or_else(|| me(m.value("agent").as_deref()));
            if id.is_empty() {
                eprintln!("нужен --id <id>");
                return 2;
            }
            let mut source: Option<String> = None;
            let mut found: Option<String> = None;
            for dir in [inbox_dir(), outbox_dir()] {
                if let Ok(entries) = fs::read_dir(dir) {
                    for e in entries.flatten() {
                        let text = fs::read_to_string(e.path()).unwrap_or_default();
                        let (_h, blocks) = split_log(&text);
                        for b in blocks.iter() {
                            if block_field(b, "ID: ").as_deref() == Some(id.as_str()) {
                                source = e
                                    .path()
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().to_string());
                                found = Some(b.clone());
                            }
                        }
                    }
                }
            }
            let Some(block) = found else {
                eprintln!("сообщение {id} не найдено");
                return 1;
            };
            let mut tags = block_tags(&block);
            for t in m.values("tag") {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
            let subject = block_field(&block, "ТЕМА: ").unwrap_or_default();
            let body: String = block
                .lines()
                .skip_while(|l| {
                    l.starts_with('[')
                        || l.starts_with("ID:")
                        || l.starts_with("ТЕМА:")
                        || l.starts_with("ТЭГИ:")
                })
                .collect::<Vec<&str>>()
                .join("\n");
            let new_block = build_message(&from, &to, &subject, &tags, &body);
            let new_id = block_field(&new_block, "ID: ").unwrap_or_default();
            let done = deliver(&from, &to, &new_block);
            println!(
                "тема поднята: {new_id} -> {} (тэги: {}; источник: {} / {id})",
                done.join(", "),
                tags.join(", "),
                source.unwrap_or_default()
            );
            0
        }
        _ => {
            println!(
                "канал агентов: send | read | watch | list | sent | find | forward\n\
                 модель: сообщение адресовано агенту, прочитанное удаляется (consume-on-read)"
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
