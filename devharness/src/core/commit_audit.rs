//! T-140: Инвариант `commit_sha` — commit из реестра обязан существовать в git
//! и принадлежать ветке агента (а не чужой агент, не main).
//!
//! `orch.rs` записывает `task.commit_sha = Some(git rev-parse HEAD)` после коммита.
//! Без проверки это можно подменить: злоумышленник или баг может записать SHA
//! из другой ветки или вообще несуществующий SHA. При верификации задачи
//! (verify/accept) харнесс будет использовать чужой commit как «доказательство».
//!
//! **Fix**: `core::commit_audit` проверяет два инварианта:
//! 1. `git cat-file -e <sha>` — SHA существует в git (object database).
//! 2. `git merge-base --contains <sha> <branch>` — SHA входит в ветку агента
//!    (является предком или совпадает с tip-ом ветки).
//!
//! Запуск git-команд — напрямую через `std::process::Command`, без shell
//! (см. `check_runner` T-112). SHA проходит через `arg`, а не через строку
//! команды — защита от shell-инъекции.

use std::path::Path;
use std::process::Command;

/// Результат аудита commit_sha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Всё ок: SHA существует и входит в ветку агента.
    Valid,
    /// SHA не существует в git (cat-file -e не нашёл объект).
    NotFound,
    /// SHA существует, но не входит в ветку агента.
    ForeignBranch,
    /// Ошибка выполнения git (не удалось запустить).
    Error(String),
}

/// Проверить, что commit существует в git (`git cat-file -e <sha>`).
///
/// `cat-file -e` возвращает exit code 0, если объект существует.
/// Принимает как полные 40-символьные SHA, так и сокращённые (git сам разрешает).
pub fn commit_exists(repo: &Path, sha: &str) -> bool {
    if sha.is_empty() {
        return false;
    }
    let result = Command::new("git")
        .arg("cat-file")
        .arg("-e")
        .arg(sha)
        .current_dir(repo)
        .status();
    match result {
        Ok(status) => status.success(),
        Err(e) => {
            // Git не запустился — логируем, но возвращаем false.
            let _ = e;
            false
        }
    }
}

