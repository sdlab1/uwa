//! Правила повышения прав: выдача роли lead / смена ведущего.
//!
//! Чистая логика без чтения конфигов/CLI. Проверяет: текущий ведущий,
//! пароль задан, секрет совпал. Возвращает внятную причину отказа.

/// Роль в системе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Worker,
    Lead,
}

/// Состояние для проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadContext {
    /// Имя/адрес текущего ведущего. None — ведущий не назначен.
    pub current_lead: Option<String>,
    /// Задан ли пароль ведущего.
    pub password_set: bool,
    /// Предоставленный секрет (сырой пароль/токен).
    pub provided_secret: Option<String>,
    /// Ожидаемый секрет (хэш/значение для сравнения в чистой модели).
    pub expected_secret: Option<String>,
}

/// Результат проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadDecision {
    /// Разрешено.
    Allow,
    /// Отказ с причиной.
    Deny(LeadReason),
}

/// Причина отказа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadReason {
    /// Ведущий не назначен.
    NoLead,
    /// Пароль не задан.
    NoPasswordSet,
    /// Нет переданного секрета.
    NoSecretProvided,
    /// Секрет не совпал.
    SecretMismatch,
    /// Текущий пользователь не является ведущим.
    NotCurrentLead,
    /// Операция требует роли lead.
    RequiresLead,
}

/// Может ли caller выдать роль lead другому агенту?
pub fn can_elevate_to_lead(
    current_caller_is_lead: bool,
    ctx: &LeadContext,
) -> LeadDecision {
    if !current_caller_is_lead {
        return LeadDecision::Deny(LeadReason::NotCurrentLead);
    }
    check_lead_privilege(ctx)
}

/// Может ли текущий ведущий сменить ведущего (transfer)?
pub fn can_transfer_lead(current_caller_is_lead: bool, ctx: &LeadContext) -> LeadDecision {
    if !current_caller_is_lead {
        return LeadDecision::Deny(LeadReason::NotCurrentLead);
    }
    check_lead_privilege(ctx)
}

/// Может ли кто-то сменить пароль ведущего?
pub fn can_change_password(current_caller_is_lead: bool, ctx: &LeadContext) -> LeadDecision {
    if !current_caller_is_lead {
        return LeadDecision::Deny(LeadReason::NotCurrentLead);
    }
    check_lead_privilege(ctx)
}

/// Базовая проверка привилегии ведущего.
fn check_lead_privilege(ctx: &LeadContext) -> LeadDecision {
    if ctx.current_lead.is_none() {
        return LeadDecision::Deny(LeadReason::NoLead);
    }
    if !ctx.password_set {
        return LeadDecision::Deny(LeadReason::NoPasswordSet);
    }
    let provided = match ctx.provided_secret.as_ref() {
        Some(s) if !s.is_empty() => s,
        Some(_) => return LeadDecision::Deny(LeadReason::NoSecretProvided),
        None => return LeadDecision::Deny(LeadReason::NoSecretProvided),
    };
    let expected = match ctx.expected_secret.as_ref() {
        Some(s) if !s.is_empty() => s,
        _ => return LeadDecision::Deny(LeadReason::NoPasswordSet),
    };
    if provided != expected {
        return LeadDecision::Deny(LeadReason::SecretMismatch);
    }
    LeadDecision::Allow
}

/// Проверить простое условие: разрешена ли операция при требовании роли lead.
pub fn require_lead(role_is_lead: bool) -> LeadDecision {
    if role_is_lead {
        LeadDecision::Allow
    } else {
        LeadDecision::Deny(LeadReason::RequiresLead)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_ctx() -> LeadContext {
        LeadContext {
            current_lead: Some("minimax".to_string()),
            password_set: true,
            provided_secret: Some("secret123".to_string()),
            expected_secret: Some("secret123".to_string()),
        }
    }

    #[test]
    fn lead_can_elevate_with_correct_password() {
        let ctx = ok_ctx();
        assert_eq!(can_elevate_to_lead(true, &ctx), LeadDecision::Allow);
    }

    #[test]
    fn non_lead_cannot_elevate() {
        let ctx = ok_ctx();
        assert_eq!(
            can_elevate_to_lead(false, &ctx),
            LeadDecision::Deny(LeadReason::NotCurrentLead)
        );
    }

    #[test]
    fn no_lead_denied() {
        let ctx = LeadContext {
            current_lead: None,
            password_set: true,
            provided_secret: Some("x".to_string()),
            expected_secret: Some("x".to_string()),
        };
        assert_eq!(can_transfer_lead(true, &ctx), LeadDecision::Deny(LeadReason::NoLead));
    }

    #[test]
    fn no_password_set_denied() {
        let ctx = LeadContext {
            current_lead: Some("lead".to_string()),
            password_set: false,
            provided_secret: Some("x".to_string()),
            expected_secret: Some("x".to_string()),
        };
        assert_eq!(can_change_password(true, &ctx), LeadDecision::Deny(LeadReason::NoPasswordSet));
    }

    #[test]
    fn no_secret_provided_denied() {
        let ctx = LeadContext {
            current_lead: Some("lead".to_string()),
            password_set: true,
            provided_secret: None,
            expected_secret: Some("x".to_string()),
        };
        assert_eq!(can_elevate_to_lead(true, &ctx), LeadDecision::Deny(LeadReason::NoSecretProvided));
    }

    #[test]
    fn secret_mismatch_denied() {
        let ctx = LeadContext {
            current_lead: Some("lead".to_string()),
            password_set: true,
            provided_secret: Some("bad".to_string()),
            expected_secret: Some("good".to_string()),
        };
        assert_eq!(can_transfer_lead(true, &ctx), LeadDecision::Deny(LeadReason::SecretMismatch));
    }

    #[test]
    fn require_lead_works() {
        assert_eq!(require_lead(true), LeadDecision::Allow);
        assert_eq!(require_lead(false), LeadDecision::Deny(LeadReason::RequiresLead));
    }
}
