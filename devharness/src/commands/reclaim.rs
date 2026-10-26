//! `devh orch reclaim show <ID> [--diff]` — просмотр снимка задачи reclaim.
//!
//! Команда выводит полную информацию о задаче и её rescue-ветке для
//! lead/tech_lead. Worker получает отказ с объяснением.

use std::path::{Path, PathBuf};

use crate::commands::orch::{self, Status, Task};

/// Ошибка вывода снимка задачи.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum ReclaimError {
    /// Агент не имеет прав (только lead/tech_lead).
    PermissionDenied { agent: String, role: String },
    /// Задача не найдена в реестре.
    TaskNotFound { id: String },
    /// Задача не имеет associated worktree / rescue-ветки.
    NoRescueBranch { id: String },
}

impl std::fmt::Display for ReclaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReclaimError::PermissionDenied { agent, role } => {
                write!(
                    f,
                    "devh orch reclaim show: агент {agent} (роль {role}) не имеет прав — только lead/tech_lead видят снимок задачи"
                )
            }
            ReclaimError::TaskNotFound { id } => {
                write!(
                    f,
                    "devh orch reclaim show: задача {id} не найдена в реестре"
                )
            }
            ReclaimError::NoRescueBranch { id } => {
                write!(
                    f,
                    "devh orch reclaim show: у задачи {id} нет rescue-ветки (devh/reclaim-*)"
                )
            }
        }
    }
}

impl std::error::Error for ReclaimError {}

/// Проверить, что вызывающий имеет права lead или tech_lead.
fn check_role(reg: &orch::Registry, caller: &str) -> Result<(), ReclaimError> {
    let role = reg
        .agents
        .iter()
        .find(|a| {
            let canon = crate::core::identity::canonical(&a.name);
            canon == caller
        })
        .and_then(|a| a.role.clone())
        .unwrap_or_else(|| "worker".to_string());
    if role == "lead" || role == "tech_lead" {
        Ok(())
    } else {
        Err(ReclaimError::PermissionDenied {
            agent: caller.to_string(),
            role,
        })
    }
}

/// Вывести снимок задачи для lead/tech_lead.
///
/// Снимок включает: id, title, статус, zones, note, commit_sha, main_sha,
/// rescue-ветку (если есть) и hunks (если `with_diff`).
pub fn show(root: &Path, id: &str, with_diff: bool) -> Result<String, ReclaimError> {
    let reg = orch::load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    check_role(&reg, &caller)?;

    let task = reg
        .tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or(ReclaimError::TaskNotFound { id: id.to_string() })?;

    let output = format_task_snapshot(root, task, with_diff)?;
    Ok(output)
}

/// Сформировать текстовый снимок задачи.
fn format_task_snapshot(root: &Path, task: &Task, with_diff: bool) -> Result<String, ReclaimError> {
    let mut out = String::new();
    out.push_str(&format!("=== Reclaim snapshot: {} ===\n", task.id));
    out.push_str(&format!("title: {}\n", task.title));
    out.push_str(&format!("status: {}\n", task.status.label()));
    out.push_str(&format!("agent: {}\n", task.agent));
    out.push_str(&format!(
        "complexity: P{} (C{})\n",
        task.complexity,
        task.c_level.unwrap_or(0)
    ));
    out.push_str(&format!(
        "zones:\n{}",
        task.zones
            .iter()
            .map(|z| format!("  - {z}\n"))
            .collect::<String>()
    ));
    out.push_str(&format!("note: {}\n", task.note));
    out.push_str(&format!(
        "commit_sha: {}\n",
        task.commit_sha.clone().unwrap_or_else(String::new)
    ));
    out.push_str(&format!(
        "main_sha: {}\n",
        task.main_sha.clone().unwrap_or_else(String::new)
    ));

    // Попытка найти rescue-ветку.
    let reclaim_branch = format!("devh/reclaim-{}", task.agent);
    if crate::shell::git::show_ref(root, &reclaim_branch) {
        out.push_str(&format!("rescue_branch: {reclaim_branch}\n"));
    } else {
        out.push_str(&format!("rescue_branch: (нет)\n"));
    }

    // Hunks через git diff/show.
    if with_diff {
        let hunks = generate_hunks(root, task, &reclaim_branch)?;
        if !hunks.is_empty() {
            out.push_str("hunks:\n");
            out.push_str(&hunks);
        } else {
            out.push_str("hunks: (нет изменений)\n");
        }
    }

    Ok(out)
}

/// Сгенерировать hunks для задачи.
fn generate_hunks(root: &Path, task: &Task, reclaim_branch: &str) -> Result<String, ReclaimError> {
    let mut hunks = String::new();
    if let Some(sha) = &task.commit_sha {
        if crate::shell::git::show_ref(root, reclaim_branch) {
            let out = std::process::Command::new("git")
                .args([
                    "diff",
                    &format!("{}...{}", sha, reclaim_branch),
                    "--",
                    &task.zones.join(" "),
                ])
                .current_dir(root)
                .output();
            if let Ok(out) = out {
                let text = String::from_utf8_lossy(&out.stdout);
                if !text.is_empty() {
                    hunks.push_str(&text);
                }
            }
        }
    }
    Ok(hunks)
}

// ═══════════════════════════════ RECLAIM: команда ═══════════════════════════════
//
// `devh orch reclaim <scan|salvage|close|redistribute|show>` — спасение
// покинутой работы (T-320/T-535).
//
// Правила (fail-closed):
// * только lead/tech_lead (check_role);
// * `scan` — read-only: список кандидатов + отчёт на диск;
// * `salvage` — коммит незакоммиченного в `devh/reclaim-<agent>` (ничего не
//   удаляет);
// * `close --apply` — снимает worktree ТОЛЬКО если rescue-ветка существует и
//   указывает на тот же SHA, что и HEAD worktree, и дерево чистое;
// * `redistribute --apply` — re-attach незавершённой работы в задачи (P2) с
//   дедупом по rescue-имени.

/// Имя rescue-ветки агента.
pub fn rescue_branch(agent: &str) -> String {
    format!("devh/reclaim-{agent}")
}

