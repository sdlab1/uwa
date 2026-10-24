//! Детектор правок вне worktree агента (по git status общего дерева).

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeLevel {
    Warn,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escape {
    pub path: String,
    pub level: EscapeLevel,
    pub reason: String,
}

impl Escape {
    pub fn is_critical(&self) -> bool {
        matches!(self.level, EscapeLevel::Critical)
    }
}

/// Read-only: найти подозрительные изменения в общем дереве.
pub fn detect(root: &Path) -> Vec<Escape> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut res = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // porcelain: "XY path" or "?? path"
        let status = &line[..2.min(line.len())];
        let path = line[2..].trim().to_string();
        let level = if status == "??" {
            EscapeLevel::Warn
        } else {
            EscapeLevel::Critical
        };
        let reason = if status == "??" {
            "untracked-файл вне worktree".to_string()
        } else {
            "изменённый tracked-файл в общем дереве main".to_string()
        };
        res.push(Escape {
            path,
            level,
            reason,
        });
    }
    res
}

pub fn has_escape(root: &Path) -> bool {
    !detect(root).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn clean_tree_no_escape() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        fs::write(d.path().join("f.txt"), "x").unwrap();
        git(d.path(), &["add", "f.txt"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        assert!(detect(d.path()).is_empty());
    }

    #[test]
    fn modified_tracked_is_critical() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        fs::write(d.path().join("f.txt"), "x").unwrap();
        git(d.path(), &["add", "f.txt"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        fs::write(d.path().join("f.txt"), "y").unwrap();
        let r = detect(d.path());
        assert_eq!(r.len(), 1);
        assert!(r[0].is_critical());
        assert_eq!(r[0].path, "f.txt");
    }

    #[test]
    fn untracked_is_warn() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        fs::write(d.path().join("f.txt"), "x").unwrap();
        git(d.path(), &["add", "f.txt"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        fs::write(d.path().join("g.txt"), "y").unwrap();
        let r = detect(d.path());
        assert_eq!(r.len(), 1);
        assert!(!r[0].is_critical());
    }

    #[test]
    fn has_escape_true_when_changes() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        fs::write(d.path().join("f.txt"), "x").unwrap();
        git(d.path(), &["add", "f.txt"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        fs::write(d.path().join("f.txt"), "y").unwrap();
        assert!(has_escape(d.path()));
    }

    #[test]
    fn has_escape_false_clean() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        git(d.path(), &["init", "-q"]);
        git(d.path(), &["config", "user.email", "t@t"]);
        git(d.path(), &["config", "user.name", "t"]);
        fs::write(d.path().join("f.txt"), "x").unwrap();
        git(d.path(), &["add", "f.txt"]);
        git(d.path(), &["commit", "-q", "-m", "base"]);
        assert!(!has_escape(d.path()));
    }

    #[test]
    fn detect_non_git_returns_empty() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert!(detect(d.path()).is_empty());
    }
}