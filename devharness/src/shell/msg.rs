//! Канал сообщений между агентами: `devh msg` (координация 2+ агентов).
//!
//! Хранилище (`$DEVH_MSG_HOME`, по умолчанию `/tmp/devh-msg`):
//!   `<канал>.log`               — журнал канала (append-only, блоки через разделитель)
//!   `<канал>.<сессия>.cursor`   — курсор сессии: сколько байт она прочитала
//!
//! Команды:
//!   devh msg send <канал> --from <агент> --subject <текст> [--to <кому>]
//!                          [--body <текст> | --body-file <файл>]
//!   devh msg read <канал> [--session <id>] [--all]
//!   devh msg watch <канал> [--session <id>] [--interval <сек>] [--once]
//!   devh msg list
//!   devh msg prune <канал> [--keep <N>]
//!
//! Оповещение при новом сообщении: `notify-send` БЕЗ urgency=critical (critical
//! демон держит до ручного закрытия — проверено 2026-10-22) + звук; дедуп по
//! хэшу, чтобы при нескольких смотрящих сессиях popup был один.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const SEP: &str = "================================================================================";
const DEFAULT_KEEP: usize = 10;
const NOTIFY_TIMEOUT_MS: u64 = 20_000;

fn home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

fn session_of(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("DEVH_SESSION").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "default".to_string())
}

fn channel_path(channel: &str) -> PathBuf {
    home().join(format!("{channel}.log"))
}

fn cursor_path(channel: &str, session: &str) -> PathBuf {
    home().join(format!("{channel}.{session}.cursor"))
}

fn notified_path(channel: &str) -> PathBuf {
    home().join(format!("{channel}.notified"))
}

/// Локальное время HH:MM (через date — без лишних зависимостей).
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

fn read_cursor(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn write_cursor(path: &Path, offset: u64) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::File::create(path) {
        let _ = writeln!(f, "{offset}");
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Блоки журнала: (start, end) в байтах. Блок = от разделителя до следующего.
fn blocks(data: &[u8]) -> Vec<(usize, usize)> {
    let sep = format!("{SEP}\n");
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut pos: usize = 0;
    while let Some(idx) = find(data, sep.as_bytes(), pos) {
        let end = idx.saturating_add(sep.len());
        out.push((pos, end));
        pos = end;
    }
    out
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    let last = haystack.len().saturating_sub(needle.len());
    let mut i = from;
    while i <= last {
        if &haystack[i..i.saturating_add(needle.len())] == needle {
            return Some(i);
        }
        i = i.saturating_add(1);
    }
    None
}

/// Уведомление: один popup на сообщение (дедуп по хэшу первых строк).
fn notify(channel: &str, text: &str) {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let key = format!("{:x}", hasher.finalize());
    let path = notified_path(channel);
    let mut entries: Vec<(String, u64)> = Vec::new();
    if let Ok(raw) = fs::read_to_string(&path) {
        for line in raw.lines() {
            let mut it = line.split_whitespace();
            if let (Some(k), Some(ts)) = (it.next(), it.next()) {
                if let Ok(t) = ts.parse::<u64>() {
                    entries.push((k.to_string(), t));
                }
            }
        }
    }
    let now = unix_now();
    if entries
        .iter()
        .any(|(k, ts)| k == &key && now.saturating_sub(*ts) < 90)
    {
        return; // дубль: уже показывали
    }
    entries.push((key, now));
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::File::create(&path) {
        for (k, ts) in entries.iter().rev().take(50).rev() {
            let _ = writeln!(f, "{k} {ts}");
        }
    }
    let subject = text
        .lines()
        .find(|l| l.starts_with("ТЕМА:"))
        .unwrap_or("новое сообщение");
    let sender = text.lines().next().unwrap_or("");
    let summary = format!("{sender} — {subject}");
    let _ = Command::new("notify-send")
        .arg("-t")
        .arg(NOTIFY_TIMEOUT_MS.to_string())
        .arg("Сессии: сообщение")
        .arg(summary)
        .output();
    let _ = Command::new("paplay")
        .arg("/usr/share/sounds/freedesktop/stereo/message.oga")
        .output();
}

/// Курсоры всех сессий канала (для prune: удаляем прочитанное ВСЕМИ).
fn cursors_of(channel: &str) -> BTreeMap<PathBuf, u64> {
    let mut out: BTreeMap<PathBuf, u64> = BTreeMap::new();
    let prefix = format!("{channel}.");
    if let Ok(dir) = fs::read_dir(home()) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".cursor") {
                let p = entry.path();
                let off = read_cursor(&p);
                out.insert(p, off);
            }
        }
    }
    out
}

