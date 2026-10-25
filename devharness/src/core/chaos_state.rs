//! Детерминированная инъекция повреждения состояния + проверка инвариантов после восстановления.
//!
//! Используется в `chaos.sh` (A7) для симуляции `kill -9` в случайной точке и
//! верификации, что после `abort + run` состояние восстанавливается чисто.
//!
//! Инварианты (как в `chaos.sh::check_invariants`):
//! 1. status парсится (open/active/done) — atomic_write без torn-файлов;
//! 2. fold(events.log) == status — A3-replay под расстрелом;
//! 3. нет .harness/**/*.tmp — осиротевшие атомарные записи;
//! 4. в worktree нет мутационных маркеров — урок ae9f619;
//! 5. run.pid отсутствует или процесс жив; stale = НАХОДКА (лог, не fail).

use std::fs;
use std::path::{Path, PathBuf};

/// Вид инъекции повреждения (детерминированно по сиду).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorruptionKind {
    /// Обрезать status-файл (torn write).
    TruncateStatus,
    /// Записать мусор в events.log.
    GarbageEventsLog,
    /// Оставить .tmp файл в .harness/.
    LeaveTmpFile,
    /// Поместить мутационный маркер в worktree.
    MutateMarkerInWorktree,
    /// Поставить stale run.pid (несуществующий процесс).
    StaleRunPid,
    /// Не повреждать (контрольная итерация).
    None,
}

impl CorruptionKind {
    /// Выбрать вид по сиду (детерминированно, 0..=4 → 5 видов).
    pub fn from_seed(seed: u64, iteration: u64) -> Self {
        let idx = (seed.wrapping_add(iteration * 7)) % 5;
        match idx {
            0 => CorruptionKind::TruncateStatus,
            1 => CorruptionKind::GarbageEventsLog,
            2 => CorruptionKind::LeaveTmpFile,
            3 => CorruptionKind::MutateMarkerInWorktree,
            4 => CorruptionKind::StaleRunPid,
            _ => CorruptionKind::None,
        }
    }
}

/// Результат проверки инвариантов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantReport {
    /// Все инварианты выполнены (bad == 0).
    pub ok: bool,
    /// Список нарушений (пусто если ok).
    pub violations: Vec<String>,
    /// Предупреждения (не блокируют ok).
    pub warnings: Vec<String>,
}

impl InvariantReport {
    pub fn new() -> Self {
        Self {
            ok: true,
            violations: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn add_violation(&mut self, msg: String) {
        self.ok = false;
        self.violations.push(msg);
    }

    pub fn add_warning(&mut self, msg: String) {
        self.warnings.push(msg);
    }
}

/// Проверить инварианты состояния item в worktree.
///
/// Возвращает отчёт: `ok=true` если нарушений нет.
/// `warnings` содержит не-блокирующие находки (например, stale run.pid).
pub fn check_invariants(item_dir: &Path, worktree: &Path) -> InvariantReport {
    let mut report = InvariantReport::new();

    // 1. status парсится (open/active/done) — atomic_write без torn-файлов.
    let status_path = item_dir.join("status");
    let status = fs::read_to_string(&status_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    match status.as_str() {
        "open" | "active" | "done" => {}
        _ if status.is_empty() => {
            report.add_violation("status файл пуст или отсутствует — torn write?".to_string());
        }
        _ => {
            report.add_violation(format!("status не парсится ('{status}') — torn-файл?"));
        }
    }

    // 2. fold(events.log) == status — A3-replay.
    let events_path = item_dir.join("events.log");
    let folded = fold_events_log(&events_path).unwrap_or_else(|| "open".to_string());
    if folded != status {
        report.add_violation(format!(
            "events.log replay -> {folded}, статус-файл {status} — рассинхрон"
        ));
    }

    // 3. нет .harness/**/*.tmp — осиротевшие атомарные записи.
    // Ищем в .harness/ (store root) рекурсивно, как в chaos.sh.
    let harness_root = item_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(item_dir);
    fn find_tmp_files(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    find_tmp_files(&path, out);
                } else if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".tmp"))
                    .unwrap_or(false)
                {
                    out.push(path);
                }
            }
        }
    }
    let mut tmp_files = Vec::new();
    find_tmp_files(harness_root, &mut tmp_files);
    for path in tmp_files {
        report.add_violation(format!("осиротевшее атомарное запись: {}", path.display()));
    }

    // 4. в worktree нет мутационных маркеров — урок ae9f619.
    const MUTANT_MARKER: &str = "changed by cargo-mutants";
    if let Ok(output) = std::process::Command::new("grep")
        .args(["-rl", MUTANT_MARKER, worktree.to_str().unwrap_or("")])
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            let paths = String::from_utf8_lossy(&output.stdout);
            report.add_violation(format!(
                "мутационный маркер в worktree ({MUTANT_MARKER}): {paths}"
            ));
        }
    }

    // 5. run.pid отсутствует или процесс жив; stale = предупреждение (не fail).
    let pid_path = item_dir.join("run.pid");
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        let pid_str = pid_str.trim();
        if !pid_str.is_empty() {
            if let Ok(pid) = pid_str.parse::<i32>() {
                // Проверить, жив ли процесс (kill(pid, 0)).
                let alive = unsafe { libc::kill(pid, 0) == 0 };
                if !alive {
                    report.add_warning(format!(
                        "stale run.pid {pid} — процесс мёртв (находка для doctor)"
                    ));
                }
            } else {
                report.add_warning(format!("run.pid содержит не-число: {pid_str}"));
            }
        }
    }

    report
}

