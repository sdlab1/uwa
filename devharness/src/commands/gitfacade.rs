//! `devh git` — ЕДИНСТВЕННАЯ точка входа для git-операций агентов.
//!
//! Зачем: ведомый должен быть заперт в своём worktree. Пока git можно было
//! звать руками, песочница была дырявой: `git -C /path/to/main reset --hard`
//! стирал чужую работу (реальный инцидент), `git push` уносил ветку мимо
//! DoD-гейта, `git checkout` в чужой зоне сносил чужие правки.
//!
//! Правила фасада:
//! * дерево операции ВЫБИРАЕТСЯ сам (ведомый → свой worktree, ведущий →
//!   главное дерево); произвольный `-C`/`--git-dir` не принимается;
//! * ведомому доступны только операции внутри своего worktree;
//! * привилегированные операции (слияние в main, push, сброс, удаление
//!   веток/worktree) — только ведущему;
//! * коммит — только по явно перечисленным путям (`--path`): в индекс не
//!   попадает ничего постороннего.
//!
//! Формат: `devh git <sub> [args]`
//!   status | diff [-- <paths…>] | log [-n N] | add --path <p…>
//!   commit --path <p…> -m <сообщение>
//!   (только ведущий) merge <агент> | push | worktree list|gc|release <агент>

use std::path::{Path, PathBuf};

use crate::shell::git;

/// Контекст операции: кто выполняет и в каком дереве.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Канонический адрес исполнителя.
    pub caller: String,
    /// Ведущий ли исполнитель.
    pub is_lead: bool,
    /// Дерево, в котором выполняется операция.
    pub tree: PathBuf,
    /// Человеческое имя дерева (для вывода).
    pub tree_label: String,
}

/// Определить исполнителя и его дерево.
///
/// Ведомый без worktree — отказ: работать можно только в своём дереве.
pub fn resolve(root: &Path) -> anyhow::Result<Ctx> {
    let reg = crate::commands::orch::load(root);
    let me = crate::core::identity::canonical(&crate::core::identity::current().name);
    let lead = reg
        .lead
        .as_ref()
        .map(|l| crate::core::identity::canonical(l));
    if lead.as_deref() == Some(me.as_str()) {
        return Ok(Ctx {
            caller: me.clone(),
            is_lead: true,
            tree: root.to_path_buf(),
            tree_label: "главное дерево".to_string(),
        });
    }
    let wt = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == me)
        .and_then(|a| a.worktree.clone());
    match wt {
        Some(w) if Path::new(&w).is_dir() => Ok(Ctx {
            caller: me.clone(),
            is_lead: false,
            tree: PathBuf::from(w),
            tree_label: format!("worktree {me}"),
        }),
        _ => anyhow::bail!(
            "devh git: у тебя нет worktree — ведомый работает только в своём дереве \
             (devh orch wt assign {}; выдача: devh orch start <ID>)",
            me
        ),
    }
}

/// Проверить пути: только относительные, без `..` — за пределы дерева не выйти.
pub fn guard_paths(paths: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if p.starts_with('/') {
            anyhow::bail!("devh git: абсолютный путь «{p}» запрещён — работаем от дерева");
        }
        if p.split('/').any(|c| c == "..") {
            anyhow::bail!("devh git: «..» в пути «{p}» запрещён — за пределы дерева не выходим");
        }
        if !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    if out.is_empty() {
        anyhow::bail!("devh git: нужен непустой список --path <путь>");
    }
    Ok(out)
}

/// В индекс не должно попасть ничего, кроме перечисленных путей.
pub fn staged_subset_ok(staged: &[String], paths: &[String]) -> bool {
    staged.iter().all(|f| {
        paths
            .iter()
            .any(|p| f == p || f.starts_with(&format!("{p}/")))
    })
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Все значения флага (повторяемый `--path a --path b`).
fn flag_values(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            if let Some(v) = it.next() {
                out.push(v.clone());
            }
        } else if let Some(rest) = a.strip_prefix(&format!("{name}=")) {
            out.push(rest.to_string());
        }
    }
    out
}

