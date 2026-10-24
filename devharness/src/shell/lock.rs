use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[allow(dead_code)]
pub struct ItemRunLock(File);

impl ItemRunLock {
    pub fn acquire(item_dir: &Path) -> Option<Self> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(item_dir.join("run.lock"))
            .ok()?;
        f.try_lock_exclusive().ok().map(|_| Self(f))
    }
}

#[allow(dead_code)]
pub struct GlobalLock(File);

impl GlobalLock {
    pub fn acquire(harness_dir: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(harness_dir.join(".lock"))?;
        f.lock_exclusive()?;
        Ok(Self(f))
    }
}

/// P7: эксклюзивный лок in-place мутационного прогона (§9.13). Для нашей
/// архитектуры `cargo-mutants --in-place` — это агент: он пишет в дерево
/// worktree, может быть убит посередине, может идти параллельно с коммитом.
/// Класс «отравленный коммит» (ae9f719: parse_load -> None попал в индекс
/// посреди in-place-прогона) умирает целиком: `mutate_gate` держит лок на
/// всё время прогона, а `devh commit`/`devh merge` берут тот же лок или
/// отказываются. Файл — в `.harness/` (вне git), лок на FS: мёртвый
/// держатель освобождает сам (flock на death), staleness-очистка не нужна.
///
/// Скоуп — одно дерево (worktree): параллельные item'ы мутируются каждый в
/// своём worktree, блокируется только комбинация «мутации в дереве X» ×
/// «commit/merge дерева X».
#[allow(dead_code)] // поле не читается: его смысл — Drop (flock снимается при смерти)
pub struct MutateLock(File);

impl MutateLock {
    /// Ok — лок взят и держится, пока живёт guard. Err(holder) — занят,
    /// holder = «pid N (label)» — для сообщения оператору.
    pub fn try_acquire(wt: &Path, label: &str) -> Result<Self, String> {
        let dir = wt.join(".harness");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("mutate.lock"))
            .map_err(|e| format!("{e}"))?;
        if f.try_lock_exclusive().is_err() {
            let holder = std::fs::read_to_string(dir.join("mutate.lock"))
                .ok()
                .and_then(|t| t.lines().next().map(str::to_string))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "неизвестный прогон".into());
            return Err(holder);
        }
        use std::io::Write;
        f.set_len(0).map_err(|e| format!("{e}"))?;
        writeln!(&f, "pid {} ({label})", std::process::id()).map_err(|e| format!("{e}"))?;
        Ok(Self(f))
    }
}

// P5 R10: лок на КАТАЛОГ аудита (`--output` прогона cargo-mutants).
//
// Класс «два прогона с одним output»: второй делает уборку того же
// `mutants.out` и убивает прогон первого, а вердикт считается по чужим
// данным. Лок сериализует: второй получает `Err(holder)` → rc=2 и имя
// держателя. Ключ — sha256 АБСОЛЮТНОГО пути output: относительный и
// абсолютный путь (`out` vs `/abs/root/out`) дают ОДИН лок. Файл — в
// `.harness/mutate/locks/` (вне git); flock снимается сам при смерти
// держателя, staleness-очистка не нужна.
//
// Держатель на время прогона — супервизор (`mutate::supervise`);
// `mutate::start` берёт лок как проверка «каталог не переписывается».
//
// РЕЕНТРАНТНОСТЬ (per-thread): flock на другом fd того же процесса даёт
// EWOULDBLOCK, хотя реального конфликта нет — наблюдалось «реестр занят
// (pid <свой pid>)», когда команда оркестратора брала лок дважды подряд
// (вложенный вызов). Поэтому ключи уже взятых локов хранятся в
// thread-local HELD: повторный захват ТОГО ЖЕ ключа в ТОМ ЖЕ потоке —
// no-op guard (file: None), конфликта не создаёт и ключ не снимает.
//
// ДРУГОЙ ПОТОК ТОГО ЖЕ ПРОЦЕССА: ждёт освобождения (внутрипроцессный
// координатор `inproc_table`), а не падает — это был источник флейка
// «реестр занят (pid свой)» в параллельных cargo-тестах. ЧУЖОЙ ПРОЦЕСС:
// по-прежнему мгновенный отказ с pid держателя — взятие задачи остаётся
// транзакционным между агентами.
thread_local! {
    static HELD: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Сколько ждать внутрипроцессный конфликт, прежде чем отказать.
const INPROC_WAIT: Duration = Duration::from_secs(15);

/// Внутрипроцессная координация локов: ключ → «занят».
///
/// `flock` защищает ОТ ДРУГИХ ПРОЦЕССОВ. Внутри одного процесса (параллельные
/// cargo-тесты одного бинаря) два разных fd одного файла конфликтуют, хотя
/// «настоящего» конфликта нет: это тот же самый процесс. Раньше это давало
/// флейк «реестр занят (pid <свой pid>)» — тесты падали по расписанию.
///
/// Поэтому держим ещё и слот на ключ: второй поток ТОГО ЖЕ процесса ЖДЁТ
/// освобождения (с потолком ожидания), а чужой процесс по-прежнему получает
/// мгновенный отказ с pid держателя — транзакционность взятия задачи цела.
fn inproc_table() -> &'static (Mutex<HashMap<String, bool>>, Condvar) {
    static TABLE: OnceLock<(Mutex<HashMap<String, bool>>, Condvar)> = OnceLock::new();
    TABLE.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

/// Занять внутрипроцессный слот ключа (ждёт, если слот занят другим потоком).
///
/// Err — слот не освободился за `INPROC_WAIT` (зависший держатель).
fn inproc_acquire(key: &str) -> Result<(), String> {
    let (mtx, cv) = inproc_table();
    let mut owners = mtx.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = Instant::now() + INPROC_WAIT;
    while owners.contains_key(key) {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "внутрипроцессный конфликт: лок держится другим потоком этого процесса \
                 дольше {}с",
                INPROC_WAIT.as_secs()
            ));
        }
        let (guard, _) = cv
            .wait_timeout(owners, deadline - now)
            .unwrap_or_else(|e| e.into_inner());
        owners = guard;
    }
    owners.insert(key.to_string(), true);
    Ok(())
}

