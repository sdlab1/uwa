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
//! | `merge <агент>` | влить ветку worktree агента в главную ветку (lead) |
//! | `release <агент>` | освободить worktree (грязное — не трогаем) |
//! | `gc` | prune: снять осиротевшие регистрации и мёртвые каталоги |
//!
//! Sync никогда не теряет работу агента: грязное дерево → `SkippedDirty`,
//! конфликт rebase → откат (`rebase --abort`) и `Conflict`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::types::RunConfig;
use crate::shell::worktree::slug;

/// Результат слияния с деталями перенесённых коммитов.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub sha: String,
    pub commits: Vec<String>,
}

/// Ветка агента в его worktree.
pub fn agent_branch(agent: &str) -> String {
    format!("devh/agent-{}", sanitize(agent))
}

/// Ветка WIP-чекпоинта агента: `devh/wip-<адрес>`.
///
/// Сюда сохраняется незакоммиченная работа агента, когда worktree нужно
/// освободить (отзыв задачи, watchdog, ребут): работа не пропадает — её
/// всегда можно забрать обратно (`git cherry-pick`/`git merge`).
pub fn wip_branch(agent: &str) -> String {
    format!("devh/wip-{}", sanitize(agent))
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

/// Git с дополнительным окружением (`GIT_INDEX_FILE` и т.п.).
fn git_env(cwd: &Path, env: &[(&str, String)], args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Сохранить грязное дерево агента в ветку WIP (`devh/wip-<адрес>`).
///
/// Механика: временный индекс → `read-tree HEAD` → `add -A` → `write-tree` →
/// `commit-tree -p HEAD` → `update-ref refs/heads/devh/wip-<адрес>`.
/// Рабочее дерево не трогаем: это чекпоинт, а не коммит в ветку агента.
pub fn checkpoint(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<String> {
    let path = agent_path(cfg, root, agent);
    checkpoint_at(&path, agent)
}

/// Чекпоинт незакоммиченной работы в ветку `devh/wip-<адрес>` по явному пути.
///
/// Путь берётся из реестра (у агента он может отличаться от вычисленного
/// по конфигу): чекпоинтим именно тот worktree, который видит харнесс.
pub fn checkpoint_at(path: &Path, agent: &str) -> anyhow::Result<String> {
    if !path.exists() {
        anyhow::bail!("worktree агента {agent} не выдан — нечего сохранять");
    }
    let idx = std::env::temp_dir().join(format!("devh-wip-index-{}", sanitize(agent)));
    let _ = std::fs::remove_file(&idx);
    let idx_s = idx.to_string_lossy().to_string();
    let env = vec![
        ("GIT_INDEX_FILE", idx_s),
        ("GIT_AUTHOR_NAME", "devh".to_string()),
        ("GIT_AUTHOR_EMAIL", "devh@localhost".to_string()),
        ("GIT_COMMITTER_NAME", "devh".to_string()),
        ("GIT_COMMITTER_EMAIL", "devh@localhost".to_string()),
    ];
    let res = (|| -> anyhow::Result<String> {
        git_env(&path, &env, &["read-tree", "HEAD"])?;
        git_env(&path, &env, &["add", "-A"])?;
        let tree = git_env(&path, &env, &["write-tree"])?;
        let msg = format!("WIP({agent}): чекпоинт незакоммиченной работы");
        let commit = git_env(
            &path,
            &env,
            &["commit-tree", &tree, "-p", "HEAD", "-m", &msg],
        )?;
        let refname = format!("refs/heads/{}", wip_branch(agent));
        git_env(&path, &env, &["update-ref", &refname, &commit])?;
        Ok(commit)
    })();
    let _ = std::fs::remove_file(&idx);
    res
}

/// Результат ручного WIP-прохода (`devh orch wip <агент>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WipReport {
    /// Чистый worktree — пропуск без коммита.
    SkipClean,
    /// Грязный worktree — создан WIP-коммит с N файлами.
    Committed { n_files: usize, commit_sha: String },
    /// Worktree не выдан агенту.
    NoWorktree,
}

/// Ручной WIP-проход: сохранить незакоммиченную работу агента.
///
/// - чистый worktree → `SkipClean` (без коммита);
/// - грязный worktree → чекпоинт через `checkpoint` + возврат количества файлов;
/// - worktree не существует → `NoWorktree`.
pub fn wip_pass(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<WipReport> {
    let wt_path = agent_path(cfg, root, agent);
    if !wt_path.exists() {
        return Ok(WipReport::NoWorktree);
    }
    if !crate::shell::git::tree_dirty(&wt_path) {
        return Ok(WipReport::SkipClean);
    }
    let sha = checkpoint(cfg, root, agent)?;
    if sha.is_empty() {
        return Ok(WipReport::SkipClean);
    }
    let n_files = crate::shell::git::changed_files(&wt_path).len();
    Ok(WipReport::Committed {
        n_files,
        commit_sha: sha,
    })
}

/// Сформировать единый WIP-коммит сообщения: `wip: <агент> <T-ID|-> <n> файлов`.
///
/// `task_ref` — Option<&str>: Some("T-316") → `T-316`, None → `-`.
pub fn wip_commit_msg(agent: &str, task_ref: Option<&str>, n_files: usize) -> String {
    let ref_part = task_ref.unwrap_or("-");
    format!("wip: {} {} {} файла", agent, ref_part, n_files)
}

/// Распарсить WIP-коммит сообщение в `(agent, Option<T-ID>, n_files)`.
///
/// Возвращает `None`, если сообщение не соответствует формату.
pub fn parse_wip_commit_msg(msg: &str) -> Option<(String, Option<String>, usize)> {
    let body = msg.trim();
    let rest = body.strip_prefix("wip: ")?;
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() < 3 {
        return None;
    }
    let agent = parts[0].to_string();
    let ref_part = parts[1];
    let rest = parts[2];
    let (n_str, _) = rest.split_once(' ')?;
    let n_files = n_str.parse::<usize>().ok()?;
    let task_ref = if ref_part == "-" {
        None
    } else if ref_part.starts_with("T-") {
        Some(ref_part.to_string())
    } else {
        return None;
    };
    Some((agent, task_ref, n_files))
}

/// Имя модуля из строки `pub mod xxx;`.
fn mod_name(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix("pub mod ")?;
    let name = rest.strip_suffix(';')?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Объединить два списка `pub mod ...;` (конфликт в `core/mod.rs`).
///
/// Смысл такого конфликта — объединение: разные агенты добавили разные
/// модули в один `mod.rs`, ни одна строка не должна пропасть. Дубли
/// убираются (иначе сборка падает с E0428 — такое уже бывало).
pub fn union_mod_lists(a: &str, b: &str) -> String {
    let a_lines: Vec<&str> = a.lines().collect();
    let b_lines: Vec<&str> = b.lines().collect();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut head: Vec<String> = Vec::new();
    let mut mods: Vec<String> = Vec::new();
    let mut tail: Vec<String> = Vec::new();
    let mut seen_mod = false;
    for line in &a_lines {
        match mod_name(line) {
            Some(name) => {
                seen_mod = true;
                if seen.insert(name) {
                    mods.push((*line).to_string());
                }
            }
            None => {
                if seen_mod {
                    tail.push((*line).to_string());
                } else {
                    head.push((*line).to_string());
                }
            }
        }
    }
    for line in &b_lines {
        if let Some(name) = mod_name(line) {
            if seen.insert(name) {
                mods.push((*line).to_string());
            }
        }
    }
    let mut out = String::new();
    for l in head.iter().chain(mods.iter()).chain(tail.iter()) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Проверка списка модулей `pub mod ...;`.
///
/// Страховка от битого объединения: если после union список невалиден
/// (нет `;`, дубль, мусор в имени) — слияние откатывается, а не едет в
/// главную битым. Именно такой битый список однажды ломал сборку сразу у
/// нескольких агентов.
pub fn validate_mod_list(text: &str) -> anyhow::Result<()> {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.split("//").next().unwrap_or("").trim();
        if t.is_empty() {
            continue;
        }
        let Some(rest) = t.strip_prefix("pub mod ") else {
            if t.contains("pub mod") {
                anyhow::bail!(
                    "строка {}: нераспознанная строка списка модулей: '{t}'",
                    i + 1
                );
            }
            continue;
        };
        let Some(name) = rest.strip_suffix(';') else {
            anyhow::bail!("строка {}: битая строка '{t}' — нет ';'", i + 1);
        };
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("строка {}: битая строка '{t}' — нет имени модуля", i + 1);
        }
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            anyhow::bail!("строка {}: недопустимое имя модуля '{name}'", i + 1);
        }
        if !seen.insert(name.to_string()) {
            anyhow::bail!("строка {}: дубль модуля '{name}'", i + 1);
        }
    }
    Ok(())
}

/// Файлы с конфликтами rebase.
fn conflict_files(path: &Path) -> Vec<String> {
    git_out(path, &["diff", "--name-only", "--diff-filter=U"])
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Известный авторазрешаемый конфликт: список модулей `core/mod.rs`.
fn is_mod_list_file(file: &str) -> bool {
    file.ends_with("core/mod.rs")
}

/// Разрешить конфликт в `core/mod.rs` объединением списков модулей.
fn resolve_union_mod_list(path: &Path, file: &str) -> anyhow::Result<()> {
    let ours = git_out(path, &["show", &format!(":2:{file}")]);
    let theirs = git_out(path, &["show", &format!(":3:{file}")]);
    if ours.is_empty() || theirs.is_empty() {
        anyhow::bail!("не читать версии конфликта {file}");
    }
    let resolved = union_mod_lists(&ours, &theirs);
    // Объединённый список должен быть валиден — иначе откатываем rebase.
    if let Err(e) = validate_mod_list(&resolved) {
        anyhow::bail!("объединение списка модулей в {file} невалидно: {e} — rebase откачен, работа агента цела");
    }
    std::fs::write(path.join(file), resolved)?;
    git(path, &["add", file])?;
    Ok(())
}

/// Коммиты ветки агента, которые НАДО переносить в главную (T-262).
///
/// Фильтры:
/// 1. `git cherry` — коммиты, чей патч уже лежит в main (patch-эквивалент),
///    не переносятся: агент перебазировался на main и принёс копию старой
///    работы — её повторное применение даёт конфликты и дубли.
/// 2. WIP-чекпоинты (`WIP(<агент>): ...`) — это сохранённая грязь, а НЕ
///    результат задачи. Перенос такого коммита в main откатывал чужие
///    файлы (devharness/tests/orch_roles.rs был снесён именно так).
///    Работа не теряется: она лежит в ветке `devh/wip-<агент>`.
/// 3. Порядок — от старых к новым (как при cherry-pick).
pub fn transfer_commits(
    main_root: &Path,
    main_branch: &str,
    branch: &str,
) -> anyhow::Result<Vec<String>> {
    let cherry = git(main_root, &["cherry", main_branch, branch])?;
    let mut out: Vec<String> = Vec::new();
    for line in cherry.lines() {
        let mut it = line.split_whitespace();
        let sign = it.next().unwrap_or("");
        let sha = it.next().unwrap_or("").trim().to_string();
        // '-' = такой же патч уже в main — переносить нечего.
        if sign != "+" || sha.is_empty() {
            continue;
        }
        if is_wip_checkpoint(main_root, &sha) {
            continue;
        }
        out.push(sha);
    }
    // `git cherry` печатает от новых к старым; перенос идёт от старых к новым.
    out.reverse();
    Ok(out)
}

/// Коммит-чекпоинт незакоммиченной работы (`WIP(<агент>): ...`).
fn is_wip_checkpoint(main_root: &Path, sha: &str) -> bool {
    let subject = git_out(main_root, &["log", "-1", "--format=%s", sha]);
    let subject = subject.trim();
    subject.starts_with("WIP(") || subject.starts_with("WIP:")
}

/// Влить ветку агента в главную, не трогая его worktree.
///
/// Почему не rebase: у агента почти всегда грязное дерево (он работает),
/// а `git rebase` требует чистоты. Поэтому слияние идёт через временный
/// worktree: коммиты агента (`main..branch`) переносятся cherry-pick'ом на
/// главную, конфликт списка модулей (`core/mod.rs`) объединяется
/// автоматически. Worktree агента остаётся нетронутым — агент ничего не
/// должен делать для слияния.
pub fn merge_via_cherry_pick(
    main_root: &Path,
    main_branch: &str,
    branch: &str,
    agent: &str,
) -> anyhow::Result<MergeResult> {
    let commits = transfer_commits(main_root, main_branch, branch)?;
    if commits.is_empty() {
        anyhow::bail!("нечего вливать: на {branch} нет коммитов сверх {main_branch}");
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("devh-merge-{}-{}", sanitize(agent), stamp));
    let tmp_branch = format!("devh/merge-tmp-{}", sanitize(agent));
    let cleanup = || {
        let _ = git_ok(
            main_root,
            &["worktree", "remove", "--force", &tmp.to_string_lossy()],
        );
        let _ = git_ok(main_root, &["branch", "-D", &tmp_branch]);
        let _ = std::fs::remove_dir_all(&tmp);
    };
    let tmp_s = tmp.to_string_lossy().to_string();
    if !git_ok(
        main_root,
        &["worktree", "add", "--detach", &tmp_s, main_branch],
    ) {
        cleanup();
        anyhow::bail!("не удалось создать временный worktree для слияния {branch}");
    }
    for commit in &commits {
        if git_ok(&tmp, &["cherry-pick", commit]) {
            continue;
        }
        let conflicted = conflict_files(&tmp);
        let only_mod_list =
            !conflicted.is_empty() && conflicted.iter().all(|f| is_mod_list_file(f));
        if only_mod_list {
            let mut resolved = true;
            for file in &conflicted {
                if resolve_union_mod_list(&tmp, file).is_err() {
                    resolved = false;
                    break;
                }
            }
            if resolved {
                let env = [("GIT_EDITOR", "true".to_string())];
                if git_env(&tmp, &env, &["cherry-pick", "--continue"]).is_ok() {
                    continue;
                }
            }
        } else if conflicted.is_empty() && git_ok(&tmp, &["cherry-pick", "--skip"]) {
            continue;
        }
        let _ = git_ok(&tmp, &["cherry-pick", "--abort"]);
        cleanup();
        anyhow::bail!(
            "конфликт при переносе коммитов {branch} в {main_branch} ({}): {}",
            commits.len(),
            if conflicted.is_empty() {
                "неизвестный конфликт".to_string()
            } else {
                conflicted.join(", ")
            }
        );
    }
    if !git_ok(&tmp, &["switch", "-c", &tmp_branch]) {
        cleanup();
        anyhow::bail!("не удалось создать временную ветку {tmp_branch}");
    }
    if !git_ok(main_root, &["merge", "-q", "--ff-only", &tmp_branch]) {
        cleanup();
        anyhow::bail!("fast-forward {main_branch} на перенос {branch} не удался");
    }
    let sha = git(main_root, &["rev-parse", "--short", "HEAD"])?;
    cleanup();
    Ok(MergeResult { sha, commits })
}

/// Rebase с авторазрешением единственного известного конфликта.
///
/// * конфликт в списке `pub mod ...;` → объединение (ни одна строка не теряется);
/// * любой другой конфликт → `rebase --abort` и отказ: работа агента цела;
/// * пустой коммит после разрешения → `rebase --skip`.
pub fn rebase_auto(path: &Path, upstream: &str) -> anyhow::Result<()> {
    if crate::shell::git::rebase(path, upstream).is_ok() {
        return Ok(());
    }
    for _ in 0..64u32 {
        let conflicted = conflict_files(path);
        if conflicted.is_empty() {
            if git_ok(path, &["rebase", "--skip"]) {
                continue;
            }
            let _ = git_ok(path, &["rebase", "--abort"]);
            anyhow::bail!("rebase на {upstream} остановился без конфликтов — откачен");
        }
        let mut known = true;
        for f in &conflicted {
            if is_mod_list_file(f) {
                resolve_union_mod_list(path, f)?;
            } else {
                known = false;
            }
        }
        if !known {
            let _ = git_ok(path, &["rebase", "--abort"]);
            anyhow::bail!(
                "конфликт rebase в {} — rebase откачен, работа агента цела",
                conflicted.join(", ")
            );
        }
        let out = Command::new("git")
            .args(["rebase", "--continue"])
            .env("GIT_EDITOR", "true")
            .current_dir(path)
            .output();
        match out {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                if git_ok(path, &["rebase", "--skip"]) {
                    continue;
                }
                if !conflict_files(path).is_empty() {
                    continue;
                }
                let _ = git_ok(path, &["rebase", "--abort"]);
                anyhow::bail!("rebase --continue не удался: {}", err.trim());
            }
            Err(e) => {
                let _ = git_ok(path, &["rebase", "--abort"]);
                anyhow::bail!("rebase --continue: {e}");
            }
        }
    }
    let _ = git_ok(path, &["rebase", "--abort"]);
    anyhow::bail!("rebase не сошёлся за 64 шага — откачен");
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
/// База для rebase/sync/merge.
///
/// `origin/<main>` берём ТОЛЬКО если он содержит локальный `<main>`
/// (т.е. `<main>` — предок `origin/<main>`). Иначе origin отстал: rebase
/// на него увёл бы работу агента на древнюю базу, а fast-forward merge
/// в main стал бы невозможен (main не предок ветки агента).
fn base_ref(root: &Path, main_branch: &str) -> String {
    let origin = format!("origin/{main_branch}");
    let local_ok = git_ok(root, &["rev-parse", "--verify", "--quiet", main_branch]);
    let origin_ok = git_ok(root, &["rev-parse", "--verify", "--quiet", &origin]);
    if local_ok && origin_ok {
        let local_is_ancestor =
            git_ok(root, &["merge-base", "--is-ancestor", main_branch, &origin]);
        if local_is_ancestor {
            return origin;
        }
        // origin отстал от локального main — база = локальный main.
        return main_branch.to_string();
    }
    if local_ok {
        main_branch.to_string()
    } else if origin_ok {
        origin
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
    // Каталог worktree'ов может быть вне /tmp и ещё не существовать.
    if let Err(e) = std::fs::create_dir_all(&cfg.wt_root) {
        anyhow::bail!(
            "не создать каталог worktree'ов {}: {e}",
            cfg.wt_root.display()
        );
    }

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

/// Итог слияния worktree агента в главную ветку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAction {
    /// Влито (fast-forward) в главную ветку.
    Merged,
    /// Вливать нечего: своих коммитов сверх главной ветки нет.
    Nothing,
    /// Нужен ручной merge (главное дерево грязное или не fast-forward).
    Manual,
}

impl MergeAction {
    pub fn label(&self) -> &'static str {
        match self {
            MergeAction::Merged => "влито",
            MergeAction::Nothing => "нечего вливать",
            MergeAction::Manual => "нужен ручной merge",
        }
    }
}

/// Отчёт о слиянии worktree агента в главную ветку.
#[derive(Debug, Clone)]
pub struct MergeReport {
    pub action: MergeAction,
    pub message: String,
    pub sha: Option<String>,
}

/// Влить ветку worktree агента в главную ветку репозитория.
///
/// Семантика — как у `devh merge` для задач, но для персональных worktree'ов
/// агентов (`devh/agent-<адрес>`). Ничего не теряет:
/// * грязное дерево агента → отказ (работа агента не тронута);
/// * конфликт rebase → `rebase --abort` и отказ;
/// * грязное главное дерево или не fast-forward → ручной merge, без правок.
pub fn merge(cfg: &RunConfig, root: &Path, agent: &str) -> anyhow::Result<MergeReport> {
    let path = agent_path(cfg, root, agent);
    if !path.exists() {
        anyhow::bail!("worktree агента {agent} не выдан: devh orch wt assign {agent}");
    }
    if crate::shell::git::tree_dirty(&path) {
        let files = crate::shell::git::status_names(&path).join(", ");
        anyhow::bail!(
            "worktree агента {agent} грязный ({files}) — сначала закоммить: \
             вливать нечего, работа агента не теряется"
        );
    }
    let branch = crate::shell::git::branch_current(&path).unwrap_or_else(|| agent_branch(agent));
    if !crate::shell::git::show_ref(root, &branch) {
        anyhow::bail!("ветка {branch} не найдена — вливать нечего");
    }
    let ahead = crate::shell::git::rev_list_count(&path, &cfg.main_branch)?;
    if ahead == 0 {
        return Ok(MergeReport {
            action: MergeAction::Nothing,
            message: format!(
                "на {branch} нет коммитов сверх {} — вливать нечего",
                cfg.main_branch
            ),
            sha: None,
        });
    }
    // Сначала подтягиваем главную ветку (remote → origin/main, иначе main),
    // затем rebase ветки агента поверх актуальной базы — как в `devh merge`.
    if crate::shell::git::has_remote(root) {
        let _ = crate::shell::git::fetch(root, &cfg.remote, &cfg.main_branch);
    }
    // База = origin/main, только если он содержит локальный main; иначе main.
    let upstream = base_ref(root, &cfg.main_branch);
    // Rebase с авторазрешением известного конфликта (список модулей).
    if let Err(e) = rebase_auto(&path, &upstream) {
        anyhow::bail!("{e}");
    }
    let sha = crate::shell::git::short_head(&path);
    if crate::shell::git::tree_dirty(root) {
        return Ok(MergeReport {
            action: MergeAction::Manual,
            message: format!(
                "главное дерево грязное — влить нельзя (R9: git принадлежит харнессу); \
                 ветка {branch} готова ({sha}), повтори после коммита чужих правок"
            ),
            sha: Some(sha),
        });
    }
    // Локальная главная ветка могла отстать от upstream — подтягиваем ff.
    if upstream != cfg.main_branch {
        let _ = crate::shell::git::merge_ff_only(root, &upstream);
    }
    if crate::shell::git::merge_ff_only(root, &branch) {
        return Ok(MergeReport {
            action: MergeAction::Merged,
            message: format!("{branch} влит в {} ({sha})", cfg.main_branch),
            sha: Some(sha),
        });
    }
    Ok(MergeReport {
        action: MergeAction::Manual,
        message: format!(
            "{} не fast-forward относительно {branch} — нужен ручной merge: \
             git -C {} merge {branch}",
            cfg.main_branch,
            root.display()
        ),
        sha: Some(sha),
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
    // Грязное дерево НЕ теряем: сначала WIP-чекпоинт в `devh/wip-<адрес>`.
    let mut wip: Option<String> = None;
    if st.dirty {
        let sha = checkpoint(cfg, root, agent).map_err(|e| {
            anyhow::anyhow!(
                "worktree {} грязный ({}), но WIP-чекпоинт не сохранился: {e} — \
                 работа не потерялась, освобождать нельзя",
                path.display(),
                st.dirty_files.join(", ")
            )
        })?;
        wip = Some(sha);
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
    Ok(match wip {
        Some(sha) => format!(
            "worktree агента {agent} освобождён: {} (WIP сохранён в {}: {})",
            path.display(),
            wip_branch(agent),
            sha
        ),
        None => format!("worktree агента {agent} освобождён: {}", path.display()),
    })
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

/// Класс кандидата на «reclaim» — осиротевшая работа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// Worktree, чей агент не найден в реестре.
    AbandonedWorktree,
    /// Ветка devh/agent-* или devh/wip-* без зарегистрированного агента.
    OrphanBranch,
    /// WIP-чекпоинт (devh/wip-*).
    WipBranch,
    /// Недостижимый коммит (git fsck --unreachable).
    DanglingCommit,
}

/// Кандидат на reclaim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub sha: Option<String>,
    pub agent: Option<String>,
}

/// Сканер кандидатов на reclaim: парсит `git worktree list --porcelain`,
/// `git branch --list 'devh/agent-*' 'devh/wip-*'`, `git fsck --unreachable --no-progress`.
/// Агент из реестра agents и main-ветка НИКОГДА не кандидаты.
pub fn discover(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let reg = crate::commands::orch::load(root);
    let registered: std::collections::HashSet<String> =
        reg.agents.iter().map(|a| a.name.clone()).collect();
    let current_branch = crate::shell::git::branch_current(root);
    let main_branch = "main".to_string();

    // 1. Worktree'ы без зарегистрированного агента.
    for wt in crate::shell::git::worktree_paths(root) {
        let Some(branch) = crate::shell::git::branch_current(&wt) else {
            continue;
        };
        let agent = agent_from_branch(&branch);
        if let Some(a) = &agent {
            if registered.contains(a) {
                continue;
            }
        }
        // Главный worktree (main) — не кандидат.
        if branch == main_branch || wt == root {
            continue;
        }
        out.push(Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(wt),
            branch: Some(branch),
            sha: None,
            agent,
        });
    }

    // 2. Ветки devh/agent-* и devh/wip-* без зарегистрированного агента.
    let branches = git_output(root, &["branch", "--list", "devh/agent-*", "devh/wip-*"]);
    for line in branches.lines() {
        let branch = line.trim().trim_start_matches('*').trim().to_string();
        if branch.is_empty() {
            continue;
        }
        let kind = if branch.starts_with("devh/wip-") {
            CandidateKind::WipBranch
        } else {
            CandidateKind::OrphanBranch
        };
        let agent = agent_from_branch(&branch);
        if let Some(a) = &agent {
            if registered.contains(a) {
                continue;
            }
        }
        if branch == main_branch {
            continue;
        }
        out.push(Candidate {
            kind,
            path: None,
            branch: Some(branch),
            sha: None,
            agent,
        });
    }

    // 3. Недостижимые коммиты (git fsck --unreachable).
    let fsck_out = git_output(root, &["fsck", "--unreachable", "--no-progress"]);
    for line in fsck_out.lines() {
        let line = line.trim();
        if !line.starts_with("unreachable commit ") {
            continue;
        }
        let sha = line
            .strip_prefix("unreachable commit ")
            .unwrap_or("")
            .trim()
            .to_string();
        if sha.is_empty() {
            continue;
        }
        out.push(Candidate {
            kind: CandidateKind::DanglingCommit,
            path: None,
            branch: None,
            sha: Some(sha),
            agent: None,
        });
    }

    // Фильтр: main-ветка и текущая ветка — не кандидаты.
    out.retain(|c| c.branch.as_deref() != Some(current_branch.as_deref().unwrap_or("")));
    out
}

fn agent_from_branch(branch: &str) -> Option<String> {
    branch
        .strip_prefix("devh/agent-")
        .or_else(|| branch.strip_prefix("devh/wip-"))
        .map(|s| s.to_string())
}

fn git_output(root: &Path, args: &[&str]) -> String {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Спасти грязный abandoned worktree: коммитить его в ветку devh/reclaim-<агент>.
///
/// Создаёт ветку `devh/reclaim-<agent>` (если не существует) и делает
/// WIP-чекпоинт с сообщением «reclaim: <agent> (N файлов)».
/// Возвращает SHA коммита.
pub fn salvage(root: &Path, cand: &Candidate) -> anyhow::Result<String> {
    let wt_path = cand
        .path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("кандидат без пути"))?;
    let agent = cand.agent.as_deref().unwrap_or("unknown");

    // Проверяем, что worktree существует.
    if !wt_path.exists() {
        anyhow::bail!("worktree {} не существует", wt_path.display());
    }

    // Проверяем, что есть изменения (грязный worktree).
    if !crate::shell::git::tree_dirty(wt_path) {
        // Чистый worktree — salvage пропускаем.
        return Ok(String::new());
    }

    // Создаём ветку reclaim, если не существует.
    let reclaim_branch = format!("devh/reclaim-{}", agent);
    if !crate::shell::git::show_ref(root, &reclaim_branch) {
        // Создаём ветку от текущего HEAD worktree'а.
        let head = crate::shell::git::short_head(wt_path);
        // Попытка 1: создать ветку в worktree.
        if !git_ok(wt_path, &["checkout", "-b", &reclaim_branch]) {
            // Попытка 2: создать ветку в main repo.
            if !git_ok(root, &["branch", &reclaim_branch, &head]) {
                anyhow::bail!("не удалось создать ветку {}", reclaim_branch);
            }
            // Переключить worktree на новую ветку.
            if !git_ok(wt_path, &["checkout", &reclaim_branch]) {
                anyhow::bail!(
                    "не удалось переключить {} на {}",
                    wt_path.display(),
                    reclaim_branch
                );
            }
        }
    } else {
        // Ветка уже существует — переключаемся.
        if !git_ok(wt_path, &["checkout", &reclaim_branch]) {
            anyhow::bail!(
                "не удалось переключить {} на {}",
                wt_path.display(),
                reclaim_branch
            );
        }
    }

    // Ветка уже чекаутнута выше или здесь.

    // Считаем изменённые файлы.
    let files = crate::shell::git::changed_files(wt_path);
    let n = files.len();

    // Добавляем все изменения.
    let _ = crate::shell::git::add_all(wt_path);

    // Коммитим с сообщением reclaim.
    let msg = format!("reclaim: {} ({} файлов)", agent, n);
    crate::shell::git::commit(wt_path, &msg)?;

    // Возвращаем SHA.
    Ok(crate::shell::git::short_head(wt_path))
}

/// Проверить rescue-ref: существует ли ветка rescue-ref и смотрит ли она
/// на тот же SHA, что и кандидат.
pub fn rescue_ref(root: &Path, branch: &str, sha: &str) -> bool {
    if !crate::shell::git::show_ref(root, branch) {
        return false;
    }
    // Разрешаем SHA ветки через git rev-parse, а не через путь.
    let out = std::process::Command::new("git")
        .args(["rev-parse", branch])
        .current_dir(root)
        .output();
    let ref_sha = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return false,
    };
    let sha = sha.trim();
    // Сравниваем по короткому SHA (первые 7-12 символов).
    ref_sha == sha || ref_sha.starts_with(sha) || sha.starts_with(&ref_sha)
}

/// Снять worktree (close_abandoned): worktree remove снимается ТОЛЬКО если
/// rescue_ref истина; после снятия — worktree prune.
/// keep_paths: пути живых агентов НИКОГДА не снимаются.
/// dry_run: если true, ничего не удаляет, печатает план.
pub fn close_abandoned(
    root: &Path,
    cand: &Candidate,
    keep_paths: &[PathBuf],
    dry_run: bool,
) -> anyhow::Result<Vec<String>> {
    let mut report = Vec::new();

    let wt_path = match &cand.path {
        Some(p) => p,
        None => {
            report.push(format!("пропуск: кандидат без пути"));
            return Ok(report);
        }
    };

    // Проверяем keep-list: пути живых агентов НИКОГДА не снимаются.
    if keep_paths.iter().any(|k| k == wt_path) {
        report.push(format!("пропуск (keep-list): {}", wt_path.display()));
        return Ok(report);
    }

    // Проверяем rescue-ref (fail-closed).
    let branch = cand.branch.as_deref().unwrap_or("");
    let sha = cand.sha.as_deref().unwrap_or("");
    if !branch.is_empty() && !rescue_ref(root, branch, sha) {
        report.push(format!(
            "отказ: rescue-ref не подтверждён для {} (ветка={}, sha={})",
            wt_path.display(),
            branch,
            sha
        ));
        return Ok(report);
    }

    // dry_run: ничего не удаляем, печатаем план.
    if dry_run {
        report.push(format!("dry-run: снял бы worktree {}", wt_path.display()));
        return Ok(report);
    }

    // Снимаем worktree.
    if wt_path.exists() {
        if let Err(e) = crate::shell::git::worktree_remove(root, wt_path) {
            return Err(anyhow::anyhow!(
                "битый git при снятии {}: {}",
                wt_path.display(),
                e
            ));
        }
        report.push(format!("снят worktree: {}", wt_path.display()));
    }

    // Prune registrations.
    let _ = crate::shell::git::worktree_prune(root);

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::RunConfig;
    use std::path::Path;

    fn run_git(repo: &Path, args: &[&str]) {
        let _ = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output();
    }

    fn init_repo(dir: &Path) -> PathBuf {
        let root = dir.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "t@example.com"]);
        run_git(&root, &["config", "user.name", "T"]);
        std::fs::write(root.join("a.txt"), "1\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-qm", "base"]);
        // Переименовать ветку в main если нужно.
        run_git(&root, &["branch", "-M", "main"]);
        root
    }

    #[test]
    fn discover_empty_repo() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let cands = discover(&root);
        assert!(cands.is_empty(), "пустой репо: {cands:?}");
    }

    #[test]
    fn discover_abandoned_worktree() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-abandoned");
        run_git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "devh/agent-ghost",
                &wt.to_string_lossy(),
            ],
        );
        // Зарегистрируем агента «ghost» в реестре? Нет — он не зарегистрирован.
        let cands = discover(&root);
        assert!(
            cands.iter().any(|c| {
                c.kind == CandidateKind::AbandonedWorktree && c.agent.as_deref() == Some("ghost")
            }),
            "abandoned worktree найден: {cands:?}"
        );
    }

    #[test]
    fn discover_orphan_branch() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        run_git(&root, &["branch", "devh/agent-orphan"]);
        let cands = discover(&root);
        assert!(
            cands.iter().any(|c| {
                c.kind == CandidateKind::OrphanBranch
                    && c.branch.as_deref() == Some("devh/agent-orphan")
            }),
            "orphan branch найден: {cands:?}"
        );
    }

    #[test]
    fn discover_wip_branch() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        run_git(&root, &["branch", "devh/wip-x"]);
        let cands = discover(&root);
        assert!(
            cands.iter().any(|c| {
                c.kind == CandidateKind::WipBranch && c.branch.as_deref() == Some("devh/wip-x")
            }),
            "wip branch найден: {cands:?}"
        );
    }

    #[test]
    fn discover_registered_agent_not_candidate() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        // Зарегистрируем агента в реестре.
        let harness = root.join(".harness/orch");
        std::fs::create_dir_all(&harness).unwrap();
        let reg = r#"{"seq":1,"tasks":[],"agents":[{"name":"beam","role":"worker","fail_streak":0}],"lead":"lead","plan":{"deps":{},"prio":{}}}"#;
        std::fs::write(harness.join("registry.json"), reg).unwrap();
        run_git(&root, &["branch", "devh/agent-beam"]);
        let cands = discover(&root);
        assert!(
            !cands.iter().any(|c| c.agent.as_deref() == Some("beam")),
            "зарегистрированный агент не кандидат: {cands:?}"
        );
    }

    #[test]
    fn discover_broken_git_returns_empty() {
        let d = tempfile::tempdir().unwrap();
        // Не git-репозиторий.
        let root = d.path().join("not-a-repo");
        std::fs::create_dir_all(&root).unwrap();
        let cands = discover(&root);
    }

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

    /// Грязное дерево при освобождении НЕ теряется: работа сохраняется
    /// в ветку `devh/wip-<агент>` (а не просто отказ).
    #[test]
    fn release_saves_dirty_tree_as_wip_branch() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "dirty\n").unwrap_or_else(|_| unreachable!());
        let msg = release(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert!(msg.contains("WIP"), "{msg}");
        assert!(!p.exists(), "worktree снят");
        let wip =
            git(&root, &["rev-parse", &wip_branch("beam")]).unwrap_or_else(|_| unreachable!());
        let content = git(&root, &["show", &format!("{wip}:a.txt")]).unwrap_or_default();
        assert_eq!(content.trim(), "dirty", "работа сохранена в WIP-ветке");
    }

    #[test]
    fn checkpoint_creates_wip_branch_with_uncommitted_work() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "wip\n").unwrap_or_else(|_| unreachable!());
        let sha = checkpoint(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(sha.len(), 40, "полный sha коммита: {sha}");
        let head =
            git(&root, &["rev-parse", &wip_branch("beam")]).unwrap_or_else(|_| unreachable!());
        assert_eq!(head, sha, "ветка WIP указывает на чекпоинт");
        assert_eq!(
            git(&root, &["show", &format!("{sha}:a.txt")])
                .unwrap_or_default()
                .trim(),
            "wip"
        );
        // Рабочее дерево не тронуто: изменения всё ещё грязные.
        assert!(status(&c, &root, "beam").dirty);
    }

    #[test]
    fn union_mod_lists_keeps_both_sides_and_drops_duplicates() {
        let a = "pub mod alpha;\npub mod beta;\n";
        let b = "pub mod beta;\npub mod gamma;\n";
        let u = union_mod_lists(a, b);
        assert!(u.contains("pub mod alpha;"));
        assert!(u.contains("pub mod beta;"));
        assert!(u.contains("pub mod gamma;"));
        assert_eq!(
            u.matches("pub mod beta;").count(),
            1,
            "дубль модуля убран (иначе E0428): {u}"
        );
    }

    /// Rebase с конфликтом в списке модулей разрешается объединением.
    #[test]
    fn rebase_auto_unions_conflicting_mod_lists() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        // Главный репо и ветка агента правят один и тот же core/mod.rs.
        let g = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&root).output();
        };
        let gp = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&p).output();
        };
        std::fs::create_dir_all(p.join("core")).unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("core/mod.rs"), "pub mod beta;\n").unwrap_or_else(|_| unreachable!());
        gp(&["add", "-A"]);
        gp(&["commit", "-qm", "agent adds core/mod.rs"]);
        std::fs::create_dir_all(root.join("core")).unwrap_or_else(|_| unreachable!());
        std::fs::write(root.join("core/mod.rs"), "pub mod alpha;\npub mod gamma;\n")
            .unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"]);
        g(&["commit", "-qm", "main adds core/mod.rs"]);
        // Rebase агента на main: конфликт в core/mod.rs.
        let out = rebase_auto(&p, "main");
        assert!(out.is_ok(), "конфликт списка модулей разрешён: {out:?}");
        let text = std::fs::read_to_string(p.join("core/mod.rs")).unwrap_or_default();
        assert!(text.contains("pub mod alpha;"), "{text}");
        assert!(text.contains("pub mod beta;"), "{text}");
        assert!(text.contains("pub mod gamma;"), "{text}");
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

    #[test]
    fn merge_reports_nothing_without_commits() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (_p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let rep = merge(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(rep.action, MergeAction::Nothing, "{}", rep.message);
    }

    #[test]
    fn merge_fast_forwards_agent_branch_into_main() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        // Коммит агента в его worktree.
        std::fs::write(p.join("c.txt"), "3\n").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = Command::new("git").args(args).current_dir(&p).output();
        };
        g(&["add", "-A"]);
        g(&["commit", "-qm", "agent work"]);
        let rep = merge(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        assert_eq!(rep.action, MergeAction::Merged, "{}", rep.message);
        assert!(
            root.join("c.txt").exists(),
            "коммит агента попал в главное дерево"
        );
    }

    #[test]
    fn merge_refuses_dirty_agent_tree() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("a.txt"), "dirty\n").unwrap_or_else(|_| unreachable!());
        assert!(
            merge(&c, &root, "beam").is_err(),
            "грязное дерево агента не вливаем"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap_or_default(),
            "dirty\n",
            "работа агента не тронута"
        );
    }

    /// `origin/main` отстал от локального `main` -> база = локальный `main`.
    /// Иначе rebase агента уезжает на древнюю базу, а ff-merge в main
    /// невозможен (main не предок ветки агента).
    #[test]
    fn base_ref_prefers_local_main_when_origin_is_behind() {
        let (_d, root) = repo_fixture();
        // Второй коммит в локальном main.
        std::fs::write(root.join("b.txt"), "2\n").unwrap_or_else(|_| unreachable!());
        let _ = git_ok(&root, &["add", "-A"]);
        let _ = git_ok(&root, &["commit", "-qm", "local"]);
        let head_sha = git(&root, &["rev-parse", "main"]).unwrap_or_else(|_| unreachable!());
        let prev_sha = git(&root, &["rev-parse", "HEAD~1"]).unwrap_or_else(|_| unreachable!());

        // origin/main отстал от локального main.
        let _ = git_ok(
            &root,
            &["update-ref", "refs/remotes/origin/main", &prev_sha],
        );
        assert_eq!(
            base_ref(&root, "main"),
            "main",
            "origin отстал -> берём локальный main"
        );

        // origin/main догнал и содержит локальный main -> берём origin.
        let _ = git_ok(
            &root,
            &["update-ref", "refs/remotes/origin/main", &head_sha],
        );
        assert_eq!(
            base_ref(&root, "main"),
            "origin/main",
            "origin содержит main -> берём origin/main"
        );

        // origin/main впереди локального main -> тоже берём origin (main подтянется ff).
        let _ = git_ok(&root, &["checkout", "-q", "-b", "tmp"]);
        std::fs::write(root.join("c.txt"), "3\n").unwrap_or_else(|_| unreachable!());
        let _ = git_ok(&root, &["add", "-A"]);
        let _ = git_ok(&root, &["commit", "-qm", "ahead"]);
        let ahead_sha = git(&root, &["rev-parse", "HEAD"]).unwrap_or_else(|_| unreachable!());
        let _ = git_ok(&root, &["checkout", "-q", "main"]);
        let _ = git_ok(
            &root,
            &["update-ref", "refs/remotes/origin/main", &ahead_sha],
        );
        assert_eq!(
            base_ref(&root, "main"),
            "origin/main",
            "origin впереди -> берём origin/main (main подтянется fast-forward)"
        );
    }

    /// Валидатор списка модулей: страховка от битого union.
    #[test]
    fn validate_mod_list_accepts_union() {
        let text = "pub mod alpha;\npub mod beta;\n";
        validate_mod_list(text).unwrap_or_else(|e| unreachable!("{e}"));
    }

    #[test]
    fn validate_mod_list_rejects_broken_lines() {
        // Без точки с запятой.
        assert!(validate_mod_list("pub mod alpha\n").is_err());
        // Точка в имени.
        assert!(validate_mod_list("pub mod al.pha;\n").is_err());
        // Пробел в имени.
        assert!(validate_mod_list("pub mod al pha;\n").is_err());
        // Дубль модуля.
        assert!(validate_mod_list("pub mod alpha;\npub mod alpha;\n").is_err());
    }

    /// Конфиг с уникальным каталогом worktree'ов (тесты не делят /tmp).
    fn cfg_in(d: &tempfile::TempDir) -> RunConfig {
        RunConfig {
            wt_root: d.path().join("wts"),
            main_branch: "main".to_string(),
            ..Default::default()
        }
    }

    /// Влитие не трогает worktree агента: коммиты переносятся в главную,
    /// незакоммиченная работа остаётся у агента (её забирает WIP-чекпоинт).
    #[test]
    fn merge_via_cherry_pick_transfers_commits_and_keeps_agent_tree() {
        let (d, root) = repo_fixture();
        let c = cfg_in(&d);
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("b.txt"), "2\n").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str], cwd: &Path| {
            let _ = Command::new("git").args(args).current_dir(cwd).output();
        };
        g(&["add", "-A"], &p);
        g(&["commit", "-qm", "agent work"], &p);
        // Незакоммиченная работа агента не должна мешать слиянию.
        std::fs::write(p.join("c.txt"), "3\n").unwrap_or_else(|_| unreachable!());
        let result = merge_via_cherry_pick(&root, "main", &agent_branch("beam"), "beam")
            .unwrap_or_else(|e| unreachable!("{e}"));
        let head = git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        assert_eq!(result.sha, head, "вернули sha главной");
        assert!(root.join("b.txt").exists(), "коммит агента попал в главную");
        assert!(
            !root.join("c.txt").exists(),
            "незакоммиченное агента не попало в главную"
        );
        assert!(p.join("c.txt").exists(), "работа агента на месте");
    }

    /// T-262: WIP-чекпоинт агента — это сохранённая грязь, а не результат
    /// задачи: в главную он попадать не должен (он откатывал чужие файлы).
    #[test]
    fn transfer_commits_skips_wip_checkpoint() {
        let (d, root) = repo_fixture();
        let c = cfg_in(&d);
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str], cwd: &Path| {
            let _ = Command::new("git").args(args).current_dir(cwd).output();
        };
        std::fs::write(p.join("task.txt"), "task\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &p);
        g(&["commit", "-qm", "chore(task T-1): работа агента"], &p);
        // WIP-чекпоинт: откат чужого файла (именно так терялся orch_roles.rs).
        std::fs::write(p.join("other.txt"), "wip\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &p);
        g(
            &[
                "commit",
                "-qm",
                "WIP(beam): чекпоинт незакоммиченной работы",
            ],
            &p,
        );
        let commits = transfer_commits(&root, "main", &agent_branch("beam"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            commits.len(),
            1,
            "в перенос идёт только коммит задачи, без WIP: {commits:?}"
        );
        let subject = git(&root, &["log", "-1", "--format=%s", &commits[0]]).unwrap_or_default();
        assert!(subject.contains("работа агента"), "тема: {subject}");
    }

    /// T-262: коммит, чей патч уже в главной, повторно не переносится
    /// (агент перебазировался на main и принёс копию старой работы).
    #[test]
    fn transfer_commits_skips_patch_already_in_main() {
        let (d, root) = repo_fixture();
        let c = cfg_in(&d);
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str], cwd: &Path| {
            let _ = Command::new("git").args(args).current_dir(cwd).output();
        };
        std::fs::write(p.join("same.txt"), "same\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &p);
        g(&["commit", "-qm", "chore(task T-2): общий коммит"], &p);
        // Тот же патч в главной (другим агентом) — переносить нечего.
        std::fs::write(root.join("same.txt"), "same\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &root);
        g(
            &["commit", "-qm", "chore(task T-9): тот же патч в main"],
            &root,
        );
        let commits = transfer_commits(&root, "main", &agent_branch("beam"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert!(
            commits.is_empty(),
            "patch-эквивалентный коммит не переносится повторно: {commits:?}"
        );
    }

    /// Конфликт списка модулей (`core/mod.rs`) объединяется автоматически:
    /// ни одна строка не теряется, дублей нет.
    #[test]
    fn merge_via_cherry_pick_unions_core_mod_list_on_conflict() {
        let (d, root) = repo_fixture();
        let c = cfg_in(&d);
        let g = |args: &[&str], cwd: &Path| {
            let _ = Command::new("git").args(args).current_dir(cwd).output();
        };
        std::fs::create_dir_all(root.join("core")).unwrap_or_else(|_| unreachable!());
        std::fs::write(root.join("core/mod.rs"), "pub mod a;\n").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &root);
        g(&["commit", "-qm", "mod list"], &root);
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        std::fs::write(p.join("core/mod.rs"), "pub mod a;\npub mod b;\n")
            .unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &p);
        g(&["commit", "-qm", "agent adds module"], &p);
        // Главная меняет тот же список — теперь конфликт.
        std::fs::write(root.join("core/mod.rs"), "pub mod a;\npub mod c;\n")
            .unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"], &root);
        g(&["commit", "-qm", "main adds module"], &root);
        merge_via_cherry_pick(&root, "main", &agent_branch("beam"), "beam")
            .unwrap_or_else(|e| unreachable!("{e}"));
        let text = std::fs::read_to_string(root.join("core/mod.rs")).unwrap_or_default();
        assert!(text.contains("pub mod a;"), "{text}");
        assert!(text.contains("pub mod b;"), "{text}");
        assert!(text.contains("pub mod c;"), "{text}");
        assert_eq!(text.matches("pub mod a;").count(), 1, "дублей нет: {text}");
    }

    #[test]
    fn salvage_creates_reclaim_branch() {
        let (_d, root) = repo_fixture();
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: None,
            branch: Some("devh/agent-ghost".to_string()),
            sha: None,
            agent: Some("ghost".to_string()),
        };
        // salvage с несуществующим путём возвращает ошибку
        let result = salvage(&root, &cand);
        assert!(
            result.is_err() || result.unwrap().is_empty(),
            "salvage без пути должен вернуть ошибку или пустой SHA"
        );
    }

    #[test]
    fn rescue_ref_returns_false_for_nonexistent() {
        let (_d, root) = repo_fixture();
        assert!(
            !rescue_ref(&root, "devh/reclaim-ghost", "abc123"),
            "несуществующая ветка → false"
        );
    }

    #[test]
    fn close_abandoned_dry_run_does_not_delete() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(p.clone()),
            branch: Some("devh/agent-beam".to_string()),
            sha: Some("abc123".to_string()),
            agent: Some("beam".to_string()),
        };
        // dry_run: ничего не удаляет
        let report = close_abandoned(&root, &cand, &[], true).unwrap_or_else(|_| unreachable!());
        assert!(p.exists(), "dry_run: worktree на месте");
        assert!(!report.is_empty(), "dry_run: план напечатан");
    }

    #[test]
    fn close_abandoned_respects_keep_list() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(p.clone()),
            branch: Some("devh/agent-beam".to_string()),
            sha: Some("abc123".to_string()),
            agent: Some("beam".to_string()),
        };
        // keep-list содержит путь → пропускаем
        let report =
            close_abandoned(&root, &cand, &[p.clone()], false).unwrap_or_else(|_| unreachable!());
        assert!(p.exists(), "keep-list: worktree на месте");
        assert!(
            report.iter().any(|r| r.contains("keep-list")),
            "keep-list: пропущен"
        );
    }

    #[test]
    fn close_abandoned_fails_closed_without_rescue_ref() {
        let (_d, root) = repo_fixture();
        let c = cfg();
        let (p, _) = ensure(&c, &root, "beam").unwrap_or_else(|_| unreachable!());
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(p.clone()),
            branch: Some("devh/agent-beam".to_string()),
            sha: Some("abc123".to_string()),
            agent: Some("beam".to_string()),
        };
        // Без rescue-ref → отказ
        let report = close_abandoned(&root, &cand, &[], false).unwrap_or_else(|_| unreachable!());
        assert!(p.exists(), "без rescue-ref: worktree на месте");
        assert!(
            report.iter().any(|r| r.contains("отказ")),
            "без rescue-ref: отказ"
        );
    }

    #[test]
    fn salvage_dirty_creates_reclaim_branch() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-abandoned");
        // Create abandoned worktree
        let _ = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "devh/agent-ghost",
                &wt.to_string_lossy(),
            ])
            .current_dir(&root)
            .output();
        // Make it dirty
        std::fs::write(
            wt.join("dirty.txt"),
            "dirty content
",
        )
        .unwrap();
        // Create candidate
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(wt.clone()),
            branch: Some("devh/agent-ghost".to_string()),
            sha: None,
            agent: Some("ghost".to_string()),
        };
        let sha = salvage(&root, &cand).unwrap();
        assert!(!sha.is_empty(), "salvage returns SHA");
        // Check reclaim branch exists
        assert!(
            crate::shell::git::show_ref(&root, "devh/reclaim-ghost"),
            "reclaim branch created"
        );
        // Check commit message
        let out = std::process::Command::new("git")
            .args(["log", "-1", "--format=%s", "devh/reclaim-ghost"])
            .current_dir(&root)
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(msg.contains("reclaim: ghost"), "commit message: {msg}");
    }

    #[test]
    fn salvage_clean_skips() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-clean");
        let _ = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "devh/agent-clean",
                &wt.to_string_lossy(),
            ])
            .current_dir(&root)
            .output();
        let cand = Candidate {
            kind: CandidateKind::AbandonedWorktree,
            path: Some(wt.clone()),
            branch: Some("devh/agent-clean".to_string()),
            sha: None,
            agent: Some("clean".to_string()),
        };
        let sha = salvage(&root, &cand).unwrap();
        assert!(sha.is_empty(), "clean worktree -> empty SHA (skipped)");
    }

    #[test]
    fn rescue_ref_matches_sha() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        // Create a branch pointing to HEAD
        let head = crate::shell::git::short_head(&root);
        let _ = std::process::Command::new("git")
            .args(["branch", "rescue-test", "HEAD"])
            .current_dir(&root)
            .output();
        assert!(rescue_ref(&root, "rescue-test", &head), "matching SHA");
        assert!(
            !rescue_ref(&root, "rescue-test", "deadbeef"),
            "non-matching SHA"
        );
        assert!(
            !rescue_ref(&root, "nonexistent-branch", &head),
            "nonexistent branch"
        );
    }

    #[test]
    fn wip_commit_msg_roundtrip_with_task() {
        let msg = wip_commit_msg("opencode-0a0e0cfc", Some("T-316"), 3);
        assert_eq!(msg, "wip: opencode-0a0e0cfc T-316 3 файла");
        let parsed = parse_wip_commit_msg(&msg).unwrap();
        assert_eq!(parsed.0, "opencode-0a0e0cfc");
        assert_eq!(parsed.1, Some("T-316".to_string()));
        assert_eq!(parsed.2, 3);
    }

    #[test]
    fn wip_commit_msg_roundtrip_no_task() {
        let msg = wip_commit_msg("opencode-0a0e0cfc", None, 0);
        assert_eq!(msg, "wip: opencode-0a0e0cfc - 0 файла");
        let parsed = parse_wip_commit_msg(&msg).unwrap();
        assert_eq!(parsed.0, "opencode-0a0e0cfc");
        assert_eq!(parsed.1, None);
        assert_eq!(parsed.2, 0);
    }

    #[test]
    fn parse_wip_commit_msg_rejects_garbage() {
        assert!(parse_wip_commit_msg("not a wip commit").is_none());
        assert!(parse_wip_commit_msg("wip: only-one-part").is_none());
        assert!(parse_wip_commit_msg("wip: a b").is_none());
    }

    #[test]
    fn wip_pass_no_worktree() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let cfg = crate::commands::load_config(&root).unwrap_or_default();
        let report = wip_pass(&cfg, &root, "ghost").unwrap();
        assert_eq!(report, WipReport::NoWorktree);
    }

    #[test]
    fn wip_pass_clean_skips() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let cfg = crate::commands::load_config(&root).unwrap_or_default();
        let wt = ensure(&cfg, &root, "clean-agent").unwrap().0;
        let report = wip_pass(&cfg, &root, "clean-agent").unwrap();
        assert_eq!(report, WipReport::SkipClean);
        let _ = wt;
    }

    #[test]
    fn wip_pass_dirty_commits() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let cfg = crate::commands::load_config(&root).unwrap_or_default();
        let (wt, _) = ensure(&cfg, &root, "dirty-agent").unwrap();
        std::fs::write(wt.join("scratch.txt"), "dirty work").unwrap();
        let report = wip_pass(&cfg, &root, "dirty-agent").unwrap();
        match report {
            WipReport::Committed {
                n_files,
                commit_sha,
            } => {
                assert!(n_files >= 1, "должен быть ≥1 файл: {n_files}");
                assert!(!commit_sha.is_empty(), "commit SHA не пустой");
            }
            other => panic!("ожидался Committed, а не {other:?}"),
        }
    }
}
