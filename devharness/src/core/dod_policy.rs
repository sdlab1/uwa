//! Политика DoD-гейта.
//!
//! Чистая логика: критерии подтверждены + машинные проверки прошли + digest зоны совпал -> Ready/NotReady.

/// Статус DoD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DodStatus {
    Ready,
    NotReady,
}

/// Недостающий пункт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingItem {
    CriteriaNotAcked,
    ChecksNotPassed,
    DigestMismatch,
    ZoneEmpty,
}

/// Контекст для проверки DoD.
#[derive(Debug, Clone)]
pub struct DodContext {
    /// Критериев всего.
    pub criteria_total: usize,
    /// Подтверждённых критериев (acked).
    pub criteria_acked: usize,
    /// Всего проверок.
    pub checks_total: usize,
    /// Прошедших проверок.
    pub checks_passed: usize,
    /// Совпал ли digest зоны с ожидаемым.
    pub digest_ok: bool,
    /// Есть ли изменения в зоне.
    pub zone_has_changes: bool,
}

impl Default for DodContext {
    fn default() -> Self {
        Self {
            criteria_total: 0,
            criteria_acked: 0,
            checks_total: 0,
            checks_passed: 0,
            digest_ok: true,
            zone_has_changes: true,
        }
    }
}

/// Оценить готовность по DoD.
pub fn evaluate(ctx: &DodContext) -> (DodStatus, Vec<MissingItem>) {
    let mut missing = Vec::new();
    if ctx.criteria_total == 0 {
        missing.push(MissingItem::CriteriaNotAcked);
    } else if ctx.criteria_acked < ctx.criteria_total {
        missing.push(MissingItem::CriteriaNotAcked);
    }
    if ctx.checks_total == 0 {
        missing.push(MissingItem::ChecksNotPassed);
    } else if ctx.checks_passed < ctx.checks_total {
        missing.push(MissingItem::ChecksNotPassed);
    }
    if !ctx.digest_ok {
        missing.push(MissingItem::DigestMismatch);
    }
    if !ctx.zone_has_changes {
        missing.push(MissingItem::ZoneEmpty);
    }
    if missing.is_empty() {
        (DodStatus::Ready, missing)
    } else {
        (DodStatus::NotReady, missing)
    }
}

/// Можно ли коммитить/сдавать?
pub fn is_ready(ctx: &DodContext) -> bool {
    matches!(evaluate(ctx).0, DodStatus::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_when_all_ok() {
        let ctx = DodContext {
            criteria_total: 2,
            criteria_acked: 2,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: true,
            zone_has_changes: true,
        };
        let (st, m) = evaluate(&ctx);
        assert_eq!(st, DodStatus::Ready);
        assert!(m.is_empty());
    }

    #[test]
    fn not_ready_missing_acked() {
        let ctx = DodContext {
            criteria_total: 2,
            criteria_acked: 1,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: true,
            zone_has_changes: true,
        };
        let (st, m) = evaluate(&ctx);
        assert_eq!(st, DodStatus::NotReady);
        assert!(m.contains(&MissingItem::CriteriaNotAcked));
    }

    #[test]
    fn not_ready_checks_failed() {
        let ctx = DodContext {
            criteria_total: 1,
            criteria_acked: 1,
            checks_total: 2,
            checks_passed: 0,
            digest_ok: true,
            zone_has_changes: true,
        };
        let (st, m) = evaluate(&ctx);
        assert_eq!(st, DodStatus::NotReady);
        assert!(m.contains(&MissingItem::ChecksNotPassed));
    }

    #[test]
    fn not_ready_digest_mismatch() {
        let ctx = DodContext {
            criteria_total: 1,
            criteria_acked: 1,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: false,
            zone_has_changes: true,
        };
        let (st, m) = evaluate(&ctx);
        assert_eq!(st, DodStatus::NotReady);
        assert!(m.contains(&MissingItem::DigestMismatch));
    }

    #[test]
    fn not_ready_empty_zone() {
        let ctx = DodContext {
            criteria_total: 1,
            criteria_acked: 1,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: true,
            zone_has_changes: false,
        };
        let (st, m) = evaluate(&ctx);
        assert_eq!(st, DodStatus::NotReady);
        assert!(m.contains(&MissingItem::ZoneEmpty));
    }

    #[test]
    fn empty_criteria_means_not_ready() {
        let ctx = DodContext {
            criteria_total: 0,
            criteria_acked: 0,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: true,
            zone_has_changes: true,
        };
        let (st, _) = evaluate(&ctx);
        assert_eq!(st, DodStatus::NotReady);
    }

    #[test]
    fn is_ready_wrapper() {
        let ctx = DodContext {
            criteria_total: 1,
            criteria_acked: 1,
            checks_total: 1,
            checks_passed: 1,
            digest_ok: true,
            zone_has_changes: true,
        };
        assert!(is_ready(&ctx));
    }
}