/// Освободить внутрипроцессный слот ключа и разбудить ожидающих.
fn inproc_release(key: &str) {
    let (mtx, cv) = inproc_table();
    {
        let mut owners = mtx.lock().unwrap_or_else(|e| e.into_inner());
        owners.remove(key);
    }
    cv.notify_all();
}

/// Лок с реентрантностью: `file: None` = no-op guard (вложенный захват),
/// `file: Some` = реальный владелец flock.
#[allow(dead_code)] // поля читаются в Drop
pub struct OutputLock {
    file: Option<File>,
    key: Option<String>,
}

impl OutputLock {
    /// Ok — лок взят и держится, пока живёт guard. Err(holder) — занят,
    /// holder = «pid N (label)» — для сообщения оператору.
    pub fn try_acquire(root: &Path, output: &Path, label: &str) -> Result<Self, String> {
        let key = crate::core::digest::sha256_hex(lock_key(root, output).as_os_str().as_bytes());
        // Вложенный захват в этом же потоке -> no-op guard (конфликта нет).
        if HELD.with(|h| h.borrow().contains(&key)) {
            return Ok(Self {
                file: None,
                key: None,
            });
        }
        let dir = root.join(".harness").join("mutate").join("locks");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
        let path = dir.join(format!("{key}.lock"));
        // Внутрипроцессный слот: поток того же процесса ЖДЁТ вместо отказа
        // (flock внутри процесса даёт ложный конфликт — флейк тестов).
        inproc_acquire(&key).map_err(|e| format!("{e} [лок: {}]", path.display()))?;
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| {
                inproc_release(&key);
                format!("{e}")
            })?;
        if f.try_lock_exclusive().is_err() {
            inproc_release(&key);
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.lines().next().map(str::to_string))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "неизвестный прогон".into());
            // Путь лока в сообщении: оператор сразу видит, КАКОЙ файл занят
            // (реестр репозитория, временный реестр теста, аудит мутаций).
            // + диагностика держателя: свой fd (утечка в процессе) или pid
            // из /proc/locks (чужой процесс) — иначе флейк не расследуем.
            return Err(format!(
                "{holder} [ключ: {key}][лок: {}]{}",
                path.display(),
                lock_diag(&path, Some(f.as_raw_fd()))
            ));
        }
        use std::io::Write;
        f.set_len(0).map_err(|e| format!("{e}"))?;
        writeln!(&f, "pid {} ({label})", std::process::id()).map_err(|e| format!("{e}"))?;
        HELD.with(|h| h.borrow_mut().insert(key.clone()));
        Ok(Self {
            file: Some(f),
            key: Some(key),
        })
    }
}

