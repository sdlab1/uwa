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
//!   status | diff [--staged] [-- <paths…>] | log [-n N] [--ahead] | add --path <p…>
//!   commit --path <p…> -m <сообщение> | show [<rev>] | branch | restore --path <p…>
//!   (только ведущий) merge <агент> | push | worktree list|gc|release <агент>
//!
//! Что это даёт ведомому: ПОЛНЫЙ рабочий набор git-операций без риска
//! уронить чужую работу — фасад сам выбирает его worktree, пути скоупятся
//! `--path`, а привилегированные операции (merge/push/reset дерева main)
//! остаются за ведущим. Никаких «разрешений на git» не нужно: фасад и есть
//! разрешение.

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
        "commit" => commit(root, &ctx, &tail),
        "show" => show(&ctx, &tail),
        "branch" => branch(&ctx),
        "restore" => restore(&ctx, &tail),
        "merge" => merge(root, &ctx, &tail),
        "push" => push(root, &ctx),
        "worktree" => worktree(root, &ctx, &tail),
        other => anyhow::bail!(
            "devh git: неизвестная подкоманда «{other}» \
             (status|diff|log|add|commit|show|branch|restore|merge|push|worktree)"
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
    // `--staged`/`--cached` — диф индекса (что уйдёт в коммит), а не рабочего
    // дерева. Агенту это нужно перед `devh git commit`, чтобы увидеть РОВНО то,
    // что он коммитит.
    let staged = tail.iter().any(|a| a == "--staged" || a == "--cached");
    let text = if staged {
        git::diff_cached(&ctx.tree, &scope)?
    } else {
        git::diff_text(&ctx.tree, &scope)?
    };
    print!("{text}");
    Ok(())
}

fn log(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let n = flag_value(tail, "-n")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(10);
    if tail.iter().any(|a| a == "--ahead") {
        // Только коммиты, которых нет в main — «что я наработал».
        // Базовая ветка: MAIN из конфига, если конфиг есть (в worktree он
        // может отсутствовать — тогда дефолт `main`).
        let base = crate::commands::load_config(&ctx.tree)
            .map(|c| c.main_branch)
            .unwrap_or_else(|_| "main".to_string());
        let text = git::log_range(&ctx.tree, &base, n)?;
        print!("{text}");
        return Ok(());
    }
    print!("{}", git::log_oneline(&ctx.tree, n));
    Ok(())
}

/// `devh git show [<rev>] [-- <paths…>]` — содержимое ревизии или файла.
/// `rev` вида `HEAD:путь` отдаёт содержимое файла на ревизии.
fn show(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let scope = rest_after(tail, "--");
    let rev = tail
        .iter()
        .find(|a| !a.starts_with('-') && !scope.contains(a))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    if rev.contains(':') {
        // «HEAD:devharness/src/lib.rs» — файл на ревизии.
        let text = git::show_file(&ctx.tree, &rev)?;
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let text = git::show_rev(&ctx.tree, &rev, &scope)?;
    print!("{text}");
    Ok(())
}

/// `devh git branch` — ветки дерева; текущая помечена `*`.
fn branch(ctx: &Ctx) -> anyhow::Result<()> {
    let current = git::branch_current(&ctx.tree).unwrap_or_default();
    for b in git::branches(&ctx.tree) {
        println!("{}{b}", if b == current { "* " } else { "  " });
    }
    Ok(())
}

/// `devh git restore --path <p…>` — откатить указанные файлы к HEAD.
/// Безопасная форма: только свои пути, только в своём worktree, индекс не
/// трогаем. (`git checkout --`/`git restore` руками — запрещено; здесь путь
/// скоупленный фасадом.)
fn restore(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let paths = guard_paths(&flag_values(tail, "--path"))?;
    git::restore_paths(&ctx.tree, &paths)?;
    println!("откачено к HEAD: {}", paths.join(", "));
    Ok(())
}

fn add(ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let paths = guard_paths(&flag_values(tail, "--path"))?;
    git::add_paths(&ctx.tree, &paths)?;
    println!("в индекс добавлено: {}", paths.join(", "));
    Ok(())
}

/// Путь лежит в зоне (файл или каталог)?
fn path_in_zone(path: &str, zone: &str) -> bool {
    let p = path.trim().trim_start_matches("./").trim_end_matches('/');
    let z = zone.trim().trim_start_matches("./").trim_end_matches('/');
    !z.is_empty() && (p == z || p.starts_with(&format!("{z}/")))
}

/// Зоны всех АКТИВНЫХ задач агента (T-488). Пусто — значит коммит запрещён:
/// без зоны нельзя отличить работу задачи от подмены чужого файла.
fn active_zones(root: &Path, caller: &str) -> Vec<String> {
    let reg = crate::commands::orch::load(root);
    let canon = crate::core::identity::canonical(caller);
    let mut zones: Vec<String> = Vec::new();
    for t in reg.tasks.iter().filter(|t| {
        t.status == crate::commands::orch::Status::Active
            && crate::core::identity::canonical(&t.agent) == canon
    }) {
        for z in &t.zones {
            let z = z.trim();
            if !z.is_empty() && !zones.iter().any(|x| x == z) {
                zones.push(z.to_string());
            }
        }
    }
    zones
}

/// Каждый коммитимый путь — внутри зоны активной задачи (T-488).
/// Нельзя коммитить `README.md`, если задача объявлена только в `src/`.
fn enforce_commit_zones(root: &Path, caller: &str, paths: &[String]) -> anyhow::Result<()> {
    let zones = active_zones(root, caller);
    if zones.is_empty() {
        anyhow::bail!(
            "devh git commit: у «{caller}» нет активной задачи с зоной — \
             коммит запрещён (T-488). Возьми задачу с --zone или попроси lead уточнить зону."
        );
    }
    let outside: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| !zones.iter().any(|z| path_in_zone(p, z)))
        .collect();
    if !outside.is_empty() {
        anyhow::bail!(
            "devh git commit: пути вне зоны задачи: {} (зоны: {}) — T-488",
            outside.join(", "),
            zones.join(", ")
        );
    }
    Ok(())
}