/// Свернуть events.log по правилам A3:
/// open --claimed--> active --merged--> done;
/// active --aborted--> open; мусорные строки пропускаем.
fn fold_events_log(events_path: &Path) -> Option<String> {
    let text = fs::read_to_string(events_path).ok()?;
    let mut state = 0u8; // 0=open, 1=active, 2=done
    for line in text.lines() {
        let ev = line.split('|').last()?.trim();
        match ev {
            "claimed" if state == 0 => state = 1,
            "merged" if state == 1 => state = 2,
            "aborted" if state == 1 => state = 0,
            _ => {}
        }
    }
    Some(
        match state {
            2 => "done",
            1 => "active",
            _ => "open",
        }
        .to_string(),
    )
}

/// Инъецировать повреждение в item_dir/worktree по виду.
///
/// Возвращает `Ok(())` если инъекция выполнена, `Err` если item_dir не существует.
pub fn inject_corruption(
    item_dir: &Path,
    worktree: &Path,
    kind: CorruptionKind,
) -> anyhow::Result<()> {
    match kind {
        CorruptionKind::TruncateStatus => {
            let status_path = item_dir.join("status");
            if status_path.exists() {
                // Обрезать до пустого или мусорной строки.
                fs::write(&status_path, "torn")?;
            }
        }
        CorruptionKind::GarbageEventsLog => {
            let events_path = item_dir.join("events.log");
            fs::write(&events_path, "garbage\ncorrupted line\nnot|a|valid|event\n")?;
        }
        CorruptionKind::LeaveTmpFile => {
            let tmp_path = item_dir.join("corrupt.tmp");
            fs::write(&tmp_path, "orphaned atomic write")?;
        }
        CorruptionKind::MutateMarkerInWorktree => {
            let marker_file = worktree.join("src").join("corrupted.rs");
            fs::create_dir_all(marker_file.parent().unwrap())?;
            fs::write(&marker_file, "// changed by cargo-mutants\n")?;
        }
        CorruptionKind::StaleRunPid => {
            let pid_path = item_dir.join("run.pid");
            // PID 999999 — гарантированно не существует.
            fs::write(&pid_path, "999999")?;
        }
        CorruptionKind::None => {}
    }
    Ok(())
}

