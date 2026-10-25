//! Фреймворк автоматических guards оркестратора.
//!
//! Guard — маленькая машинная проверка инварианта (зоны, worktree, список
//! модулей, зависимости задач). Оркестратор прогоняет набор guards перед
//! выдачей/сдачей задачи и получает единый отчёт: ok / warn / problem.

pub mod backlog_guard;
pub mod decomposition_guard;
pub mod identity_guard;
pub mod deps_guard;
pub mod merge_guard;
pub mod module_list_guard;
pub mod task_state_guard;
pub mod worktree_guard;
pub mod zone_guard;
pub mod zone_overlap_guard;

use std::path::PathBuf;

/// Вердикт одного guard'а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// Инвариант выполнен.
    Ok,
    /// Подозрительно, но работу не блокирует.
    Warn(String),
    /// Инвариант нарушен — действие блокируется.
    Problem(String),
}

impl GuardVerdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, GuardVerdict::Ok)
    }

    pub fn is_problem(&self) -> bool {
        matches!(self, GuardVerdict::Problem(_))
    }

    /// Текст причины (для Ok — пусто).
    pub fn message(&self) -> &str {
        match self {
            GuardVerdict::Ok => "",
            GuardVerdict::Warn(m) | GuardVerdict::Problem(m) => m,
        }
    }
}

/// Контекст прогона: что проверяем и где.
#[derive(Debug, Clone, Default)]
pub struct GuardContext {
    pub task_id: Option<String>,
    pub agent: Option<String>,
    pub root: PathBuf,
}

impl GuardContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            task_id: None,
            agent: None,
            root: root.into(),
        }
    }

    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.task_id = Some(task.into());
        self
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }
}

/// Один автоматический инвариант.
pub trait Guard {
    /// Стабильный идентификатор (для отчёта и дедупа).
    fn id(&self) -> &'static str;
    /// Проверка; побочных эффектов быть не должно.
    fn run(&self, ctx: &GuardContext) -> GuardVerdict;
}

/// Итог прогона набора guards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardReport {
    entries: Vec<(String, GuardVerdict)>,
}

impl GuardReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, guard: impl Into<String>, verdict: GuardVerdict) {
        self.entries.push((guard.into(), verdict));
    }

    pub fn entries(&self) -> &[(String, GuardVerdict)] {
        &self.entries
    }

    /// Нарушения (Problem), в порядке добавления.
    pub fn problems(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .filter(|(_, v)| v.is_problem())
            .map(|(id, v)| (id.as_str(), v.message()))
            .collect()
    }

    /// Предупреждения (Warn), в порядке добавления.
    pub fn warnings(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .filter(|(_, v)| matches!(v, GuardVerdict::Warn(_)))
            .map(|(id, v)| (id.as_str(), v.message()))
            .collect()
    }

    /// Чисто — ни одного Problem (Warn допустим).
    pub fn is_clean(&self) -> bool {
        !self.entries.iter().any(|(_, v)| v.is_problem())
    }

    /// Короткая сводка для лога/письма.
    pub fn summary(&self) -> String {
        let ok = self.entries.iter().filter(|(_, v)| v.is_ok()).count();
        format!(
            "guards: {} всего, {} ok, {} warn, {} problem",
            self.entries.len(),
            ok,
            self.warnings().len(),
            self.problems().len()
        )
    }
}

/// Прогнать набор guards и собрать отчёт.
pub fn run_all(guards: &[&dyn Guard], ctx: &GuardContext) -> GuardReport {
    let mut report = GuardReport::new();
    for g in guards {
        report.push(g.id(), g.run(ctx));
    }
    report
}

