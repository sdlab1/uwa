//! Guard декомпозиции: пропускает только безопасное разбиение сложной задачи.
//!
//! Проверка чистая (реестр не трогает) и опирается на контракт
//! [`crate::core::task_decomposition`], добавляя запреты на расширение
//! полномочий lead, немашинные проверки и опасные зоны.

use crate::core::guards::{Guard, GuardContext, GuardVerdict};
use crate::core::task_decomposition::{
    decompose, ChildProposal, Constraints, DecompositionError, ParentTask,
};
use std::fmt;

/// Стабильный id guard'а.
pub const ID: &str = "decomposition_guard";

/// Вход guard'а: родитель, ограничения и предложенные подзадачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionInput {
    pub parent: ParentTask,
    pub constraints: Constraints,
    pub proposal: Vec<ChildProposal>,
}

/// Структурированное нарушение: поле + правило + человекочитаемая причина.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub field: &'static str,
    pub rule: &'static str,
    pub message: String,
}

impl Violation {
    fn new(field: &'static str, rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            rule,
            message: message.into(),
        }
    }
}

impl From<DecompositionError> for Violation {
    fn from(e: DecompositionError) -> Self {
        Self::new(e.field(), e.rule(), e.to_string())
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.field, self.rule, self.message)
    }
}

/// Запрещённые действия: расширение полномочий lead и обход гейтов.
const FORBIDDEN: &[(&str, &str)] = &[
    ("elevat", "расширение полномочий lead"),
    ("passwd", "расширение полномочий lead"),
    ("orch role", "расширение полномочий lead"),
    ("lead --set", "расширение полномочий lead"),
    ("role --role lead", "расширение полномочий lead"),
    ("--override", "обход DoD-гейта"),
    ("override dod", "обход DoD-гейта"),
    ("--no-test", "отключение машинных проверок"),
    ("bypass", "обход гейта"),
    ("force push", "разрушающая git-операция"),
    ("git push", "git — только lead"),
    ("git merge", "git — только lead"),
    ("wt merge", "git — только lead"),
    ("--in-place", "мутация дерева аудита"),
];

/// Разрешённые «машинные» префиксы проверки.
const CHECK_PREFIXES: &[&str] = &[
    "cargo ", "devh ", "bash ", "sh ", "python", "npm ", "npx ", "make ", "go ", "rustc ",
    "pytest", "./", "target/",
];

fn forbidden_in(text: &str) -> Option<&'static str> {
    let low = text.to_lowercase();
    FORBIDDEN
        .iter()
        .find(|(needle, _)| low.contains(needle))
        .map(|(_, why)| *why)
}

fn is_machine_check(check: &str) -> bool {
    let c = check.trim();
    if c.is_empty() || c.starts_with('#') {
        return false;
    }
    CHECK_PREFIXES.iter().any(|p| c.starts_with(p))
}

fn safe_zone(zone: &str) -> Result<(), Violation> {
    let z = zone.trim();
    if z.is_empty() {
        return Err(Violation::new("zone", "zone не пуста", "пустая зона"));
    }
    if z.starts_with('/') || z.starts_with('~') || z.contains("..") {
        return Err(Violation::new(
            "zone",
            "zone относительна и без ..",
            format!("опасная зона: {z}"),
        ));
    }
    Ok(())
}

/// Guard безопасной декомпозиции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionGuard {
    input: DecompositionInput,
}

impl DecompositionGuard {
    pub fn new(input: DecompositionInput) -> Self {
        Self { input }
    }

    /// Полная проверка; при нарушении — структурированная причина.
    pub fn validate(&self) -> Result<(), Violation> {
        for (index, child) in self.input.proposal.iter().enumerate() {
            safe_zone(&child.zone)?;
            if !is_machine_check(&child.check) {
                return Err(Violation::new(
                    "check",
                    "check машинный",
                    format!(
                        "подзадача #{index} ({}): check не является машинной командой: {:?}",
                        child.id, child.check
                    ),
                ));
            }
            for (field, text) in [
                ("title", child.title.as_str()),
                ("zone", child.zone.as_str()),
                ("check", child.check.as_str()),
            ] {
                if let Some(why) = forbidden_in(text) {
                    return Err(Violation::new(
                        field,
                        "нет запрещённых действий",
                        format!(
                            "подзадача #{index} ({}): {why} в {field}: {text:?}",
                            child.id
                        ),
                    ));
                }
            }
        }
        decompose(
            &self.input.parent,
            &self.input.constraints,
            &self.input.proposal,
        )?;
        Ok(())
    }
}

impl Guard for DecompositionGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        match self.validate() {
            Ok(()) => GuardVerdict::Ok,
            Err(v) => GuardVerdict::Problem(v.to_string()),
        }
    }
}

