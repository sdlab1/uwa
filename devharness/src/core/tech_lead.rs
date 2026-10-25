//! Логика технического лида (tech-lead).
//!
//! Роль `tech_lead` выдаётся агенту ведущим (`devh orch role --agent X
//! --role tech_lead`) и даёт ровно три права — без прав ведущего
//! (merge/push/mutate/passwd):
//!
//! 1. **Ревью коммитов** — проверка, что `commit_sha` закрытой задачи
//!    существует, лежит в ветке агента и дошёл до `main`
//!    (на базе `core::commit_audit`).
//! 2. **Уточнение модели** — сверка декларации модели агента с реестром
//!    (журнал `core::identity_ledger`).
//! 3. **Потолок сложности** — явное значение C, которое агент может взять
//!    (приоритет над потолком, вычисленным по модели).
//!
//! Модуль чистый: IO только на чтение (git-проверка и журнал деклараций).

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::commit_audit::{audit_commit, is_commit_in_branch, AuditOutcome};
use crate::core::identity_ledger::EventLabel;

/// Проблема, найденная ревью коммита.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// У закрытой задачи не проставлен `commit_sha`.
    MissingSha,
    /// SHA не найден в репозитории.
    ShaNotFound,
    /// Коммит существует, но не входит в ветку агента (затерян/подменён).
    NotInAgentBranch,
    /// Коммит есть в ветке агента, но ещё не влит в `main` (ждёт слияния).
    NotInMain,
    /// Ветка агента неизвестна — принадлежность коммита проверить нельзя.
    NoBranchKnown,
    /// Ошибка git при проверке коммита.
    AuditError,
}

impl Issue {
    /// Короткая метка проблемы (для отчёта и письма).
    pub fn as_str(&self) -> &'static str {
        match self {
            Issue::MissingSha => "нет commit_sha",
            Issue::ShaNotFound => "SHA не найден в репозитории",
            Issue::NotInAgentBranch => "коммит не в ветке агента",
            Issue::NotInMain => "коммит не в main",
            Issue::NoBranchKnown => "ветка агента неизвестна",
            Issue::AuditError => "ошибка проверки git",
        }
    }
}

/// Находка ревью: одна на задачу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFinding {
    pub task_id: String,
    pub agent: String,
    pub sha: Option<String>,
    pub issue: Issue,
    pub detail: String,
}

impl CommitFinding {
    /// Строка отчёта: `T-123 [agent] abc1234 — проблема`.
    pub fn line(&self) -> String {
        let sha = self.sha.as_deref().unwrap_or("—");
        format!(
            "{} [{}] {} — {}",
            self.task_id,
            self.agent,
            sha,
            self.issue.as_str()
        )
    }
}

/// Минимальный срез задачи, нужный для ревью.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitTask {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub commit_sha: Option<String>,
    pub main_sha: Option<String>,
}

/// Ревью коммитов агентов.
///
/// Проверяются только закрытые (`done`) задачи без `main_sha`: если SHA уже
/// в `main`, коммит слит и проверять нечего. Для каждой такой задачи ищется
/// ровно одна проблема (или ни одной — коммит на месте и в `main`).
///
/// `branches` — карта «агент → ветка worktree»; если ветки нет, проверяется
/// только существование коммита.
pub fn review_commits(
    repo: &Path,
    tasks: &[CommitTask],
    branches: &BTreeMap<String, String>,
) -> Vec<CommitFinding> {
    let mut out = Vec::new();
    for t in tasks {
        if t.status != "done" {
            continue;
        }
        if t
            .main_sha
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        let sha = match t.commit_sha.as_deref() {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                out.push(CommitFinding {
                    task_id: t.id.clone(),
                    agent: t.agent.clone(),
                    sha: None,
                    issue: Issue::MissingSha,
                    detail: "задача закрыта, но commit_sha не проставлен".to_string(),
                });
                continue;
            }
        };
        let branch = branches.get(&t.agent).map(|s| s.as_str());
        let finding = match branch {
            None => {
                if crate::core::commit_audit::commit_exists(repo, &sha) {
                    Some(CommitFinding {
                        task_id: t.id.clone(),
                        agent: t.agent.clone(),
                        sha: Some(sha.clone()),
                        issue: Issue::NoBranchKnown,
                        detail: "коммит есть, но ветка агента не записана в реестре".to_string(),
                    })
                } else {
                    Some(CommitFinding {
                        task_id: t.id.clone(),
                        agent: t.agent.clone(),
                        sha: Some(sha.clone()),
                        issue: Issue::ShaNotFound,
                        detail: "SHA не найден в репозитории".to_string(),
                    })
                }
            }
            Some(b) => match audit_commit(repo, &sha, b) {
                AuditOutcome::NotFound => Some(CommitFinding {
                    task_id: t.id.clone(),
                    agent: t.agent.clone(),
                    sha: Some(sha.clone()),
                    issue: Issue::ShaNotFound,
                    detail: "SHA не найден в репозитории".to_string(),
                }),
                AuditOutcome::ForeignBranch => Some(CommitFinding {
                    task_id: t.id.clone(),
                    agent: t.agent.clone(),
                    sha: Some(sha.clone()),
                    issue: Issue::NotInAgentBranch,
                    detail: format!("коммит не входит в ветку агента {b}"),
                }),
                AuditOutcome::Error(e) => Some(CommitFinding {
                    task_id: t.id.clone(),
                    agent: t.agent.clone(),
                    sha: Some(sha.clone()),
                    issue: Issue::AuditError,
                    detail: e,
                }),
                AuditOutcome::Valid => {
                    if is_commit_in_branch(repo, &sha, "main") {
                        None
                    } else {
                        Some(CommitFinding {
                            task_id: t.id.clone(),
                            agent: t.agent.clone(),
                            sha: Some(sha.clone()),
                            issue: Issue::NotInMain,
                            detail: "коммит есть в ветке агента, но не влит в main".to_string(),
                        })
                    }
                }
            },
        };
        if let Some(f) = finding {
            out.push(f);
        }
    }
    out
}

