//! `devh orch guard` — единый прогон автоматических guards оркестратора.
//!
//! До сих пор guards жили только под юнит-тестами: `core::guards::run_all()`
//! не вызывался ни одной командой, поэтому инварианты (зоны, зависимости,
//! идентичность, буфер, worktree, слияние) проверялись лишь на канонических
//! примерах. Команда делает их проверку живой: снимок РЕАЛЬНОГО реестра →
//! отчёт ok/warn/problem → код возврата.
//!
//! Коды возврата (контракт §КОДЫ ВОЗВРАТА):
//!   0 — проблем нет (warn допустимы),
//!   1 — найдена actionable-проблема (инвариант нарушен),
//!   2 — среда не готова (реестр недоступен/бит).

use std::collections::HashMap;
use std::path::Path;

use crate::core::guards::backlog_guard::{self, Severity};
use crate::core::commit_audit::{commit_exists, is_commit_in_branch};
use crate::core::guards::commit_review_guard::{self, CommitFacts, CommitRecord, MAIN_BRANCH};
use crate::core::guards::deps_guard::check_deps;
use crate::core::guards::identity_guard;
use crate::core::guards::module_list_guard;
use crate::core::guards::worktree_guard;
use crate::core::guards::zone_overlap_guard::{self, ZoneTask};
use crate::core::guards::{decomposition_guard, GuardReport, GuardVerdict};
use crate::core::task_deps::{DepStatus, DepTask};

use super::orch::{self, Registry, Status};

/// Минимальный буфер ведущего (норма O1).
pub const BUFFER_MIN: usize = 15;
/// Минимальная очередь ведомого (норма O1).
pub const QUEUE_MIN: usize = 2;