fn sample_input() -> DecompositionInput {
    DecompositionInput {
        parent: ParentTask {
            id: "T-100".to_string(),
            p: 3,
            c: 6,
            zones: vec!["devharness/src/core".to_string()],
        },
        constraints: Constraints {
            max_children: 4,
            model_ceiling_c: 5,
            existing_ids: vec!["T-001".to_string()],
        },
        proposal: vec![ChildProposal {
            id: "T-101".to_string(),
            title: "часть 1".to_string(),
            zone: "devharness/src/core/x.rs".to_string(),
            p: 1,
            c: 3,
            dod: vec!["сделано".to_string()],
            check: "cargo test -p devh --lib x".to_string(),
            deps: Vec::new(),
        }],
    }
}

/// Самопроверка guard'а: валидный вход проходит, испорченный — блокируется.
pub fn selfcheck() -> GuardVerdict {
    let good = DecompositionGuard::new(sample_input());
    if !good.run(&GuardContext::default()).is_ok() {
        return GuardVerdict::Problem(format!(
            "decomposition_guard: валидная декомпозиция отклонена: {}",
            good.run(&GuardContext::default()).message()
        ));
    }
    let mut bad_input = sample_input();
    bad_input.proposal[0].c = 9;
    let bad = DecompositionGuard::new(bad_input);
    if bad.run(&GuardContext::default()).is_ok() {
        return GuardVerdict::Problem(
            "decomposition_guard: небезопасная декомпозиция не заблокирована".to_string(),
        );
    }
    GuardVerdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> DecompositionGuard {
        DecompositionGuard::new(sample_input())
    }

    #[test]
    fn valid_decomposition_passes() {
        let g = guard();
        assert_eq!(g.run(&GuardContext::default()), GuardVerdict::Ok);
        assert!(g.validate().is_ok());
    }

    #[test]
    fn raised_complexity_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].p = 4;
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "complexity");
        assert!(v.rule.contains("родителя"));

        let mut input = sample_input();
        input.proposal[0].c = 7;
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "complexity");
    }

    #[test]
    fn zone_escape_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].zone = "devharness/src/shell/msg.rs".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "zone");
        assert!(v.rule.contains("внутри родительской"));
    }

    #[test]
    fn dangerous_zone_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].zone = "devharness/src/../main".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "zone");
        assert!(v.rule.contains(".."));
    }

    #[test]
    fn lead_authority_expansion_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].title = "elevate: выдать себе lead".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "title");
        assert!(v.rule.contains("запрещённых"));

        let mut input = sample_input();
        input.proposal[0].check = "devh orch elevate me --password x".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "check");
    }

    #[test]
    fn dod_dilution_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].dod = vec!["  ".to_string()];
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "dod");
        assert!(v.rule.contains("DoD"));
    }

    #[test]
    fn non_machine_check_is_blocked() {
        let mut input = sample_input();
        input.proposal[0].check = "посмотреть глазами".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "check");
        assert!(v.rule.contains("машинный"));
    }

    #[test]
    fn duplicate_titles_and_ids_are_blocked() {
        let mut input = sample_input();
        let mut second = input.proposal[0].clone();
        second.id = "T-102".to_string();
        input.proposal.push(second.clone());
        let v = DecompositionGuard::new(input.clone())
            .validate()
            .unwrap_err();
        assert_eq!(v.field, "title");

        second.title = "часть 2".to_string();
        input.proposal[1] = second;
        input.proposal[1].id = "T-101".to_string();
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "id");
    }

    #[test]
    fn dep_cycle_is_blocked() {
        let mut input = sample_input();
        let mut second = ChildProposal {
            id: "T-102".to_string(),
            title: "часть 2".to_string(),
            ..input.proposal[0].clone()
        };
        input.proposal[0].deps = vec!["T-102".to_string()];
        second.deps = vec!["T-101".to_string()];
        input.proposal.push(second);
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "deps");
        assert!(v.rule.contains("цикл"));
    }

    #[test]
    fn sub_task_limit_is_blocked() {
        let mut input = sample_input();
        input.constraints.max_children = 1;
        let mut second = input.proposal[0].clone();
        second.id = "T-102".to_string();
        second.title = "часть 2".to_string();
        input.proposal.push(second);
        let v = DecompositionGuard::new(input).validate().unwrap_err();
        assert_eq!(v.field, "children");
    }

    #[test]
    fn validation_does_not_mutate_input() {
        let input = sample_input();
        let snapshot = input.clone();
        let g = DecompositionGuard::new(input.clone());
        let _ = g.run(&GuardContext::default());
        assert_eq!(g.input, snapshot);
    }

    #[test]
    fn selfcheck_is_green() {
        assert_eq!(selfcheck(), GuardVerdict::Ok);
    }

    #[test]
    fn problem_message_carries_field_and_rule() {
        let mut input = sample_input();
        input.proposal[0].c = 9;
        let verdict = DecompositionGuard::new(input).run(&GuardContext::default());
        let msg = verdict.message();
        assert!(msg.contains("complexity"), "{msg}");
        assert!(msg.contains("потолок") || msg.contains("выше"), "{msg}");
    }
}
