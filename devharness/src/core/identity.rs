//! Автоидентификация агента в канале `devh msg`.
//!
//! Зачем: имя агента задавалось руками (`DEVH_AGENT=minimax devh msg …`).
//! Без него все агенты становились «default» — адресация и треды путались,
//! уведомления приходили не туда, а соседний агент не знал, кто «я».
//!
//! Теперь имя выводится из контекста СЕССИИ, который рантайм и так знает:
//!
//!   1. `--agent` (флаг)   — явный выбор на этот вызов;
//!   2. `DEVH_AGENT` (env) — обратная совместимость (перекрывает авто);
//!   3. `AGENT_SESSION_ID` — ключ сессии рантайма (goose/opencode/…);
//!   4. рантайм в предках   — процесс `goose acp`, `opencode acp`, …;
//!   5. TTY (+ cwd)         — терминал;
//!   6. cwd                 — последний fallback.
//!
//! Имя привязывается к ключу сессии в `$DEVH_MSG_HOME/agents/<хэш>.json`:
//! живёт между вызовами, переживает рестарт рантайма с тем же ключом и
//! НЕ требует ручной настройки. `devh msg claim <имя>` — закрепить своё
//! имя (после claim авто-имя больше не генерируется).
//!
//! Ключ сессии из п.3/4 уникален для КАЖДОГО агента: два соседа в одном
//! репозитории получают разные имена и разные входящие.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Рантаймы агентов: `comm` процесса, который запускает сессию.
/// Имя из этого списка становится префиксом авто-имени (`goose-1a2b`).
const KNOWN_RUNTIMES: [&str; 10] = [
    "goose",
    "opencode",
    "claude",
    "codex",
    "aider",
    "crush",
    "amp",
    "cline",
    "continue",
    "cursor-agent",
];

/// Где взялось имя — для отчёта `devh msg whoami`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Флаг `--agent`.
    Explicit,
    /// Переменная окружения `DEVH_AGENT`.
    Env,
    /// Ранее закреплённая привязка сессии.
    Stored,
    /// Ключ сессии рантайма (`AGENT_SESSION_ID`).
    Session,
    /// Процесс рантайма в предках.
    Runtime,
    /// Терминал.
    Tty,
    /// Рабочий каталог (fallback).
    Cwd,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Explicit => "флаг --agent",
            Source::Env => "DEVH_AGENT",
            Source::Stored => "привязка сессии",
            Source::Session => "AGENT_SESSION_ID",
            Source::Runtime => "рантайм-процесс",
            Source::Tty => "tty",
            Source::Cwd => "рабочий каталог",
        }
    }
}

/// Кто я в канале.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Имя агента (имя файла входящих/исходящих).
    pub name: String,
    /// Откуда взято имя.
    pub source: Source,
    /// Ключ сессии (к чему привязано имя в `$DEVH_MSG_HOME/agents`).
    pub key: String,
    /// Имя только что создано — показать подсказку «закрепить» один раз.
    pub fresh: bool,
}

/// Хранилище сообщений (общее для всех агентов машины).
fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Короткий хэш ключа сессии — имя файла привязки.
fn key_hash(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Имя файла привязки для ключа.
fn binding_path_in(home: &Path, key: &str) -> PathBuf {
    home.join("agents")
        .join(format!("{}.json", key_hash(key)))
}

/// Прочитать привязанное имя (None — привязки нет).
fn read_binding_in(home: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(binding_path_in(home, key)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Записать привязку (атомарно: tmp + rename).
fn write_binding_in(home: &Path, key: &str, name: &str, label: &str) -> io::Result<()> {
    let path = binding_path_in(home, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "key": key,
        "name": name,
        "label": label,
        "updated": unix_now(),
    });
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&body).unwrap_or_default())?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Имя из сессии: `session-<4hex>` / `goose-<4hex>` / `tty-<4hex>` / `cwd-<4hex>`.
fn auto_name(label: &str, key: &str) -> String {
    let short: String = key_hash(key).chars().take(4).collect();
    let prefix = match label {
        "session" => "ag",
        other => other,
    };
    let prefix: String = prefix
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let prefix = if prefix.is_empty() {
        "ag".to_string()
    } else {
        prefix
    };
    format!("{prefix}-{short}")
}

/// Проверить имя, которое агент хочет закрепить за собой.
pub fn sanitize_name(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("имя пустое".to_string());
    }
    if trimmed.chars().count() > 32 {
        return Err("имя длиннее 32 символов".to_string());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("имя: только буквы, цифры, '-', '_', '.'".to_string());
    }
    Ok(trimmed.to_string())
}

/// Ключ и метка контекста сессии.
fn context() -> (String, String) {
    for var in ["AGENT_SESSION_ID", "DEVH_SESSION"] {
        if let Ok(id) = std::env::var(var) {
            let id = id.trim().to_string();
            if !id.is_empty() {
                return (format!("session:{id}"), "session".to_string());
            }
        }
    }
    if let Some((pid, comm, start)) = runtime_ancestor() {
        return (format!("proc:{pid}:{start}"), comm);
    }
    if let Some(tty) = tty_name() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return (
            format!("tty:{tty}:{}", key_hash(&cwd)),
            "tty".to_string(),
        );
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (format!("cwd:{}", key_hash(&cwd)), "cwd".to_string())
}

