//! Управление worktree'ами агентов: изоляция, учёт, авто-выдача, sync с main.
//!
//! Канон (см. `devh workflow` §WORKTREE): каждый агент работает в СВОЁМ
//! worktree — `devh/agent-<адрес>` в `<wt_root>/devh-<slug>-<адрес>`.
//! Worktree выдаётся АВТОМАТИЧЕСКИ при `devh orch start`: агент не думает
//! о путях и не делает промежуточных запросов — путь печатается сразу.
//!
//! Подкоманды (`devh orch wt ...`):
//!
//! | подкоманда | что делает |
//! | --- | --- |
//! | `list` | учёт: агент → путь, ветка, HEAD, грязное?, ahead/behind main |
//! | `assign <агент>` | выдать worktree (создать, если нет) и напечатать путь |
//! | `path <агент>` | только путь (для скриптов) |
//! | `sync [<агент>\|--all]` | подтянуть изменения главного репо в worktree |
//! | `release <агент>` | освободить worktree (грязное — не трогаем) |
//! | `gc` | prune: снять осиротевшие регистрации и мёртвые каталоги |
//!
//! Sync никогда не теряет работу агента: грязное дерево → `SkippedDirty`,
//! конфликт rebase → откат (`rebase --abort`) и `Conflict`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::types::RunConfig;
use crate::shell::worktree::slug;

/// Ветка агента в его worktree.
pub fn agent_branch(agent: &str) -> String {
    format!("devh/agent-{}", sanitize(agent))
}

/// Канонический путь worktree агента (детерминированный: агент ↔ путь).
pub fn agent_path(cfg: &RunConfig, root: &Path, agent: &str) -> PathBuf {
    cfg.wt_root
        .join(format!("devh-{}-{}", slug(root), sanitize(agent)))
}

/// Имя каталога-префикса worktree'ов этого репозитория.
pub fn prefix(_cfg: &RunConfig, root: &Path) -> String {
    format!("devh-{}-", slug(root))
}

/// Канонизация имени агента для пути/ветки (только буквы, цифры, `-`, `_`).
fn sanitize(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        "agent".to_string()
    } else {
        s
    }
}

fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Зарегистрирован ли путь в `git worktree list`.
pub fn registered(root: &Path, path: &Path) -> bool {
    let want = path.to_string_lossy().to_string();
    git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|l| l.trim() == want)
}

/// Все зарегистрированные worktree'ы репозитория.
pub fn registered_paths(root: &Path) -> Vec<PathBuf> {
    git_out(root, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|l| PathBuf::from(l.trim()))
        .collect()
}

/// Состояние worktree агента.
#[derive(Debug, Clone, Default)]
pub struct WtStatus {
    /// Адрес агента-владельца.
    pub agent: String,
    /// Путь worktree.
    pub path: PathBuf,
    /// Ветка (None — detached/не выдан).
    pub branch: Option<String>,
    /// Короткий SHA HEAD.
    pub head: String,
    /// Есть незакоммиченные изменения.
    pub dirty: bool,
    /// Имена изменённых файлов (грязное дерево).
    pub dirty_files: Vec<String>,
    /// Коммитов впереди main.
    pub ahead: u32,
    /// Коммитов позади main.
    pub behind: u32,
    /// Каталог существует.
    pub exists: bool,
    /// Git знает про этот worktree.
    pub registered: bool,
}

/// Что сделал sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    /// Уже на уровне main.
    UpToDate,
    /// Подтянули fast-forward (своих коммитов не было).
    FastForward,
    /// Перебазировали свои коммиты поверх main.
    Rebased,
    /// Дерево грязное — не трогали.
    SkippedDirty,
    /// Конфликт rebase — откатили, ничего не потеряно.
    Conflict,
}

impl SyncAction {
    pub fn label(self) -> &'static str {
        match self {
            SyncAction::UpToDate => "актуален",
            SyncAction::FastForward => "fast-forward",
            SyncAction::Rebased => "rebase",
            SyncAction::SkippedDirty => "пропущен (грязное дерево)",
            SyncAction::Conflict => "конфликт (откачено)",
        }
    }
}

/// Итог sync.
#[derive(Debug, Clone)]
pub struct SyncReport {
    /// Что сделали.
    pub action: SyncAction,
    /// Человекочитаемое пояснение.
    pub message: String,
    /// Состояние worktree после sync.
    pub status: WtStatus,
}

/// База для сравнения: `origin/main`, если есть, иначе локальный `main`.
fn base_ref(root: &Path, main_branch: &str) -> String {
    if git_ok(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("origin/{main_branch}"),
        ],
    ) {
        format!("origin/{main_branch}")
    } else if git_ok(root, &["rev-parse", "--verify", "--quiet", main_branch]) {
        main_branch.to_string()
    } else {
        "HEAD".to_string()
    }
}

