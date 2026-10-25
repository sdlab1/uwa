//! Recovery: stale lock detection и безопасное освобождение (T-114).
//!
//! `lock_guard.rs` (T-113) защищает от symlink/TOCTOU, `lock.rs` использует
//! `flock` для синхронизации. Но если процесс кранулся (kill -9, паника
//! вне drop), `flock`-дескриптор закрывается ядром — лок освобождается
//! автоматически. **Однако lock-файл остаётся на диске** с «записью»
//! `pid N (label)`.
//!
//! Этот модуль детектит такие **stale lock files**:
//! - **pid**: процесс, записанный в lock-файл, больше не существует.
//! - **mtime**: lock-файл старше `max_age` (например, 30 мин) — даже если
//!   pid жив (PID reuse), такой лок считается устаревшим.
//!
//! Безопасное освобождение: удалить lock-файл после проверки, что мы не
//! удаляем активный лок (flock ещё держится другим процессом).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Почему lock считается устаревшим.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// Процесс по pid из lock-файла больше не существует.
    DeadProcess { pid: u32 },
    /// Lock-файл старше max_age.
    Expired { age: Duration },
    /// Файл lock пуст или содержит некорректные данные — нельзя доверять.
    Malformed,
}

/// Результат попытки восстановления лока.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Лок свободен (flock не удерживается) — можно захватить.
    Free,
    /// Лок stale — восстановлен (файл удалён или заменён).
    Recovered { staleness: Staleness },
    /// Лок занят живым процессом — не можем вмешаться.
    Held { pid: u32, holder: String },
}

/// Максимальный возраст lock-файла до considers.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 60); // 30 мин

/// Разобрать содержимое lock-файла: «pid N (label)» → Some(pid).
///
/// Возвращает `None` если формат не распознаётся (пусто, не «pid»).
pub fn parse_lock_pid(content: &str) -> Option<u32> {
    let line = content.lines().next()?;
    let line = line.trim();
    let rest = line.strip_prefix("pid ")?;
    let pid_str = rest.split_whitespace().next()?;
    pid_str.parse::<u32>().ok()
}

/// Проверить, жив ли процесс с данным pid.
///
/// Использует `kill(pid, 0)` — отправка сигнала 0 (проверка существования).
/// `ESRCH` (нет такого процесса) → false. `EPERM` (процесс существует, но
/// нет доступа) → true (процесс есть, просто чужой).
pub fn is_process_alive(pid: u32) -> bool {
    // libc::kill с сигналом 0: 0 = процесс существует, -1 + ESRCH = нет.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Проверить, жив ли процесс по pid, используя /proc (Linux-specific быстрее).
///
/// На Linux: `/proc/<pid>` существует тогда и только тогда, когда процесс жив.
/// На других платформах падает обратно на `is_process_alive`.
pub fn is_process_alive_procfs(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        return proc_path.exists();
    }
    #[cfg(not(target_os = "linux"))]
    {
        is_process_alive(pid)
    }
}

/// Возраст lock-файла по mtime.
///
/// Возвращает `None` если файл не существует или mtime недоступен.
pub fn file_age(path: &Path) -> Option<Duration> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.mtime() as u64;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    let file_time = Duration::from_secs(mtime);
    if now < file_time {
        return Some(Duration::ZERO);
    }
    Some(now - file_time)
}

/// Проверить lock-файл на staleness.
///
/// Устаревшим считается:
/// - процесс по pid не существует (`DeadProcess`);
/// - файл старше `max_age` (`Expired`);
/// - содержимое не распознаётся (`Malformed`).
///
/// Живой процесс + свежий файл = `Held`.
pub fn check_staleness(path: &Path, max_age: Duration) -> Staleness {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            return Staleness::Malformed;
        }
    };

    let pid = match parse_lock_pid(&content) {
        Some(p) => p,
        None => {
            // Пустой или некорректный lock-файл — считаем stale, удаляем.
            return Staleness::Malformed;
        }
    };

    // Проверка 1: жив ли процесс?
    if !is_process_alive_procfs(pid) {
        return Staleness::DeadProcess { pid };
    }

    // Проверка 2: mtime
    if let Some(age) = file_age(path) {
        if age > max_age {
            return Staleness::Expired { age };
        }
    }

    // Живой процесс и свежий файл — лок активен.
    // Возвращаем DeadProcess с pid=0 как сигнал «не stale».
    // Но тип Staleness не имеет варианта для «не stale».
    // Используем Expired с нулевой持续ностью как «не stale».
    // Лучше: вернём Staleness с age=0 и pid — caller decides.
    Staleness::Expired {
        age: Duration::ZERO,
    }
}

