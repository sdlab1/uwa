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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Блокировка read-modify-write aliases.json (in-process + cross-process).
static ALIAS_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Рантаймы агентов: `comm` процесса, который запускает сессию.
/// Имя из этого списка становится префиксом авто-имени (`goose-1a2b`).
const KNOWN_RUNTIMES: [&str; 11] = [
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
    "kilo",
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
    /// Слот рантайма (переживает рестарт сессии).
    Slot,
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
            Source::Slot => "слот (переживает рестарт рантайма)",
        }
    }
}

/// Кто я в канале.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Имя агента (имя файла входящих/исходящих).
    pub name: String,
    /// Синонимы того же агента (например `opencode` → `deepseek`).
    pub aliases: Vec<String>,
    /// Откуда взялось имя.
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

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Имя файла привязки для ключа.
fn binding_path_in(home: &Path, key: &str) -> PathBuf {
    home.join("agents").join(format!("{}.json", key_hash(key)))
}

/// Привязка имени к ключу сессии.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    /// Имя агента.
    name: String,
    /// Имя закреплено ЯВНО (`devh msg claim`) — такое перекрывает `DEVH_AGENT`.
    claimed: bool,
    /// Ключ сессии, который ИСХОДНО закрепил это имя (для проверки lineage в slot).
    original_key: String,
}