impl Drop for OutputLock {
    fn drop(&mut self) {
        // Ключ снимает только внешний guard (file: Some); вложенный no-op
        // (file: None) ключ не трогает — иначе внешний лок освободился бы
        // слишком рано.
        if let Some(key) = self.key.take() {
            // 1) Снять flock: ядро освобождает лок при закрытии fd.
            //    Это ДОЛЖНО идти раньше пробуждения ожидающих, иначе
            //    ожидающий проснётся и упрётся в ещё занятый flock.
            self.file = None;
            // 2) Снять thread-local reentrancy-метку.
            HELD.with(|h| {
                h.borrow_mut().remove(&key);
            });
            // 3) Освободить внутрипроцессный слот и разбудить ожидающих.
            inproc_release(&key);
        } else {
            self.file = None;
        }
    }
}

/// Диагностика конфликта лока: кто держит файл.
///
/// Возвращает короткую строку для сообщения об ошибке:
///   - «свои fd: 7,9» — в ЭТОМ процессе остались открытые дескрипторы
///     (утечка лока: guard не вы dropped / fd унаследован потомком);
///   - «держит pid N» — лок держит чужой процесс (видно из /proc/locks);
///   - пусто — держатель не найден (гонка между проверкой и чтением).
fn lock_diag(path: &Path, own_fd: Option<std::os::unix::io::RawFd>) -> String {
    let mut out = String::new();
    // 1) Дескрипторы этого процесса, указывающие на файл лока.
    //    `own_fd` — тот дескриптор, который мы только что открыли и который
    //    лок НЕ держит (попытка flock провалилась) — его исключаем, иначе
    //    «свои fd» всегда показывал бы сами себя и диагностика была бы
    //    бесполезной. Остальные fd этого процесса — кандидаты на утечку.
    let mut own: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
        for entry in entries.flatten() {
            let fd = entry.file_name().to_string_lossy().to_string();
            if let Some(skip) = own_fd {
                if fd.parse::<std::os::unix::io::RawFd>().ok() == Some(skip) {
                    continue;
                }
            }
            if let Ok(target) = std::fs::read_link(entry.path()) {
                if target == path {
                    own.push(fd);
                }
            }
        }
    }
    if !own.is_empty() {
        out.push_str(&format!(" [свои fd: {}]", own.join(",")));
    }
    // 2) Кто держит flock снаружи: /proc/locks (inode устройство:inode).
    if let Ok(meta) = std::fs::metadata(path) {
        let ino = meta.ino();
        let dev = meta.dev();
        if let Ok(text) = std::fs::read_to_string("/proc/locks") {
            for line in text.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 6 {
                    continue;
                }
                let Some(pid) = parts.get(4) else { continue };
                let Some(file_id) = parts.get(5) else { continue };
                let Some((dev_hex, ino_str)) = file_id.split_once(':') else {
                    continue;
                };
                let Ok(ino_num) = ino_str.parse::<u64>() else {
                    continue;
                };
                let Ok(dev_num) = u64::from_str_radix(dev_hex, 16) else {
                    continue;
                };
                if ino_num == ino && dev_num == dev {
                    out.push_str(&format!(" [держит pid {pid}]"));
                }
            }
        }
    }
    out
}

