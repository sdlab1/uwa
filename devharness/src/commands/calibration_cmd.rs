//! `devh orch calibrate` — построение эталона по замерам.
//!
//! Команда — «мост» между сбором замеров (их пишет verify в
//! `quality.jsonl`) и эталоном (`calibration.jsonl`):
//!
//! * `devh orch calibrate --c 3` — по уровню C3: печатает замеры, строит
//!   эталон и пишет его в `calibration.jsonl`;
//! * `devh orch calibrate` — по всем уровням сразу (C1..C4);
//! * `--agent NAME` — кто снимает эталон (по умолчанию ведущий);
//! * `--dry-run` — показать, что будет построено, ничего не писать;
//! * `--export` — единый снимок оценки (`ledger_export`): сводка по трём
//!   ledger'ам + `eval_snapshot.json` (унести снимок за пределы машины).
//!   Битая строка ledger'а — отказ экспорта (fail-closed, rc=2).
//!
//! Недостаток замеров — не «ошибка программы», а «среда не готова»
//! (код возврата 2): честно говорим, что эталон уровня ещё не собран.

use crate::commands::DevhError;
use crate::core::calibration;
use crate::core::calibration_guard;
use std::path::Path;

/// Агент по умолчанию (ведущий снимает эталон).
pub const DEFAULT_AGENT: &str = calibration_guard::DEFAULT_REFERENCE_AGENT;

/// Разобранные аргументы команды.
#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    /// Уровень сложности (None — все уровни).
    pub level: Option<u8>,
    /// Агент, снимающий эталон.
    pub agent: String,
    /// Только показать, ничего не писать.
    pub dry_run: bool,
    /// Экспорт единого снимка оценки вместо построения эталона.
    pub export: bool,
}

/// Разобрать аргументы `devh orch calibrate`.
pub fn parse_args(args: &[String]) -> anyhow::Result<Args> {
    let mut level: Option<u8> = None;
    let mut agent = DEFAULT_AGENT.to_string();
    let mut dry_run = false;
    let mut export = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--c" | "--level" => {
                let value = args.get(i + 1).ok_or_else(|| -> anyhow::Error {
                    DevhError::action("devh orch calibrate: после --c нужен уровень (0..10)").into()
                })?;
                let parsed: u8 = value.parse().map_err(|_| -> anyhow::Error {
                    DevhError::action(format!(
                        "devh orch calibrate: неверный уровень '{value}' (ожидается число 0..10)"
                    ))
                    .into()
                })?;
                if parsed > 10 {
                    return Err(
                        DevhError::action("devh orch calibrate: уровень вне шкалы 0..10").into(),
                    );
                }
                level = Some(parsed);
                i += 2;
            }
            "--agent" => {
                let value = args.get(i + 1).ok_or_else(|| -> anyhow::Error {
                    DevhError::action("devh orch calibrate: после --agent нужен адрес агента")
                        .into()
                })?;
                agent = value.clone();
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--export" => {
                export = true;
                i += 1;
            }
            other => {
                return Err(DevhError::action(format!(
                    "devh orch calibrate: неизвестный аргумент '{other}'"
                ))
                .into())
            }
        }
    }
    Ok(Args {
        level,
        agent,
        dry_run,
        export,
    })
}

/// Одна строка отчёта по уровню.
fn level_line(root: &Path, agent: &str, level: u8) -> (String, Option<calibration::Reference>) {
    let ms = calibration::measure_tasks(root, agent, Some(level));
    let required = calibration::MIN_REFERENCE_TASKS;
    let mut lines = vec![format!("C{}: замеров={}/{}", level, ms.len(), required)];
    for m in &ms {
        lines.push(format!(
            "  {}: score={:.4} err_w={:.2} warn_w={:.2} runs={} build_ok={}",
            m.task_id,
            m.score(),
            m.errors_weighted,
            m.warnings_weighted,
            m.check_runs,
            m.build_ok
        ));
    }
    match calibration::build_reference(root, agent, level) {
        Some(reference) => {
            lines.push(format!(
                "  эталон: score={:.4} build={:.2} err_w={:.2} warn_w={:.2} comment={:.3} runs={:.2} rework={:.2}",
                reference.metrics.score,
                reference.metrics.build_ok_ratio,
                reference.metrics.avg_errors_weighted,
                reference.metrics.avg_warnings_weighted,
                reference.metrics.avg_comment_ratio,
                reference.metrics.avg_runs,
                reference.metrics.avg_rework
            ));
            (lines.join("\n"), Some(reference))
        }
        None => {
            lines.push(format!("  эталон НЕ построен: нужно ≥ {required} замеров"));
            (lines.join("\n"), None)
        }
    }
}

/// Выполнить команду.
/// Экспорт единого снимка оценки (T-511): сводка ledger'ов + файл снимка.
fn export_snapshot(root: &Path) -> anyhow::Result<()> {
    let (exports, snap) = crate::core::ledger_export::export_bundle(root).map_err(|e| {
        anyhow::Error::new(DevhError::env(format!(
            "devh orch calibrate --export: снимок оценки не собран: {e}"
        )))
    })?;
    for e in exports.iter() {
        println!("  {}", e.summary());
    }
    println!("  снимок: записей={}", snap.total());
    let path = crate::core::ledger_export::write_snapshot(root, &snap).map_err(|e| {
        anyhow::Error::new(DevhError::env(format!(
            "devh orch calibrate --export: не записать снимок: {e}"
        )))
    })?;
    println!("  → снимок записан: {}", path.display());
    Ok(())
}