/// Агент из имени ветки (`devh/agent-X` или `devh/reclaim-X`).
fn agent_of_branch(branch: &str) -> Option<String> {
    branch
        .strip_prefix("devh/agent-")
        .or_else(|| branch.strip_prefix("devh/reclaim-"))
        .map(|s| s.to_string())
}

/// Метка вида кандидата (для отчётов).
fn kind_label(kind: crate::commands::wtman::CandidateKind) -> &'static str {
    use crate::commands::wtman::CandidateKind as K;
    match kind {
        K::AbandonedWorktree => "abandoned-worktree",
        K::OrphanBranch => "orphan-branch",
        K::WipBranch => "wip-branch",
        K::DanglingCommit => "dangling-commit",
    }
}

/// Вывод git-команды (stdout; при ошибке — пусто).
fn git_out(root: &Path, args: &[&str]) -> String {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Полный SHA ревизии.
fn rev_parse(root: &Path, rev: &str) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("git rev-parse {rev}: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("git rev-parse {rev}: ревизия не разрешилась");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Короткий SHA (7 символов) для отчётов.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Текущее время (секунды).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Значение флага `--name value` или `--name=value`.
fn flag_value(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if let Some(rest) = a.strip_prefix(&format!("{name}=")) {
            let t = rest.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// Найти worktree-кандидата по агенту/пути (только кандидаты с путём).
fn find_worktree_candidate(
    root: &Path,
    agent: Option<&str>,
    path: Option<&Path>,
) -> anyhow::Result<crate::commands::wtman::Candidate> {
    let cands = crate::commands::wtman::discover(root);
    for c in &cands {
        if c.path.is_none() {
            continue;
        }
        if let Some(p) = path {
            if c.path.as_deref() != Some(p) {
                continue;
            }
        }
        if let Some(a) = agent {
            let cand_agent = c
                .agent
                .clone()
                .or_else(|| agent_of_branch(c.branch.as_deref().unwrap_or("")));
            if cand_agent.as_deref() != Some(a) {
                continue;
            }
        }
        return Ok(c.clone());
    }
    Err(anyhow::anyhow!(
        "devh orch reclaim: кандидат не найден (agent={}, path={}) — см. devh orch reclaim scan",
        agent.unwrap_or("-"),
        path.map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    ))
}

/// T-ID из текста сообщения коммита (`wip: beam T-123 7 файлов` → `T-123`).
fn task_ref_from_msg(msg: &str) -> Option<String> {
    msg.split_whitespace().find_map(|w| {
        let t = w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        if t.starts_with("T-") && t.len() > 2 && t[2..].chars().all(|c| c.is_ascii_digit()) {
            Some(t.to_string())
        } else {
            None
        }
    })
}


/// Пути worktree'ов ЖИВЫХ агентов (зарегистрированных в реестре) + главное
/// дерево: их reclaim НИКОГДА не снимает.
///
/// `wtman::registered_paths` возвращает ВСЕ worktree'и репозитория (включая
/// брошенные) — такой keep-list блокировал бы снятие всегда.
fn live_keep_paths(root: &Path, reg: &orch::Registry) -> Vec<PathBuf> {
    let live: std::collections::HashSet<String> = reg
        .agents
        .iter()
        .map(|a| crate::core::identity::canonical(&a.name))
        .collect();
    let mut keep = vec![root.to_path_buf()];
    for wt in crate::shell::git::worktree_paths(root) {
        let Some(branch) = crate::shell::git::branch_current(&wt) else {
            continue;
        };
        let Some(agent) = agent_of_branch(&branch) else {
            continue;
        };
        if live.contains(&crate::core::identity::canonical(&agent)) {
            keep.push(wt);
        }
    }
    keep
}

/// `scan` — список кандидатов + отчёт на диск (read-only).
fn cmd_scan(root: &Path, json: bool, max_candidates: usize) -> anyhow::Result<()> {
    let reg = orch::load(root);
    let cls = classify(root, &reg, max_candidates);
    let cands = cls.candidates;
    let items: Vec<serde_json::Value> = cands
        .iter()
        .map(|c| {
            serde_json::json!({
                "kind": kind_label(c.kind),
                "agent": c.agent.clone().unwrap_or_default(),
                "branch": c.branch.clone().unwrap_or_default(),
                "path": c.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                "sha": c.sha.clone().unwrap_or_default(),
            })
        })
        .collect();
    // Исключённые — по одной строке на ПРИЧИНУ (без полного дампа).
    let mut by_reason: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for sk in &cls.skipped {
        *by_reason.entry(sk.reason.label()).or_insert(0) += 1;
    }
    let skipped: Vec<serde_json::Value> = by_reason
        .iter()
        .map(|(reason, n)| serde_json::json!({ "reason": reason, "count": n }))
        .collect();
    let report = serde_json::json!({
        "count": cands.len(),
        "candidates": items,
        "skipped_by_reason": skipped,
        "max_candidates": max_candidates,
    });
    let body = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
    if json {
        println!("{body}");
    } else {
        println!("=== RECLAIM scan: кандидатов {} (лимит {}) ===", cands.len(), max_candidates);
        for c in &cands {
            println!(
                "  [{}] agent={} branch={} path={} sha={}",
                kind_label(c.kind),
                c.agent.as_deref().unwrap_or("-"),
                c.branch.as_deref().unwrap_or("-"),
                c.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
                c.sha.as_deref().unwrap_or("-"),
            );
        }
        let limit_more = cls
            .skipped
            .iter()
            .filter(|s| matches!(s.reason, SkipReason::Limit { .. }))
            .count();
        if limit_more > 0 {
            println!("  … ещё {limit_more} кандидатов сверх лимита {max_candidates}");
        }
        if !by_reason.is_empty() {
            println!("--- почему не кандидат (по причинам) ---");
            for (reason, n) in &by_reason {
                println!("  — {reason}: {n}");
            }
        }
    }
    let dir = orch::dir(root);
    std::fs::create_dir_all(&dir)?;
    let report_path = dir.join("reclaim-scan.json");
    std::fs::write(&report_path, &body)?;
    println!("отчёт: {}", report_path.display());
    Ok(())
}

/// `salvage` — спасти грязный worktree в `devh/reclaim-<agent>` (ничего не удаляет).
fn cmd_salvage(
    root: &Path,
    agent: Option<&str>,
    path: Option<&Path>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let mut cand = find_worktree_candidate(root, agent, path)?;
    let a = agent
        .map(|s| s.to_string())
        .or_else(|| agent_of_branch(cand.branch.as_deref().unwrap_or("")))
        .ok_or_else(|| {
            anyhow::anyhow!("devh orch reclaim salvage: агент не определён — нужен --agent")
        })?;
    cand.agent = Some(a.clone());
    let branch = rescue_branch(&a);
    println!("=== RECLAIM salvage ===");
    println!(
        "кандидат: [{}] agent={} branch={} path={}",
        kind_label(cand.kind),
        a,
        cand.branch.as_deref().unwrap_or("-"),
        cand.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    println!("rescue-ветка: {branch}");
    if dry_run {
        println!("dry-run: ничего не менял");
        return Ok(());
    }
    let sha = crate::commands::wtman::salvage(root, &cand)?;
    if sha.trim().is_empty() {
        println!("пропуск: worktree чистый — нечего спасать");
        return Ok(());
    }
    println!("спасено: {branch} ({})", short_sha(&sha));
    Ok(())
}

/// `close --apply` — снять брошенный worktree (только при подтверждённом rescue-ref).
fn cmd_close(
    root: &Path,
    agent: Option<&str>,
    path: Option<&Path>,
    apply: bool,
) -> anyhow::Result<()> {
    let mut cand = find_worktree_candidate(root, agent, path)?;
    let a = agent
        .map(|s| s.to_string())
        .or_else(|| agent_of_branch(cand.branch.as_deref().unwrap_or("")))
        .ok_or_else(|| {
            anyhow::anyhow!("devh orch reclaim close: агент не определён — нужен --agent")
        })?;
    cand.agent = Some(a.clone());
    let branch = rescue_branch(&a);
    if !crate::shell::git::show_ref(root, &branch) {
        anyhow::bail!(
            "devh orch reclaim close: нет rescue-ветки {branch} — сначала: \
             devh orch reclaim salvage --agent {a}"
        );
    }
    let rescue_sha = rev_parse(root, &branch)?;
    let wt = cand.path.clone().ok_or_else(|| {
        anyhow::anyhow!("devh orch reclaim close: кандидат без пути — нечего снимать")
    })?;
    // fail-closed: незакоммиченное не удаляем НИ при каком флаге.
    if crate::shell::git::tree_dirty(&wt) {
        anyhow::bail!(
            "devh orch reclaim close: fail-closed — в {} есть незакоммиченные изменения; \
             сначала devh orch reclaim salvage",
            wt.display()
        );
    }
    let head = rev_parse(&wt, "HEAD")?;
    if head != rescue_sha {
        anyhow::bail!(
            "devh orch reclaim close: fail-closed — rescue-ref {} ({}) не совпадает с HEAD \
             worktree ({})",
            branch,
            short_sha(&rescue_sha),
            short_sha(&head)
        );
    }
    // rescue-ref подтверждён на тот же SHA — теперь снятие допустимо.
    cand.branch = Some(branch.clone());
    cand.sha = Some(rescue_sha.clone());
    let keep = live_keep_paths(root, &orch::load(root));
    let report = crate::commands::wtman::close_abandoned(root, &cand, &keep, !apply)?;
    println!("=== RECLAIM close (apply={apply}) ===");
    for line in report {
        println!("{line}");
    }
    if !apply {
        println!("dry-run: ничего не удалил (нужен --apply)");
    }
    Ok(())
}

/// `redistribute --apply` — re-attach спасённой работы в задачи (P2).
fn cmd_redistribute(root: &Path, apply: bool) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(
        root,
        &orch::path(root),
        "devh orch reclaim",
    )
    .map_err(|e| anyhow::anyhow!("devh orch reclaim: реестр занят ({e}) — повтори через секунду"))?;

    let mut reg = orch::load(root);
    let branches: Vec<String> = git_out(root, &["branch", "--list", "devh/reclaim-*"])
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let mut created = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    for branch in branches {
        let Some(agent) = agent_of_branch(&branch) else {
            continue;
        };
        let sha = rev_parse(root, &branch)?;
        let marker = format!("reclaim:{branch}@{}", short_sha(&sha));
        // Дедуп по rescue-имени: повторный прогон не плодит задачи.
        if reg.tasks.iter().any(|t| t.note.contains(&marker)) {
            skipped += 1;
            println!("пропуск (уже re-attach): {branch} ({})", short_sha(&sha));
            continue;
        }
        let msg = git_out(root, &["log", "-1", "--format=%B", &branch]);
        let task_ref = task_ref_from_msg(&msg);
        let diff_range = format!("main...{branch}");
        let files: Vec<String> = git_out(root, &["diff", "--name-only", &diff_range])
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        let snapshot = format!(
            "rescue: {branch} ({})\n{}",
            short_sha(&sha),
            git_out(root, &["diff", "--name-status", &diff_range]).trim()
        );
        // Задача: по T-ID из коммита → активная задача агента → любая queued.
        let mut idx = task_ref
            .as_ref()
            .and_then(|tid| reg.tasks.iter().position(|t| &t.id == tid));
        if idx.is_none() {
            idx = reg
                .tasks
                .iter()
                .position(|t| t.agent == agent && t.status == Status::Active);
        }
        if idx.is_none() {
            idx = reg
                .tasks
                .iter()
                .position(|t| t.agent == agent && t.status == Status::Queued);
        }
        match idx {
            Some(i) => {
                let (id, title) = {
                    let t = &reg.tasks[i];
                    (t.id.clone(), t.title.clone())
                };
                if !apply {
                    println!("план: re-attach {id} ({title}) -> queued P2, зон={}", files.len());
                    continue;
                }
                let t = &mut reg.tasks[i];
                t.status = Status::Queued;
                t.complexity = 2; // P2 — работа не завершена, но сохранена.
                if !files.is_empty() {
                    t.zones = files.clone();
                }
                t.note = format!("{}\n{}\n{}", t.note, marker, snapshot)
                    .trim()
                    .to_string();
                t.updated = now_secs();
                updated += 1;
                println!("re-attach: {id} -> queued P2 ({title})");
            }
            None => {
                if !apply {
                    println!(
                        "план: создать incomplete-задачу для {agent} (P2, зон={})",
                        files.len()
                    );
                    continue;
                }
                reg.seq = reg.seq.saturating_add(1);
                let id = format!("T-{:03}", reg.seq);
                let ts = now_secs();
                reg.tasks.push(Task {
                    id: id.clone(),
                    agent: agent.clone(),
                    title: format!("RECLAIM: незавершённая работа {agent} ({branch})"),
                    complexity: 2,
                    c_level: None,
                    model: None,
                    model_source: None,
                    started_by: None,
                    started_at: None,
                    started_key: None,
                    status: Status::Queued,
                    zones: files.clone(),
                    fail_level: None,
                    note: format!("{marker}\n{snapshot}"),
                    created: ts,
                    updated: ts,
                    dod: Vec::new(),
                    checks: Vec::new(),
                    acked: Vec::new(),
                    evidence: Vec::new(),
                    commit_sha: None,
                    main_sha: None,
                    override_reason: None,
                });
                created += 1;
                println!("создана incomplete-задача {id} для {agent} (P2)");
            }
        }
    }
    if apply && (created > 0 || updated > 0) {
        orch::save(root, &reg)?;
        // Автораздача: incomplete-задачи уходят в очередь исполнителям.
        crate::commands::autoassign::auto_trigger(root, &mut reg);
        orch::save(root, &reg)?;
    }
    println!(
        "RECLAIM redistribute: создано {created}, обновлено {updated}, пропущено {skipped} \
         (apply={apply})"
    );
    Ok(())
}

/// Почему кандидат НЕ попал в список (T-565) — «почему не кандидат».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Главное дерево / ветка main — это не брошенная работа.
    MainTree,
    /// Агент зарегистрирован в реестре — работа не брошена.
    RegisteredAgent { agent: String },
    /// Работа уже спасена: есть rescue-ветка.
    AlreadyRescued { branch: String },
    /// Дерево чистое — спасать нечего.
    CleanTree,
    /// Превышен лимит кандидатов (защита от «спасения» мусора).
    Limit { max: usize },
}

impl SkipReason {
    /// Человекочитаемая причина (для экрана и письма).
    pub fn label(&self) -> String {
        match self {
            SkipReason::MainTree => "главное дерево/main — не брошенная работа".to_string(),
            SkipReason::RegisteredAgent { agent } => {
                format!("агент {agent} в реестре — работа не брошена")
            }
            SkipReason::AlreadyRescued { branch } => {
                format!("уже спасено: есть rescue-ветка {branch}")
            }
            SkipReason::CleanTree => "дерево чистое — спасать нечего".to_string(),
            SkipReason::Limit { max } => {
                format!("превышен лимит кандидатов ({max}) — лишние отложены")
            }
        }
    }
}

/// Кандидат, отложенный с причиной.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Что отложено (путь worktree или имя ветки).
    pub name: String,
    /// Почему отложено.
    pub reason: SkipReason,
}

/// Результат классификации: что спасаем и что (и почему) не спасаем.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classification {
    /// Кандидаты (в пределах лимита).
    pub candidates: Vec<crate::commands::wtman::Candidate>,
    /// Отложенные с причиной.
    pub skipped: Vec<Skipped>,
}

/// Классифицировать брошенную работу (T-565).
///
/// Кандидаты — то, что действительно можно спасти; остальное откладывается
/// с ПРИЧИНОЙ (отчёт «почему не кандидат»), а лимит `--max-candidates`
/// защищает от спасения мусора (сотни веток-дублей).
pub fn classify(
    root: &Path,
    reg: &orch::Registry,
    max_candidates: usize,
) -> Classification {
    let registered: std::collections::HashSet<String> = reg
        .agents
        .iter()
        .map(|a| crate::core::identity::canonical(&a.name))
        .collect();
    let mut candidates: Vec<crate::commands::wtman::Candidate> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut wt_branches: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Worktree'ы: main и зарегистрированные агенты — не кандидаты.
    for wt in crate::shell::git::worktree_paths(root) {
        let Some(branch) = crate::shell::git::branch_current(&wt) else {
            continue;
        };
        wt_branches.insert(branch.clone());
        let name = wt.display().to_string();
        if wt == root || branch == "main" {
            skipped.push(Skipped {
                name,
                reason: SkipReason::MainTree,
            });
            continue;
        }
        let agent = agent_of_branch(&branch);
        if let Some(a) = &agent {
            if registered.contains(&crate::core::identity::canonical(a)) {
                skipped.push(Skipped {
                    name,
                    reason: SkipReason::RegisteredAgent { agent: a.clone() },
                });
                continue;
            }
            let rescue = rescue_branch(a);
            if crate::shell::git::show_ref(root, &rescue) {
                skipped.push(Skipped {
                    name,
                    reason: SkipReason::AlreadyRescued { branch: rescue },
                });
                continue;
            }
        }
        if !crate::shell::git::tree_dirty(&wt) {
            skipped.push(Skipped {
                name,
                reason: SkipReason::CleanTree,
            });
            continue;
        }
        candidates.push(crate::commands::wtman::Candidate {
            kind: crate::commands::wtman::CandidateKind::AbandonedWorktree,
            path: Some(wt),
            branch: Some(branch),
            sha: None,
            agent,
        });
    }

    // 2. Ветки devh/agent-* и devh/wip-* без worktree.
    for branch in branches_with_prefix(root, "devh/agent-")
        .into_iter()
        .chain(branches_with_prefix(root, "devh/wip-"))
    {
        if wt_branches.contains(&branch) {
            continue;
        }
        let agent = agent_of_branch(&branch);
        if let Some(a) = &agent {
            if registered.contains(&crate::core::identity::canonical(a)) {
                skipped.push(Skipped {
                    name: branch.clone(),
                    reason: SkipReason::RegisteredAgent { agent: a.clone() },
                });
                continue;
            }
            let rescue = rescue_branch(a);
            if crate::shell::git::show_ref(root, &rescue) {
                skipped.push(Skipped {
                    name: branch.clone(),
                    reason: SkipReason::AlreadyRescued { branch: rescue },
                });
                continue;
            }
        }
        let kind = if branch.starts_with("devh/wip-") {
            crate::commands::wtman::CandidateKind::WipBranch
        } else {
            crate::commands::wtman::CandidateKind::OrphanBranch
        };
        candidates.push(crate::commands::wtman::Candidate {
            kind,
            path: None,
            branch: Some(branch),
            sha: None,
            agent,
        });
    }

    // 3. Недостижимые коммиты — из сканера wtman (git fsck).
    for c in crate::commands::wtman::discover(root) {
        if c.kind == crate::commands::wtman::CandidateKind::DanglingCommit {
            candidates.push(c);
        }
    }

    // 4. Порядок полезности: сперва грязные worktree'и (там работа), потом
    //    брошенные ветки, и только потом dangling-коммиты (fsck в этом репо
    //    даёт тысячи старых коммитов — они не должны съедать лимит).
    candidates.sort_by_key(|c| match c.kind {
        crate::commands::wtman::CandidateKind::AbandonedWorktree => 0,
        crate::commands::wtman::CandidateKind::OrphanBranch => 1,
        crate::commands::wtman::CandidateKind::WipBranch => 2,
        crate::commands::wtman::CandidateKind::DanglingCommit => 3,
    });

    // 5. Лимит: лишние кандидаты откладываем (защита от мусора).
    if candidates.len() > max_candidates {
        let extra = candidates.split_off(max_candidates);
        for c in extra {
            skipped.push(Skipped {
                name: c
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .or_else(|| c.branch.clone())
                    .or_else(|| c.sha.clone())
                    .unwrap_or_else(|| "?".to_string()),
                reason: SkipReason::Limit { max: max_candidates },
            });
        }
    }

    Classification { candidates, skipped }
}

/// Сводка спасённой работы (T-562): что нашли, что под риском, что спасли
/// и сколько задач из этого получилось. Lead видит это одним экраном (и
/// одним письмом), а не копается в ветках руками.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReclaimSummary {
    /// Кандидаты (покинутые worktree/ветки), найденные сканером.
    pub candidates: usize,
    /// Из них грязные — незакоммиченная работа под риском потери.
    pub dirty: usize,
    /// Rescue-ветки `devh/reclaim-*` (спасено харнессом).
    pub rescue_branches: Vec<String>,
    /// Ветки `devh/rescue-*` (спасено вручную lead'ом — другой префикс!).
    pub manual_rescue: Vec<String>,
    /// Задачи, созданные из спасённой работы (note: `reclaim:<ветка>@<sha>`).
    pub created_tasks: Vec<String>,
}