/// Ближайший предок — процесс известного рантайма агента.
fn runtime_ancestor() -> Option<(u32, String, String)> {
    let mut pid = std::process::id();
    let mut hops: u32 = 0;
    while hops < 24 {
        let comm = read_comm(pid);
        if let Some(name) = &comm {
            if KNOWN_RUNTIMES.contains(&name.as_str()) {
                return Some((pid, name.clone(), read_starttime(pid)));
            }
        }
        let Some(ppid) = read_ppid(pid) else {
            break;
        };
        if ppid == 0 || ppid == pid {
            break;
        }
        pid = ppid;
        hops = hops.saturating_add(1);
    }
    None
}

/// `comm` процесса (имя без пути).
fn read_comm(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = text.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Родительский pid процесса (по /proc/<pid>/stat: поле после comm).
fn read_ppid(pid: u32) -> Option<u32> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = text.rfind(')')?;
    let rest = text.get(close.saturating_add(1)..)?.trim();
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// Время старта процесса (поле 22 /proc/<pid>/stat) — защита от переиспользования pid.
fn read_starttime(pid: u32) -> String {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return "0".to_string();
    };
    let Some(close) = text.rfind(')') else {
        return "0".to_string();
    };
    let Some(rest) = text.get(close.saturating_add(1)..) else {
        return "0".to_string();
    };
    rest.split_whitespace()
        .nth(19)
        .unwrap_or("0")
        .to_string()
}

/// Терминал процесса (`/dev/pts/N`), если есть.
fn tty_name() -> Option<String> {
    for fd in [0, 1, 2] {
        let target = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()?;
        let text = target.to_string_lossy().to_string();
        if text.starts_with("/dev/pts/") || text.starts_with("/dev/tty") {
            return Some(text);
        }
    }
    None
}

/// Имя агента по контексту (без записи привязки).
pub fn resolve(explicit: Option<&str>) -> Identity {
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Identity {
            name: name.to_string(),
            source: Source::Explicit,
            key: "explicit".to_string(),
            fresh: false,
        };
    }
    if let Ok(env) = std::env::var("DEVH_AGENT") {
        let env = env.trim().to_string();
        if !env.is_empty() {
            return Identity {
                name: env,
                source: Source::Env,
                key: "env".to_string(),
                fresh: false,
            };
        }
    }
    let (key, label) = context();
    let home = msg_home();
    if let Some(name) = read_binding_in(&home, &key) {
        return Identity {
            name,
            source: Source::Stored,
            key,
            fresh: false,
        };
    }
    let name = auto_name(&label, &key);
    let _ = write_binding_in(&home, &key, &name, &label);
    let source = match label.as_str() {
        "session" => Source::Session,
        "tty" => Source::Tty,
        "cwd" => Source::Cwd,
        _ => Source::Runtime,
    };
    Identity {
        name,
        source,
        key,
        fresh: true,
    }
}

/// Текущая идентичность (авто).
pub fn current() -> Identity {
    resolve(None)
}

/// Закрепить своё имя за текущей сессией.
pub fn claim(raw: &str) -> Result<Identity, String> {
    let name = sanitize_name(raw)?;
    let (key, label) = context();
    let home = msg_home();
    write_binding_in(&home, &key, &name, &label)
        .map_err(|e| format!("не записать привязку: {e}"))?;
    Ok(Identity {
        name,
        source: Source::Stored,
        key,
        fresh: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_plain_names() {
        assert_eq!(sanitize_name("minimax"), Ok("minimax".to_string()));
        assert_eq!(sanitize_name(" deep-1 "), Ok("deep-1".to_string()));
        assert_eq!(sanitize_name("a_b.c"), Ok("a_b.c".to_string()));
    }

    #[test]
    fn sanitize_rejects_empty_and_path_separators() {
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("   ").is_err());
        assert!(sanitize_name("../evil").is_err());
        assert!(sanitize_name("a/b").is_err());
        assert!(sanitize_name(&"x".repeat(33)).is_err());
    }

    #[test]
    fn auto_name_has_prefix_and_hash() {
        let n = auto_name("goose", "proc:123:456");
        assert!(n.starts_with("goose-"), "имя: {n}");
        assert_eq!(n.len(), "goose-".len().saturating_add(4));
        let s = auto_name("session", "session:abc");
        assert!(s.starts_with("ag-"), "имя: {s}");
    }

    #[test]
    fn key_hash_is_stable_and_short() {
        assert_eq!(key_hash("proc:1:2"), key_hash("proc:1:2"));
        assert_ne!(key_hash("proc:1:2"), key_hash("proc:1:3"));
        assert_eq!(key_hash("proc:1:2").len(), 8);
    }

    #[test]
    fn binding_roundtrip_in_temp_home() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        assert!(read_binding_in(home, "proc:1:2").is_none());
        write_binding_in(home, "proc:1:2", "deepseek", "goose").unwrap_or_default();
        assert_eq!(
            read_binding_in(home, "proc:1:2"),
            Some("deepseek".to_string())
        );
        // другой ключ — другое имя
        assert!(read_binding_in(home, "proc:9:9").is_none());
    }

    #[test]
    fn explicit_name_wins() {
        let id = resolve(Some("кто-то"));
        assert_eq!(id.name, "кто-то");
        assert_eq!(id.source, Source::Explicit);
        assert!(!id.fresh);
    }

    #[test]
    fn proc_helpers_read_current_process() {
        let pid = std::process::id();
        assert!(read_comm(pid).is_some());
        assert!(read_ppid(pid).is_some());
        assert!(!read_starttime(pid).is_empty());
    }
}