/// Выполнить команду.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let parsed = parse_args(args)?;
    // T-511: экспорт снимка — отдельный режим, эталон не строим и не пишем.
    if parsed.export {
        return export_snapshot(root);
    }
    let levels: Vec<u8> = match parsed.level {
        Some(level) => vec![level],
        None => calibration_guard::LEVELS.to_vec(),
    };

    let mut built = 0usize;
    let mut missing: Vec<u8> = Vec::new();
    for level in levels {
        let (text, reference) = level_line(root, &parsed.agent, level);
        println!("{text}");
        match reference {
            Some(reference) => {
                built += 1;
                if !parsed.dry_run {
                    calibration::append_reference(root, &reference)?;
                    println!("  → записан в {}", calibration::ledger_path(root).display());
                } else {
                    println!("  → dry-run: не записываем");
                }
            }
            None => missing.push(level),
        }
    }

    if parsed.level.is_some() && !missing.is_empty() {
        let level = missing[0];
        let err: anyhow::Error = DevhError::env(format!(
            "devh orch calibrate: для уровня C{level} не хватает замеров (нужно ≥ {}, есть {}) — эталон не собран",
            calibration::MIN_REFERENCE_TASKS,
            calibration::measure_tasks(root, &parsed.agent, Some(level)).len()
        ))
        .into();
        return Err(err);
    }

    println!(
        "итог: построено={} не собрано={}{}",
        built,
        missing.len(),
        if missing.is_empty() {
            String::new()
        } else {
            format!(
                " (уровни: {})",
                missing
                    .iter()
                    .map(|l| format!("C{l}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::quality::QualitySample;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devh-calibrate-cmd-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("создать tmp");
        dir
    }

    fn sample(task: &str, level: u8) -> QualitySample {
        QualitySample {
            ts: 1,
            agent: "minimax".to_string(),
            model: "test".to_string(),
            task_id: Some(task.to_string()),
            c_level: level,
            added_lines: 10,
            comment_lines: 2,
            code_lines: 8,
            check_runs: 1,
            errors_final: 0,
            warnings_final: 0,
            errors_weighted: 0.0,
            warnings_weighted: 0.0,
            error_categories: BTreeMap::new(),
            warning_categories: BTreeMap::new(),
            build_ok: true,
            duration_secs: Some(10.0),
            deliberation_actions: 3,
            rework_rounds: 0,
            identified: true,
        }
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_level_and_agent() {
        let a = parse_args(&args(&["--c", "3", "--agent", "beam"])).expect("args");
        assert_eq!(a.level, Some(3));
        assert_eq!(a.agent, "beam");
        assert!(!a.dry_run);
        let b = parse_args(&args(&[])).expect("args");
        assert_eq!(b.level, None);
        assert_eq!(b.agent, DEFAULT_AGENT);
        let c = parse_args(&args(&["--dry-run"])).expect("args");
        assert!(c.dry_run);
        assert!(!c.export);
    }

    #[test]
    fn parse_export_flag() {
        assert!(parse_args(&args(&["--export"])).expect("args").export);
        assert!(!parse_args(&args(&[])).expect("args").export);
    }

    #[test]
    fn export_writes_snapshot_file() {
        let root = tmp_root("export");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("T-{i}"), 1)).unwrap();
        }
        let reference = calibration::build_reference(&root, "minimax", 1).expect("эталон");
        calibration::append_reference(&root, &reference).unwrap();
        run(&root, &args(&["--export"])).expect("экспорт снимка");
        let snap_path = root
            .join(".harness")
            .join("orch")
            .join("export")
            .join("eval_snapshot.json");
        assert!(
            snap_path.exists(),
            "снимок записан: {}",
            snap_path.display()
        );
        let text = fs::read_to_string(&snap_path).unwrap_or_default();
        assert!(
            text.contains("\"quality\""),
            "снимок содержит quality: {text}"
        );
        assert!(
            text.contains("\"calibration\""),
            "снимок содержит calibration: {text}"
        );
        // Экспорт не строит и не добавляет эталоны.
        assert_eq!(calibration::load_references(&root).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_ledger_is_export_env_error() {
        let root = tmp_root("export-broken");
        // Битая строка quality.jsonl — экспорт невозможен (fail-closed).
        let dir = root.join(".harness").join("orch");
        fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        fs::write(dir.join("quality.jsonl"), "{это не json}\n").unwrap_or_else(|_| unreachable!());
        let err = run(&root, &args(&["--export"])).expect_err("битый ledger — отказ");
        let devh = err.downcast_ref::<DevhError>().expect("DevhError");
        assert_eq!(devh.code, crate::commands::ExitCode::EnvError);
        assert!(
            format!("{err}").contains("снимок оценки не собран"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_rejects_bad_level() {
        assert!(parse_args(&args(&["--c", "abc"])).is_err());
        assert!(parse_args(&args(&["--c"])).is_err());
        assert!(parse_args(&args(&["--c", "77"])).is_err());
        assert!(parse_args(&args(&["--wat"])).is_err());
    }

    #[test]
    fn missing_measurements_is_env_error() {
        let root = tmp_root("missing");
        crate::core::quality::append_sample(&root, &sample("T-1", 1)).unwrap();
        let err = run(&root, &args(&["--c", "1"])).expect_err("не хватает замеров");
        let devh = err.downcast_ref::<DevhError>().expect("DevhError");
        assert_eq!(devh.code, crate::commands::ExitCode::EnvError);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_and_writes_reference() {
        let root = tmp_root("build");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("T-{i}"), 1)).unwrap();
        }
        run(&root, &args(&["--c", "1"])).expect("эталон построен");
        let refs = calibration::load_references(&root);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].c_level, 1);
        assert_eq!(refs[0].metrics.tasks, 4);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_does_not_write() {
        let root = tmp_root("dry");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("T-{i}"), 1)).unwrap();
        }
        run(&root, &args(&["--c", "1", "--dry-run"])).expect("dry-run");
        assert!(calibration::load_references(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
