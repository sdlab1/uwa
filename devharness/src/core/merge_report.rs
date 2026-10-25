//! Отчёт о слиянии: перечень перенесённых файлов и SHA.
//!
//! Строится из фактического списка перенесённых коммитов (git diff / show),
//! а не из текста задачи. Используется при автослиянии worktree агента.

use std::path::Path;
use std::process::Command;

use crate::commands::wtman::{transfer_commits, MergeAction};
use crate::core::identity::canonical;
use crate::shell::msg::{safe_name, SEP};
use crate::shell::git::short_head;

/// Информация о перенесённом коммите.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferredCommit {
    /// SHA коммита.
    pub sha: String,
    /// Тема коммита.
    pub subject: String,
    /// Список изменённых файлов в этом коммите.
    pub files: Vec<TransferredFile>,
}

/// Информация об изменённом файле.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferredFile {
    /// Путь к файлу.
    pub path: String,
    /// Тип изменения: added/modified/deleted/renamed.
    pub change_type: String,
}

/// Полный отчёт о слиянии.
#[derive(Debug, Clone)]
pub struct MergeReportDetail {
    /// Итоговое действие слияния.
    pub action: MergeAction,
    /// Сообщение для пользователя.
    pub message: String,
    /// SHA головного коммита после слияния.
    pub head_sha: Option<String>,
    /// Список перенесённых коммитов с файлами.
    pub commits: Vec<TransferredCommit>,
    /// Общее количество перенесённых коммитов.
    pub total_commits: usize,
    /// Общее количество изменённых файлов.
    pub total_files: usize,
}

impl MergeReportDetail {
    /// Сформировать текстовое письмо для агента.
    pub fn to_letter(&self, agent: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("=== ОТЧЁТ О СЛИЯНИИ ({}) ===\n", agent));
        out.push_str(&format!("Действие: {}\n", self.action.label()));
        out.push_str(&format!("{}\n\n", self.message));

        if let Some(sha) = &self.head_sha {
            out.push_str(&format!("HEAD: {sha}\n"));
        }

        if self.commits.is_empty() {
            out.push_str("Перенесённых коммитов нет.\n");
            return out;
        }

        out.push_str(&format!(
            "Перенесено коммитов: {}\n",
            self.total_commits
        ));
        out.push_str(&format!(
            "Изменено файлов: {}\n\n",
            self.total_files
        ));

        out.push_str("Детали по коммитам:\n");
        for commit in &self.commits {
            out.push_str(&format!(
                "  {} — {}\n",
                &commit.sha[..8.min(commit.sha.len())],
                commit.subject
            ));
            for file in &commit.files {
                out.push_str(&format!(
                    "    {}  {}\n",
                    file.change_type,
                    file.path
                ));
            }
        }

        out
    }
}

/// Получить список изменённых файлов в коммите.
fn get_commit_files(repo_root: &Path, sha: &str) -> Vec<TransferredFile> {
    let output = Command::new("git")
        .args(["show", "--name-status", "--format=", sha])
        .current_dir(repo_root)
        .output();

    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let change_type = parts[0].to_string();
            let path = parts[1..].join(" ");
            files.push(TransferredFile { path, change_type });
        }
    }

    files
}

/// Получить тему коммита.
fn get_commit_subject(repo_root: &Path, sha: &str) -> String {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%s", sha])
        .current_dir(repo_root)
        .output();

    let Ok(out) = output else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }

    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Построить детальный отчёт о слиянии.
///
/// Использует `transfer_commits` для получения списка коммитов, которые будут перенесены,
/// затем анализирует каждый коммит для получения списка файлов.
pub fn build_merge_report(
    main_root: &Path,
    main_branch: &str,
    branch: &str,
    agent: &str,
) -> anyhow::Result<MergeReportDetail> {
    let commits_sha = transfer_commits(main_root, main_branch, branch)?;
    let mut commits = Vec::new();
    let mut total_files = 0;

    for sha in &commits_sha {
        let subject = get_commit_subject(main_root, sha);
        let files = get_commit_files(main_root, sha);
        total_files += files.len();
        commits.push(TransferredCommit {
            sha: sha.clone(),
            subject,
            files,
        });
    }

    let (action, message, head_sha) = if commits_sha.is_empty() {
        (MergeAction::Nothing, format!("на {branch} нет коммитов сверх {main_branch} — вливать нечего"), None)
    } else {
        let sha = short_head(main_root);
        (MergeAction::Merged, format!("{branch} будет влит в {main_branch}"), Some(sha))
    };

    Ok(MergeReportDetail {
        action,
        message,
        head_sha,
        total_commits: commits.len(),
        total_files,
        commits,
    })
}