/// Разбор аргументов вручную: подкоманда + позиционные + флаги.
/// KISS: clap здесь спорит с позиционным именем канала, а нам нужен
/// «проход как есть» (как у `devh mutate start`).
struct Args {
    sub: String,
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn flag(&self, name: &str) -> bool {
        self.flags
            .iter()
            .any(|(k, v)| k == name && v.is_none())
    }
    fn value(&self, name: &str) -> Option<String> {
        self.flags
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.clone())
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
    let rest: Vec<&String> = it.collect();
    let mut i = 0usize;
    while i < rest.len() {
        let a = rest[i].as_str();
        if let Some(name) = a.strip_prefix("--") {
            if let Some((k, v)) = name.split_once('=') {
                flags.push((k.to_string(), Some(v.to_string())));
            } else if rest
                .get(i.saturating_add(1))
                .map(|n| !n.starts_with("--"))
                .unwrap_or(false)
            {
                let v = rest[i.saturating_add(1)].clone();
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

fn send(args: &[String]) -> i32 {
    let m = parse_args(args);
    match m.sub.as_str() {
        "send" => {
            let channel = m.first();
            let from = m.value("from").unwrap_or_else(|| "агент".to_string());
            let to = m.value("to").unwrap_or_else(|| "все".to_string());
            let subject = m.value("subject").unwrap_or_default();
            let body = match m.value("body-file") {
                Some(p) => fs::read_to_string(&p).unwrap_or_default(),
                None => m.value("body").unwrap_or_default(),
            };
            let path = channel_path(&channel);
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let block = format!(
                "\n[{}] ОТ: {from} — КОМУ: {to}\nТЕМА: {subject}\n{}\n{SEP}\n",
                now_hm(),
                body.trim_end()
            );
            let mut ok = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .is_ok();
            if ok {
                if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&path) {
                    ok = writeln!(f, "{block}").is_ok();
                }
            }
            if ok {
                println!("отправлено в канал {channel}");
                0
            } else {
                eprintln!("не могу писать в {path:?}");
                1
            }
        }
        "read" => {
            let channel = m.first();
            let session = session_of(m.value("session").as_deref());
            let all = m.flag("all");
            let path = channel_path(&channel);
            let data = fs::read(&path).unwrap_or_default();
            let cur = cursor_path(&channel, &session);
            let from = if all { 0 } else { read_cursor(&cur) };
            let from = from.min(data.len() as u64) as usize;
            if from < data.len() {
                let text = String::from_utf8_lossy(&data[from..]).to_string();
                print!("{text}");
                if !all {
                    write_cursor(&cur, data.len() as u64);
                }
            }
            0
        }
        "watch" => {
            let channel = m.first();
            let session = session_of(m.value("session").as_deref());
            let interval: u64 = m
                .value("interval")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3);
            let once = m.flag("once");
            let path = channel_path(&channel);
            let cur = cursor_path(&channel, &session);
            loop {
                let data = fs::read(&path).unwrap_or_default();
                let from = read_cursor(&cur).min(data.len() as u64) as usize;
                if from < data.len() {
                    let text = String::from_utf8_lossy(&data[from..]).to_string();
                    write_cursor(&cur, data.len() as u64);
                    println!("{text}");
                    notify(&channel, &text);
                }
                if once {
                    break;
                }
                std::thread::sleep(Duration::from_secs(interval));
            }
            0
        }
        "list" => {
            let mut rows: Vec<(String, usize, usize)> = Vec::new();
            if let Ok(dir) = fs::read_dir(home()) {
                for entry in dir.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Some(chan) = name.strip_suffix(".log") {
                        let data = fs::read(entry.path()).unwrap_or_default();
                        rows.push((chan.to_string(), blocks(&data).len(), data.len()));
                    }
                }
            }
            rows.sort();
            if rows.is_empty() {
                println!("каналов нет ({})", home().display());
            }
            for (chan, n, size) in rows {
                println!("{chan}: сообщений={n} байт={size}");
            }
            0
        }
        "prune" => {
            let channel = m.first();
            let keep: usize = m
                .value("keep")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(DEFAULT_KEEP);
            let path = channel_path(&channel);
            let data = fs::read(&path).unwrap_or_default();
            let cursors = cursors_of(&channel);
            if cursors.is_empty() {
                println!("нет курсоров сессий — не удаляю");
                return 0;
            }
            let limit = cursors.values().copied().min().unwrap_or(0);
            let blks = blocks(&data);
            let read: Vec<(usize, usize)> = blks
                .iter()
                .copied()
                .filter(|(_, e)| (*e as u64) <= limit)
                .collect();
            let unread: Vec<(usize, usize)> = blks
                .iter()
                .copied()
                .filter(|(_, e)| (*e as u64) > limit)
                .collect();
            let keep_from = read.len().saturating_sub(keep);
            let removed: Vec<(usize, usize)> = read[..keep_from].to_vec();
            if removed.is_empty() {
                println!("удалять нечего (прочитанное в окне последних {keep} сообщений)");
                return 0;
            }
            let removed_bytes: usize = removed.iter().map(|(s, e)| e.saturating_sub(*s)).sum();
            let mut kept: Vec<u8> = Vec::new();
            for (s, e) in read[keep_from..].iter().chain(unread.iter()) {
                kept.extend_from_slice(&data[*s..*e]);
            }
            let _ = fs::write(&path, &kept);
            let first_removed = removed.first().map(|(s, _)| *s).unwrap_or(0);
            for (cpath, off) in cursors.iter() {
                let new_off = if *off <= first_removed as u64 {
                    *off
                } else {
                    off.saturating_sub(removed_bytes as u64)
                };
                write_cursor(cpath, new_off);
            }
            println!(
                "канал {channel}: удалено сообщений={} ({} байт), осталось={}",
                removed.len(),
                removed_bytes,
                read.len().saturating_sub(keep_from) + unread.len()
            );
            0
        }
        _ => {
            println!("каналы агентов: send | read | watch | list | prune");
            2
        }
    }
}

/// Точка входа: `devh msg ...` (args — всё после «msg»).
pub fn run(args: &[String]) -> i32 {
    let owned: Vec<String> = args.to_vec();
    send(&owned)
}