/// Статус worktree агента (без создания).
pub fn status(cfg: &RunConfig, root: &Path, agent: &str) -> WtStatus {
    let path = agent_path(cfg, root, agent);
    let exists = path.exists();
    let registered = registered(root, &path);
    let mut st = WtStatus {
        agent: agent.to_string(),
        path: path.clone(),
        exists,
        registered,
        ..Default::default()
    };
    if !exists {
        return st;
    }
    st.branch = {
        let b = git_out(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let b = b.trim().to_string();
        if b.is_empty() || b == "HEAD" {
            None
        } else {
            Some(b)
        }
    };
    st.head = git_out(&path, &["rev-parse", "--short", "HEAD"])
        .trim()
        .to_string();
    let porcelain = git_out(&path, &["status", "--porcelain"]);
    st.dirty_files = porcelain
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    st.dirty = !st.dirty_files.is_empty();
    let base = base_ref(root, &cfg.main_branch);
    st.ahead = count(&path, &format!("{base}..HEAD"));
    st.behind = count(&path, &format!("HEAD..{base}"));
    st
}

fn count(wt: &Path, range: &str) -> u32 {
    git_out(wt, &["rev-list", "--count", range])
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Выдать worktree агенту (создать, если нет). Идемпотентно.
pub fn ensure(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<(PathBuf, bool)> {
    let path = agent_path(cfg, root, agent);
    let branch = agent_branch(agent);

    if path.exists() && registered(root, &path) {
        return Ok((path, false));
    }
    if path.exists() {
        // Каталог есть, git его не знает: сначала чистим регистрации.
        let _ = git_ok(root, &["worktree", "prune"]);
        if registered(root, &path) {
            return Ok((path, false));
        }
        let empty = std::fs::read_dir(&path)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(&path);
        } else {
            anyhow::bail!(
                "worktree {} существует, но не зарегистрирован в git и не пуст — \
                 возможно, это чужая работа: разберись руками (или devh orch wt release {})",
                path.display(),
                agent
            );
        }
    }

    if git_ok(
        root,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    ) {
        git(root, &["worktree", "add", &path.to_string_lossy(), &branch])?;
    } else {
        let base = if git_ok(
            root,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", cfg.main_branch),
            ],
        ) {
            cfg.main_branch.clone()
        } else {
            "HEAD".to_string()
        };
        git(
            root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                &base,
            ],
        )?;
    }
    Ok((path, true))
}

/// Подтянуть изменения главного репо в worktree агента.
///
/// Ничего не теряет: грязное дерево → пропуск; конфликт rebase → откат.
pub fn sync(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<SyncReport> {
    let path = agent_path(cfg, root, agent);
    if !path.exists() {
        anyhow::bail!("worktree агента {agent} не выдан: devh orch wt assign {agent}");
    }
    let mut st = status(cfg, root, agent);
    if st.dirty {
        let files = st.dirty_files.join(", ");
        return Ok(SyncReport {
            action: SyncAction::SkippedDirty,
            message: format!(
                "дерево грязное ({}) — sync пропущен, работа агента не тронута",
                files
            ),
            status: st,
        });
    }
    // Обновить refs главного репо (если есть remote) — не чаще, чем сам sync.
    if git_ok(root, &["remote"]) {
        let _ = git_ok(root, &["fetch", "-q", "origin", &cfg.main_branch]);
    }
    let base = base_ref(root, &cfg.main_branch);
    st = status(cfg, root, agent);
    if st.behind == 0 {
        return Ok(SyncReport {
            action: SyncAction::UpToDate,
            message: format!("уже на уровне {base}"),
            status: st,
        });
    }
    if st.ahead == 0 {
        git(&path, &["merge", "--ff-only", &base])?;
        let after = status(cfg, root, agent);
        return Ok(SyncReport {
            action: SyncAction::FastForward,
            message: format!("подтянут {base} (fast-forward)"),
            status: after,
        });
    }
    // Есть свои коммиты — rebase поверх main; при конфликте откатываем.
    let out = Command::new("git")
        .args(["rebase", &base])
        .current_dir(&path)
        .output();
    let ok = out.as_ref().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        let _ = git_ok(&path, &["rebase", "--abort"]);
        let err = out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
            .unwrap_or_default();
        let after = status(cfg, root, agent);
        return Ok(SyncReport {
            action: SyncAction::Conflict,
            message: format!(
                "конфликт rebase на {base} — rebase откачен, изменения агента целы: {}",
                err.trim().lines().last().unwrap_or("").trim()
            ),
            status: after,
        });
    }
    let after = status(cfg, root, agent);
    Ok(SyncReport {
        action: SyncAction::Rebased,
        message: format!("свои коммиты перебазированы поверх {base}"),
        status: after,
    })
}

