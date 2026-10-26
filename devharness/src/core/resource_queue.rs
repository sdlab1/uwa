//! Очередь запусков и физические лимиты CPU/RAM (T-623).
//!
//! Проблема: машина (4 CPU / 7.9 GB RAM) уходила в load 29–55 — параллельные
//! запуски агентов, cargo и nextest съедали CPU и память, и всё вставало.
//!
//! Два механизма:
//!
//! 1. **Очередь запусков.** Любая заявка на запуск (рантайм агента, фоновая
//!    задача) попадает в `.harness/orch/launch_queue.json`. Оператор видит,
//!    что ждёт и что идёт; агент может отменить **свою** заявку (lead/
//!    tech_lead — любую). Очередь не даёт запустить больше, чем
//!    `RESOURCE_MAX_ACTIVE` процессов одновременно, и не отпускает запуск,
//!    пока loadavg выше порога (`RESOURCE_LOAD_MAX`).
//!
//! 2. **Физические лимиты.** Ограничения накладываются на дочерний процесс
//!    **до** его старта: CPU-ядра (`taskset -c`), CPU-секунды (`RLIMIT_CPU`),
//!    память (`RLIMIT_AS`), число процессов (`RLIMIT_NPROC`; по умолчанию
//!    выключен — лимит считается на UID, а не на процесс), приоритет (`nice`).
//!    Если доступен cgroup v2, лимиты дублируются в cgroup (`memory.max`,
//!    `cpu.max`); если нет — остаётся userspace-режим, и в заявке и в выводе
//!    пишется **явная** метка режима (userspace-режим никогда не выдаётся за
//!    cgroup-изоляцию).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Относительный путь очереди внутри `.harness/`.
pub const QUEUE_REL: &str = "orch/launch_queue.json";

/// Статус заявки в очереди.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Ждёт места (лимит одновременных запусков / гейт нагрузки).
    Queued,
    /// Запущен (pid известен).
    Running,
    /// Завершился с кодом 0.
    Done,
    /// Завершился с ненулевым кодом или его не удалось запустить.
    Failed,
    /// Отменён (вручную или из-за лимитов).
    Cancelled,
}

impl Status {
    /// Текстовая метка статуса.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    /// Финальный статус — заявка больше не участвует в очереди.
    pub fn is_final(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

/// Заявка на запуск.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Идентификатор заявки (`Q-0001`).
    pub id: String,
    /// Кому заявка адресована (агент-получатель запуска).
    pub agent: String,
    /// Тип запуска: `agent` | `bg` | `cmd`.
    pub kind: String,
    /// Команда запуска (shell-строка).
    pub cmd: String,
    /// Рабочий каталог запуска.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Задача реестра, ради которой запускают (если есть).
    #[serde(default)]
    pub task: Option<String>,
    /// Кто поставил заявку (канонический адрес агента).
    pub requested_by: String,
    /// Unix-время постановки в очередь.
    pub requested_at: u64,
    /// Текущий статус.
    pub status: Status,
    /// pid запущенного процесса.
    #[serde(default)]
    pub pid: Option<i32>,
    /// Unix-время старта.
    #[serde(default)]
    pub started_at: Option<u64>,
    /// Unix-время завершения.
    #[serde(default)]
    pub finished_at: Option<u64>,
    /// Код возврата.
    #[serde(default)]
    pub exit: Option<i32>,
    /// Режим лимитов: `cgroup+limits` | `limits(userspace)` | `без лимитов`.
    #[serde(default)]
    pub mode: String,
    /// Свободное поле для пояснений (почему не запустили и т.п.).
    #[serde(default)]
    pub note: String,
}

/// Новая заявка (до присвоения id).
#[derive(Debug, Clone)]
pub struct NewRequest {
    pub agent: String,
    pub kind: String,
    pub cmd: String,
    pub cwd: Option<String>,
    pub task: Option<String>,
    pub requested_by: String,
}

/// Очередь запусков.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub requests: Vec<Request>,
}