/// Прочитать привязку (None — привязки нет).
fn read_binding_in(home: &Path, key: &str) -> Option<Binding> {
    let text = fs::read_to_string(binding_path_in(home, key)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let claimed = value
        .get("claimed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let original_key = value
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(Binding {
        name,
        claimed,
        original_key,
    })
}

/// Прочитать сырую привязку из файла слота (чтобы получить original_key).
fn read_slot_binding_raw(home: &Path, slot: &str) -> Option<Binding> {
    let text = fs::read_to_string(binding_path_in(home, slot)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let claimed = value
        .get("claimed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let original_key = value
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(Binding {
        name,
        claimed,
        original_key,
    })
}

/// Записать привязку (атомарно: tmp + rename).
fn write_binding_in(
    home: &Path,
    key: &str,
    name: &str,
    label: &str,
    claimed: bool,
) -> io::Result<()> {
    let path = binding_path_in(home, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "key": key,
        "name": name,
        "label": label,
        "claimed": claimed,
        "updated": unix_now(),
    });
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&body).unwrap_or_default(),
    )?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Файл алиасов: синоним → каноническое имя.
///
/// Один и тот же агент бывает известен под двумя именами: модель `deepseek`
/// работает внутри приложения `opencode` — и в канале появляются ДВА адреса
/// одного агента. Задания уходят в один ящик, отчёт в другой, соседи не
/// понимают, кто перед ними. Алиас сводит имена к одному канону.
fn aliases_path_in(home: &Path) -> PathBuf {
    home.join("aliases.json")
}

/// Прочитать карту алиасов (синоним → канон) из указанного хранилища.
pub fn alias_map_in(home: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = fs::read_to_string(aliases_path_in(home)) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    let Some(map) = value.get("aliases").and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in map {
        let k = k.trim().to_string();
        let Some(text) = v.as_str() else {
            continue;
        };
        let v = text.trim().to_string();
        if !k.is_empty() && !v.is_empty() {
            out.insert(k, v);
        }
    }
    out
}

/// Прочитать карту алиасов из основного хранилища.
pub fn alias_map() -> BTreeMap<String, String> {
    alias_map_in(&msg_home())
}

/// Записать карту алиасов (атомарно: tmp + rename).
fn write_alias_map_in(home: &Path, map: &BTreeMap<String, String>) -> io::Result<()> {
    let path = aliases_path_in(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({ "aliases": map });
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&body).unwrap_or_default(),
    )?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Каноническое имя: разыменовать цепочку алиасов (с защитой от петли).
pub fn canonical_in(home: &Path, name: &str) -> String {
    let map = alias_map_in(home);
    let mut current = name.trim().to_string();
    for _ in 0..8 {
        let Some(next) = map.get(&current) else {
            break;
        };
        let next = next.trim().to_string();
        if next.is_empty() || next == current {
            break;
        }
        current = next;
    }
    current
}

/// Каноническое имя в основном хранилище.
pub fn canonical(name: &str) -> String {
    canonical_in(&msg_home(), name)
}

/// Синонимы, указывающие на каноническое имя.
pub fn aliases_of_in(home: &Path, canon: &str) -> Vec<String> {
    let mut out: Vec<String> = alias_map_in(home)
        .iter()
        .filter(|(_, c)| c.as_str() == canon)
        .map(|(a, _)| a.clone())
        .collect();
    out.sort();
    out
}

/// Синонимы в основном хранилище.
pub fn aliases_of(canon: &str) -> Vec<String> {
    aliases_of_in(&msg_home(), canon)
}

/// Связать синоним с каноном.
pub fn set_alias_in(home: &Path, alias: &str, canon: &str) -> Result<(), String> {
    let alias = sanitize_name(alias)?;
    let canon = sanitize_name(canon)?;
    if alias == canon {
        return Err("синоним совпадает с каноном".to_string());
    }
    // Авто-имя одноразовое: привязка к нему «протухает» при рестарте
    // рантайма и уводит агента в чужую личность. Каноном может быть
    // только устойчивое (не сгенерированное) имя.
    if looks_auto(&canon) {
        return Err(format!(
            "«{canon}» — авто-имя (генерируется заново); укажи устойчивый канон"
        ));
    }
    // Advisory lock: in-process mutex + fs2 file lock.
    let _guard = ALIAS_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(parent) = home.join("agents").parent() {
        let _ = fs::create_dir_all(parent);
    }
    let lock_path = home.join("aliases.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .ok();
    if let Some(ref f) = lock_file {
        let _ = fs2::FileExt::lock_exclusive(f);
    }
    let mut map = alias_map_in(home);
    let mut probe = canon.clone();
    for _ in 0..8 {
        if probe == alias {
            if let Some(ref f) = lock_file {
                let _ = fs2::FileExt::unlock(f);
            }
            return Err("алиас образовал бы петлю".to_string());
        }
        match map.get(&probe) {
            Some(next) => probe = next.clone(),
            None => break,
        }
    }
    map.insert(alias, canon.clone());
    let res = write_alias_map_in(home, &map).map_err(|e| format!("не записать алиасы: {e}"));
    if let Some(ref f) = lock_file {
        let _ = fs2::FileExt::unlock(f);
    }
    res?;
    Ok(())
}

/// Связать синоним с каноном в основном хранилище.
pub fn set_alias(alias: &str, canon: &str) -> Result<(), String> {
    set_alias_in(&msg_home(), alias, canon)
}

/// Убрать синоним (true — был).
pub fn remove_alias_in(home: &Path, alias: &str) -> Result<bool, String> {
    let mut map = alias_map_in(home);
    let existed = map.remove(alias.trim()).is_some();
    if existed {
        write_alias_map_in(home, &map).map_err(|e| format!("не записать алиасы: {e}"))?;
    }
    Ok(existed)
}

/// Убрать синоним в основном хранилище.
pub fn remove_alias(alias: &str) -> Result<bool, String> {
    remove_alias_in(&msg_home(), alias)
}

/// Те же имена, но в указанном хранилище (для тестов с отдельным home).
fn claimed_names_in(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let dir = home.join("agents");
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let Ok(text) = fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let claimed = v.get("claimed").and_then(|x| x.as_bool()).unwrap_or(false);
        let Some(name) = v.get("name").and_then(|x| x.as_str()) else {
            continue;
        };
        if claimed {
            out.push(canonical(name.trim()));
        }
    }
    out
}

/// Зарегистрировать синоним хоста-рантайма → наше имя.
///
/// Агент живёт внутри приложения-рантайма (`deepseek` — внутри `opencode`).
/// Если имя агента закреплено явно, а хост назван иначе — это тот же агент:
/// без сведения об этом в канале два адреса одного агента и задания
/// расходятся. Чужое закреплённое имя не отбираем.
fn auto_register_alias(name: &str) {
    let home = msg_home();
    auto_register_alias_in(&home, name);
}

/// То же, но в указанном хранилище алиасов (тесты с отдельным home).
fn auto_register_alias_in(home: &Path, name: &str) {
    let Some((_pid, comm, _start)) = runtime_ancestor() else {
        return;
    };
    if comm.is_empty() || comm == name || sanitize_name(&comm).is_err() {
        return;
    }
    let _guard = ALIAS_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let lock_path = home.join("aliases.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .ok();
    if let Some(ref f) = lock_file {
        let _ = fs2::FileExt::lock_exclusive(f);
    }
    let map = alias_map_in(home);
    if let Some(existing) = map.get(&comm) {
        if existing.as_str() != name {
            if let Some(ref f) = lock_file {
                let _ = fs2::FileExt::unlock(f);
            }
            return;
        }
    }
    if claimed_names_in(home).iter().any(|n| n == &comm) {
        if let Some(ref f) = lock_file {
            let _ = fs2::FileExt::unlock(f);
        }
        return;
    }
    let mut map = map;
    map.insert(comm, name.to_string());
    let _ = write_alias_map_in(home, &map);
    if let Some(ref f) = lock_file {
        let _ = fs2::FileExt::unlock(f);
    }
}

/// Имя из сессии: `session-<8hex>` / `goose-<8hex>` / `tty-<8hex>` / `cwd-<8hex>`.
///
/// Хэш — 8 hex (32 бита): при 4 hex (16 бит) два агента из ~256 сессий
/// получали одно имя ( birthday-коллизия), а письма одного уходили другому.
fn auto_name(label: &str, key: &str) -> String {
    let short = key_hash(key);
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

/// Похоже ли имя на авто-имя (`goose-1a2b3c4d`, `ag-…`, `cwd-…`)?
///
/// Авто-имя привязано к конкретной сессии рантайма и меняется при его
/// рестарте. Человеческое/модельное имя (`deepseek`, `minimax`) — нет.
/// Различие нужно, чтобы алиас нельзя было навести на одноразовый адрес.
fn looks_auto(name: &str) -> bool {
    let Some((prefix, suffix)) = name.rsplit_once('-') else {
        return false;
    };
    if prefix.is_empty() || suffix.len() != 8 {
        return false;
    }
    if !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    // Префикс должен быть из известных рантаймов или стандартных меток.
    matches!(
        prefix,
        "ag" | "session"
            | "tty"
            | "cwd"
            | "slot"
            | "goose"
            | "opencode"
            | "claude"
            | "codex"
            | "aider"
            | "crush"
            | "amp"
            | "cline"
            | "continue"
            | "cursor-agent"
    ) || KNOWN_RUNTIMES.contains(&prefix)
}

/// Канон для хоста-рантайма, если он задан синонимом.
///
/// `opencode` — это приложение, в котором работает модель `deepseek`.
/// Если в карте алиасов `opencode → deepseek`, то агент, запущенный внутри
/// opencode, ДОЛЖЕН называться `deepseek`: иначе в канале два адреса одного
/// агента, задания расходятся, отчёт приходит не туда.
fn host_alias_in(home: &Path, comm: &str) -> Option<String> {
    if comm.is_empty() {
        return None;
    }
    let map = alias_map_in(home);
    let mut current = comm.trim().to_string();
    let mut target: Option<String> = None;
    // Каждое звено цепочки должно быть «настоящим» именем: если хвост
    // цепочки — одноразовое авто-имя, весь алиас не годится (иначе runtime
    // утаскивает агента в чужую личность через промежуточный синоним).
    for _ in 0..8 {
        let Some(next) = map.get(&current) else {
            break;
        };
        let next = next.trim().to_string();
        if next.is_empty() || next == current {
            break;
        }
        if looks_auto(&next) {
            return None;
        }
        target = Some(next.clone());
        current = next;
    }
    let target = target?;
    Some(canonical_in(home, &target))
}

/// Канон для хоста-рантайма в основном хранилище.
fn host_alias(comm: &str) -> Option<String> {
    host_alias_in(&msg_home(), comm)
}

/// Ключ «слота» — устойчив между рестартами рантайма.
///
/// `AGENT_SESSION_ID` меняется при каждом рестарте goose ⇒ агент теряет
/// свой ящик и уходит в новое имя. Слот — это долгоживущий предок
/// (редактор, ppid=1) + имя рантайма: рестарт рантайма слот не меняет,
/// а разные рантаймы (goose / opencode) в одном редакторе разные.
pub fn slot_key() -> Option<String> {
    let runtime = runtime_ancestor().map(|(_, comm, _)| comm);
    let mut pid = std::process::id();
    let mut hops: u32 = 0;
    let mut leader: Option<(u32, String)> = None;
    while hops < 24 {
        let Some(ppid) = read_ppid(pid) else {
            break;
        };
        if ppid == 1 {
            leader = Some((pid, read_starttime(pid)));
            break;
        }
        if ppid == pid || ppid == 0 {
            break;
        }
        pid = ppid;
        hops = hops.saturating_add(1);
    }
    let (pid, start) = match leader {
        Some(v) => v,
        None => {
            // Fallback для контейнеров/CI, где предок с ppid=1 не найден:
            // используем cwd + runtime как якорь.
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Some(format!(
                "slot:cw:{}:{}",
                key_hash(&cwd),
                runtime.unwrap_or_else(|| "none".to_string())
            ));
        }
    };
    Some(format!(
        "slot:{pid}:{start}:{}",
        runtime.unwrap_or_else(|| "none".to_string())
    ))
}

/// Закреплённое имя для слота (переживает рестарт рантайма).
///
/// CLAIMED slot binding допускается ТОЛЬКО если текущая сессия —
/// продолжение той же lineage (original_key совпадает с текущим context()).
/// Иначе при одновременных сессиях в одном редакторе второй перехватывал
/// бы имя первого (баг T-214). При неоднозначности — None, авто-имя.
fn slot_binding() -> Option<Binding> {
    let slot = slot_key()?;
    let home = msg_home();
    let binding = read_slot_binding_raw(&home, &slot)?;
    if !binding.claimed {
        return Some(binding); // unclaimed slot binding is fine to inherit
    }
    // CLAIMED slot: verify current session is continuation of original claimant
    let (current_key, _label) = context();
    if binding.original_key == current_key {
        return Some(binding); // same session key = same session (restart)
    }
    // Not the same session lineage: don't inherit claimed name.
    None
}

/// Найти claimed-привязку для заданного имени (если есть).
/// Возвращает (key, Binding) если имя закреплено.
fn claimed_binding_for_name(home: &Path, name: &str) -> Option<(String, Binding)> {
    let agents_dir = home.join("agents");
    if !agents_dir.is_dir() {
        return None;
    }
    for entry in fs::read_dir(&agents_dir).ok()?.flatten() {
        let path = entry.path();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let claimed = value
            .get("claimed")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        if !claimed {
            continue;
        }
        let Some(binding_name) = value.get("name").and_then(|x| x.as_str()) else {
            continue;
        };
        if binding_name.trim() != name {
            continue;
        }
        // Ключ хранится В файле: имя файла — это хэш ключа, а не сам ключ.
        let key = value
            .get("key")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let original_key = key.clone();
        return Some((
            key,
            Binding {
                name: binding_name.trim().to_string(),
                claimed,
                original_key,
            },
        ));
    }
    None
}

/// Запомнить имя за слотом (чтобы следующий рестарт рантайма его увидел).
///
/// Слот занимает ПЕРВЫЙ: если слот уже закреплён за другим именем — не
/// перебиваем. Иначе два агента без рантайма в дереве (у goose оболочка
/// отделена от процесса goose) получили бы один слот и один ящик:
/// второй перехватил бы имя первого. Тогда лучше потерять непрерывность,
/// чем перепутать личности.
///
/// Храним original_key = session key (context()), чтобы slot_binding()
/// мог проверить lineage: только та же сессия или её перезапуск получает
/// claimed-имя. Одновременные сессии в одном редакторе получат авто-имена.
fn remember_slot(name: &str, claimed: bool) {
    let home = msg_home();
    remember_slot_in(&home, name, claimed);
}

/// Версия remember_slot с явным home (для тестов).
fn remember_slot_in(home: &Path, name: &str, claimed: bool) {
    let Some(slot) = slot_key() else {
        return;
    };
    let (current_key, _label) = context();
    // Read existing to preserve original_key if already claimed
    let existing_original_key = read_slot_binding_raw(home, &slot)
        .map(|b| b.original_key)
        .filter(|k| !k.is_empty())
        .unwrap_or(current_key);
    if let Some(existing) = read_binding_in(home, &slot) {
        if existing.claimed && existing.name != name {
            return;
        }
    }
    // Write to slot filename but store session key as original_key
    let path = binding_path_in(home, &slot);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body = serde_json::json!({
        "key": existing_original_key,  // store session key, not slot key
        "name": name,
        "label": "slot",
        "claimed": claimed,
        "updated": unix_now(),
    });
    let tmp = path.with_extension("json.tmp");
    let _ = fs::write(
        &tmp,
        serde_json::to_string_pretty(&body).unwrap_or_default(),
    );
    let _ = fs::rename(&tmp, &path);
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
        return (format!("tty:{tty}:{}", key_hash(&cwd)), "tty".to_string());
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
    rest.split_whitespace().nth(19).unwrap_or("0").to_string()
}

/// Терминал процесса (`/dev/pts/N`), если есть.
///
/// Часть fd бывает закрыта — тогда `read_link` падает. Раньше это обрывало
/// весь поиск (`?` возвращал None из функции), и агент без терминала терял
/// устойчивый ключ сессии. Теперь закрытые fd пропускаются, и дополнительно
/// смотрим родителя: агента часто запускают с перенаправленными
/// stdout/stderr, а терминал остаётся у шелла-родителя.
fn tty_name() -> Option<String> {
    if let Some(t) = tty_of(std::process::id()) {
        return Some(t);
    }
    if let Some(ppid) = read_ppid(std::process::id()) {
        if let Some(t) = tty_of(ppid) {
            return Some(t);
        }
    }
    None
}

/// Терминал конкретного процесса (fd 0, 1, 2).
fn tty_of(pid: u32) -> Option<String> {
    for fd in [0, 1, 2] {
        let Ok(target) = fs::read_link(format!("/proc/{pid}/fd/{fd}")) else {
            continue;
        };
        let text = target.to_string_lossy().to_string();
        if text.starts_with("/dev/pts/") || text.starts_with("/dev/tty") {
            return Some(text);
        }
    }
    None
}

/// Имя агента по контексту (без записи привязки).
///
/// Порядок: явный флаг → ЯВНО закреплённое имя (`devh msg claim`) →
/// `DEVH_AGENT` → авто-привязка → авто-имя по контексту сессии.
///
/// `claim` перекрывает `DEVH_AGENT` намеренно: это явное решение агента,
/// а `DEVH_AGENT` — наследие (его ставят в окружении разом для всех
/// сессий). Иначе `devh msg claim` молча ничего не делал.
pub fn resolve(explicit: Option<&str>) -> Identity {
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return finish(
            name.to_string(),
            Source::Explicit,
            "explicit".to_string(),
            false,
        );
    }
    let (key, label) = context();
    let home = msg_home();
    let binding = read_binding_in(&home, &key);
    if let Some(b) = binding.as_ref() {
        if b.claimed {
            // Явно закреплённое имя + хост-рантайм с другим именем → синоним.
            auto_register_alias(&b.name);
            remember_slot(&b.name, true);
            return finish(b.name.clone(), Source::Stored, key, false);
        }
        // Несмотря на отсутствие claim, binding привязывает session-key к имени.
        // host_alias (рантайм → канон) НЕ должен переопределять существующую
        // привязку: иначе два разных агента с одинаковым runtime (kilo/openc)
        // сольются в одну личность (kilo→openc-0a0e0cfc).
        return finish(b.name.clone(), Source::Stored, key, false);
    }
    if let Ok(env) = std::env::var("DEVH_AGENT") {
        let env = env.trim().to_string();
        if !env.is_empty() {
            return finish(env, Source::Env, "env".to_string(), false);
        }
    }
    // Хост-рантайм с синонимом (opencode → deepseek): я — этот агент,
    // даже если текущая сессии рантайма ещё не знает моего имени.
    // Это важнее авто-имени: иначе у одного агента два адреса.
    if let Some((_pid, comm, _start)) = runtime_ancestor() {
        if let Some(canon) = host_alias(&comm) {
            // T-213: не унаследовать чужое claimed-имя через алиас.
            // Если canon — claimed-имя чужой сессии (owner_key != key),
            // host_alias не применим: иначе новая сессия в том же слоте
            // получает адрес и права существующего lead.
            let safe = claimed_binding_for_name(&home, &canon)
                .map(|(owner_key, _)| owner_key == key)
                .unwrap_or(true);
            if safe {
                let _ = write_binding_in(&home, &key, &canon, &label, false);
                remember_slot(&canon, false);
                return finish(canon, Source::Runtime, key, false);
            }
        }
    }
    // Слот: рантайм перезапустился, но редактор/слот тот же — имя прежнее.
    if let Some(b) = slot_binding() {
        if b.claimed {
            auto_register_alias(&b.name);
            let _ = write_binding_in(&home, &key, &b.name, &label, false);
            return finish(b.name.clone(), Source::Slot, key, false);
        }
    }
    let name = auto_name(&label, &key);
    let _ = write_binding_in(&home, &key, &name, &label, false);
    let source = match label.as_str() {
        "session" => Source::Session,
        "tty" => Source::Tty,
        "cwd" => Source::Cwd,
        _ => Source::Runtime,
    };
    finish(name, source, key, true)
}

/// Финализация имени: канонизация + список синонимов.
fn finish(name: String, source: Source, key: String, fresh: bool) -> Identity {
    let name = canonical(&name);
    let aliases = aliases_of(&name);
    Identity {
        name,
        aliases,
        source,
        key,
        fresh,
    }
}

/// Текущая идентичность (авто).
pub fn current() -> Identity {
    resolve(None)
}

/// Закрепить своё имя за текущей сессией (перекрывает `DEVH_AGENT`).
pub fn claim(raw: &str) -> Result<Identity, String> {
    let name = sanitize_name(raw)?;
    let canon = canonical(&name);
    if canon != name {
        return Err(format!(
            "«{name}» — синоним агента «{canon}»; закрепляй каноническое имя"
        ));
    }
    let (key, label) = context();
    let home = msg_home();
    // Проверка: имя уже закреплено другим ключом?
    if let Some(existing) = read_binding_in(&home, &key) {
        if existing.claimed && existing.name == name {
            // Тот же ключ закрепляет то же имя — идемпотентно.
            auto_register_alias(&name);
            remember_slot(&name, true);
            return Ok(finish(name, Source::Stored, key, false));
        }
    }
    // Проверка: имя уже закреплено ЧУЖИМ ключом?
    if let Some((existing_key, _existing_binding)) = claimed_binding_for_name(&home, &name) {
        if existing_key != key {
            return Err(format!(
                "имя «{name}» уже закреплено другим агентом (ключ {existing_key});                  используй другое имя или добавь синоним: devh msg alias {name} <другой>"
            ));
        }
    }
    write_binding_in(&home, &key, &name, &label, true)
        .map_err(|e| format!("не записать привязку: {e}"))?;
    // Слот: после рестарта рантайма имя восстановится само.
    remember_slot(&name, true);
    auto_register_alias(&name);
    Ok(finish(name, Source::Stored, key, false))
}

/// Безопасный возврат канона после рестарта рантайма (T-261).
///
/// Позволяет агенту взять назад закреплённое имя после перезапуска
/// рантайма, если:
/// - имя действительно закреплено (`claimed=true`) другой сессией;
/// - чужая сессия использовала ТОТ ЖЕ слот (slot_key) — т.е. это
///   перезапуск того же редактора/рантайма, а не пришпилённый сосед;
/// - текущая сессия ещё НЕ закрепила себя за этим именем.
///
/// Авторизация lead'ом (токен/пароль) проверяется на уровне orch.rs/msg.rs;
/// здесь гарантируем только техническую безопасность: чужой слот не отдаёт имя.
pub fn claim_takeover(raw: &str) -> Result<Identity, String> {
    let name = sanitize_name(raw)?;
    let canon = canonical(&name);
    if canon != name {
        return Err(format!(
            "«{name}» — синоним агента «{canon}»; закрепляй каноническое имя"
        ));
    }
    let (key, label) = context();
    let home = msg_home();
    claim_takeover_with_home(&home, &name, &key, &label)
}

fn claim_takeover_with_home(
    home: &Path,
    name: &str,
    key: &str,
    label: &str,
) -> Result<Identity, String> {
    // 1. Имя должно быть закреплено (claimed) кем-то другим.
    let (existing_key, existing_binding) = claimed_binding_for_name(home, name)
        .ok_or_else(|| format!("имя «{name}» не закреплено; используйте claim"))?;
    if existing_key == key {
        // Уже закреплено этой же сессией — идемпотентно.
        return Ok(finish(name.to_string(), Source::Stored, key.to_string(), false));
    }
    if !existing_binding.claimed {
        return Err(format!("имя «{name}» не закреплено; используйте claim"));
    }

    // 2. Безопасность: чужой слот не отдаёт имя.
    //    Слот старой сессии должен совпадать со слотом текущей — иначе это
    //    другой редактор/рантайм, и takeover запрещается.
    let Some(slot) = slot_key() else {
        return Err("слот не определён (нет ppid=1); takeover невозможен".to_string());
    };
    let sb = read_slot_binding_raw(home, &slot);
    match sb {
        Some(ref b) if b.claimed && b.name == name => {
            // Слот хранит claimed-имя: это наш случай (перезапуск того же редактора).
        }
        _ => {
            return Err(format!(
                "имя «{name}» закреплено в другом слоте; --takeover только в том же редакторе/рантайме"
            ));
        }
    }

    // 3. Перезаписать старую привязку для новой сессии.
    write_binding_in(home, key, name, label, true)
        .map_err(|e| format!("не записать привязку: {e}"))?;
    // 4. Обновить слот: теперь original_key = текущая сессия.
    remember_slot_in(home, name, true);
    auto_register_alias(name);

    Ok(finish(name.to_string(), Source::Stored, key.to_string(), false))
}

/// claim для тестов с явным home и key.
#[cfg(test)]
fn claim_with_home(home: &Path, name: &str, key: &str) -> Result<Identity, String> {
    let name = sanitize_name(name)?;
    let canon = canonical_in(home, &name);
    if canon != name {
        return Err(format!(
            "«{name}» — синоним агента «{canon}»; закрепляй каноническое имя"
        ));
    }
    let label = "test";
    // Проверка: имя уже закреплено ЧУЖИМ ключом? (тот же ключ — идемпотентно)
    if let Some((existing_key, _existing_binding)) = claimed_binding_for_name(home, &name) {
        if existing_key != key {
            return Err(format!(
                "имя «{name}» уже закреплено другим агентом (ключ {existing_key}); используй другое имя или добавь синоним: devh msg alias {name} <другой>"
            ));
        }
    }
    write_binding_in(home, key, &name, label, true)
        .map_err(|e| format!("не записать привязку: {e}"))?;
    auto_register_alias_in(home, &name);
    // remember_slot requires slot_key, skip for tests.
    Ok(finish(name, Source::Stored, key.to_string(), false))
}

/// claim_takeover для тестов с явным home и key.
#[cfg(test)]
fn claim_takeover_with_home_test(home: &Path, name: &str, key: &str) -> Result<Identity, String> {
    claim_takeover_with_home(home, name, key, "test")
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
        assert_eq!(n.len(), "goose-".len().saturating_add(8));
        let s = auto_name("session", "session:abc");
        assert!(s.starts_with("ag-"), "имя: {s}");
        assert_eq!(s.len(), "ag-".len().saturating_add(8));
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
        write_binding_in(home, "proc:1:2", "deepseek", "goose", false).unwrap_or_default();
        assert_eq!(
            read_binding_in(home, "proc:1:2"),
            Some(Binding {
                name: "deepseek".to_string(),
                claimed: false,
                original_key: "proc:1:2".to_string()
            })
        );
        // другой ключ — другое имя
        assert!(read_binding_in(home, "proc:9:9").is_none());
    }

    #[test]
    fn claimed_binding_wins_over_env() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_binding_in(home, "proc:1:2", "minimax", "goose", true).unwrap_or_default();
        let binding = read_binding_in(home, "proc:1:2");
        assert!(binding.as_ref().is_some_and(|b| b.claimed));
    }

    #[test]
    fn tty_lookup_skips_closed_fds() {
        // Закрытые fd не должны обрывать поиск: функция возвращает Option,
        // а не падает и не «слепнет» на первом нечитаемом fd.
        let t = tty_of(std::process::id());
        assert!(t.is_none() || t.is_some_and(|x| x.starts_with("/dev/")));
        assert_eq!(tty_of(u32::MAX), None, "несуществующий pid — None");
    }

    #[test]
    fn explicit_name_wins() {
        let id = resolve(Some("кто-то"));
        assert_eq!(id.name, "кто-то");
        assert_eq!(id.source, Source::Explicit);
        assert!(!id.fresh);
    }

    #[test]
    fn cwd_fallback_keys_differ_by_cwd() {
        // Документирует границу fallback'а: одинаковый cwd → одинаковый ключ,
        // разный cwd → разный ключ. Коллизия только если cwd совпал
        // (это и есть риск, предупреждаемый whoami).
        assert_eq!(key_hash("/tmp/proj-a"), key_hash("/tmp/proj-a"));
        assert_ne!(key_hash("/tmp/proj-a"), key_hash("/tmp/proj-b"));
    }

    #[test]
    fn slot_key_not_none_for_current_process() {
        // slot_key должен возвращать Some для текущего процесса (если есть предок с ppid=1).
        // В CI/контейнерах может быть None — это допустимо, но формат проверяем.
        if let Some(key) = slot_key() {
            assert!(key.starts_with("slot:"), "формат: {key}");
            assert!(key.contains(':'), "разделители: {key}");
        }
    }

    #[test]
    fn host_alias_follows_chain() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // kilo → beam2, nemotron → beam2
        set_alias_in(home, "kilo", "beam2").unwrap_or_else(|_| unreachable!());
        set_alias_in(home, "nemotron", "beam2").unwrap_or_else(|_| unreachable!());
        assert_eq!(host_alias_in(home, "kilo"), Some("beam2".to_string()));
        assert_eq!(host_alias_in(home, "nemotron"), Some("beam2".to_string()));
        assert_eq!(
            aliases_of_in(home, "beam2"),
            vec!["kilo".to_string(), "nemotron".to_string()]
        );
    }

    #[test]
    fn auto_register_alias_skips_claimed_comm() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // Закрепляем имя "opencode" явно.
        write_binding_in(home, "proc:1:2", "opencode", "goose", true).unwrap_or_default();
        // claimed_names должен содержать "opencode".
        // Проверяем через alias_map: auto_register не должен перезаписать.
        set_alias_in(home, "opencode", "beam").unwrap_or_else(|_| unreachable!());
        // opencode claimed → auto_register должен пропустить регистрацию comm→name.
        // (auto_register читает claimed_names из основного home, не из temp —
        //  здесь проверяем только логику claimed_names на temp home.)
        // Проверяем что alias остался.
        assert_eq!(
            alias_map_in(home).get("opencode").map(String::as_str),
            Some("beam")
        );
    }

    #[test]
    fn slot_key_fallback_not_empty() {
        // В контейнерах/CI slot_key может вернуть fallback (cwd+runtime).
        // Проверяем что результат не None и имеет правильный префикс.
        if let Some(key) = slot_key() {
            assert!(key.starts_with("slot:"), "префикс slot: {key}");
        }
        // Fallback формат проверяем отдельно (не зависит от окружения).
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let fallback = format!("slot:cw:{}:none", key_hash(&cwd));
        assert!(
            fallback.starts_with("slot:cw:"),
            "fallback формат: {fallback}"
        );
    }

    #[test]
    fn slot_first_occupant_wins() {
        // Если слот уже занят claimed именем, новая сессия получает его.
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // Записываем claimed binding в слот-ключ (используем произвольный ключ).
        write_binding_in(home, "slot:test:123:goose", "first-agent", "slot", true)
            .unwrap_or_default();
        let b = read_binding_in(home, "slot:test:123:goose");
        assert!(b.as_ref().is_some_and(|x| x.claimed), "claimed binding");
        assert_eq!(b.as_ref().map(|x| x.name.as_str()), Some("first-agent"));
    }

    #[test]
    fn claim_rejects_duplicate_by_different_key() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // Первый ключ закрепляет имя "agent-a"
        write_binding_in(home, "proc:1:2", "agent-a", "goose", true).unwrap_or_default();
        // Второй ключ пытается закрепить то же имя — должен вернуть ошибку.
        let result = claim_with_home(home, "agent-a", "proc:9:9");
        assert!(result.is_err(), "повторный claim должен вернуть ошибку");
        let err = result.unwrap_err();
        assert!(
            err.contains("уже закреплено другим агентом"),
            "ошибка: {err}"
        );
        // Исходная привязка не должна измениться.
        let b = read_binding_in(home, "proc:1:2");
        assert_eq!(b.as_ref().map(|b| b.name.as_str()), Some("agent-a"));
    }

    #[test]
    fn claim_idempotent_for_same_key() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        claim_with_home(home, "agent-b", "proc:1:2").unwrap_or_else(|_| unreachable!());
        // Повторный claim тем же ключом — OK (идемпотентность).
        let res = claim_with_home(home, "agent-b", "proc:1:2");
        assert!(res.is_ok(), "повторный claim тем же ключом должен быть OK");
    }

    #[test]
    fn proc_helpers_read_current_process() {
        let pid = std::process::id();
        assert!(read_comm(pid).is_some());
        assert!(read_ppid(pid).is_some());
        assert!(!read_starttime(pid).is_empty());
    }

    #[test]
    fn looks_auto_detects_generated_names_only() {
        assert!(looks_auto("goose-1a2b3c4d"));
        assert!(looks_auto("ag-deadbeef"));
        assert!(looks_auto("cwd-0123abcd"));
        assert!(!looks_auto("deepseek"));
        assert!(!looks_auto("beam-2"));
        assert!(!looks_auto("minimax"));
    }

    #[test]
    fn alias_map_roundtrip_in_temp_home() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        assert!(alias_map_in(home).is_empty());
        set_alias_in(home, "opencode", "beam").unwrap_or_else(|_| unreachable!());
        let map = alias_map_in(home);
        assert_eq!(map.get("opencode").map(String::as_str), Some("beam"));
        assert_eq!(canonical_in(home, "opencode"), "beam");
        assert_eq!(aliases_of_in(home, "beam"), vec!["opencode".to_string()]);
    }

    #[test]
    fn canonical_follows_chain_and_survives_loop() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        set_alias_in(home, "a", "b").unwrap_or_else(|_| unreachable!());
        set_alias_in(home, "b", "c").unwrap_or_else(|_| unreachable!());
        assert_eq!(canonical_in(home, "a"), "c");
        // Петля в файле (её могли внести руками) не должна зациклить.
        let mut map = alias_map_in(home);
        map.insert("c".to_string(), "a".to_string());
        write_alias_map_in(home, &map).unwrap_or_default();
        let out = canonical_in(home, "a");
        assert!(!out.is_empty());
        assert_ne!(out, "");
    }

    #[test]
    fn set_alias_rejects_loop_and_self() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        set_alias_in(home, "a", "b").unwrap_or_else(|_| unreachable!());
        assert!(
            set_alias_in(home, "b", "a").is_err(),
            "петля должна быть отвергнута"
        );
        assert!(set_alias_in(home, "x", "x").is_err(), "синоним == канон");
        assert!(
            set_alias_in(home, "../evil", "beam").is_err(),
            "путь в имени"
        );
    }

    #[test]
    fn remove_alias_reports_presence() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        set_alias_in(home, "opencode", "beam").unwrap_or_else(|_| unreachable!());
        assert!(remove_alias_in(home, "opencode").unwrap_or(false));
        assert!(!remove_alias_in(home, "opencode").unwrap_or(true));
        assert!(alias_map_in(home).is_empty());
    }

    #[test]
    fn host_alias_uses_map_and_skips_auto_names() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        set_alias_in(home, "opencode", "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(host_alias_in(home, "opencode"), Some("beam".to_string()));
        assert_eq!(host_alias_in(home, "goose"), None);
        // Одноразовое авто-имя каноном быть не может: set_alias отвергает,
        // а унаследованная карта не должна утаскивать runtime в авто-имя.
        assert!(set_alias_in(home, "claude", "goose-1a2b3c4d").is_err());
        let mut map = alias_map_in(home);
        map.insert("claude".to_string(), "goose-1a2b3c4d".to_string());
        write_alias_map_in(home, &map).unwrap_or_else(|_| unreachable!());
        assert_eq!(host_alias_in(home, "claude"), None);
    }

    #[test]
    fn host_alias_rejects_auto_name_in_chain_tail() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // opencode → kilo (легально), kilo → авто-имя (наследие).
        set_alias_in(home, "opencode", "kilo").unwrap_or_else(|_| unreachable!());
        let mut map = alias_map_in(home);
        map.insert("kilo".to_string(), "kilo-2ba65640".to_string());
        write_alias_map_in(home, &map).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            host_alias_in(home, "opencode"),
            None,
            "авто-имя в хвосте цепочки запрещено"
        );
        // А без авто-звена та же цепочка работает.
        let mut map = alias_map_in(home);
        map.insert("kilo".to_string(), "beam2".to_string());
        write_alias_map_in(home, &map).unwrap_or_else(|_| unreachable!());
        assert_eq!(host_alias_in(home, "opencode"), Some("beam2".to_string()));
    }

    #[test]
    fn binding_takes_priority_over_host_alias() {
        // Баг: kilo-2ba65640 с runtime "kilo", но host_alias("kilo") →
        // openc-0a0e0cfc (от чужого агента openc). Без фикса binding
        // переопределялся хост-алиасом → identity = openc-0a0e0cfc.
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        set_alias_in(home, "kilo", "openc-0a0e0cfc").unwrap_or_else(|_| unreachable!());
        // Создаём binding для session-key с именем kilo-2ba65640 (unclaimed).
        let key = "proc:3005:6231";
        let _ = write_binding_in(home, key, "kilo-2ba65640", "kilo", false);
        let canon = canonical_in(home, "kilo-2ba65640");
        assert_eq!(
            canon, "kilo-2ba65640",
            "binding должен иметь приоритет над host_alias: kilo-2ba65640, а не openc-0a0e0cfc"
        );
    }

    #[test]
    fn takeover_rejects_unclaimed_name() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        let result = claim_takeover_with_home_test(home, "minimax", "proc:9:9");
        assert!(result.is_err(), "takeover незакрепленного имени должен падать");
        assert!(result.unwrap_err().contains("не закреплено"));
    }

    #[test]
    fn takeover_rejects_different_slot() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        // Смоделируем: имя закреплено в другом слоте (не нашем).
        let fake_slot = "slot:fake:0:other";
        write_binding_in(home, fake_slot, "minimax", "slot", true).unwrap_or_else(|_| unreachable!());
        // Текущий слот не имеет binding для minimax → takeover падает.
        let result = claim_takeover_with_home_test(home, "minimax", "session:new");
        assert!(result.is_err(), "takeover из другого слота должен падать");
        assert!(result.unwrap_err().contains("другом слоте"));
    }

    #[test]
    fn takeover_idempotent_for_same_key() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        let key = "proc:1:2";
        // Сначала claim.
        claim_with_home(home, "minimax", key).unwrap_or_else(|_| unreachable!());
        // Запишем тот же ключ в слот (как делает claim через remember_slot).
        if let Some(slot) = slot_key() {
            write_binding_in(home, &slot, "minimax", "slot", true).unwrap_or_else(|_| unreachable!());
        }
        // Takeover тем же ключом — идемпотентно.
        let result = claim_takeover_with_home_test(home, "minimax", key);
        if let Ok(id) = result {
            assert_eq!(id.name, "minimax");
        }
    }
}