/// Итоговый потолок C агента: явное значение (выданное ведущим или
/// tech-lead) имеет приоритет над потолком, вычисленным по модели.
pub fn effective_ceiling(explicit: Option<u8>, model_ceiling: u8) -> u8 {
    explicit.unwrap_or(model_ceiling)
}

/// Декларация модели агента (сверка журнала идентичностей с реестром).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelDeclaration {
    /// Декларация есть и совпадает с реестром.
    Declared { model: String },
    /// Декларации нет (агент не заявлял модель) — fail-closed.
    Missing,
    /// Декларация расходится с реестром — fail-closed до подтверждения.
    Mismatch {
        declared: String,
        registered: String,
    },
}

impl ModelDeclaration {
    /// Текст для отчёта tech-lead.
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelDeclaration::Declared { .. } => "декларация совпадает с реестром",
            ModelDeclaration::Missing => "декларации модели нет",
            ModelDeclaration::Mismatch { .. } => "декларация расходится с реестром",
        }
    }

    /// Fail-closed: агент без подтверждённой декларации не получает задачи.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, ModelDeclaration::Declared { .. })
    }
}

/// Последняя задекларированная модель агента (из журнала идентичностей).
pub fn declared_model(agent: &str) -> Option<String> {
    crate::core::identity_ledger::read_agent_events(agent)
        .iter()
        .rev()
        .find_map(|e| match e.label {
            EventLabel::Declare | EventLabel::ModelChange => e.model.clone(),
            _ => None,
        })
}

/// Сверить декларацию модели с реестром.
///
/// `registered` — модель из реестра (`None` — модель не зафиксирована).
pub fn model_declaration(agent: &str, registered: Option<&str>) -> ModelDeclaration {
    let Some(declared) = declared_model(agent) else {
        return ModelDeclaration::Missing;
    };
    match registered {
        None => ModelDeclaration::Declared { model: declared },
        Some(reg) if reg.trim() == declared.trim() => ModelDeclaration::Declared { model: declared },
        Some(reg) => ModelDeclaration::Mismatch {
            declared,
            registered: reg.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_ceiling_wins_over_model() {
        assert_eq!(effective_ceiling(Some(5), 3), 5);
        assert_eq!(effective_ceiling(None, 3), 3);
        assert_eq!(effective_ceiling(Some(0), 3), 0);
    }

    #[test]
    fn missing_declaration_is_not_confirmed() {
        let d = ModelDeclaration::Missing;
        assert!(!d.is_confirmed());
    }

    #[test]
    fn declared_matches_registered() {
        let d = ModelDeclaration::Declared {
            model: "stepfun-ai/step-5-preview".to_string(),
        };
        assert!(d.is_confirmed());
    }

    #[test]
    fn mismatch_is_not_confirmed() {
        let d = ModelDeclaration::Mismatch {
            declared: "a".to_string(),
            registered: "b".to_string(),
        };
        assert!(!d.is_confirmed());
    }
}