fn rest_after(args: &[String], marker: &str) -> Vec<String> {
    match args.iter().position(|a| a == marker) {
        Some(i) => args.iter().skip(i.saturating_add(1)).cloned().collect(),
        None => Vec::new(),
    }
}

/// Точка входа: `devh git …`.
pub fn git_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let sub = args
        .first()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if sub.is_empty() {
        anyhow::bail!(
            "devh git: нужен подкоманда — status | diff | log | add | commit | merge | push | worktree"
        );
    }
    let ctx = resolve(root)?;
    let tail: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub.as_str() {
        "status" => status(&ctx),
        "diff" => diff(&ctx, &tail),
        "log" => log(&ctx, &tail),
        "add" => add(&ctx, &tail),
        "commit" => commit(&ctx, &tail),
        "merge" => merge(root, &ctx, &tail),
        "push" => push(root, &ctx),
        "worktree" => worktree(root, &ctx, &tail),
        other => anyhow::bail!(
            "devh git: неизвестная подкоманда «{other}» \
             (status|diff|log|add|commit|merge|push|worktree)"
        ),
    }
}

fn status(ctx: &Ctx) -> anyhow::Result<()> {
    println!("дерево: {} ({})", ctx.tree.display(), ctx.tree_label);
    println!(
        "ветка: {}",
        git::branch_current(&ctx.tree).unwrap_or_else(|| "—".to_string())
    );
    println!("HEAD:  {}", git::short_head(&ctx.tree));
    let staged = git::staged_names(&ctx.tree);
    if staged.is_empty() {
        println!("в индексе: пусто");
    } else {
        println!("в индексе:");
        for f in staged {
            println!("  + {f}");
        }
    }
    let changed = git::status_names(&ctx.tree);
    if changed.is_empty() {
        println!("изменения: нет");
    } else {
        println!("изменения:");
        for f in changed {
            println!("  * {f}");
        }
    }
    Ok(())
}

fn diff(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let scope = rest_after(tail, "--").join(" ");
    let text = git::diff_text(&ctx.tree, &scope)?;
    print!("{text}");
    Ok(())
}

fn log(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let n = flag_value(tail, "-n")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(10);
    print!("{}", git::log_oneline(&ctx.tree, n));
    Ok(())
}

fn add(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let paths = guard_paths(&flag_values(tail, "--path"))?;
    git::add_paths(&ctx.tree, &paths)?;
    println!("в индекс добавлено: {}", paths.join(", "));
    Ok(())
}

fn commit(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let paths = guard_paths(&flag_values(tail, "--path"))?;
    let msg = flag_value(tail, "-m")
        .or_else(|| flag_value(tail, "--message"))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| anyhow::anyhow!("devh git commit: нужен текст (-m \"…\")"))?;
    git::add_paths(&ctx.tree, &paths)?;
    let staged = git::staged_names(&ctx.tree);
    if !staged_subset_ok(&staged, &paths) {
        let extra: Vec<&String> = staged
            .iter()
            .filter(|f| {
                !paths
                    .iter()
                    .any(|p| *f == p || f.starts_with(&format!("{p}/")))
            })
            .collect();
        anyhow::bail!(
            "devh git commit: в индексе есть файлы вне --path: {} — коммитим только свои пути",
            extra
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<&str>>()
                .join(", ")
        );
    }
    git::commit(&ctx.tree, &msg)?;
    println!("коммит: {} ({})", git::short_head(&ctx.tree), msg);
    Ok(())
}

fn merge(root: &Path, ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    if !ctx.is_lead {
        anyhow::bail!("devh git merge: только ведущий (слияние в main — привилегия)");
    }
    let agent = tail
        .first()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("devh git merge: нужен агент"))?;
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    let rep = crate::commands::wtman::merge(&cfg, root, &agent)?;
    println!("{agent}: {} — {}", rep.action.label(), rep.message);
    Ok(())
}