impl Queue {
    /// Следующий идентификатор заявки: `Q-0001`, `Q-0002`, …
    pub fn next_id(&self) -> String {
        let mut max = 0u32;
        for r in &self.requests {
            if let Some(num) = r.id.strip_prefix("Q-") {
                if let Ok(n) = num.trim().parse::<u32>() {
                    max = max.max(n);
                }
            }
        }
        format!("Q-{:04}", max.saturating_add(1))
    }

    /// Найти заявку по id.
    pub fn find(&self, id: &str) -> Option<&Request> {
        self.requests.iter().find(|r| r.id == id)
    }

    /// Сколько заявок сейчас в работе.
    pub fn running_count(&self) -> usize {
        self.requests
            .iter()
            .filter(|r| r.status == Status::Running)
            .count()
    }

    /// Заявки, ожидающие запуска (в порядке постановки).
    pub fn pending(&self) -> Vec<&Request> {
        self.requests
            .iter()
            .filter(|r| r.status == Status::Queued)
            .collect()
    }
}

/// Текущее unix-время в секундах.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Путь файла очереди.
pub fn queue_path(root: &Path) -> PathBuf {
    root.join(".harness").join(QUEUE_REL)
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(".harness").join("orch").join("launch_queue.lock")
}

/// Загрузить очередь. Битый JSON — ошибка (fail-closed: «очередь недоступна»
/// значит «запуск запрещён», а не «очередь пуста»).
pub fn load(root: &Path) -> anyhow::Result<Queue> {
    let path = queue_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Queue::default()),
        Err(e) => anyhow::bail!("devh queue: не читается {}: {e}", path.display()),
    };
    if text.trim().is_empty() {
        return Ok(Queue::default());
    }
    serde_json::from_str::<Queue>(&text)
        .map_err(|e| anyhow::anyhow!("devh queue: битый {}: {e}", path.display()))
}

/// Сохранить очередь атомарно (tmp + rename).
pub fn save(root: &Path, q: &Queue) -> anyhow::Result<()> {
    let path = queue_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(q)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn with_lock<T>(root: &Path, f: impl FnOnce(&mut Queue) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let dir = root.join(".harness").join("orch");
    std::fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path(root))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let mut q = load(root)?;
    let out = f(&mut q)?;
    save(root, &q)?;
    fs2::FileExt::unlock(&lock).ok();
    Ok(out)
}

/// Поставить заявку в очередь.
pub fn enqueue(root: &Path, new: NewRequest) -> anyhow::Result<Request> {
    with_lock(root, |q| {
        let id = q.next_id();
        let req = Request {
            id,
            agent: new.agent,
            kind: new.kind,
            cmd: new.cmd,
            cwd: new.cwd,
            task: new.task,
            requested_by: new.requested_by,
            requested_at: now_secs(),
            status: Status::Queued,
            pid: None,
            started_at: None,
            finished_at: None,
            exit: None,
            mode: String::new(),
            note: String::new(),
        };
        q.requests.push(req.clone());
        Ok(req)
    })
}

/// Атомарно зарезервировать слот запуска под заявку.
///
/// Гонка `max_active` (два `pump` одновременно) закрывается здесь: проверка
/// числа запущенных и перевод заявки в `Running` делаются под ОДНИМ локом.
pub fn reserve(root: &Path, id: &str, max_active: u32) -> anyhow::Result<()> {
    with_lock(root, |q| {
        let running = q
            .requests
            .iter()
            .filter(|r| r.status == Status::Running)
            .count();
        if running >= max_active as usize {
            anyhow::bail!("достигнут лимит одновременных запусков: {running}/{max_active}");
        }
        let r = q
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("devh queue: заявка {id} не найдена"))?;
        if r.status != Status::Queued {
            anyhow::bail!(
                "devh queue: заявка {id} не в очереди ({})",
                r.status.as_str()
            );
        }
        r.status = Status::Running;
        r.started_at = Some(now_secs());
        r.pid = None;
        Ok(())
    })
}

/// Убрать cgroup-каталог запуска (best effort — cgroup может быть недоступен).
pub fn cgroup_cleanup(pid: i32) {
    if pid <= 0 {
        return;
    }
    let dir = Path::new("/sys/fs/cgroup").join(format!("devh-{pid}"));
    let _ = std::fs::remove_dir(dir);
}