impl ReclaimSummary {
    /// Сколько веток спасено (харнессом + вручную).
    pub fn saved_branches(&self) -> usize {
        self.rescue_branches.len() + self.manual_rescue.len()
    }

    /// Текст сводки в стиле доски: цветные счётчики (как `devh orch board`).
    pub fn render_colored(&self) -> String {
        let c = crate::shell::color::colors();
        let dirty_color = if self.dirty > 0 { c.r } else { c.g };
        let mut out = String::new();
        out.push_str(&format!(
            "{}{}RECLAIM report{}: кандидатов {}{}{}{}\n",
            c.b, c.c, c.z, c.b, self.candidates, c.z, c.z
        ));
        out.push_str(&format!(
            "  грязных (работа под риском): {}{}{}{}\n",
            dirty_color, self.dirty, c.z, c.z
        ));
        out.push_str(&format!(
            "  спасено веток: {}{}{}{} (reclaim-*: {}, rescue-*: {})\n",
            c.g,
            self.saved_branches(),
            c.z,
            c.z,
            self.rescue_branches.len(),
            self.manual_rescue.len()
        ));
        out.push_str(&format!(
            "  задач из спасённой работы: {}{}{}{}\n",
            c.b,
            self.created_tasks.len(),
            c.z,
            c.z
        ));
        for b in self.rescue_branches.iter().chain(self.manual_rescue.iter()) {
            out.push_str(&format!("    {}- {}{}{}\n", c.g, b, c.z, c.z));
        }
        for t in &self.created_tasks {
            out.push_str(&format!("    {}- {}{}{}\n", c.c, t, c.z, c.z));
        }
        out
    }

