use std::path::{Path, PathBuf};
use std::process::Command;

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

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

pub fn root_from_cwd() -> anyhow::Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    anyhow::ensure!(out.status.success(), "devh: не git-репозиторий");
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

pub fn has_remote(root: &Path) -> bool {
    !git_out(root, &["remote"]).trim().is_empty()
}

pub fn branch_current(root: &Path) -> Option<String> {
    let s = git_out(root, &["branch", "--show-current"]);
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn show_ref(root: &Path, branch: &str) -> bool {
    git_ok(
        root,
        &[
            "show-ref",
            "-q",
            "--verify",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub fn worktree_add_new(root: &Path, branch: &str, path: &Path) -> anyhow::Result<()> {
    git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &path.to_string_lossy(),
            "HEAD",
        ],
    )
    .map(|_| ())
}

pub fn worktree_add_existing(root: &Path, branch: &str, path: &Path) -> anyhow::Result<()> {
    git(root, &["worktree", "add", &path.to_string_lossy(), branch]).map(|_| ())
}

pub fn worktree_remove(root: &Path, path: &Path) -> anyhow::Result<()> {
    git(
        root,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )
    .map(|_| ())
}

pub fn branch_delete(root: &Path, branch: &str) -> anyhow::Result<()> {
    git(root, &["branch", "-D", branch]).map(|_| ())
}

/// Сбросить reuse-worktree на ветку задачи `branch`, сохранив `target/`.
/// Аварийные состояния прошлой задачи (краш посреди merge/rebase/cherry-pick)
/// снимаем молча — в общем пути это чужой мусор. Ветку задачи могло не быть
/// (reuse-путь помнит чужую) — тогда создаём от `base` (обычно main).
/// `clean -fd` БЕЗ `-x` выкидывает untracked-неигнорируемое, но цельно
/// оставляет gitignored `target/` — ради его тепла весь reuse и затевается.
pub fn reuse_checkout(wt: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    for st in ["merge", "rebase", "cherry-pick"] {
        let _ = git(wt, &[st, "--abort"]);
    }
    if git_ok(
        wt,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    ) {
        git(wt, &["checkout", "-f", branch])?;
    } else {
        git(wt, &["checkout", "-b", branch, base])?;
    }
    git(wt, &["reset", "--hard"])?;
    let _ = git(wt, &["clean", "-fd"]);
    Ok(())
}

pub fn add_all(wt: &Path) -> anyhow::Result<()> {
    git(wt, &["add", "-A"]).map(|_| ())
}

pub fn has_staged(wt: &Path) -> bool {
    !git_ok(wt, &["diff", "--cached", "--quiet"])
}

pub fn staged_stat(wt: &Path) -> String {
    git_out(wt, &["diff", "--cached", "--stat"])
}

pub fn staged_deletions(wt: &Path) -> usize {
    git_out(wt, &["diff", "--cached", "--diff-filter=D", "--name-only"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

pub fn commit(wt: &Path, msg: &str) -> anyhow::Result<()> {
    git(wt, &["commit", "-q", "-m", msg]).map(|_| ())
}

pub fn push(wt: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    git(wt, &["push", "-q", "-u", remote, branch]).map(|_| ())
}

pub fn push_main(wt: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
    git(wt, &["push", "-q", remote, &format!("HEAD:{main}")]).map(|_| ())
}

pub fn push_delete(wt: &Path, remote: &str, branch: &str) -> bool {
    git_ok(wt, &["push", "-q", remote, "--delete", branch])
}

pub fn fetch(root: &Path, remote: &str, main: &str) -> anyhow::Result<()> {
    git(root, &["fetch", "-q", remote, main]).map(|_| ())
}

pub fn rebase(wt: &Path, onto: &str) -> anyhow::Result<()> {
    git(wt, &["rebase", onto]).map(|_| ())
}

pub fn fetch_dot_branch(root: &Path, branch: &str, main: &str) -> bool {
    git_ok(root, &["fetch", "-q", ".", &format!("{branch}:{main}")])
}

pub fn merge_ff_only(root: &Path, branch: &str) -> bool {
    git_ok(root, &["merge", "-q", "--ff-only", branch])
}

pub fn rev_list_count(wt: &Path, main: &str) -> anyhow::Result<u32> {
    let s = git(wt, &["rev-list", "--count", &format!("{main}..HEAD")])?;
    Ok(s.trim().parse().unwrap_or(0))
}

pub fn tree_dirty(wt: &Path) -> bool {
    !git_out(wt, &["status", "--porcelain"]).trim().is_empty()
}

pub fn diff_names(wt: &Path) -> Vec<String> {
    let out = git_out(wt, &["diff", "--name-only", "HEAD"]);
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect()
}

pub fn status_names(wt: &Path) -> Vec<String> {
    git_out(wt, &["status", "--porcelain"])
        .lines()
        .filter_map(|l| {
            let name = l.get(3..)?.trim().to_string();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

pub fn changed_files(wt: &Path) -> Vec<String> {
    let mut all = diff_names(wt);
    all.extend(status_names(wt));
    all.sort();
    all.dedup();
    all
}

fn names_scoped(wt: &Path, kind: &str, scope: &str) -> Vec<String> {
    let mut args: Vec<String> = if kind == "diff" {
        vec!["diff".into(), "--name-only".into(), "HEAD".into()]
    } else {
        vec!["status".into(), "--porcelain".into()]
    };
    args.push("--".into());
    for s in scope.split_whitespace() {
        args.push(s.to_string());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = git_out(wt, &refs);
    out.lines()
        .filter_map(|l| {
            let name = if kind == "diff" {
                l.trim().to_string()
            } else {
                l.get(3..)?.trim().to_string()
            };
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

pub fn changed_files_scoped(wt: &Path, scope: &str) -> Vec<String> {
    let mut files = names_scoped(wt, "diff", scope);
    if files.is_empty() {
        files = names_scoped(wt, "status", scope);
    }
    files.retain(|f| wt.join(f).is_file());
    files.sort();
    files.dedup();
    files
}

pub fn review_diff(wt: &Path) -> String {
    let _ = git_ok(wt, &["add", "-N", "."]);
    let out = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(wt)
        .output()
        .ok();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    text.lines().take(400).collect::<Vec<_>>().join("\n")
}

pub fn short_head(wt: &Path) -> String {
    git_out(wt, &["rev-parse", "--short", "HEAD"])
}

pub fn log_oneline(root: &Path, n: usize) -> String {
    let out = Command::new("git")
        .args(["log", "--oneline", &format!("-{n}")])
        .current_dir(root)
        .output()
        .ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

pub fn ls_remote_ok(root: &Path, remote: &str) -> bool {
    git_ok(root, &["ls-remote", "-q", remote, "HEAD"])
}
