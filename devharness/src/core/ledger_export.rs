//! Экспорт ledger'ов оценки в ЕДИНЫЙ снимок — fail-closed.
//!
//! Оценка опирается на три append-only ledger'а:
//! * `quality.jsonl` — наблюдения о качестве работы агента;
//! * `model_ledger.jsonl` — сэмплы `cargo check` (ошибки/предупреждения с весами);
//! * `calibration.jsonl` — эталоны по уровням.
//!
//! Экспорт нужен, чтобы унести снимок оценки за пределы машины (аудит,
//! разбор «почему агент получил такой потолок»). При этом битая строка —
//! НЕ «пропустить и поехали»: это fail-closed с диагностикой (ledger →
//! снимок → эталон → рейтинг). Молча пропущенная строка тихо портит балл,
//! поэтому ошибка разбора — это ошибка экспорта.

use crate::core::autotune::CheckSample;
use crate::core::calibration::Reference;
use crate::core::quality::QualitySample;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Какой ledger экспортируем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerKind {
    /// `.harness/orch/quality.jsonl` — наблюдения о качестве.
    Quality,
    /// `.harness/orch/model_ledger.jsonl` — сэмплы `cargo check`.
    Model,
    /// `.harness/orch/calibration.jsonl` — эталоны.
    Calibration,
}

impl LedgerKind {
    /// Все три вида ledger'ов.
    pub fn all() -> [LedgerKind; 3] {
        [
            LedgerKind::Quality,
            LedgerKind::Model,
            LedgerKind::Calibration,
        ]
    }

    /// Короткое имя вида ledger'а.
    pub fn name(self) -> &'static str {
        match self {
            LedgerKind::Quality => "quality",
            LedgerKind::Model => "model_ledger",
            LedgerKind::Calibration => "calibration",
        }
    }

    /// Путь к ledger'у.
    pub fn path(self, root: &Path) -> PathBuf {
        match self {
            LedgerKind::Quality => crate::core::quality::ledger_path(root),
            LedgerKind::Model => crate::core::autotune::ledger_path(root),
            LedgerKind::Calibration => crate::core::calibration::ledger_path(root),
        }
    }
}

/// Битая строка ledger'а — экспорт невозможен (fail-closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLine {
    /// Какой ledger'ь сломан.
    pub kind: &'static str,
    /// Номер строки (1-based).
    pub line_no: usize,
    /// Причина (сообщение serde).
    pub reason: String,
}

impl fmt::Display for BrokenLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: битая строка {}: {}",
            self.kind, self.line_no, self.reason
        )
    }
}

impl std::error::Error for BrokenLine {}

/// Сводка по одному ledger'у.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerExport {
    /// Имя вида ledger'а.
    pub kind: &'static str,
    /// Откуда читали.
    pub source: PathBuf,
    /// Сколько записей разобрано.
    pub records: usize,
    /// Размер файла в байтах.
    pub bytes: u64,
}

impl LedgerExport {
    /// Строка для отчёта.
    pub fn summary(&self) -> String {
        format!(
            "{}: записей={} байт={} ({})",
            self.kind,
            self.records,
            self.bytes,
            self.source.display()
        )
    }
}

/// Единый снимок трёх ledger'ов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalSnapshot {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Наблюдения о качестве.
    pub quality: Vec<QualitySample>,
    /// Сэмплы `cargo check`.
    pub model_ledger: Vec<CheckSample>,
    /// Эталоны по уровням.
    pub calibration: Vec<Reference>,
}

impl EvalSnapshot {
    /// Сколько всего записей в снимке.
    pub fn total(&self) -> usize {
        self.quality.len() + self.model_ledger.len() + self.calibration.len()
    }
}

/// Строгий разбор ledger'а: каждая непустая строка — валидный JSON.
fn parse_lines<T: serde::de::DeserializeOwned>(
    kind: &'static str,
    path: &Path,
) -> Result<Vec<T>, BrokenLine> {
    let Ok(text) = fs::read_to_string(path) else {
        // Ledger'а нет — это не «битая строка», а пустой ledger.
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(t) {
            Ok(v) => out.push(v),
            Err(e) => {
                return Err(BrokenLine {
                    kind,
                    line_no: i + 1,
                    reason: e.to_string(),
                })
            }
        }
    }
    Ok(out)
}