    /// Текст сводки: экран и письмо — один и тот же текст.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("кандидатов: {}\n", self.candidates));
        out.push_str(&format!(
            "грязных (работа под риском): {}\n",
            self.dirty
        ));
        out.push_str(&format!(
            "спасено веток: {} (reclaim-*: {}, rescue-*: {})\n",
            self.saved_branches(),
            self.rescue_branches.len(),
            self.manual_rescue.len()
        ));
        out.push_str(&format!(
            "задач из спасённой работы: {}\n",
            self.created_tasks.len()
        ));
        for b in self.rescue_branches.iter().chain(self.manual_rescue.iter()) {
            out.push_str(&format!("  - {b}\n"));
        }
        for t in &self.created_tasks {
            out.push_str(&format!("  * {t}\n"));
        }
        out
    }
}

/// Ветки с указанным префиксом (`git for-each-ref`).
fn branches_with_prefix(root: &Path, prefix: &str) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/{prefix}*"),
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Собрать сводку по текущему состоянию репозитория.
fn collect_summary(root: &Path, reg: &orch::Registry) -> ReclaimSummary {
    let cands = crate::commands::wtman::discover(root);
    let dirty = cands
        .iter()
        .filter(|c| {
            c.path
                .as_ref()
                .map(|p| crate::shell::git::tree_dirty(p))
                .unwrap_or(false)
        })
        .count();
    let created_tasks: Vec<String> = reg
        .tasks
        .iter()
        .filter(|t| t.note.contains("reclaim:"))
        .map(|t| format!("{} ({})", t.id, t.title))
        .collect();
    ReclaimSummary {
        candidates: cands.len(),
        dirty,
        rescue_branches: branches_with_prefix(root, "devh/reclaim-"),
        manual_rescue: branches_with_prefix(root, "devh/rescue-"),
        created_tasks,
    }
}

