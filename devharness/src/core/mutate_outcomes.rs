//! outcome-сводка по мутационному аудиту: разбор outcomes.json cargo-mutants.

use serde::Deserialize;

/// Сводка исходов мутационного аудита.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeSummary {
    pub total: usize,
    pub caught: usize,
    pub missed: usize,
    pub timeout: usize,
    pub unviable: usize,
    pub unknown: usize,
}

/// Разобрать outcomes.json cargo-mutants в сводку.
///
/// Битый/пустой JSON → unknown (не «чисто»): total=0, unknown=1.
pub fn parse_outcomes(text: &str) -> OutcomeSummary {
    #[derive(Deserialize)]
    struct Outcome {
        summary: Option<String>,
    }
    #[derive(Deserialize)]
    struct Root {
        outcomes: Option<Vec<Outcome>>,
    }

    let Ok(root) = serde_json::from_str::<Root>(text) else {
        return OutcomeSummary {
            total: 0,
            caught: 0,
            missed: 0,
            timeout: 0,
            unviable: 0,
            unknown: 1,
        };
    };

    let outcomes = match root.outcomes {
        Some(v) => v,
        None => {
            return OutcomeSummary {
                total: 0,
                caught: 0,
                missed: 0,
                timeout: 0,
                unviable: 0,
                unknown: 1,
            };
        }
    };

    let mut summary = OutcomeSummary {
        total: 0,
        caught: 0,
        missed: 0,
        timeout: 0,
        unviable: 0,
        unknown: 0,
    };

    for o in outcomes {
        summary.total += 1;
        match o.summary.as_deref() {
            Some("CaughtMutant") => summary.caught += 1,
            Some("MissedMutant") => summary.missed += 1,
            Some("Timeout") => summary.timeout += 1,
            Some("Unviable") => summary.unviable += 1,
            _ => summary.unknown += 1,
        }
    }

    // Пустой список outcomes → считается unknown (не «чисто»).
    if summary.total == 0 && summary.unknown == 0 {
        summary.unknown = 1;
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_is_unknown() {
        let s = parse_outcomes("");
        assert_eq!(s.total, 0);
        assert_eq!(s.unknown, 1);
    }

    #[test]
    fn broken_json_is_unknown() {
        let s = parse_outcomes("{not json");
        assert_eq!(s.total, 0);
        assert_eq!(s.unknown, 1);
    }

    #[test]
    fn empty_outcomes_is_unknown() {
        let s = parse_outcomes(r#"{"outcomes": []}"#);
        assert_eq!(s.total, 0);
        assert_eq!(s.unknown, 1);
    }

    #[test]
    fn caught_mutant() {
        let s = parse_outcomes(r#"{"outcomes": [{"summary": "CaughtMutant"}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.caught, 1);
        assert_eq!(s.missed, 0);
        assert_eq!(s.timeout, 0);
        assert_eq!(s.unviable, 0);
        assert_eq!(s.unknown, 0);
    }

    #[test]
    fn missed_mutant() {
        let s = parse_outcomes(r#"{"outcomes": [{"summary": "MissedMutant"}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.missed, 1);
    }

    #[test]
    fn timeout() {
        let s = parse_outcomes(r#"{"outcomes": [{"summary": "Timeout"}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.timeout, 1);
    }

    #[test]
    fn unviable() {
        let s = parse_outcomes(r#"{"outcomes": [{"summary": "Unviable"}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.unviable, 1);
    }

    #[test]
    fn unknown_summary() {
        let s = parse_outcomes(r#"{"outcomes": [{"summary": "SomethingElse"}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.unknown, 1);
    }

    #[test]
    fn no_summary_field() {
        let s = parse_outcomes(r#"{"outcomes": [{}]}"#);
        assert_eq!(s.total, 1);
        assert_eq!(s.unknown, 1);
    }

    #[test]
    fn mixed_outcomes() {
        let json = r#"{"outcomes": [
            {"summary": "CaughtMutant"},
            {"summary": "MissedMutant"},
            {"summary": "Timeout"},
            {"summary": "Unviable"}
        ]}"#;
        let s = parse_outcomes(json);
        assert_eq!(s.total, 4);
        assert_eq!(s.caught, 1);
        assert_eq!(s.missed, 1);
        assert_eq!(s.timeout, 1);
        assert_eq!(s.unviable, 1);
        assert_eq!(s.unknown, 0);
    }

    #[test]
    fn missing_outcomes_field() {
        let s = parse_outcomes(r#"{"foo": "bar"}"#);
        assert_eq!(s.total, 0);
        assert_eq!(s.unknown, 1);
    }
}
