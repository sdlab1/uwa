use super::gates::{Gate, Verdict};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Commit { msg: String },
    Retry { gate: Gate, tail: String },
    Die { reason: String },
}

impl Default for Step {
    /// Нейтральное значение для cargo-mutants: мутант подставляет
    /// `Default::default()` вместо тела `next()` — без Default такой мутант НЕ
    /// КОМПИЛИРУЕТСЯ (unviable = дыра, регламент P9). Ручной impl: у варианта
    /// Retry поле Gate, а Default для Gate не нужен и не вводится.
    fn default() -> Self {
        Step::Die {
            reason: String::new(),
        }
    }
}

impl Step {
    pub fn commit_msg(&self) -> Option<&str> {
        match self {
            Step::Commit { msg } => Some(msg),
            _ => None,
        }
    }
}

pub fn next(rounds_done: u32, max_fix: u32, verdict: &Verdict, summary_msg: &str) -> Step {
    match verdict {
        Verdict::Pass => Step::Commit {
            msg: summary_msg.to_string(),
        },
        Verdict::EnvBlocked { missing } => Step::Die {
            reason: format!("блокер СРЕДЫ: {missing} — fix-раунд бессмыслен"),
        },
        Verdict::Fail { gate, tail } if rounds_done < max_fix => Step::Retry {
            gate: gate.clone(),
            tail: tail.clone(),
        },
        Verdict::Fail { gate, .. } => Step::Die {
            reason: format!("гейт {gate:?} красный после {max_fix} раундов"),
        },
        // Блокер спеки: fix-раунд агента чужой код не починит — умираем сразу,
        // человека чинит спеку (P7: red.expect).
        Verdict::SpecBlock { gate, tail } => Step::Die {
            reason: format!("БЛОКЕР СПЕКИ ({gate:?}): {tail}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gates::Gate;

    #[test]
    fn plan_env_blocked_dies_immediately() {
        let v = Verdict::EnvBlocked {
            missing: "muex".into(),
        };
        assert!(matches!(next(0, 3, &v, "m"), Step::Die { .. }));
    }

    #[test]
    fn plan_retries_then_dies_at_max_fix() {
        let v = Verdict::Fail {
            gate: Gate::Verify,
            tail: "x".into(),
        };
        assert!(matches!(next(0, 3, &v, "m"), Step::Retry { .. }));
        assert!(matches!(next(2, 3, &v, "m"), Step::Retry { .. }));
        assert!(matches!(next(3, 3, &v, "m"), Step::Die { .. }));
    }

    #[test]
    fn plan_commit_carries_summary() {
        let step = next(0, 3, &Verdict::Pass, "feat: x");
        let msg = step.commit_msg().unwrap_or_else(|| unreachable!());
        assert_eq!(msg, "feat: x");
    }
}
