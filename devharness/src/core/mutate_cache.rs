//! Кэш пойманных мутантов: чтение mutants.out/iterator.json.

use std::collections::HashSet;

/// Ключ мутанта: file:line + replacement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MutantKey {
    pub file: String,
    pub line: u32,
    pub replacement: String,
}

/// Кэш пойманных мутантов.
#[derive(Debug, Clone, Default)]
pub struct MutantCache {
    caught: HashSet<MutantKey>,
}

impl MutantCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить пойманного мутанта в кэш.
    pub fn insert(&mut self, key: MutantKey) {
        self.caught.insert(key);
    }

    /// Проверить, пойман ли мутант ранее.
    pub fn is_caught(&self, key: &MutantKey) -> bool {
        self.caught.contains(key)
    }

    /// Количество пойманных.
    pub fn len(&self) -> usize {
        self.caught.len()
    }

    /// Пуст ли кэш.
    pub fn is_empty(&self) -> bool {
        self.caught.is_empty()
    }
}

/// Разобрать JSON из mutants.out/iterator.json и извлечь пойманных мутантов.
///
/// Формат: {"outcomes": [{"scenario": {"Mutant": {"name": "...", "file": "...", "span": {"start": {"line": N}}}}, "summary": "CaughtMutant", "replacement": "..."}]}
pub fn parse_iterator(text: &str) -> MutantCache {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Span {
        start: Option<Start>,
    }
    #[derive(Deserialize)]
    struct Start {
        line: Option<u32>,
    }
    #[derive(Deserialize)]
    struct Mutant {
        file: Option<String>,
        span: Option<Span>,
    }
    #[derive(Deserialize)]
    struct Scenario {
        #[serde(rename = "Mutant")]
        mutant: Option<Mutant>,
    }
    #[derive(Deserialize)]
    struct Outcome {
        scenario: Option<Scenario>,
        summary: Option<String>,
        replacement: Option<String>,
    }
    #[derive(Deserialize)]
    struct Root {
        outcomes: Option<Vec<Outcome>>,
    }

    let mut cache = MutantCache::new();

    let Ok(root) = serde_json::from_str::<Root>(text) else {
        return cache;
    };
    let Some(outcomes) = root.outcomes else {
        return cache;
    };

    for o in outcomes {
        if o.summary.as_deref() != Some("CaughtMutant") {
            continue;
        }
        let Some(scenario) = o.scenario else { continue };
        let Some(mutant) = scenario.mutant else {
            continue;
        };
        let file = mutant.file.unwrap_or_default();
        let line = mutant
            .span
            .and_then(|s| s.start)
            .and_then(|s| s.line)
            .unwrap_or(0);
        let replacement = o.replacement.unwrap_or_default();
        if !file.is_empty() {
            cache.insert(MutantKey {
                file,
                line,
                replacement,
            });
        }
    }

    cache
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache() {
        let c = MutantCache::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn insert_and_check() {
        let mut c = MutantCache::new();
        let key = MutantKey {
            file: "src/lib.rs".to_string(),
            line: 10,
            replacement: "foo".to_string(),
        };
        c.insert(key.clone());
        assert!(c.is_caught(&key));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn not_caught() {
        let c = MutantCache::new();
        let key = MutantKey {
            file: "src/lib.rs".to_string(),
            line: 10,
            replacement: "foo".to_string(),
        };
        assert!(!c.is_caught(&key));
    }

    #[test]
    fn parse_iterator_empty() {
        let c = parse_iterator("");
        assert!(c.is_empty());
    }

    #[test]
    fn parse_iterator_broken() {
        let c = parse_iterator("{not json");
        assert!(c.is_empty());
    }

    #[test]
    fn parse_iterator_caught() {
        let json = r#"{"outcomes": [{"scenario": {"Mutant": {"file": "src/lib.rs", "span": {"start": {"line": 10}}}}, "summary": "CaughtMutant", "replacement": "foo"}]}"#;
        let c = parse_iterator(json);
        assert_eq!(c.len(), 1);
        let key = MutantKey {
            file: "src/lib.rs".to_string(),
            line: 10,
            replacement: "foo".to_string(),
        };
        assert!(c.is_caught(&key));
    }

    #[test]
    fn parse_iterator_missed_not_cached() {
        let json = r#"{"outcomes": [{"scenario": {"Mutant": {"file": "src/lib.rs", "span": {"start": {"line": 10}}}}, "summary": "MissedMutant", "replacement": "foo"}]}"#;
        let c = parse_iterator(json);
        assert!(c.is_empty());
    }

    #[test]
    fn parse_iterator_multiple() {
        let json = r#"{"outcomes": [
            {"scenario": {"Mutant": {"file": "a.rs", "span": {"start": {"line": 1}}}}, "summary": "CaughtMutant", "replacement": "x"},
            {"scenario": {"Mutant": {"file": "b.rs", "span": {"start": {"line": 2}}}}, "summary": "CaughtMutant", "replacement": "y"}
        ]}"#;
        let c = parse_iterator(json);
        assert_eq!(c.len(), 2);
    }
}