/// Все заявки ( newest last ).
pub fn list(root: &Path) -> anyhow::Result<Vec<Request>> {
    Ok(load(root)?.requests)
}

/// Заявка по id.
pub fn get(root: &Path, id: &str) -> anyhow::Result<Option<Request>> {
    Ok(load(root)?.requests.into_iter().find(|r| r.id == id))
}

/// Отметить заявку запущенной (pid + режим лимитов).
pub fn mark_running(root: &Path, id: &str, pid: i32, mode: &str) -> anyhow::Result<Request> {
    with_lock(root, |q| {
        let r = q
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("devh queue: заявка {id} не найдена"))?;
        r.status = Status::Running;
        r.pid = Some(pid);
        r.started_at = Some(now_secs());
        r.mode = mode.to_string();
        Ok(r.clone())
    })
}

/// Отметить заявку завершённой.
pub fn mark_finished(root: &Path, id: &str, exit: i32) -> anyhow::Result<Request> {
    with_lock(root, |q| {
        let r = q
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("devh queue: заявка {id} не найдена"))?;
        r.status = if exit == 0 {
            Status::Done
        } else {
            Status::Failed
        };
        r.exit = Some(exit);
        r.finished_at = Some(now_secs());
        Ok(r.clone())
    })
}

/// Отменить заявку. Право: автор заявки либо lead/tech_lead.
pub fn cancel(root: &Path, id: &str, caller: &str, privileged: bool) -> anyhow::Result<Request> {
    let pid = with_lock(root, |q| {
        let r = q
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("devh queue: заявка {id} не найдена"))?;
        if r.requested_by != caller && !privileged {
            anyhow::bail!(
                "devh queue: отменить заявку {id} может только автор ({}) или lead/tech_lead",
                r.requested_by
            );
        }
        if r.status.is_final() {
            anyhow::bail!(
                "devh queue: заявка {id} уже завершена ({}) — отменять нечего",
                r.status.as_str()
            );
        }
        let pid = r.pid;
        r.status = Status::Cancelled;
        r.finished_at = Some(now_secs());
        r.note = format!("отменена {} (caller={})", caller, now_secs());
        Ok(pid)
    })?;
    if let Some(pid) = pid {
        kill_pid(pid);
    }
    get(root, id)?.ok_or_else(|| anyhow::anyhow!("devh queue: заявка {id} исчезла при отмене"))
}

/// Лидер своей группы процессов? (`/proc/<pid>/stat`: pgrp после `comm`).
///
/// Только если pgrp == pid, группу можно убивать целиком (`kill(-pid)`).
/// Иначе `kill(-pid)` убьёт ЧУЖУЮ группу (например, нашу own) — поэтому
/// проверка обязательна, а не «на глаз».
fn is_group_leader(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(") ") else {
        return false;
    };
    let mut it = rest.split_whitespace();
    let _state = it.next(); // state
    let _ppid = it.next(); // ppid
    it.next().and_then(|p| p.parse::<i32>().ok()) == Some(pid) // pgrp
}