fn push(root: &Path, ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.is_lead {
        anyhow::bail!("devh git push: только ведущий");
    }
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    if !git::has_remote(root) {
        anyhow::bail!("devh git push: remote не настроен");
    }
    git::push_main(root, &cfg.remote, &cfg.main_branch)?;
    println!("push: {}/{}", cfg.remote, cfg.main_branch);
    Ok(())
}

fn worktree(root: &Path, ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    if !ctx.is_lead {
        anyhow::bail!("devh git worktree: только ведущий");
    }
    let sub = tail.first().map(|s| s.trim().to_string()).unwrap_or_default();
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    match sub.as_str() {
        "list" => {
            let reg = crate::commands::orch::load(root);
            for a in reg.agents.iter() {
                let path = a
                    .worktree
                    .clone()
                    .unwrap_or_else(|| "—".to_string());
                let dirty = Path::new(&path).is_dir() && git::tree_dirty(Path::new(&path));
                println!(
                    "{:<10} {}{}",
                    a.name,
                    path,
                    if dirty { "  (грязный)" } else { "" }
                );
            }
            Ok(())
        }
        "gc" => {
            let removed = crate::commands::wtman::gc(&cfg, root, &[])?;
            if removed.is_empty() {
                println!("gc: удалять нечего");
            } else {
                for r in removed {
                    println!("gc: удалён {r}");
                }
            }
            Ok(())
        }
        "release" => {
            let agent = tail
                .get(1)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("devh git worktree release: нужен агент"))?;
            let msg = crate::commands::wtman::release(&cfg, root, &agent)?;
            println!("{agent}: {msg}");
            Ok(())
        }
        other => anyhow::bail!(
            "devh git worktree: неизвестная подкоманда «{other}» (list|gc|release <агент>)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "devh-gitfacade-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_else(|_| unreachable!());
        d
    }

    #[test]
    fn guard_paths_rejects_escape_and_absolute() {
        assert!(guard_paths(&["../main".to_string()]).is_err(), ".. запрещён");
        assert!(guard_paths(&["/etc/passwd".to_string()]).is_err(), "absolute запрещён");
        assert!(guard_paths(&[]).is_err(), "пустой список запрещён");
        let ok = guard_paths(&["devharness/src".to_string(), "devharness/src".to_string()])
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(ok, vec!["devharness/src".to_string()], "дубли убраны");
    }

    #[test]
    fn staged_subset_rejects_foreign_files() {
        let paths = vec!["devharness/src/core".to_string()];
        assert!(staged_subset_ok(
            &["devharness/src/core/rate.rs".to_string()],
            &paths
        ));
        assert!(!staged_subset_ok(
            &["devharness/src/core/rate.rs".to_string(), "AGENTS.md".to_string()],
            &paths
        ));
    }

    #[test]
    fn worker_without_worktree_is_refused() {
        let root = tmp();
        let err = resolve(&root).expect_err("нет worktree — отказ");
        assert!(err.to_string().contains("нет worktree"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worker_resolves_to_own_worktree() {
        let root = tmp();
        let wt = root.join("wt-agent");
        std::fs::create_dir_all(&wt).unwrap_or_else(|_| unreachable!());
        let mut reg = crate::commands::orch::load(&root);
        let me = crate::core::identity::canonical(&crate::core::identity::current().name);
        reg.agents.push(crate::commands::orch::AgentState {
            name: me.clone(),
            worktree: Some(wt.display().to_string()),
            ..Default::default()
        });
        crate::commands::orch::save(&root, &reg).unwrap_or_else(|_| unreachable!());
        let ctx = resolve(&root).unwrap_or_else(|_| unreachable!());
        assert_eq!(ctx.tree, wt, "ведомый работает в своём worktree");
        assert!(!ctx.is_lead);
        let _ = std::fs::remove_dir_all(&root);
    }
}