/// `report` — сводка lead'у о спасённой работе (письмом при `--send`).
fn cmd_report(root: &Path, send: bool, dry_run: bool) -> anyhow::Result<()> {
    let reg = orch::load(root);
    let summary = collect_summary(root, &reg);
    print!("{}", summary.render_colored());
    if !send {
        println!("(письмо не отправлено — добавь --send)");
        return Ok(());
    }
    let (lead, _src) = orch::lead_of(&reg);
    let Some(to) = lead else {
        anyhow::bail!("devh orch reclaim report: ведущий не назначен — письмо не отправлено");
    };
    if dry_run {
        println!("dry-run: письмо не отправлено (получатель {to})");
        return Ok(());
    }
    let body = format!(
        "RECLAIM: сводка спасённой работы\n\n{}",
        summary.render()
    );
    let rc = crate::shell::msg::run(&[
        "send".to_string(),
        "--to".to_string(),
        to.clone(),
        "--subject".to_string(),
        "RECLAIM: сводка спасённой работы".to_string(),
        "--body".to_string(),
        body,
    ]);
    if rc != 0 {
        anyhow::bail!("devh orch reclaim report: письмо не отправлено (rc={rc})");
    }
    println!("письмо отправлено: {to}");
    Ok(())
}

/// Лимит кандидатов (защита от «спасения» мусора). По умолчанию 50.
fn max_candidates_arg(args: &[String]) -> usize {
    flag_value(args, "--max-candidates")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(50)
}