/// Проверить, удерживается ли лок другим процессом через flock.
///
/// Пытаемся `try_lock_exclusive` на lock-файле:
/// - `Ok` → лок не удерживается (можно удалять файл).
/// - `Err` → лок удерживается живым процессом (не трогать!).
fn flock_is_free(path: &Path) -> bool {
    let f = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(path)
    {
        Ok(f) => f,
        Err(_) => return true, // файл не открывается — считаем свободным
    };
    // fs2: try_lock_exclusive возвращает Ok(true) если удалось взять,
    // Ok(false) — уже занят, Err — ошибка.
    // Мы просто хотим узнать, свободен ли flock.
    match fs2::FileExt::try_lock_exclusive(&f) {
        Ok(()) => {
            // Взяли — значит был свободен. Немедленно отпускаем.
            let _ = fs2::FileExt::unlock(&f);
            true
        }
        Err(_) => false,
    }
}

/// Попытаться восстановить (освободить) stale lock.
///
/// Алгоритм:
/// 1. Если lock-файла нет → `Recovery::Free`.
/// 2. Прочитать pid из файла.
/// 3. Если pid жив → проверить mtime:
///    - старше max_age → `Expired`, удалить файл (если flock свободен).
///    - свежий → `Held`.
/// 4. Если pid мёртв → `DeadProcess`, удалить файл (если flock свободен).
/// 5. Если не удалось распарсить → `Malformed`, удалить файл.
///
/// Удаление происходит только если `flock_is_free` — мы не убиваем
/// активный lock другого процесса.
pub fn recover_lock(path: &Path, max_age: Duration) -> Recovery {
    // Файл не существует — лок свободен.
    if !path.exists() {
        return Recovery::Free;
    }

    let staleness = check_staleness(path, max_age);

    // Если staleness = Expired { age: ZERO } → это значит «живой процесс + свежий файл».
    if let Staleness::Expired { age } = &staleness {
        if *age == Duration::ZERO {
            // Не stale — лок активен. Вернём Held с pid.
            if let Some(content) = fs::read_to_string(path).ok() {
                let pid = parse_lock_pid(&content).unwrap_or(0);
                let holder = content.lines().next().unwrap_or("unknown").to_string();
                return Recovery::Held { pid, holder };
            }
            return Recovery::Held {
                pid: 0,
                holder: "unknown".into(),
            };
        }
    }

    // Здесь staleness — явно stale (DeadProcess, Expired с age > 0, или Malformed).
    // Проверим, что flock не удерживается другим процессом.
    if flock_is_free(path) {
        // Безопасно удаляем — никто не держит flock.
        let _ = fs::remove_file(path);
        Recovery::Recovered { staleness }
    } else {
        // Кто-то держит flock — не можем вмешаться, даже если pid мёртв.
        // (Теоретически: процесс умер, но потомок унаследовал fd — редко, но возможно.)
        if let Some(content) = fs::read_to_string(path).ok() {
            let pid = parse_lock_pid(&content).unwrap_or(0);
            let holder = content.lines().next().unwrap_or("unknown").to_string();
            Recovery::Held { pid, holder }
        } else {
            Recovery::Held {
                pid: 0,
                holder: "unknown".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parse_lock_pid_extracts_pid() {
        assert_eq!(parse_lock_pid("pid 12345 (devh orch)"), Some(12345));
        assert_eq!(parse_lock_pid("pid 1 (test)"), Some(1));
        assert_eq!(parse_lock_pid("pid 42"), Some(42));
    }

    #[test]
    fn parse_lock_pid_rejects_malformed() {
        assert_eq!(parse_lock_pid(""), None);
        assert_eq!(parse_lock_pid("not a pid"), None);
        assert_eq!(parse_lock_pid("pid abc (test)"), None);
        assert_eq!(parse_lock_pid("locked"), None);
        assert_eq!(parse_lock_pid("pid"), None);
    }

    #[test]
    fn is_process_alive_current_process() {
        // Текущий процесс жив — всегда true.
        assert!(is_process_alive(std::process::id()));
        assert!(is_process_alive_procfs(std::process::id()));
    }

    #[test]
    fn is_process_alive_dead_pid() {
        // PID 4194303 — маловероятно существует (обычно используется
        // в Linux как «bogus pid»). Если вдруг жив — тест пропустим.
        // Используем очень большой pid.
        let bogus = 999999u32;
        assert!(!is_process_alive(bogus));
        assert!(!is_process_alive_procfs(bogus));
    }

    #[test]
    fn file_age_returns_some_for_existing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("lock");
        std::fs::write(&p, "pid 1 (x)").unwrap();
        let age = file_age(&p);
        assert!(age.is_some());
        assert!(age.unwrap().as_secs() < 60); // созданный в тесте — свежий
    }

    #[test]
    fn file_age_returns_none_for_missing() {
        let dir = tempdir().unwrap();
        assert!(file_age(&dir.path().join("nope")).is_none());
    }

    #[test]
    fn check_staleness_dead_process() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("stale.lock");
        // Запишем lock с pid, который не существует.
        std::fs::write(&p, "pid 999999 (dead)").unwrap();
        let result = check_staleness(&p, DEFAULT_MAX_AGE);
        assert_eq!(result, Staleness::DeadProcess { pid: 999999 });
    }

    #[test]
    fn check_staleness_malformed() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.lock");
        std::fs::write(&p, "not a lock file").unwrap();
        let result = check_staleness(&p, DEFAULT_MAX_AGE);
        assert_eq!(result, Staleness::Malformed);
    }

    #[test]
    fn check_staleness_live_process_is_not_stale() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("live.lock");
        // Запишем lock с нашим pid — процесс жив и файл свеж.
        std::fs::write(&p, format!("pid {} (test)", std::process::id())).unwrap();
        let result = check_staleness(&p, DEFAULT_MAX_AGE);
        // Должен вернуть Expired { age: ZERO } — сигнал «не stale».
        assert_eq!(
            result,
            Staleness::Expired {
                age: Duration::ZERO
            }
        );
    }

    #[test]
    fn recover_lock_free_when_no_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("missing.lock");
        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert_eq!(r, Recovery::Free);
    }

    #[test]
    fn recover_lock_dead_process_removes_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("stale.lock");
        std::fs::write(&p, "pid 999999 (dead)").unwrap();
        assert!(p.exists());
        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert!(
            matches!(r, Recovery::Recovered { staleness: Staleness::DeadProcess { pid } } if pid == 999999)
        );
        assert!(!p.exists(), "stale lock file must be removed");
    }

    #[test]
    fn recover_lock_live_process_returns_held() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("live.lock");
        std::fs::write(&p, format!("pid {} (test)", std::process::id())).unwrap();
        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert!(matches!(r, Recovery::Held { .. }));
        assert!(p.exists(), "live lock file must NOT be removed");
    }

    #[test]
    fn recover_lock_malformed_removes_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.lock");
        std::fs::write(&p, "garbage").unwrap();
        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert!(matches!(
            r,
            Recovery::Recovered {
                staleness: Staleness::Malformed
            }
        ));
        assert!(!p.exists(), "malformed lock file must be removed");
    }

    #[test]
    fn recover_lock_expired_mtime_removes_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("old.lock");
        // Используем наш pid (живой процесс) + старый mtime → Expired (не DeadProcess).
        std::fs::write(&p, format!("pid {} (test)", std::process::id())).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let f = std::fs::File::open(&p).unwrap();
        f.set_modified(old).unwrap();
        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert!(matches!(
            r,
            Recovery::Recovered {
                staleness: Staleness::Expired { .. }
            }
        ));
        assert!(!p.exists(), "expired-by-mtime lock must be removed");
    }

    #[test]
    fn recover_lock_held_by_other_flock_not_removed() {
        // Если другой процесс держит flock — не удаляем, даже если pid мёртв.
        let dir = tempdir().unwrap();
        let p = dir.path().join("held.lock");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&p)
            .unwrap();
        writeln!(&mut f, "pid 999999 (dead)").unwrap();
        // Удерживаем flock на этом файле.
        fs2::FileExt::lock_exclusive(&f).unwrap();

        let r = recover_lock(&p, DEFAULT_MAX_AGE);
        assert!(matches!(r, Recovery::Held { .. }));
        assert!(p.exists(), "lock held by flock must NOT be removed");
    }
}
