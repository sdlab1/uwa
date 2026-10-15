use super::types::ItemId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Claimed,
    Merged,
    Aborted,
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

pub fn can_claim(active: u32, max_active: u32) -> bool {
    max_active == 0 || active < max_active
}

pub fn next_id(existing: &[ItemId]) -> ItemId {
    ItemId(existing.iter().map(|i| i.0).max().unwrap_or(0) + 1)
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