/// Собрать отчёт guards по снимку реестра. Побочных эффектов нет.
pub fn collect(root: &Path, reg: &Registry) -> GuardReport {
    let mut report = GuardReport::new();

    // ── идентичность: один канон = один ключ ──
    let home = identity_guard::msg_home();
    report.push(identity_guard::ID, identity_guard::check(&home));

    // ── список модулей ядра: нет дублей/битых объявлений ──
    let mod_rs = root.join("devharness/src/core/mod.rs");
    match module_list_guard::validate_file(&mod_rs) {
        Ok(issues) if issues.is_empty() => {
            report.push("module_list_guard", GuardVerdict::Ok);
        }
        Ok(issues) => {
            let blocking = issues
                .iter()
                .filter(|i| !matches!(i, module_list_guard::ModListIssue::Malformed { .. }))
                .count();
            let text = issues
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            report.push(
                "module_list_guard",
                if blocking > 0 {
                    GuardVerdict::Problem(text)
                } else {
                    GuardVerdict::Warn(text)
                },
            );
        }
        Err(e) => report.push(
            "module_list_guard",
            GuardVerdict::Warn(format!("не прочитан {}: {e}", mod_rs.display())),
        ),
    }

    // ── зависимости: нет неизвестных ссылок и циклов ──
    let dep_tasks: Vec<DepTask> = reg
        .tasks
        .iter()
        .map(|t| DepTask {
            id: t.id.clone(),
            status: match t.status {
                Status::Queued => DepStatus::Queued,
                Status::Active => DepStatus::Active,
                Status::Done => DepStatus::Done,
                Status::Failed => DepStatus::Failed,
                Status::Revoked => DepStatus::Revoked,
            },
            depends_on: reg.plan.deps.get(&t.id).cloned().unwrap_or_default(),
        })
        .collect();
    let dep_issues = check_deps(&dep_tasks);
    report.push(
        "deps_guard",
        if dep_issues.is_empty() {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Problem(
                dep_issues
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        },
    );

    // ── зоны: пересечение активных задач разных агентов (O5) ──
    let zone_tasks: Vec<ZoneTask> = reg
        .tasks
        .iter()
        .filter(|t| matches!(t.status, Status::Queued | Status::Active))
        .map(|t| ZoneTask {
            id: t.id.clone(),
            agent: t.agent.clone(),
            zones: t.zones.clone(),
            active: t.status == Status::Active,
        })
        .collect();
    let active_overlaps = zone_overlap_guard::find_overlaps(&zone_tasks);
    report.push(
        "zone_overlap_guard",
        if active_overlaps.is_empty() {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Problem(
                active_overlaps
                    .iter()
                    .map(|o| o.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        },
    );
    let queued_only: Vec<ZoneTask> = zone_tasks
        .iter()
        .map(|t| ZoneTask {
            active: false,
            ..t.clone()
        })
        .collect();
    let queued_pairs = zone_overlap_guard::find_overlaps(&queued_only);
    report.push(
        "zone_overlap_queued",
        if queued_pairs.is_empty() {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Warn(format!(
                "{} пересечений в очередях (выдача заблокирует zone_guard)",
                queued_pairs.len()
            ))
        },
    );

    // ── буфер и очереди (O1) ──
    let (lead, _) = orch::lead_of(reg);
    let buffer_size = orch::buffer_tasks(reg, lead.as_deref()).len();
    let queues: Vec<(String, usize)> = reg
        .agents
        .iter()
        .filter(|a| Some(a.name.as_str()) != lead.as_deref())
        .map(|a| {
            let n = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Queued)
                .count();
            (a.name.clone(), n)
        })
        .collect();
    let backlog_issues = backlog_guard::check_backlog(buffer_size, BUFFER_MIN, &queues, QUEUE_MIN);
    let backlog_problem = backlog_issues
        .iter()
        .any(|i| matches!(i.severity, Severity::Problem));
    report.push(
        "backlog_guard",
        if backlog_issues.is_empty() {
            GuardVerdict::Ok
        } else {
            let text = backlog_issues
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            if backlog_problem {
                GuardVerdict::Problem(text)
            } else {
                GuardVerdict::Warn(text)
            }
        },
    );

    // ── worktree'и исполнителей ──
    let mut wt_problems: Vec<String> = Vec::new();
    let mut wt_warns: Vec<String> = Vec::new();
    for a in reg.agents.iter() {
        let Some(wt) = a.worktree.clone() else {
            continue;
        };
        let state = worktree_guard::inspect(Path::new(&wt), a.wt_branch.as_deref());
        for issue in worktree_guard::check_worktree(&state) {
            let text = format!("{}: {issue}", a.name);
            if issue.is_blocking() {
                wt_problems.push(text);
            } else {
                wt_warns.push(text);
            }
        }
    }
    report.push(
        "worktree_guard",
        if !wt_problems.is_empty() {
            GuardVerdict::Problem(wt_problems.join("; "))
        } else if !wt_warns.is_empty() {
            GuardVerdict::Warn(wt_warns.join("; "))
        } else {
            GuardVerdict::Ok
        },
    );

    // ── слияние: «влито» без SHA / некорректный SHA ──
    let merge_problems = merge_problems(reg);
    report.push(
        "merge_guard",
        if merge_problems.is_empty() {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Problem(merge_problems.join("; "))
        },
    );

    // ── ревью коммитов: commit_sha в ветке агента, main_sha в main ──
    let (cr_problems, cr_warns) = commit_review_findings(root, reg);
    report.push(
        "commit_review_guard",
        if !cr_problems.is_empty() {
            GuardVerdict::Problem(cr_problems.join("; "))
        } else if !cr_warns.is_empty() {
            GuardVerdict::Warn(cr_warns.join("; "))
        } else {
            GuardVerdict::Ok
        },
    );

    // ── самопроверка guard'а декомпозиции ──
    report.push(decomposition_guard::ID, decomposition_guard::selfcheck());

    report
}

/// Находки ревью коммитов по реестру: закрытые задачи с недоказанным
/// `commit_sha`/`main_sha`. Возвращает (проблемы, предупреждения).
///
/// Git дергается только для закрытых задач, и каждый SHA проверяется один раз:
/// батч-коммит (одна работа — много задач) не плодит вызовов. Кэш живёт внутри
/// одного прогона, побочных эффектов нет.
fn commit_review_findings(root: &Path, reg: &Registry) -> (Vec<String>, Vec<String>) {
    let branches: HashMap<String, String> = reg
        .agents
        .iter()
        .filter_map(|a| {
            a.wt_branch
                .as_ref()
                .map(|b| (crate::core::identity::canonical(&a.name), b.clone()))
        })
        .collect();
    let mut exists: HashMap<String, bool> = HashMap::new();
    let mut in_branch: HashMap<(String, String), bool> = HashMap::new();
    let mut problems: Vec<String> = Vec::new();
    let mut warns: Vec<String> = Vec::new();
    for t in reg.tasks.iter().filter(|t| t.status == Status::Done) {
        let record = CommitRecord {
            task_id: t.id.clone(),
            agent: crate::core::identity::canonical(&t.agent),
            status: "done".to_string(),
            commit_sha: t.commit_sha.clone(),
            main_sha: t.main_sha.clone(),
        };
        let branch = branches
            .get(&record.agent)
            .map(String::as_str)
            .filter(|b| !b.trim().is_empty());
        let mut exists_of = |sha: &str| *exists.entry(sha.to_string()).or_insert_with(|| commit_exists(root, sha));
        let mut in_of = |sha: &str, br: &str| {
            *in_branch
                .entry((sha.to_string(), br.to_string()))
                .or_insert_with(|| is_commit_in_branch(root, sha, br))
        };
        let sha_ok = record
            .commit_sha
            .as_deref()
            .is_some_and(|s| exists_of(s.trim()));
        let main_ok = record
            .main_sha
            .as_deref()
            .is_some_and(|s| exists_of(s.trim()) && in_of(s.trim(), MAIN_BRANCH));
        let branch_ok = match (record.commit_sha.as_deref(), branch) {
            (Some(s), Some(br)) if sha_ok => in_of(s.trim(), br),
            _ => false,
        };
        let facts = CommitFacts {
            agent_branch_known: branch.is_some(),
            commit_in_agent_branch: branch_ok,
            main_sha_in_main: main_ok,
            commit_exists: sha_ok,
        };
        for issue in commit_review_guard::check_commit(&record, branch, &facts) {
            let text = issue.to_string();
            if commit_review_guard::is_blocking(&issue) {
                problems.push(text);
            } else {
                warns.push(text);
            }
        }
    }
    (problems, warns)
}

/// Проблемы слияния по реестру: состояние «влито» без SHA и SHA не как git-хэш.
fn merge_problems(reg: &Registry) -> Vec<String> {
    let mut out = Vec::new();
    for a in reg.agents.iter() {
        let Some(state) = a.merge_state.clone() else {
            continue;
        };
        let sha = a.merged_sha.clone();
        match (state.as_str(), sha.as_deref()) {
            ("влито", None) => out.push(format!("{}: merge_state=влито без merged_sha", a.name)),
            ("влито", Some(sha)) => {
                if !crate::core::guards::merge_guard::valid_sha(sha) {
                    out.push(format!("{}: некорректный SHA «{sha}»", a.name));
                }
            }
            (_, Some(sha)) => {
                if !crate::core::guards::merge_guard::valid_sha(sha) {
                    out.push(format!("{}: некорректный SHA «{sha}»", a.name));
                }
            }
            _ => {}
        }
    }
    out
}

/// Текстовый отчёт: одна строка на guard.
pub fn render(report: &GuardReport) -> String {
    let mut s = String::from("=== GUARDS ОРКЕСТРАТОРА ===\n");
    for (id, verdict) in report.entries() {
        let (mark, text) = match verdict {
            GuardVerdict::Ok => ("✓", String::new()),
            GuardVerdict::Warn(m) => ("⚠", m.clone()),
            GuardVerdict::Problem(m) => ("✗", m.clone()),
        };
        if text.is_empty() {
            s.push_str(&format!("{mark} {id}: ок\n"));
        } else {
            s.push_str(&format!("{mark} {id}: {text}\n"));
        }
    }
    s.push_str(&format!("итог: {}\n", report.summary()));
    s
}

/// Точка входа CLI: `devh orch guard [--json]`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg_path = orch::path(root);
    if !reg_path.exists() {
        return Err(crate::commands::DevhError::env(format!(
            "devh orch guard: нет реестра {} — сначала devh init",
            reg_path.display()
        ))
        .into());
    }
    let reg = orch::load(root);
    let report = collect(root, &reg);
    if args.iter().any(|a| a == "--json") {
        let entries: Vec<serde_json::Value> = report
            .entries()
            .iter()
            .map(|(id, v)| {
                serde_json::json!({
                    "guard": id,
                    "verdict": match v {
                        GuardVerdict::Ok => "ok",
                        GuardVerdict::Warn(_) => "warn",
                        GuardVerdict::Problem(_) => "problem",
                    },
                    "message": v.message(),
                })
            })
            .collect();
        let body = serde_json::json!({
            "guards": entries,
            "problems": report.problems().len(),
            "warnings": report.warnings().len(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print!("{}", render(&report));
    }
    if !report.is_clean() {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch guard: {} проблем — смотри отчёт",
            report.problems().len()
        ))
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::orch::{AgentState, Task};

    #[test]
    fn report_has_all_guards() {
        let reg = Registry::default();
        let report = collect(Path::new("/nonexistent-root"), &reg);
        assert!(report.entries().len() >= 8, "ожидались все guards");
        assert!(!report
            .problems()
            .iter()
            .any(|(id, _)| *id == "module_list_guard"));
    }

    #[test]
    fn render_marks_problem_and_warn() {
        let mut report = GuardReport::new();
        report.push("a", GuardVerdict::Ok);
        report.push("b", GuardVerdict::Warn("хорошо бы".to_string()));
        report.push("c", GuardVerdict::Problem("сломано".to_string()));
        let text = render(&report);
        assert!(text.contains("✓ a: ок"));
        assert!(text.contains("⚠ b: хорошо бы"));
        assert!(text.contains("✗ c: сломано"));
        assert!(text.contains("1 problem"));
    }

    #[test]
    fn merge_state_without_sha_is_problem() {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "beam".to_string(),
            merge_state: Some("влито".to_string()),
            ..Default::default()
        });
        let problems = merge_problems(&reg);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("merged_sha"));
    }

    #[test]
    fn merge_state_with_valid_sha_is_ok() {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "beam".to_string(),
            merge_state: Some("влито".to_string()),
            merged_sha: Some("abc1234".to_string()),
            ..Default::default()
        });
        assert!(merge_problems(&reg).is_empty());
    }

    #[test]
    fn unknown_dependency_is_problem() {
        let mut reg = Registry::default();
        reg.tasks.push(Task {
            id: "T-001".to_string(),
            agent: "beam".to_string(),
            title: "t".to_string(),
            complexity: 1,
            c_level: Some(1),
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
            dod: vec!["x".to_string()],
            checks: vec!["cargo test".to_string()],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        });
        reg.plan.deps.insert("T-001".to_string(), vec!["T-999".to_string()]);
        let report = collect(Path::new("/nonexistent-root"), &reg);
        let problems = report.problems();
        assert!(
            problems
                .iter()
                .any(|(id, msg)| *id == "deps_guard" && msg.contains("T-999")),
            "ожидалась проблема зависимостей: {problems:?}"
        );
    }
}
