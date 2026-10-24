//! Передача задачи между агентами.
//!
//! Смена исполнителя с сохранением DoD и зоны. Чистые правила.

/// Результат передачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffResult {
    Ok,
    Err(HandoffError),
}

/// Ошибка передачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffError {
    AgentNotFound,
    TaskNotFound,
    TaskNotActive,
    WorktreeDirty,
    SameAgent,
    MissingZone,
}

/// Параметры передачи.
#[derive(Debug, Clone)]
pub struct HandoffReq {
    pub task_id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub from_worktree_dirty: bool,
    pub to_agent_exists: bool,
    pub task_active: bool,
    pub task_has_zone: bool,
}

/// Провести валидацию передачи задачи.
pub fn validate(req: &HandoffReq) -> HandoffResult {
    if req.task_id.is_empty() {
        return HandoffResult::Err(HandoffError::TaskNotFound);
    }
    if req.from_agent == req.to_agent {
        return HandoffResult::Err(HandoffError::SameAgent);
    }
    if !req.to_agent_exists {
        return HandoffResult::Err(HandoffError::AgentNotFound);
    }
    if !req.task_active {
        return HandoffResult::Err(HandoffError::TaskNotActive);
    }
    if !req.task_has_zone {
        return HandoffResult::Err(HandoffError::MissingZone);
    }
    if req.from_worktree_dirty {
        return HandoffResult::Err(HandoffError::WorktreeDirty);
    }
    HandoffResult::Ok
}

/// Можно ли передать задачу?
pub fn can_handoff(req: &HandoffReq) -> bool {
    matches!(validate(req), HandoffResult::Ok)
}

/// Сохранить ли DoD и зоны при передаче.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preserved {
    pub dod_preserved: bool,
    pub zone_preserved: bool,
}

/// При валидной передаче DoD и зоны сохраняются.
pub fn preserved_on_handoff(ok: bool) -> Preserved {
    if ok {
        Preserved {
            dod_preserved: true,
            zone_preserved: true,
        }
    } else {
        Preserved {
            dod_preserved: false,
            zone_preserved: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_req() -> HandoffReq {
        HandoffReq {
            task_id: "T-1".into(),
            from_agent: "a1".into(),
            to_agent: "a2".into(),
            from_worktree_dirty: false,
            to_agent_exists: true,
            task_active: true,
            task_has_zone: true,
        }
    }

    #[test]
    fn valid_handoff_ok() {
        let r = ok_req();
        assert!(can_handoff(&r));
        assert_eq!(validate(&r), HandoffResult::Ok);
    }

    #[test]
    fn same_agent_rejected() {
        let mut r = ok_req();
        r.to_agent = "a1".into();
        assert_eq!(validate(&r), HandoffResult::Err(HandoffError::SameAgent));
    }

    #[test]
    fn nonexistent_agent_rejected() {
        let mut r = ok_req();
        r.to_agent_exists = false;
        assert_eq!(validate(&r), HandoffResult::Err(HandoffError::AgentNotFound));
    }

    #[test]
    fn dirty_worktree_rejected() {
        let mut r = ok_req();
        r.from_worktree_dirty = true;
        assert_eq!(validate(&r), HandoffResult::Err(HandoffError::WorktreeDirty));
    }

    #[test]
    fn task_not_active_rejected() {
        let mut r = ok_req();
        r.task_active = false;
        assert_eq!(validate(&r), HandoffResult::Err(HandoffError::TaskNotActive));
    }

    #[test]
    fn missing_zone_rejected() {
        let mut r = ok_req();
        r.task_has_zone = false;
        assert_eq!(validate(&r), HandoffResult::Err(HandoffError::MissingZone));
    }

    #[test]
    fn preserved_when_ok() {
        let p = preserved_on_handoff(true);
        assert!(p.dod_preserved && p.zone_preserved);
    }

    #[test]
    fn preserved_when_bad() {
        let p = preserved_on_handoff(false);
        assert!(!p.dod_preserved && !p.zone_preserved);
    }
}
