//! C3-СПЛИТ T-199: mutation hardening — запрет --in-place и ручных правок артефактов аудита.
//!
//! Guard отклоняет конфиг мутации с --in-place (мутирует дерево агента) и запрет
//! ручной правки артефактов аудита.

use std::path::Path;

/// Ошибки проверки hardening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardeningError {
    InPlaceForbidden,
    ManualArtifactEditForbidden(String),
}

/// Проверить конфиг мутации на наличие запрещённых флагов.
///
/// Возвращает `Err(HardeningError::InPlaceForbidden)` если найден `--in-place`.
pub fn check_mutation_config(args: &[String]) -> Result<(), HardeningError> {
    for arg in args {
        if arg == "--in-place" {
            return Err(HardeningError::InPlaceForbidden);
        }
    }
    Ok(())
}

/// Проверить, что артефакт аудита не редактировался вручную.
///
/// Артефакты аудита: `mutants.out`, `iterator.json`, `mutants.log` в директории аудита.
/// Проверка по mtime: если mtime артефакта новее, чем последняя запись в ledger,
/// считаем что файл правили вручную.
pub fn check_audit_artifact_not_manual(
    artifact_path: &Path,
    ledger_path: &Path,
) -> Result<(), HardeningError> {
    let artifact_meta = std::fs::metadata(artifact_path);
    let ledger_meta = std::fs::metadata(ledger_path);

    if let (Ok(a), Ok(l)) = (artifact_meta, ledger_meta) {
        if let (Ok(a_time), Ok(l_time)) = (a.modified(), l.modified()) {
            if a_time > l_time {
                return Err(HardeningError::ManualArtifactEditForbidden(
                    artifact_path.display().to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Проверить все артефакты в директории аудита.
pub fn check_audit_directory(audit_dir: &Path, ledger_path: &Path) -> Vec<HardeningError> {
    let mut errors = Vec::new();
    let artifacts = ["mutants.out", "iterator.json", "mutants.log"];

    for name in artifacts {
        let path = audit_dir.join(name);
        if path.exists() {
            if let Err(e) = check_audit_artifact_not_manual(&path, ledger_path) {
                errors.push(e);
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn in_place_config_rejected() {
        let args = vec!["cargo".to_string(), "mutants".to_string(), "--in-place".to_string()];
        let err = check_mutation_config(&args).unwrap_err();
        assert_eq!(err, HardeningError::InPlaceForbidden);
    }

    #[test]
    fn normal_config_accepted() {
        let args = vec!["cargo".to_string(), "mutants".to_string(), "--json".to_string()];
        assert!(check_mutation_config(&args).is_ok());
    }

    #[test]
    fn empty_config_accepted() {
        let args = vec![];
        assert!(check_mutation_config(&args).is_ok());
    }

    #[test]
    fn in_place_in_middle_rejected() {
        let args = vec!["cargo".to_string(), "--in-place".to_string(), "mutants".to_string()];
        let err = check_mutation_config(&args).unwrap_err();
        assert_eq!(err, HardeningError::InPlaceForbidden);
    }

    #[test]
    fn audit_artifact_not_manual_ok() {
        let tmp = TempDir::new().unwrap();
        let ledger = tmp.path().join("ledger.jsonl");
        let artifact = tmp.path().join("mutants.out");

        fs::write(&ledger, "{}").unwrap();
        fs::write(&artifact, "content").unwrap();

        // Artifact older than ledger (just written, so roughly same time or ledger newer)
        // We just test it doesn't error for normal case
        assert!(check_audit_artifact_not_manual(&artifact, &ledger).is_ok());
    }

    #[test]
    fn audit_directory_check_finds_manual_edits() {
        let tmp = TempDir::new().unwrap();
        let ledger = tmp.path().join("ledger.jsonl");
        let artifact = tmp.path().join("mutants.out");

        fs::write(&ledger, "{}").unwrap();
        fs::write(&artifact, "content").unwrap();

        // Wait a bit and "edit" the artifact
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&artifact, "manual edit").unwrap();

        let errors = check_audit_directory(tmp.path(), &ledger);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            HardeningError::ManualArtifactEditForbidden(_)
        ));
    }

    #[test]
    fn audit_directory_ignores_missing_files() {
        let tmp = TempDir::new().unwrap();
        let ledger = tmp.path().join("ledger.jsonl");
        fs::write(&ledger, "{}").unwrap();

        let errors = check_audit_directory(tmp.path(), &ledger);
        assert!(errors.is_empty());
    }
}