/// Ключ лока: абсолютный путь без `.`/`..` — без IO (путь существует не
/// обязан). Относительный резолвится от `root` — так же, как от cwd
/// cargo-mutants, который запускается в корне репозитория.
fn lock_key(root: &Path, output: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        cwd.join(root)
    };
    let base = if output.is_absolute() {
        output.to_path_buf()
    } else {
        root.join(output)
    };
    let mut out = PathBuf::new();
    for c in base.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutate_lock_excludes_in_process() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let g = MutateLock::try_acquire(dir.path(), "mutation")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        let busy = MutateLock::try_acquire(dir.path(), "second");
        let err = busy.err().unwrap_or_else(|| unreachable!());
        assert!(err.starts_with("pid "), "holder = {err}");
        assert!(err.contains("mutation"), "holder = {err}");
        drop(g);
        // мёртвый держатель не оставляет stale: лок свободен
        assert!(MutateLock::try_acquire(dir.path(), "third").is_ok());
    }

    /// P5 R10: относительный и абсолютный путь output = ОДИН лок.
    ///
    /// Второй поток ТОГО ЖЕ процесса не получает ложный отказ, а ЖДЁТ
    /// освобождения (внутрипроцессный координатор) — это лечит флейк
    /// «реестр занят (pid <свой>)» в параллельных cargo-тестах.
    #[test]
    fn output_lock_relative_and_absolute_share_one_key() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path().to_path_buf();
        let g = OutputLock::try_acquire(root.as_path(), Path::new("audit"), "devh mutate start")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        // Тот же каталог, но абсолютным путём и с лишним `.`: тот же лок.
        let root2 = root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let res = OutputLock::try_acquire(&root2, &root2.join(".").join("audit"), "second");
            let _ = tx.send(res.is_ok());
            res
        });
        // Пока держим первый лок — второй поток ЖДЁТ (а не падает).
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "второй поток того же процесса должен ждать, а не получить отказ"
        );
        drop(g);
        let got = waiter.join().unwrap_or_else(|_| unreachable!());
        let held = got.unwrap_or_else(|e| unreachable!("после освобождения лок берётся: {e}"));
        drop(held);
        let third = OutputLock::try_acquire(&root, &root.join("audit"), "third");
        assert!(
            third.is_ok(),
            "мёртвый держатель не оставляет stale: {:?}",
            third.err()
        );
    }

    /// Внутрипроцессный конфликт: второй поток ЖДЁТ и затем берёт лок —
    /// ложных «занято (pid свой)» больше нет.
    #[test]
    fn output_lock_same_process_waits_instead_of_failing() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path().to_path_buf();
        let g = OutputLock::try_acquire(&root, Path::new("audit"), "first")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        let root2 = root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let res = OutputLock::try_acquire(&root2, Path::new("audit"), "second");
            let _ = tx.send(res.is_ok());
            res
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "второй поток ждёт освобождения"
        );
        drop(g);
        let got = waiter.join().unwrap_or_else(|_| unreachable!());
        let held = got.unwrap_or_else(|e| unreachable!("после drop лок берётся: {e}"));
        drop(held);
    }

    /// Разные каталоги output блокируют друг друга.
    #[test]
    fn output_lock_separates_different_outputs() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path();
        let a = OutputLock::try_acquire(root, Path::new("out-a"), "a")
            .unwrap_or_else(|_| unreachable!("out-a свободен"));
        let b = OutputLock::try_acquire(root, Path::new("out-b"), "b")
            .unwrap_or_else(|_| unreachable!("out-b не зависит от out-a"));
        drop((a, b));
    }

    /// Реентрантность: вложенный захват ТОГО ЖЕ ключа в том же потоке —
    /// no-op (Ok), а не ложный конфликт «занят (pid свой)».
    #[test]
    fn output_lock_is_reentrant_in_same_thread() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path();
        let outer = OutputLock::try_acquire(root, Path::new("audit"), "outer")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        // Вложенный захват того же ключа — конфликта нет.
        let inner = OutputLock::try_acquire(root, Path::new("audit"), "inner")
            .unwrap_or_else(|e| unreachable!("вложенный захват обязан быть no-op: {e}"));
        drop(inner);
        // После выхода из внешнего guard лок освобождён.
        drop(outer);
        assert!(
            OutputLock::try_acquire(root, Path::new("audit"), "after").is_ok(),
            "после drop внешнего guard лок свободен"
        );
    }

    /// Ключ вложенного no-op-guard не снимает ключ внешнего: пока держится
    /// внешний guard, другой поток всё ещё ЖДЁТ лок (и не получает его сразу).
    #[test]
    fn nested_noop_does_not_release_outer_lock() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path();
        let outer = OutputLock::try_acquire(root, Path::new("audit"), "outer")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        let inner = OutputLock::try_acquire(root, Path::new("audit"), "inner")
            .unwrap_or_else(|e| unreachable!("вложенный захват — no-op: {e}"));
        drop(inner);
        // Другой поток ждёт, пока внешний guard освободит лок.
        let root2 = root.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let res = OutputLock::try_acquire(&root2, Path::new("audit"), "other");
            let _ = tx.send(res.is_ok());
            res
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no-op-guard не должен отпускать внешний лок"
        );
        drop(outer);
        let got = waiter.join().unwrap_or_else(|_| unreachable!());
        let held = got.unwrap_or_else(|e| unreachable!("после drop внешнего guard лок свободен: {e}"));
        drop(held);
    }
}