/// Общий selfcheck встроенных guard'ов (без чтения реестра и без побочных
/// эффектов): каждый guard проверяет себя на каноническом примере.
pub fn selfcheck() -> GuardReport {
    let mut report = GuardReport::new();
    report.push(decomposition_guard::ID, decomposition_guard::selfcheck());
    report.push(identity_guard::ID, identity_guard::selfcheck());
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysOk;
    impl Guard for AlwaysOk {
        fn id(&self) -> &'static str {
            "always_ok"
        }
        fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
            GuardVerdict::Ok
        }
    }

    struct AlwaysProblem;
    impl Guard for AlwaysProblem {
        fn id(&self) -> &'static str {
            "always_problem"
        }
        fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
            GuardVerdict::Problem("зона пересекается".to_string())
        }
    }

    struct AlwaysWarn;
    impl Guard for AlwaysWarn {
        fn id(&self) -> &'static str {
            "always_warn"
        }
        fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
            GuardVerdict::Warn("worktree вне /tmp".to_string())
        }
    }

    #[test]
    fn verdict_classification() {
        assert!(GuardVerdict::Ok.is_ok());
        assert!(!GuardVerdict::Ok.is_problem());
        assert!(GuardVerdict::Warn("w".into()).is_problem() == false);
        assert!(GuardVerdict::Problem("p".into()).is_problem());
        assert_eq!(GuardVerdict::Ok.message(), "");
        assert_eq!(GuardVerdict::Problem("p".into()).message(), "p");
    }

    #[test]
    fn empty_report_is_clean() {
        let r = GuardReport::new();
        assert!(r.is_clean());
        assert!(r.problems().is_empty());
        assert_eq!(r.summary(), "guards: 0 всего, 0 ok, 0 warn, 0 problem");
    }

    #[test]
    fn builtin_selfcheck_is_clean() {
        let report = selfcheck();
        assert!(report.is_clean(), "{:?}", report.problems());
        let ids: Vec<String> = report.entries().iter().map(|(id, _)| id.clone()).collect();
        assert!(
            ids.iter().any(|id| id == "decomposition_guard"),
            "строенный selfcheck обязан включать decomposition_guard: {ids:?}"
        );
        assert!(
            ids.iter().any(|id| id == "identity_guard"),
            "guard идентичностей обязан вызываться из selfcheck: {ids:?}"
        );
        assert_eq!(report.entries().len(), 2);
    }

    #[test]
    fn warn_does_not_block() {
        let mut r = GuardReport::new();
        r.push("w", GuardVerdict::Warn("подозрительно".into()));
        assert!(r.is_clean(), "warn не блокирует");
        assert_eq!(r.warnings().len(), 1);
    }

    #[test]
    fn problem_blocks_and_is_reported() {
        let mut r = GuardReport::new();
        r.push("p", GuardVerdict::Problem("нарушено".into()));
        assert!(!r.is_clean());
        assert_eq!(r.problems(), vec![("p", "нарушено")]);
        assert_eq!(r.summary(), "guards: 1 всего, 0 ok, 0 warn, 1 problem");
    }

    #[test]
    fn run_all_keeps_order_and_counts() {
        let guards: Vec<&dyn Guard> = vec![&AlwaysOk, &AlwaysWarn, &AlwaysProblem];
        let ctx = GuardContext::new("/tmp/repo");
        let r = run_all(&guards, &ctx);
        let ids: Vec<&str> = r.entries().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["always_ok", "always_warn", "always_problem"]);
        assert!(!r.is_clean());
        assert_eq!(r.summary(), "guards: 3 всего, 1 ok, 1 warn, 1 problem");
    }

    #[test]
    fn context_carries_task_and_agent() {
        let ctx = GuardContext::new("/tmp/repo")
            .with_task("T-116")
            .with_agent("opencode-0a0e0cfc");
        assert_eq!(ctx.task_id.as_deref(), Some("T-116"));
        assert_eq!(ctx.agent.as_deref(), Some("opencode-0a0e0cfc"));
        assert_eq!(ctx.root, PathBuf::from("/tmp/repo"));
    }

    #[test]
    fn all_ok_report_is_clean_and_summarized() {
        let guards: Vec<&dyn Guard> = vec![&AlwaysOk, &AlwaysOk];
        let r = run_all(&guards, &GuardContext::default());
        assert!(r.is_clean());
        assert!(r.problems().is_empty());
        assert_eq!(r.summary(), "guards: 2 всего, 2 ok, 0 warn, 0 problem");
    }
}