/// Освободить worktree агента. Грязное дерево НЕ трогаем — это работа агента.
pub fn release(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<String> {
    let path = agent_path(cfg, root, agent);
    if !path.exists() {
        return Ok(format!(
            "worktree агента {agent} не выдан — нечего освобождать"
        ));
    }
    let st = status(cfg, root, agent);
    if st.dirty {
        anyhow::bail!(
            "worktree {} грязный ({}): сначала закоммить или сохрани — \
             освобождать нельзя, работа не потеряется",
            path.display(),
            st.dirty_files.join(", ")
        );
    }
    let branch = st.branch.clone();
    if registered(root, &path) {
        git(
            root,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        )?;
    } else {
        let _ = std::fs::remove_dir_all(&path);
    }
    if let Some(b) = branch.as_ref() {
        if b.starts_with("devh/agent-") {
            let _ = git_ok(root, &["branch", "-D", b]);
        }
    }
    Ok(format!(
        "worktree агента {agent} освобождён: {}",
        path.display()
    ))
}

/// Уборка: prune регистраций + снос мёртвых каталогов worktree'ов этого репо.
///
/// `keep` — пути, которые нельзя трогать (выданные агентам).
pub fn gc(cfg: &RunConfig, root: &Path, keep: &[PathBuf]) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();
    let _ = git_ok(root, &["worktree", "prune"]);
    let registered = registered_paths(root);
    let prefix = prefix(cfg, root);
    let mut entries: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&cfg.wt_root) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with(&prefix) {
                entries.push(p);
            }
        }
    }
    for p in entries {
        if registered.iter().any(|r| r == &p) || keep.iter().any(|k| k == &p) {
            continue;
        }
        // Каталог с .git-файлом = worktree: сносим (его нет в git — работа не потеряна).
        let is_wt = p.join(".git").exists();
        if is_wt && std::fs::remove_dir_all(&p).is_ok() {
            report.push(format!("снят мёртвый worktree: {}", p.display()));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RunConfig {
        RunConfig {
            wt_root: PathBuf::from("/tmp/wtman-test"),
            main_branch: "main".to_string(),
            ..Default::default()
        }
    }

    /// Репозиторий с одним коммитом на `main`.
    fn repo_fixture() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let root = d.path().join("repo");
        std::fs::create_dir_all(&root).unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&root).output();
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        std::fs::write(root.join("a.txt"), "1\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"]);
        g(&["commit", "-qm", "base"]);
        (d, root)
    }

    #[test]
    fn branch_and_path_are_stable() {
        let c = cfg();
        let p1 = agent_path(&c, Path::new("/repo"), "beam");
        let p2 = agent_path(&c, Path::new("/repo"), "beam");
        assert_eq!(p1, p2);
        assert!(p1.to_string_lossy().ends_with("-beam"), "{p1:?}");
        assert_eq!(agent_branch("beam"), "devh/agent-beam");
        assert_ne!(
            agent_path(&c, Path::new("/repo"), "beam"),
            agent_path(&c, Path::new("/repo"), "beam2")
        );
    }

    #[test]
    fn ensure_creates_and_is_idempotent() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, created) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert!(created);
        assert!(p.exists());
        assert!(registered(&root, &p));
        let (p2, created2) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(p, p2);
        assert!(!created2, "повторный ensure не пересоздаёт");
    }

    #[test]
    fn status_reports_dirty_tree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "2\n").unwrap_or_else(|_| unreachable!());
        let st = status(&c, &root, "beam");
        assert!(st.dirty, "изменённый файл должен давать dirty");
        assert!(st.dirty_files.iter().any(|f| f.contains("a.txt")));
    }

    #[test]
    fn sync_fast_forwards_clean_worktree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        // Новый коммит в главном репо.
        std::fs::write(root.join("b.txt"), "2\n").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&root).output();
        };
        g(&["add", "-A"]);
        g(&["commit", "-qm", "second"]);
        let rep = sync(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(rep.action, SyncAction::FastForward, "{}", rep.message);
        assert!(
            p.join("b.txt").exists(),
            "изменения главного репо подтянуты"
        );
    }

    #[test]
    fn sync_skips_dirty_tree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "dirty\n").unwrap_or_else(|_| unreachable!());
        std::fs::write(root.join("b.txt"), "2\n").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&root).output();
        };
        g(&["add", "-A"]);
        g(&["commit", "-qm", "second"]);
        let rep = sync(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(rep.action, SyncAction::SkippedDirty);
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap_or_default(),
            "dirty\n",
            "работа агента не тронута"
        );
    }

    #[test]
    fn release_refuses_dirty_tree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "dirty\n").unwrap_or_else(|_| unreachable!());
        assert!(
            release(&c, &root, "beam").is_err(),
            "грязное дерево не освобождаем"
        );
        assert!(p.exists(), "каталог остался на месте");
    }

    #[test]
    fn release_removes_clean_worktree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        release(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert!(!p.exists(), "worktree снят");
        assert!(!registered(&root, &p));
    }

    #[test]
    fn gc_removes_orphan_directory() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        std::fs::create_dir_all(&c.wt_root).unwrap_or_else(|_| unreachable!());
        let orphan = agent_path(&c, &root, "ghost");
        std::fs::create_dir_all(&orphan).unwrap_or_else(|_| unreachable!());
        std::fs::write(orphan.join(".git"), "gitdir: /nowhere\n")
            .unwrap_or_else(|_| unreachable!());
        let report = gc(&c, &root, &[]).unwrap_or_else(|_| unreachable!());
        assert!(!report.is_empty(), "сирота снесена: {report:?}");
        assert!(!orphan.exists());
    }
}