/// Восстановить состояние после повреждения (simulate abort + run).
///
/// Удаляет мусорные файлы, чистит маркеры, сбрасывает status в open.
pub fn recover_state(item_dir: &Path, worktree: &Path) -> anyhow::Result<()> {
    // Удалить .tmp файлы в .harness/ (рекурсивно) и в item_dir.
    let harness_root = item_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(item_dir);
    fn remove_tmp_files(dir: &Path) {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    remove_tmp_files(&path);
                } else if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".tmp"))
                    .unwrap_or(false)
                {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
    remove_tmp_files(harness_root);
    // Также проверить сам item_dir.
    if let Ok(entries) = fs::read_dir(item_dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().ends_with(".tmp") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    // Удалить мутационные маркеры в worktree (src/corrupted.rs).
    const MUTANT_MARKER: &str = "changed by cargo-mutants";
    if let Ok(output) = std::process::Command::new("grep")
        .args(["-rl", MUTANT_MARKER, worktree.to_str().unwrap_or("")])
        .output()
    {
        if output.status.success() {
            let paths = String::from_utf8_lossy(&output.stdout);
            for path in paths.lines() {
                let _ = fs::remove_file(path);
            }
        }
    }
    // Сбросить status в open (abort делает это).
    let status_path = item_dir.join("status");
    fs::write(&status_path, "open")?;
    // Дописать aborted в events.log.
    let events_path = item_dir.join("events.log");
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)?;
    writeln!(file, "aborted")?;
    // Удалить stale run.pid.
    let pid_path = item_dir.join("run.pid");
    if pid_path.exists() {
        fs::remove_file(&pid_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_item_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempdir().unwrap();
        let item_dir = tmp.path().join(".harness").join("items").join("W-001");
        fs::create_dir_all(&item_dir).unwrap();
        let worktree = tmp.path().join("wt");
        fs::create_dir_all(&worktree).unwrap();
        // Инициализировать чистое состояние.
        fs::write(item_dir.join("status"), "active").unwrap();
        fs::write(item_dir.join("events.log"), "claimed\n").unwrap();
        fs::write(item_dir.join("run.pid"), std::process::id().to_string()).unwrap();
        (tmp, item_dir, worktree)
    }

    #[test]
    fn clean_state_passes_invariants() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        let report = check_invariants(&item_dir, &worktree);
        assert!(report.ok, "clean state: violations={:?}", report.violations);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn truncate_status_detected() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::TruncateStatus).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        assert!(!report.ok);
        assert!(report.violations.iter().any(|v| v.contains("torn")));
    }

    #[test]
    fn garbage_events_log_detected() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::GarbageEventsLog).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        assert!(!report.ok);
        assert!(report.violations.iter().any(|v| v.contains("рассинхрон")));
    }

    #[test]
    fn tmp_file_detected() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::LeaveTmpFile).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        assert!(!report.ok);
        assert!(report.violations.iter().any(|v| v.contains("атомарное")));
    }

    #[test]
    fn mutate_marker_in_worktree_detected() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::MutateMarkerInWorktree).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        assert!(!report.ok);
        assert!(report
            .violations
            .iter()
            .any(|v| v.contains("мутационный маркер")));
    }

    #[test]
    fn stale_run_pid_is_warning_not_fail() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::StaleRunPid).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        // stale run.pid — это предупреждение, не нарушение (ok должно остаться true).
        assert!(report.ok, "stale run.pid должен быть warning, не violation");
        assert!(report.warnings.iter().any(|w| w.contains("stale run.pid")));
    }

    #[test]
    fn recovery_restores_clean_state() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        // Инъецируем все повреждения сразу.
        inject_corruption(&item_dir, &worktree, CorruptionKind::TruncateStatus).unwrap();
        inject_corruption(&item_dir, &worktree, CorruptionKind::GarbageEventsLog).unwrap();
        inject_corruption(&item_dir, &worktree, CorruptionKind::LeaveTmpFile).unwrap();
        inject_corruption(&item_dir, &worktree, CorruptionKind::MutateMarkerInWorktree).unwrap();
        inject_corruption(&item_dir, &worktree, CorruptionKind::StaleRunPid).unwrap();

        // Восстановить.
        recover_state(&item_dir, &worktree).unwrap();

        // Проверить, что всё чисто.
        let report = check_invariants(&item_dir, &worktree);
        assert!(
            report.ok,
            "after recovery: violations={:?}",
            report.violations
        );
        assert!(report.warnings.is_empty());
        // status должен быть open.
        let status = fs::read_to_string(item_dir.join("status")).unwrap();
        assert_eq!(status.trim(), "open");
        // events.log должен содержать aborted.
        let events = fs::read_to_string(item_dir.join("events.log")).unwrap();
        assert!(events.contains("aborted"));
    }

    #[test]
    fn fold_events_log_correctly_replays() {
        let (_tmp, item_dir, _worktree) = setup_item_dir();
        // claimed -> active
        assert_eq!(
            fold_events_log(&item_dir.join("events.log")).unwrap(),
            "active"
        );

        // claimed, merged -> done
        fs::write(item_dir.join("events.log"), "claimed\nmerged\n").unwrap();
        assert_eq!(
            fold_events_log(&item_dir.join("events.log")).unwrap(),
            "done"
        );

        // claimed, aborted -> open
        fs::write(item_dir.join("events.log"), "claimed\naborted\n").unwrap();
        assert_eq!(
            fold_events_log(&item_dir.join("events.log")).unwrap(),
            "open"
        );

        // мусорные строки игнорируются; aborted в состоянии done не срабатывает (как в bash awk)
        fs::write(
            item_dir.join("events.log"),
            "claimed\nmerged\nnot|valid\naborted\n",
        )
        .unwrap();
        assert_eq!(
            fold_events_log(&item_dir.join("events.log")).unwrap(),
            "done"
        );
    }

    #[test]
    fn corruption_kind_from_seed_deterministic() {
        // Один и тот же seed + iteration = тот же CorruptionKind.
        let k1 = CorruptionKind::from_seed(42, 1);
        let k2 = CorruptionKind::from_seed(42, 1);
        assert_eq!(k1, k2);

        // Разные iteration = разные виды (цикл по 5).
        let kinds: Vec<_> = (1..=5).map(|i| CorruptionKind::from_seed(100, i)).collect();
        assert_eq!(kinds.len(), 5);
        // Все 5 видов должны встретиться ровно по разу (перестановка).
        let mut sorted = kinds.clone();
        sorted.sort_by_key(|k| format!("{k:?}"));
        let expected = vec![
            CorruptionKind::GarbageEventsLog,
            CorruptionKind::LeaveTmpFile,
            CorruptionKind::MutateMarkerInWorktree,
            CorruptionKind::StaleRunPid,
            CorruptionKind::TruncateStatus,
        ];
        assert_eq!(sorted, expected);
    }

    #[test]
    fn inject_corruption_none_does_nothing() {
        let (_tmp, item_dir, worktree) = setup_item_dir();
        inject_corruption(&item_dir, &worktree, CorruptionKind::None).unwrap();
        let report = check_invariants(&item_dir, &worktree);
        assert!(report.ok);
    }
}