/// Убить процесс запуска: сначала его ГРУППУ (setsid в `apply_child`),
/// затем сам pid; SIGTERM → пауза → SIGKILL.
pub fn kill_pid(pid: i32) {
    if pid <= 0 {
        return;
    }
    let group = is_group_leader(pid);
    unsafe {
        if group {
            libc::kill(-pid, libc::SIGTERM);
        }
        libc::kill(pid, libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    if crate::shell::proc::pid_alive(pid) {
        unsafe {
            if group {
                libc::kill(-pid, libc::SIGKILL);
            }
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Физические лимиты запуска.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// Сколько CPU-ядер доступно процессу (`taskset`).
    pub cpu_cores: Option<u32>,
    /// Потолок CPU-секунд (`RLIMIT_CPU`).
    pub cpu_secs: Option<u64>,
    /// Потолок адресного пространства в байтах (`RLIMIT_AS`).
    pub mem_bytes: Option<u64>,
    /// Потолок процессов (`RLIMIT_NPROC`; считается на UID — по умолчанию off).
    pub max_procs: Option<u32>,
    /// Приоритет (`nice`).
    pub nice: i32,
    /// Сколько запусков одновременно разрешено.
    pub max_active: u32,
    /// Порог loadavg, выше которого очередь не отпускает запуск (`None` = off).
    pub load_max: Option<f64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            cpu_cores: None,
            cpu_secs: None,
            mem_bytes: None,
            max_procs: None,
            nice: 10,
            max_active: 2,
            load_max: None,
        }
    }
}

fn opt_u64(kv: &HashMap<String, String>, key: &str) -> Option<u64> {
    kv.get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("off"))
        .and_then(|v| v.parse::<u64>().ok())
}

fn opt_u32(kv: &HashMap<String, String>, key: &str) -> Option<u32> {
    opt_u64(kv, key).and_then(|v| u32::try_from(v).ok())
}

/// Общий объём памяти системы (байт) из /proc/meminfo.
pub fn mem_total_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

/// Порог нагрузки по умолчанию: 2×CPU, но не ниже 4 (как гейт мутаций).
pub fn default_load_max() -> f64 {
    let cpus = crate::core::load::cpu_count() as f64;
    (cpus * 2.0).max(4.0)
}

impl Limits {
    /// Лимиты из набора KEY=VALUE (для тестов и для config-файла).
    pub fn from_kv(kv: &HashMap<String, String>) -> Limits {
        let cpu_cores = opt_u32(kv, "RESOURCE_CPU_CORES").filter(|n| *n > 0);
        let cpu_secs = opt_u64(kv, "RESOURCE_CPU_SECS").filter(|n| *n > 0);
        let max_procs = opt_u32(kv, "RESOURCE_MAX_PROCS").filter(|n| *n > 0);
        let max_active = opt_u32(kv, "RESOURCE_MAX_ACTIVE")
            .filter(|n| *n > 0)
            .unwrap_or(2);
        let mem_raw = kv.get("RESOURCE_MEM_MB").map(|v| v.trim()).unwrap_or("");
        let mem_bytes = if mem_raw.eq_ignore_ascii_case("off") {
            None
        } else if mem_raw.is_empty() || mem_raw == "0" {
            // авто: 80% системной памяти, делённые на число одновременных
            // запусков — иначе два процесса по 60% RAM дают OOM (инцидент:
            // машина доходила до 99–100% памяти).
            mem_total_bytes().map(|t| {
                let share = t.saturating_mul(80) / 100 / max_active as u64;
                share.max(256 * 1024 * 1024)
            })
        } else {
            mem_raw
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|mb| mb.saturating_mul(1024 * 1024))
        };
        let nice = kv
            .get("RESOURCE_NICE")
            .map(|v| v.trim())
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(10);
        let load_raw = kv.get("RESOURCE_LOAD_MAX").map(|v| v.trim()).unwrap_or("");
        let load_max = if load_raw.eq_ignore_ascii_case("off") {
            None
        } else if load_raw.is_empty() {
            Some(default_load_max())
        } else {
            load_raw.parse::<f64>().ok().filter(|v| v.is_finite())
        };
        Limits {
            cpu_cores,
            cpu_secs,
            mem_bytes,
            max_procs,
            nice,
            max_active,
            load_max,
        }
    }

    /// Лимиты из `.harness/config` + окружения `HARNESS_*`.
    pub fn from_config(root: &Path) -> Limits {
        let mut kv: HashMap<String, String> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(root.join(".harness/config")) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    kv.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        for key in [
            "RESOURCE_CPU_CORES",
            "RESOURCE_CPU_SECS",
            "RESOURCE_MEM_MB",
            "RESOURCE_MAX_PROCS",
            "RESOURCE_MAX_ACTIVE",
            "RESOURCE_LOAD_MAX",
            "RESOURCE_NICE",
        ] {
            if let Ok(v) = std::env::var(format!("HARNESS_{key}")) {
                if !v.trim().is_empty() {
                    kv.insert(key.to_string(), v);
                }
            }
        }
        Limits::from_kv(&kv)
    }

    /// Есть ли хоть один физический лимит.
    pub fn any_limit(&self) -> bool {
        self.cpu_cores.is_some()
            || self.cpu_secs.is_some()
            || self.mem_bytes.is_some()
            || self.max_procs.is_some()
    }

    /// Человекочитаемое описание лимитов.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!(
            "cpu_cores={}",
            self.cpu_cores
                .map(|n| n.to_string())
                .unwrap_or_else(|| "off".to_string())
        ));
        parts.push(format!(
            "cpu_secs={}",
            self.cpu_secs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "off".to_string())
        ));
        parts.push(format!(
            "mem={}",
            self.mem_bytes
                .map(|b| format!("{}MB", b / (1024 * 1024)))
                .unwrap_or_else(|| "off".to_string())
        ));
        parts.push(format!(
            "procs={}",
            self.max_procs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "off(uid)".to_string())
        ));
        parts.push(format!("nice={}", self.nice));
        parts.push(format!("max_active={}", self.max_active));
        parts.push(format!(
            "load_max={}",
            self.load_max
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "off".to_string())
        ));
        parts.join(" ")
    }

    /// Можно ли сейчас запускать (лимит одновременных + гейт нагрузки).
    /// Возвращает `Ok(())` или причину блокировки.
    pub fn gate(&self, running: usize) -> Result<(), String> {
        if running >= self.max_active as usize {
            return Err(format!(
                "достигнут лимит одновременных запусков: {running}/{}",
                self.max_active
            ));
        }
        if let Some(max) = self.load_max {
            if let Some(load) = crate::core::load::load_average() {
                if load > max {
                    return Err(format!(
                        "load {load:.1} > порог {max:.1} — запуск отложен (RESOURCE_LOAD_MAX=off отключает)"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Наложить лимиты на дочерний процесс (до его старта).
pub fn apply_child(cmd: &mut Command, limits: &Limits) {
    let nice = limits.nice;
    let cpu_secs = limits.cpu_secs;
    let mem = limits.mem_bytes;
    let procs = limits.max_procs;
    unsafe {
        cmd.pre_exec(move || {
            // Своя группа процессов: отмена заявки убивает ВСЮ группу
            // (рантайм + его дети), а не только обёртку `bash -c`.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if nice != 0 {
                libc::setpriority(libc::PRIO_PROCESS, 0, nice);
            }
            if let Some(s) = cpu_secs {
                set_rlimit(libc::RLIMIT_CPU, s, s)?;
            }
            if let Some(b) = mem {
                set_rlimit(libc::RLIMIT_AS, b, b)?;
            }
            if let Some(p) = procs {
                set_rlimit(libc::RLIMIT_NPROC, p as u64, p as u64)?;
            }
            Ok(())
        });
    }
}

fn set_rlimit(res: libc::__rlimit_resource_t, soft: u64, hard: u64) -> std::io::Result<()> {
    let rl = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    if unsafe { libc::setrlimit(res, &rl) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Есть ли `taskset` в PATH (для ограничения по ядрам).
pub fn have_taskset() -> bool {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join("taskset");
        if candidate.is_file() {
            return true;
        }
    }
    false
}

/// Наложить лимиты в cgroup v2 (если корень доступен на запись).
pub fn cgroup_apply(pid: i32, limits: &Limits) -> anyhow::Result<()> {
    let root = Path::new("/sys/fs/cgroup");
    let dir = root.join(format!("devh-{pid}"));
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("cgroup: {e}"))?;
    if let Some(b) = limits.mem_bytes {
        std::fs::write(dir.join("memory.max"), b.to_string())?;
    }
    if let Some(c) = limits.cpu_cores {
        std::fs::write(dir.join("cpu.max"), format!("{c} 100000"))?;
    }
    std::fs::write(dir.join("cgroup.procs"), pid.to_string())?;
    Ok(())
}

/// Результат запуска с лимитами.
#[derive(Debug, Clone)]
pub struct Spawned {
    pub pid: i32,
    pub mode: String,
    pub note: String,
}

/// Запустить shell-команду с физическими лимитами (в фоне).
pub fn spawn_limited(
    cmd_line: &str,
    cwd: &Path,
    limits: &Limits,
    log: Option<&Path>,
) -> anyhow::Result<Spawned> {
    let mut cmd = if let Some(cores) = limits.cpu_cores {
        if have_taskset() {
            let mut c = Command::new("taskset");
            let list = if cores <= 1 {
                "0".to_string()
            } else {
                format!("0-{}", cores - 1)
            };
            c.arg("-c").arg(list).arg("bash").arg("-c").arg(cmd_line);
            c
        } else {
            let mut c = Command::new("bash");
            c.arg("-c").arg(cmd_line);
            c
        }
    } else {
        let mut c = Command::new("bash");
        c.arg("-c").arg(cmd_line);
        c
    };
    // Лог запуска: stdout/stderr процесса ПИШУТСЯ в файл (иначе оператор
    // видит только стартовую строку и не может понять, что сломалось).
    let log_file = match log {
        Some(log) => {
            if let Some(parent) = log.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            Some(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(log)?,
            )
        }
        None => None,
    };
    cmd.current_dir(cwd).stdin(Stdio::null());
    match log_file.as_ref() {
        Some(f) => {
            let out = f.try_clone()?;
            let err = f.try_clone()?;
            cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    apply_child(&mut cmd, limits);
    let child = cmd.spawn()?;
    let pid = child.id() as i32;
    if let Some(mut f) = log_file {
        let _ = writeln!(
            f,
            "devh launch: pid={pid} cmd={cmd_line} limits=[{}]",
            limits.describe()
        );
        let _ = f.flush();
    }
    // cgroup — если доступен; иначе честно фиксируем userspace-режим.
    let (mode, note) = match cgroup_apply(pid, limits) {
        Ok(()) => ("cgroup+limits".to_string(), String::new()),
        Err(e) => (
            "limits(userspace)".to_string(),
            format!("cgroup v2 недоступен ({e}) — лимиты userspace (RLIMIT/taskset)"),
        ),
    };
    Ok(Spawned { pid, mode, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn limits_defaults_are_safe() {
        let l = Limits::from_kv(&kv(&[]));
        assert!(l.cpu_cores.is_none(), "CPU-ядра по умолчанию не режем");
        assert!(
            l.max_procs.is_none(),
            "RLIMIT_NPROC по умолчанию off (счёт на UID)"
        );
        assert_eq!(l.max_active, 2, "по умолчанию два одновременных запуска");
        assert!(l.load_max.is_some(), "гейт нагрузки включён по умолчанию");
    }

    #[test]
    fn limits_read_from_kv() {
        let l = Limits::from_kv(&kv(&[
            ("RESOURCE_CPU_CORES", "2"),
            ("RESOURCE_CPU_SECS", "900"),
            ("RESOURCE_MEM_MB", "1024"),
            ("RESOURCE_MAX_ACTIVE", "3"),
        ]));
        assert_eq!(l.cpu_cores, Some(2));
        assert_eq!(l.cpu_secs, Some(900));
        assert_eq!(l.mem_bytes, Some(1024 * 1024 * 1024));
        assert_eq!(l.max_active, 3);
        assert!(l.any_limit());
    }

    #[test]
    fn mem_off_and_junk_do_not_panic() {
        let l = Limits::from_kv(&kv(&[("RESOURCE_MEM_MB", "off")]));
        assert!(l.mem_bytes.is_none());
        let l = Limits::from_kv(&kv(&[("RESOURCE_MEM_MB", "junk")]));
        assert!(l.mem_bytes.is_none());
    }

    #[test]
    fn gate_blocks_over_limit() {
        let l = Limits {
            max_active: 2,
            load_max: None,
            ..Limits::default()
        };
        assert!(l.gate(1).is_ok());
        assert!(l.gate(2).is_err());
    }

    #[test]
    fn reserve_takes_slot_and_blocks_over_limit() {
        let dir = std::env::temp_dir().join(format!("devh-rq-reserve-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".harness/orch")).ok();
        let _ = std::fs::remove_file(dir.join(".harness/orch/launch_queue.json"));
        let mk = || NewRequest {
            agent: "step5".into(),
            kind: "agent".into(),
            cmd: "true".into(),
            cwd: None,
            task: None,
            requested_by: "step5".into(),
        };
        let a = enqueue(&dir, mk()).expect("enqueue a");
        let b = enqueue(&dir, mk()).expect("enqueue b");
        // Лимит 1: первый забирает слот, второй — отказ.
        reserve(&dir, &a.id, 1).expect("первый забирает слот");
        let blocked = reserve(&dir, &b.id, 1);
        assert!(blocked.is_err(), "второй не должен пройти: {blocked:?}");
        assert!(
            blocked
                .unwrap_err()
                .to_string()
                .contains("лимит одновременных"),
            "внятная причина отказа"
        );
        // Повторное резервирование той же заявки — отказ (она уже не queued).
        let again = reserve(&dir, &a.id, 2);
        assert!(again.is_err(), "нельзя резервировать не-queued заявку");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_leader_detection_matches_proc() {
        // Обычный процесс (тест) — не лидер группы, если pgid != pid.
        let me = std::process::id() as i32;
        let leader = is_group_leader(me);
        let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap_or_default();
        let pgrp = stat.rsplit_once(") ").and_then(|(_, rest)| {
            rest.split_whitespace()
                .nth(2)
                .and_then(|p| p.parse::<i32>().ok())
        });
        assert_eq!(leader, pgrp == Some(me), "решение совпадает с /proc");
        // Несуществующий pid — не лидер (и не паникуем).
        assert!(!is_group_leader(4_000_000));
    }

    #[test]
    fn mem_default_is_shared_between_concurrent_runs() {
        let l = Limits::from_kv(&kv(&[]));
        if let Some(total) = mem_total_bytes() {
            let expected = (total * 80 / 100 / l.max_active as u64).max(256 * 1024 * 1024);
            assert_eq!(
                l.mem_bytes,
                Some(expected),
                "дефолт памяти делится на число одновременных запусков"
            );
        }
    }

    #[test]
    fn queue_id_is_monotonic() {
        let mut q = Queue::default();
        assert_eq!(q.next_id(), "Q-0001");
        q.requests.push(sample("Q-0007"));
        assert_eq!(q.next_id(), "Q-0008");
    }

    fn sample(id: &str) -> Request {
        Request {
            id: id.to_string(),
            agent: "a".into(),
            kind: "agent".into(),
            cmd: "true".into(),
            cwd: None,
            task: None,
            requested_by: "a".into(),
            requested_at: 0,
            status: Status::Queued,
            pid: None,
            started_at: None,
            finished_at: None,
            exit: None,
            mode: String::new(),
            note: String::new(),
        }
    }

    #[test]
    fn enqueue_and_cancel_roundtrip() {
        let dir = std::env::temp_dir().join(format!("devh-rq-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".harness/orch")).ok();
        let req = enqueue(
            &dir,
            NewRequest {
                agent: "step5".into(),
                kind: "agent".into(),
                cmd: "true".into(),
                cwd: None,
                task: None,
                requested_by: "step5".into(),
            },
        )
        .expect("enqueue");
        assert_eq!(req.status, Status::Queued);
        let again = enqueue(
            &dir,
            NewRequest {
                agent: "step5".into(),
                kind: "agent".into(),
                cmd: "true".into(),
                cwd: None,
                task: None,
                requested_by: "step5".into(),
            },
        )
        .expect("enqueue 2");
        assert_ne!(again.id, req.id, "id не повторяются");
        // Чужой агент отменить не может.
        let denied = cancel(&dir, &req.id, "other", false);
        assert!(denied.is_err(), "отмена чужой заявки запрещена");
        let done = cancel(&dir, &req.id, "step5", false).expect("cancel");
        assert_eq!(done.status, Status::Cancelled);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn running_count_counts_only_running() {
        let mut q = Queue::default();
        let mut r = sample("Q-0001");
        r.status = Status::Running;
        q.requests.push(r);
        q.requests.push(sample("Q-0002"));
        assert_eq!(q.running_count(), 1);
        assert_eq!(q.pending().len(), 1);
    }
}