/// Проверить, что commit входит в ветку
/// (`git merge-base --is-ancestor <sha> <branch>`).
///
/// `--is-ancestor` возвращает exit code 0, если `<sha>` является предком
/// (или совпадает с) tip-ом `<branch>`.
pub fn is_commit_in_branch(repo: &Path, sha: &str, branch: &str) -> bool {
    if sha.is_empty() || branch.is_empty() {
        return false;
    }
    let result = Command::new("git")
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(sha)
        .arg(branch)
        .current_dir(repo)
        .status();
    match result {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// Комбинированный аудит: SHA должен существовать И быть в ветке агента.
///
/// Возвращает детализированный результат для диагностики.
pub fn audit_commit(repo: &Path, sha: &str, branch: &str) -> AuditOutcome {
    if !commit_exists(repo, sha) {
        return AuditOutcome::NotFound;
    }
    if !is_commit_in_branch(repo, sha, branch) {
        return AuditOutcome::ForeignBranch;
    }
    AuditOutcome::Valid
}

/// Сократить SHA до 7 символов (как `git log` по умолчанию).
/// При длине < 7 возвращает как есть (не падает).
pub fn short_sha(sha: &str) -> String {
    if sha.len() <= 7 {
        sha.to_string()
    } else {
        sha[..7].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as ProcCommand;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Глобальный счётчик для уникальных имён веток в тестах.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Создать временный git-репо с одним коммитом и вернуть (path, sha, branch).
    fn make_repo() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let repo = dir.path();

        // Инициализируем репо.
        ProcCommand::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();

        // Настраиваем git.
        ProcCommand::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        ProcCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();

        let branch = format!("devh/agent-test-{n}");
        let _ = ProcCommand::new("git")
            .args(["checkout", "-b", &branch])
            .current_dir(repo)
            .output();

        // Создаём первый коммит.
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        ProcCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(repo)
            .output()
            .unwrap();
        ProcCommand::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Получаем SHA.
        let sha = ProcCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();

        (dir, sha, branch)
    }

    #[test]
    fn commit_exists_finds_real_commit() {
        let (dir, sha, _branch) = make_repo();
        assert!(commit_exists(dir.path(), &sha));
    }

    #[test]
    fn commit_exists_rejects_bogus_sha() {
        let (dir, _sha, _branch) = make_repo();
        assert!(!commit_exists(dir.path(), "deadbeefdeadbeef"));
    }

    #[test]
    fn commit_exists_rejects_empty_sha() {
        let (dir, _sha, _branch) = make_repo();
        assert!(!commit_exists(dir.path(), ""));
    }

    #[test]
    fn commit_exists_accepts_abbreviated_sha() {
        let (dir, sha, _branch) = make_repo();
        // 7-символьное сокращение — достаточно для однокоммитного репо.
        let short = &sha[..7];
        assert!(commit_exists(dir.path(), short));
    }

    #[test]
    fn is_commit_in_branch_true_for_own_commit() {
        let (dir, sha, branch) = make_repo();
        assert!(is_commit_in_branch(dir.path(), &sha, &branch));
    }

    #[test]
    fn is_commit_in_branch_false_for_foreign_ref() {
        let (dir, sha, branch) = make_repo();
        // Добавим второй коммит и создадем ветку от первого (sha = init commit).
        std::fs::write(dir.path().join("file2.txt"), "more\n").unwrap();
        ProcCommand::new("git")
            .args(["add", "file2.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        ProcCommand::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Ветка 'other' указывает на init-commit (sha), а не на tip.
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let other = format!("devh/agent-other-{n}");
        let _ = ProcCommand::new("git")
            .args(["branch", &other, &sha])
            .current_dir(dir.path())
            .output();
        // sha (init commit) — предок other? Да, он есть tip-ом other.
        // Но второй коммит НЕ является предком other.
        // Проверим: второй коммит не в ветке other.
        let second_sha = ProcCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let second_sha = String::from_utf8_lossy(&second_sha.stdout)
            .trim()
            .to_string();
        assert!(!is_commit_in_branch(dir.path(), &second_sha, &other));
    }

    #[test]
    fn audit_commit_valid_for_own_branch() {
        let (dir, sha, branch) = make_repo();
        let result = audit_commit(dir.path(), &sha, &branch);
        assert_eq!(result, AuditOutcome::Valid);
    }

    #[test]
    fn audit_commit_not_found_for_bogus_sha() {
        let (dir, _sha, branch) = make_repo();
        let result = audit_commit(dir.path(), "zzzfake12345", &branch);
        assert_eq!(result, AuditOutcome::NotFound);
    }

    #[test]
    fn audit_commit_foreign_branch_for_commit_outside_branch() {
        let (dir, sha, branch) = make_repo();
        // Добавим второй коммит на branch, затем создадим other от init-commit.
        std::fs::write(dir.path().join("file2.txt"), "more\n").unwrap();
        ProcCommand::new("git")
            .args(["add", "file2.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        ProcCommand::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let second_sha = ProcCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let second_sha = String::from_utf8_lossy(&second_sha.stdout)
            .trim()
            .to_string();

        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let other = format!("devh/agent-other-{n}");
        let _ = ProcCommand::new("git")
            .args(["branch", &other, &sha])
            .current_dir(dir.path())
            .output();

        // second_sha — не предок other (other указывает на init).
        let result = audit_commit(dir.path(), &second_sha, &other);
        assert_eq!(result, AuditOutcome::ForeignBranch);
    }

    #[test]
    fn short_sha_truncates_long_sha() {
        let long = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(short_sha(long), "0123456");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
    }

    #[test]
    fn is_commit_in_branch_false_for_empty_branch() {
        let (dir, sha, _branch) = make_repo();
        assert!(!is_commit_in_branch(dir.path(), &sha, ""));
    }
}