fn commit(root: &Path, ctx: &Ctx, tail: &[String]) -> anyhow::Result<()> {
    let paths = guard_paths(&flag_values(tail, "--path"))?;
    let msg = flag_value(tail, "-m")
        .or_else(|| flag_value(tail, "--message"))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| anyhow::anyhow!("devh git commit: нужен текст (-m \"…\")"))?;
    enforce_commit_zones(root, &ctx.caller, &paths)?;
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
    let sub = tail
        .first()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    match sub.as_str() {
        "list" => {
            let reg = crate::commands::orch::load(root);
            for a in reg.agents.iter() {
                let path = a.worktree.clone().unwrap_or_else(|| "—".to_string());
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
        assert!(
            guard_paths(&["../main".to_string()]).is_err(),
            ".. запрещён"
        );
        assert!(
            guard_paths(&["/etc/passwd".to_string()]).is_err(),
            "absolute запрещён"
        );
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
            &[
                "devharness/src/core/rate.rs".to_string(),
                "AGENTS.md".to_string()
            ],
            &paths
        ));
    }

    #[test]
    fn restore_requires_paths_and_rejects_escape() {
        // restore без --path — отказ (не «откатить всё»).
        assert!(guard_paths(&[]).is_err(), "пустой список — отказ");
        // `..` и абсолютный путь — отказ (песочница дерева).
        assert!(guard_paths(&["../main".to_string()]).is_err());
        assert!(guard_paths(&["/etc/passwd".to_string()]).is_err());
    }

    /// `rest_after` — разбор `-- <paths…>`: пути после маркера, до — нет.
    #[test]
    fn rest_after_collects_paths_after_marker() {
        let args: Vec<String> = ["--staged", "--", "a.rs", "b.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            rest_after(&args, "--"),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
        assert!(rest_after(&args, "@@").is_empty());
    }

    /// `flag_values` собирает ПОВТОРЯЕМЫЕ `--path a --path b` (нужно для
    /// add/commit/restore с несколькими файлами).
    #[test]
    fn flag_values_collects_repeated_paths() {
        let args: Vec<String> = ["--path", "a.rs", "--path=b.rs", "tail"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            flag_values(&args, "--path"),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
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
    /// T-488: путь вне зоны задачи не коммитится.
    #[test]
    fn zone_prefix_contains_files_not_parent_dirs() {
        let zones = vec!["devharness/src/shell".to_string()];
        let ok = |paths: &[&str]| {
            paths.iter().all(|p| {
                let p = p.to_string();
                zones.iter().any(|z| path_in_zone(&p, z))
            })
        };
        assert!(ok(&["devharness/src/shell/msg.rs"]));
        assert!(!ok(&["devharness/src"]));
        assert!(!ok(&["README.md"]));
    }

    /// T-488: зона-файл не расширяется на соседний файл с тем же префиксом.
    #[test]
    fn exact_file_zone_and_parent_prefix() {
        assert!(path_in_zone("README.md", "README.md"));
        assert!(!path_in_zone("README.md.bak", "README.md"));
        assert!(path_in_zone("a/b/c.rs", "a"));
        assert!(!path_in_zone("ab/c.rs", "a"));
    }
}
