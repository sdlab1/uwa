use super::types::ItemId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Стартовое состояние. `Default` — обязателен: cargo-mutants подставляет
    /// `Default::default()` вместо тела функции (P9: unviable = дыра).
    #[default]
    Open,
    Active,
    Done,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Active => "active",
            Status::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Status {
        match s.trim() {
            "active" => Status::Active,
            "done" => Status::Done,
            _ => Status::Open,
        }
    }

    /// Является ли статус «закрытым» (не занимает worktree).
    pub fn is_done(&self) -> bool {
        matches!(self, Status::Done)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Event {
    /// Стартовое событие. `Default` — обязателен (см. Status: мутант-P9).
    #[default]
    Claimed,
    Merged,
    Aborted,
}

impl Event {
    pub fn as_str(&self) -> &'static str {
        match self {
            Event::Claimed => "claimed",
            Event::Merged => "merged",
            Event::Aborted => "aborted",
        }
    }
}

/// Разбор поля события из строки events.log; None — мусорная строка.
pub fn event_parse(s: &str) -> Option<Event> {
    match s.trim() {
        "claimed" => Some(Event::Claimed),
        "merged" => Some(Event::Merged),
        "aborted" => Some(Event::Aborted),
        _ => None,
    }
}

/// Строка events.log: «{дата} | {W-NNN} | {event}» — берём последнее поле
/// события; мусорные строки (ручной редактуры, обрезка записи) пропускаем.
pub fn events_from_log(text: &str) -> Vec<Event> {
    text.lines()
        .filter_map(|l| l.split(" | ").last().and_then(event_parse))
        .collect()
}

#[derive(Debug, thiserror::Error)]
#[error("недопустимый переход {from:?} + {event:?}")]
pub struct InvalidTransition {
    pub from: Status,
    pub event: Event,
}

pub fn transition(s: Status, ev: Event) -> Result<Status, InvalidTransition> {
    match (s, ev) {
        (Status::Open, Event::Claimed) => Ok(Status::Active),
        (Status::Active, Event::Merged) => Ok(Status::Done),
        (Status::Active, Event::Aborted) => Ok(Status::Open),
        (s, e) => Err(InvalidTransition { from: s, event: e }),
    }
}

/// Решение claim-фазы по предыдущему статусу (чистая, план A3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaimAction {
    #[default]
    Claim,
    Resume,
    RejectDone,
}

pub fn claim_action(prev: Status) -> ClaimAction {
    match prev {
        Status::Done => ClaimAction::RejectDone,
        Status::Active => ClaimAction::Resume,
        Status::Open => ClaimAction::Claim,
    }
}

/// Свёртка журнала events.log в статус (replay для doctor, план A3).
/// Недопустимые строки журнала молча пропускаются — doctor не умирает.
pub fn fold(events: &[Event]) -> Status {
    let mut s = Status::Open;
    for &e in events {
        if let Ok(next) = transition(s, e) {
            s = next;
        }
    }
    s
}

pub fn can_claim(active: u32, max_active: u32) -> bool {
    max_active == 0 || active < max_active
}

pub fn next_id(existing: &[ItemId]) -> ItemId {
    ItemId(
        existing
            .iter()
            .map(|i| i.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_rejects_shortcuts() {
        assert!(transition(Status::Done, Event::Claimed).is_err());
        assert!(transition(Status::Open, Event::Merged).is_err());
        assert!(matches!(
            transition(Status::Active, Event::Aborted),
            Ok(Status::Open)
        ));
        assert!(matches!(
            transition(Status::Open, Event::Claimed),
            Ok(Status::Active)
        ));
        assert!(matches!(
            transition(Status::Active, Event::Merged),
            Ok(Status::Done)
        ));
    }

    #[test]
    fn claim_action_claims_open() {
        assert_eq!(claim_action(Status::Open), ClaimAction::Claim);
    }

    #[test]
    fn claim_action_resumes_active() {
        assert_eq!(claim_action(Status::Active), ClaimAction::Resume);
    }

    #[test]
    fn claim_action_rejects_done() {
        assert_eq!(claim_action(Status::Done), ClaimAction::RejectDone);
    }

    #[test]
    fn fold_replay_matches_transition() {
        assert_eq!(fold(&[Event::Claimed, Event::Merged]), Status::Done);
        assert_eq!(fold(&[Event::Claimed, Event::Aborted]), Status::Open);
        assert_eq!(fold(&[]), Status::Open);
    }

    #[test]
    fn events_from_log_parses_and_skips_garbage() {
        let log = "2026-10-19 | W-001 | claimed\n2026-10-20 | W-001 | merged\nшум без полей\n2026-10-21 | W-001 | Claimed\n2026-10-22 | W-001 | claimed | лишнее-поле\n";
        assert_eq!(events_from_log(log), vec![Event::Claimed, Event::Merged]);
        assert_eq!(fold(&events_from_log(log)), Status::Done);
        assert!(events_from_log("").is_empty());
        assert!(events_from_log("вообще не журнал").is_empty());
    }

    #[test]
    fn event_codec_roundtrip_and_garbage() {
        assert_eq!(Event::Claimed.as_str(), "claimed");
        assert_eq!(Event::Merged.as_str(), "merged");
        assert_eq!(Event::Aborted.as_str(), "aborted");
        for ev in [Event::Claimed, Event::Merged, Event::Aborted] {
            assert_eq!(event_parse(ev.as_str()), Some(ev));
        }
        assert_eq!(event_parse(" Claimed "), None);
        assert_eq!(event_parse("шум"), None);
    }

    #[test]
    fn wip_zero_disables_limit() {
        assert!(can_claim(100, 0));
        assert!(!can_claim(3, 3));
        assert!(can_claim(2, 3));
    }

    #[test]
    fn next_id_is_max_plus_one() {
        let ids = vec![ItemId(1), ItemId(8), ItemId(9)];
        assert_eq!(next_id(&ids), ItemId(10));
        assert_eq!(next_id(&[]), ItemId(1));
    }
}