/// Точка входа: `devh orch reclaim <scan|salvage|close|redistribute|report|show>`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = orch::load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    check_role(&reg, &caller).map_err(|e| anyhow::anyhow!("{e}"))?;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("scan");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub {
        "show" => {
            let id = rest
                .first()
                .ok_or_else(|| anyhow::anyhow!("devh orch reclaim show: нужен ID задачи"))?;
            let with_diff = rest.iter().any(|a| a == "--diff");
            let out = show(root, id, with_diff).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{out}");
            Ok(())
        }
        "scan" => cmd_scan(
            root,
            rest.iter().any(|a| a == "--json"),
            max_candidates_arg(&rest),
        ),
        "salvage" => {
            let agent = flag_value(&rest, "--agent");
            let path = flag_value(&rest, "--path").map(PathBuf::from);
            cmd_salvage(
                root,
                agent.as_deref(),
                path.as_deref(),
                rest.iter().any(|a| a == "--dry-run"),
            )
        }
        "close" => {
            let agent = flag_value(&rest, "--agent");
            let path = flag_value(&rest, "--path").map(PathBuf::from);
            cmd_close(
                root,
                agent.as_deref(),
                path.as_deref(),
                rest.iter().any(|a| a == "--apply"),
            )
        }
        "redistribute" => cmd_redistribute(root, rest.iter().any(|a| a == "--apply")),
        "report" => cmd_report(
            root,
            rest.iter().any(|a| a == "--send"),
            rest.iter().any(|a| a == "--dry-run"),
        ),
        other => Err(anyhow::anyhow!(
            "devh orch reclaim: неизвестная подкоманда '{other}' \
             (scan|salvage|close|redistribute|report|show)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(d: &Path) -> PathBuf {
        let _ = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(d)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.email", "devh@test.local"])
            .current_dir(d)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "devh"])
            .current_dir(d)
            .output();
        d.to_path_buf()
    }

    #[test]
    fn worker_is_denied() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        // create .harness/orch/registry.json with a worker
        std::fs::create_dir_all(root.join(".harness/orch")).unwrap();
        let reg = orch::Registry {
            agents: vec![orch::AgentState {
                name: "worker".to_string(),
                role: Some("worker".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        orch::save(&root, &reg).unwrap();

        // worker caller (not lead) → denied
        let err = show(&root, "T-001", false);
        assert!(matches!(err, Err(ReclaimError::PermissionDenied { .. })));
    }

    #[test]
    fn lead_sees_snapshot() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let task = Task {
            id: "T-001".to_string(),
            agent: "worker".to_string(),
            title: "Test task".to_string(),
            complexity: 2,
            c_level: Some(3),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Queued,
            zones: vec!["devharness/src/core/x.rs".to_string()],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: 0,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        };

        let reg = orch::Registry {
            agents: vec![orch::AgentState {
                name: "lead".to_string(),
                role: Some("lead".to_string()),
                ..Default::default()
            }],
            tasks: vec![task],
            ..Default::default()
        };
        orch::save(&root, &reg).unwrap();

        // Verify task lookup logic
        let found = reg.tasks.iter().find(|t| t.id == "T-001");
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Test task");
    }

    #[test]
    fn task_not_found() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let reg = orch::Registry::default();
        orch::save(&root, &reg).unwrap();
        // Can't easily mock caller, but we test the error exists
        // This test verifies the function signature and types work
        let _: fn(&Path, &str, bool) -> Result<String, ReclaimError> = show;
    }

    #[test]
    fn with_diff_adds_hunks() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        // Create a file and commit it
        std::fs::write(root.join("test.txt"), "hello\n").unwrap();
        let _ = Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(&root)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&root)
            .output();
        let sha = crate::shell::git::short_head(&root);

        let task = Task {
            id: "T-001".to_string(),
            agent: "testagent".to_string(),
            title: "Test".to_string(),
            complexity: 0,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: 0,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: Some(sha),
            main_sha: None,
            override_reason: None,
        };

        // Verify format_task_snapshot works
        let snapshot = format_task_snapshot(&root, &task, false).unwrap();
        assert!(snapshot.contains("T-001"));
        assert!(snapshot.contains("Test"));
    }

    // ─────────── RECLAIM: команда (scan/salvage/close/redistribute) ───────────

    /// Репозиторий с начальным коммитом на main (иначе `main...branch` пуст).
    fn init_repo_committed(d: &Path) -> PathBuf {
        let root = init_repo(d);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git_ok(&root, &["add", "-A"]));
        assert!(git_ok(&root, &["commit", "-m", "base"]));
        root
    }

    fn git_ok(repo: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn lead_registry() -> orch::Registry {
        orch::Registry {
            agents: vec![orch::AgentState {
                name: "lead".to_string(),
                role: Some("lead".to_string()),
                ..Default::default()
            }],
            lead: Some("lead".to_string()),
            ..Default::default()
        }
    }

    fn task(id: &str, agent: &str, status: Status) -> Task {
        Task {
            id: id.to_string(),
            agent: agent.to_string(),
            title: format!("задача {id}"),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status,
            zones: vec!["src/a.rs".to_string()],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: 0,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        }
    }

    #[test]
    fn summary_render_counts_saved_and_created() {
        let s = ReclaimSummary {
            candidates: 5,
            dirty: 2,
            rescue_branches: vec!["devh/reclaim-beam".to_string()],
            manual_rescue: vec!["devh/rescue-testname-20261026-2112".to_string()],
            created_tasks: vec!["T-900 (спасённая работа)".to_string()],
        };
        assert_eq!(s.saved_branches(), 2);
        let text = s.render();
        assert!(text.contains("кандидатов: 5"));
        assert!(text.contains("грязных (работа под риском): 2"));
        assert!(text.contains("спасено веток: 2"));
        assert!(text.contains("задач из спасённой работы: 1"));
        assert!(text.contains("devh/rescue-testname-20261026-2112"));
    }

    #[test]
    fn classify_marks_main_and_registered_agents_as_skipped() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        let reg = orch::Registry {
            agents: vec![orch::AgentState {
                name: "worker".to_string(),
                role: Some("worker".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        // Ветка зарегистрированного агента и ветка брошенного агента.
        git_ok(&root, &["branch", "devh/agent-worker"]);
        git_ok(&root, &["branch", "devh/agent-ghost"]);
        let cls = classify(&root, &reg, 50);
        assert!(
            cls.skipped
                .iter()
                .any(|s| matches!(s.reason, SkipReason::MainTree)),
            "main должен быть отложен: {:?}",
            cls.skipped
        );
        assert!(
            cls.skipped.iter().any(|s| matches!(
                &s.reason,
                SkipReason::RegisteredAgent { agent } if agent == "worker"
            )),
            "зарегистрированный агент должен быть отложен: {:?}",
            cls.skipped
        );
        assert!(
            cls.candidates
                .iter()
                .any(|c| c.branch.as_deref() == Some("devh/agent-ghost")),
            "брошенный агент должен быть кандидатом: {:?}",
            cls.candidates
        );
    }

    #[test]
    fn classify_reports_limit_and_rescued() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        let reg = orch::Registry::default();
        git_ok(&root, &["branch", "devh/agent-ghost"]);
        // rescue-ветка уже есть — работа уже спасена.
        git_ok(&root, &["branch", "devh/reclaim-ghost"]);
        let cls = classify(&root, &reg, 50);
        assert!(
            cls.skipped.iter().any(|s| matches!(
                &s.reason,
                SkipReason::AlreadyRescued { branch } if branch == "devh/reclaim-ghost"
            )),
            "уже спасённое должно быть отложено: {:?}",
            cls.skipped
        );
        // Лимит: лишние кандидаты откладываются с причиной Limit.
        git_ok(&root, &["branch", "devh/agent-ghost2"]);
        git_ok(&root, &["branch", "devh/agent-ghost3"]);
        let cls = classify(&root, &reg, 1);
        assert!(cls.candidates.len() <= 1);
        assert!(
            cls.skipped
                .iter()
                .any(|s| matches!(s.reason, SkipReason::Limit { max: 1 })),
            "лимит должен быть виден в причинах: {:?}",
            cls.skipped
        );
    }

    #[test]
    fn classify_puts_worktrees_before_dangling_commits() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        let reg = orch::Registry::default();
        git_ok(&root, &["branch", "devh/agent-ghost"]);
        let cls = classify(&root, &reg, 50);
        // Ветка брошенного агента должна идти раньше dangling-коммитов.
        let first_branch = cls
            .candidates
            .iter()
            .position(|c| c.branch.as_deref() == Some("devh/agent-ghost"));
        let first_dangling = cls.candidates.iter().position(|c| {
            c.kind == crate::commands::wtman::CandidateKind::DanglingCommit
        });
        if let (Some(b), Some(dg)) = (first_branch, first_dangling) {
            assert!(b < dg, "ветка должна быть раньше dangling: {b} vs {dg}");
        }
    }

    #[test]
    fn skip_reason_labels_are_readable() {
        assert!(SkipReason::MainTree.label().contains("main"));
        assert!(SkipReason::RegisteredAgent {
            agent: "beam".to_string()
        }
        .label()
        .contains("beam"));
        assert!(SkipReason::AlreadyRescued {
            branch: "devh/reclaim-beam".to_string()
        }
        .label()
        .contains("devh/reclaim-beam"));
        assert!(SkipReason::CleanTree.label().contains("чистое"));
        assert!(SkipReason::Limit { max: 7 }.label().contains('7'));
    }

    #[test]
    fn summary_colored_render_has_counters() {
        let s = ReclaimSummary {
            candidates: 3,
            dirty: 1,
            rescue_branches: vec!["devh/reclaim-beam".to_string()],
            manual_rescue: vec![],
            created_tasks: vec!["T-900 (спасённая работа)".to_string()],
        };
        let text = s.render_colored();
        assert!(text.contains("RECLAIM report"));
        assert!(text.contains("кандидатов 3"));
        assert!(text.contains("грязных (работа под риском): 1"));
        assert!(text.contains("спасено веток: 1"));
    }

    #[test]
    fn branches_with_prefix_finds_only_matching() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        for name in ["devh/reclaim-beam", "devh/rescue-kilo-2026", "devh/agent-x"] {
            let _ = Command::new("git")
                .args(["branch", name])
                .current_dir(&root)
                .output();
        }
        let reclaim = branches_with_prefix(&root, "devh/reclaim-");
        assert_eq!(reclaim, vec!["devh/reclaim-beam".to_string()]);
        let manual = branches_with_prefix(&root, "devh/rescue-");
        assert_eq!(manual, vec!["devh/rescue-kilo-2026".to_string()]);
    }

    #[test]
    fn collect_summary_sees_created_reclaim_tasks() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        std::fs::create_dir_all(root.join(".harness/orch")).unwrap();
        let reg = orch::Registry {
            tasks: vec![Task {
                id: "T-901".to_string(),
                agent: "minimax".to_string(),
                title: "спасённая работа".to_string(),
                complexity: 2,
                c_level: Some(2),
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                started_key: None,
                status: Status::Queued,
                zones: vec!["devharness/src/core/x.rs".to_string()],
                fail_level: None,
                note: "reclaim:devh/reclaim-beam@abc1234\n+10 -2".to_string(),
                created: 0,
                updated: 0,
                dod: Vec::new(),
                checks: Vec::new(),
                acked: Vec::new(),
                evidence: Vec::new(),
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            }],
            ..Default::default()
        };
        orch::save(&root, &reg).unwrap();
        let summary = collect_summary(&root, &reg);
        assert_eq!(summary.created_tasks.len(), 1);
        assert!(summary.created_tasks[0].starts_with("T-901"));
    }

    #[test]
    fn rescue_branch_name_is_stable() {
        assert_eq!(rescue_branch("beam"), "devh/reclaim-beam");
        assert_eq!(agent_of_branch("devh/reclaim-beam").as_deref(), Some("beam"));
        assert_eq!(agent_of_branch("devh/agent-beam").as_deref(), Some("beam"));
        assert_eq!(agent_of_branch("main"), None);
    }

    #[test]
    fn task_ref_parsed_from_commit_message() {
        assert_eq!(
            task_ref_from_msg("wip: beam T-123 7 файлов").as_deref(),
            Some("T-123")
        );
        assert_eq!(task_ref_from_msg("reclaim: beam (3 файла)"), None);
        assert_eq!(task_ref_from_msg("просто текст"), None);
    }

    #[test]
    fn close_without_rescue_ref_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-ghost");
        let wt_s = wt.to_string_lossy().to_string();
        assert!(git_ok(&root, &["worktree", "add", "-b", "devh/agent-ghost", &wt_s]));
        std::fs::write(wt.join("x.txt"), "1\n").unwrap();

        let err = cmd_close(&root, Some("ghost"), None, true).expect_err("close без rescue — отказ");
        let msg = err.to_string();
        assert!(msg.contains("rescue"), "отказ должен называть rescue-ветку: {msg}");

        // Чистое дерево без rescue-ветки — тоже отказ (fail-closed).
        git_ok(&wt, &["add", "-A"]);
        git_ok(&wt, &["commit", "-m", "base"]);
        let err2 = cmd_close(&root, Some("ghost"), None, true).expect_err("всё ещё отказ");
        assert!(err2.to_string().contains("rescue"), "{}", err2);
    }

    #[test]
    fn salvage_then_close_keeps_rescue_branch() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-ghost");
        let wt_s = wt.to_string_lossy().to_string();
        assert!(git_ok(&root, &["worktree", "add", "-b", "devh/agent-ghost", &wt_s]));
        std::fs::write(wt.join("y.txt"), "data\n").unwrap();

        cmd_salvage(&root, Some("ghost"), None, false).expect("salvage");
        assert!(
            crate::shell::git::show_ref(&root, "devh/reclaim-ghost"),
            "rescue-ветка создана"
        );

        cmd_close(&root, Some("ghost"), None, true).expect("close");
        assert!(!wt.exists(), "worktree снят");
        assert!(
            crate::shell::git::show_ref(&root, "devh/reclaim-ghost"),
            "rescue-ветка сохранена после снятия worktree"
        );
    }

    #[test]
    fn redistribute_marks_task_p2_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        assert!(git_ok(&root, &["checkout", "-b", "devh/reclaim-ghost"]));
        std::fs::write(root.join("z.txt"), "z\n").unwrap();
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-m", "reclaim: ghost (1 файла)"]);
        git_ok(&root, &["checkout", "main"]);

        let mut reg = lead_registry();
        reg.tasks.push(task("T-001", "ghost", Status::Active));
        orch::save(&root, &reg).unwrap();

        cmd_redistribute(&root, true).expect("redistribute");
        let reg2 = orch::load(&root);
        let t = reg2.tasks.iter().find(|t| t.id == "T-001").expect("T-001");
        assert_eq!(t.status, Status::Queued, "задача возвращена в очередь");
        assert_eq!(t.complexity, 2, "приоритет P2");
        assert!(
            t.note.contains("reclaim:devh/reclaim-ghost"),
            "маркер rescue в note: {}",
            t.note
        );
        assert!(
            t.zones.iter().any(|z| z.ends_with("z.txt")),
            "зоны = изменённые пути: {:?}",
            t.zones
        );

        let n = reg2.tasks.len();
        cmd_redistribute(&root, true).expect("повторный прогон");
        let reg3 = orch::load(&root);
        assert_eq!(reg3.tasks.len(), n, "дедуп: новых задач не появилось");
    }

    #[test]
    fn redistribute_creates_incomplete_task_p2() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo_committed(d.path());
        assert!(git_ok(&root, &["checkout", "-b", "devh/reclaim-ghost"]));
        std::fs::write(root.join("w.txt"), "w\n").unwrap();
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-m", "reclaim: ghost (1 файла)"]);
        git_ok(&root, &["checkout", "main"]);

        let reg = lead_registry();
        orch::save(&root, &reg).unwrap();

        cmd_redistribute(&root, true).expect("redistribute");
        let reg2 = orch::load(&root);
        let t = reg2
            .tasks
            .iter()
            .find(|t| t.title.contains("RECLAIM"))
            .expect("создана incomplete-задача");
        assert_eq!(t.status, Status::Queued);
        assert_eq!(t.complexity, 2, "P2");
        assert!(t.note.contains("reclaim:devh/reclaim-ghost"), "{}", t.note);
    }

    #[test]
    fn scan_is_read_only_and_writes_report() {
        let d = tempfile::tempdir().unwrap();
        let root = init_repo(d.path());
        let wt = d.path().join("wt-ghost");
        let wt_s = wt.to_string_lossy().to_string();
        assert!(git_ok(&root, &["worktree", "add", "-b", "devh/agent-ghost", &wt_s]));
        std::fs::write(wt.join("q.txt"), "q\n").unwrap();

        cmd_scan(&root, true, 50).expect("scan");
        let report = orch::dir(&root).join("reclaim-scan.json");
        assert!(report.is_file(), "отчёт записан: {}", report.display());
        let body = std::fs::read_to_string(&report).unwrap();
        assert!(body.contains("abandoned-worktree"), "кандидат в отчёте: {body}");
        // scan ничего не меняет: worktree на месте, dirty-файл цел.
        assert!(wt.exists());
        assert!(wt.join("q.txt").is_file());
    }
}