/// Экспорт одного ledger'а (строгий, fail-closed).
pub fn export_kind(root: &Path, kind: LedgerKind) -> Result<LedgerExport, BrokenLine> {
    let path = kind.path(root);
    let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let records = match kind {
        LedgerKind::Quality => parse_lines::<QualitySample>(kind.name(), &path)?.len(),
        LedgerKind::Model => parse_lines::<CheckSample>(kind.name(), &path)?.len(),
        LedgerKind::Calibration => parse_lines::<Reference>(kind.name(), &path)?.len(),
    };
    Ok(LedgerExport {
        kind: kind.name(),
        source: path,
        records,
        bytes,
    })
}

/// Собрать единый снимок всех трёх ledger'ов (fail-closed).
pub fn snapshot(root: &Path) -> Result<EvalSnapshot, BrokenLine> {
    let quality =
        parse_lines::<QualitySample>(LedgerKind::Quality.name(), &LedgerKind::Quality.path(root))?;
    let model_ledger =
        parse_lines::<CheckSample>(LedgerKind::Model.name(), &LedgerKind::Model.path(root))?;
    let calibration = parse_lines::<Reference>(
        LedgerKind::Calibration.name(),
        &LedgerKind::Calibration.path(root),
    )?;
    Ok(EvalSnapshot {
        ts: now_secs(),
        quality,
        model_ledger,
        calibration,
    })
}

/// Экспорт всех трёх ledger'ов: сводки + единый снимок.
pub fn export_bundle(root: &Path) -> Result<(Vec<LedgerExport>, EvalSnapshot), BrokenLine> {
    let mut exports = Vec::new();
    for kind in LedgerKind::all() {
        exports.push(export_kind(root, kind)?);
    }
    let snap = snapshot(root)?;
    Ok((exports, snap))
}

/// Записать снимок в `.harness/orch/export/eval_snapshot.json`.
pub fn write_snapshot(root: &Path, snap: &EvalSnapshot) -> io::Result<PathBuf> {
    let dir = crate::commands::orch::dir(root).join("export");
    fs::create_dir_all(&dir)?;
    let path = dir.join("eval_snapshot.json");
    let text = serde_json::to_string_pretty(snap)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&path, text)?;
    Ok(path)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devh-ledger-export-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("создать tmp");
        dir
    }

    fn orch_dir(root: &Path) -> PathBuf {
        root.join(".harness").join("orch")
    }

    #[test]
    fn export_reads_three_ledgers() {
        let root = tmp_root("three");
        let dir = orch_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("quality.jsonl"),
            "{\"ts\":1,\"agent\":\"minimax\",\"model\":\"m\",\"task_id\":\"T-1\",\"c_level\":1,\"build_ok\":true}\n",
        )
        .unwrap();
        fs::write(
            dir.join("model_ledger.jsonl"),
            "{\"ts\":1,\"agent\":\"minimax\",\"model\":\"m\",\"runs\":1,\"errors\":0,\"warnings\":0,\"build_ok\":true}\n",
        )
        .unwrap();
        fs::write(dir.join("calibration.jsonl"), "").unwrap();

        let (exports, snap) = export_bundle(&root).expect("экспорт");
        assert_eq!(exports.len(), 3);
        assert_eq!(exports[0].records, 1);
        assert_eq!(exports[1].records, 1);
        assert_eq!(exports[2].records, 0);
        assert_eq!(snap.quality.len(), 1);
        assert_eq!(snap.model_ledger.len(), 1);
        assert_eq!(snap.total(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_line_is_fail_closed() {
        let root = tmp_root("broken");
        let dir = orch_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("quality.jsonl"),
            "{\"ts\":1,\"agent\":\"minimax\",\"model\":\"m\"}\n{НЕ JSON}\n",
        )
        .unwrap();
        let err = export_kind(&root, LedgerKind::Quality).expect_err("битая строка");
        assert_eq!(err.line_no, 2);
        assert_eq!(err.kind, "quality");
        assert!(err.to_string().contains("битая строка 2"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_ledger_is_empty_not_error() {
        let root = tmp_root("missing");
        let exp = export_kind(&root, LedgerKind::Model).expect("пустой ledger");
        assert_eq!(exp.records, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshot_is_written_to_export_dir() {
        let root = tmp_root("write");
        let snap = snapshot(&root).expect("снимок");
        let path = write_snapshot(&root, &snap).expect("запись снимка");
        assert!(path.exists());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("model_ledger"));
        let _ = fs::remove_dir_all(&root);
    }
}
