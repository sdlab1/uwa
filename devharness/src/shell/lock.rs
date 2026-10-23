use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

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
// Другой поток того же процесса конфликтует честно (через flock) —
// взятие задачи остаётся транзакционным.
thread_local! {
    static HELD: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
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
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("{e}"))?;
        if f.try_lock_exclusive().is_err() {
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.lines().next().map(str::to_string))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "неизвестный прогон".into());
            return Err(holder);
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
            HELD.with(|h| {
                h.borrow_mut().remove(&key);
            });
        }
        // file сбрасывается здесь -> flock освобождается ядром.
        self.file = None;
    }
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

    /// P5 R10: относительный и абсолютный путь output = ОДИН лок, holder —
    /// «pid N (label)», после смерти держателя лок свободен.
    ///
    /// ВАЖНО: конфликт проверяем из ДРУГОГО потока — в том же потоке
    /// захват того же ключа реентрантен (no-op), это лечит флейк
    /// «реестр занят (pid <свой>)».
    #[test]
    fn output_lock_relative_and_absolute_share_one_key() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path().to_path_buf();
        let g = OutputLock::try_acquire(root.as_path(), Path::new("audit"), "devh mutate start")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        // Тот же каталог, но абсолютным путём и с лишним `.`: тот же лок.
        let root2 = root.clone();
        let busy = std::thread::spawn(move || {
            OutputLock::try_acquire(&root2, &root2.join(".").join("audit"), "second")
        })
        .join()
        .unwrap_or_else(|_| unreachable!());
        let err = busy.err().unwrap_or_else(|| unreachable!("занят"));
        assert!(err.starts_with("pid "), "holder = {err}");
        assert!(err.contains("devh mutate start"), "holder = {err}");
        drop(g);
        assert!(
            OutputLock::try_acquire(&root, &root.join("audit"), "third").is_ok(),
            "мёртвый держатель не оставляет stale"
        );
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
    /// внешний guard, чужой поток всё ещё видит лок занятым.
    #[test]
    fn nested_noop_does_not_release_outer_lock() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path();
        let outer = OutputLock::try_acquire(root, Path::new("audit"), "outer")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        let inner = OutputLock::try_acquire(root, Path::new("audit"), "inner")
            .unwrap_or_else(|e| unreachable!("вложенный захват — no-op: {e}"));
        drop(inner);
        // Другой поток ( spawned ) честно конфликтует: лок занят.
        let root2 = root.to_path_buf();
        let busy = std::thread::spawn(move || {
            OutputLock::try_acquire(&root2, Path::new("audit"), "other")
        })
        .join()
        .unwrap_or_else(|_| unreachable!());
        assert!(busy.is_err(), "внешний guard держит лок: {:?}", busy.err());
        drop(outer);
    }
}