/// Получить SHA головного коммита ветки.
fn head_sha(repo_root: &Path, branch: &str) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", branch])
        .current_dir(repo_root)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("не удалось получить SHA для {branch}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Отправить отчёт о слиянии как письмо агенту.
pub fn send_merge_report(
    home: &std::path::Path,
    agent: &str,
    report: &MergeReportDetail,
) -> anyhow::Result<()> {
    let canon = canonical(agent);
    let letter = report.to_letter(&canon);
    let subject = format!("ОТЧЁТ О СЛИЯНИИ: {}", report.action.label());

    let from = "devh-merge";
    let to = vec![canon.clone()];
    let id = format!("merge-report-{}", uuid::Uuid::new_v4().simple().to_string()[..8].to_string());
    let stamp = chrono::Local::now().format("%H:%M").to_string();

    let block = format!(
        "[{stamp}] ОТ: {from} — КОМУ: {}\nID: {id}\nТРЕД: merge-report\nТЕМА: {subject}\nТЕЛО:\n{letter}\n",
        to.join(", ")
    );

    let inbox_path = home.join("inbox").join(format!("{}.log", safe_name(&canon)));
    if let Some(parent) = inbox_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&inbox_path, format!("{}{}\n", block, SEP)).ok();

    // Также в исходящие
    let outbox_path = home.join("outbox").join(format!("{}.log", safe_name(from)));
    if let Some(parent) = outbox_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&outbox_path, format!("{}{}\n", block, SEP)).ok();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::wtman::MergeAction;
    use std::fs;
    use tempfile::tempdir;

    fn init_git_repo(dir: &Path) {
        Command::new("git").args(["init", "-q"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(dir).output().unwrap();
        // Создаём initial commit на ветке main
        Command::new("git").args(["checkout", "-b", "main"]).current_dir(dir).output().unwrap();
    }

    #[test]
    fn build_merge_report_empty() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        fs::write(root.join("README.md"), "init\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(root).output().unwrap();
        Command::new("git").args(["commit", "-qm", "init"]).current_dir(root).output().unwrap();

        // Создаём ветку агента без коммитов
        Command::new("git").args(["branch", "devh/agent-test"]).current_dir(root).output().unwrap();

        let report = build_merge_report(root, "main", "devh/agent-test", "test").unwrap();
        assert_eq!(report.action, MergeAction::Nothing);
        assert_eq!(report.total_commits, 0);
        assert_eq!(report.total_files, 0);
    }

    #[test]
    fn build_merge_report_with_commits() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        fs::write(root.join("a.txt"), "1\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(root).output().unwrap();
        Command::new("git").args(["commit", "-qm", "base"]).current_dir(root).output().unwrap();

        // Ветка агента с коммитом - создаём её и переключаемся
        Command::new("git").args(["branch", "devh/agent-test"]).current_dir(root).output().unwrap();
        Command::new("git").args(["checkout", "devh/agent-test"]).current_dir(root).output().unwrap();
        fs::write(root.join("b.txt"), "2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(root).output().unwrap();
        Command::new("git").args(["commit", "-qm", "agent work"]).current_dir(root).output().unwrap();
        // Вернуться на main
        Command::new("git").args(["checkout", "main"]).current_dir(root).output().unwrap();

        let report = build_merge_report(root, "main", "devh/agent-test", "test").unwrap();
        assert_eq!(report.action, MergeAction::Merged);
        assert_eq!(report.total_commits, 1);
        assert_eq!(report.total_files, 1);
        assert_eq!(report.commits[0].files[0].path, "b.txt");
    }

    #[test]
    fn merge_report_letter_format() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_git_repo(root);
        fs::write(root.join("a.txt"), "1\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(root).output().unwrap();
        Command::new("git").args(["commit", "-qm", "base"]).current_dir(root).output().unwrap();
        Command::new("git").args(["branch", "devh/agent-test"]).current_dir(root).output().unwrap();
        Command::new("git").args(["checkout", "devh/agent-test"]).current_dir(root).output().unwrap();
        fs::write(root.join("b.txt"), "2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(root).output().unwrap();
        Command::new("git").args(["commit", "-qm", "agent work"]).current_dir(root).output().unwrap();
        Command::new("git").args(["checkout", "main"]).current_dir(root).output().unwrap();

        let report = build_merge_report(root, "main", "devh/agent-test", "test").unwrap();
        let letter = report.to_letter("test");
        assert!(letter.contains("ОТЧЁТ О СЛИЯНИИ"));
        assert!(letter.contains("влито"));
        assert!(letter.contains("b.txt"));
        assert!(letter.contains("agent work"));
    }
}