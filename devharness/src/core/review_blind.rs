//! review_blind: определение слепых зон в review-процессе (P8 третья петля).
//!
//! Слепая зона = код, который не покрывается мутационными тестами
//! (mutant survived) и не проверяется обычными тестами.
//!
//! Этот модуль — каркас: регистрация и проверка того, что review_blind
//! корректно импортируется и компилируется в составе core.

/// Состояние слепой зоны.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlindZoneKind {
    /// Мутант выжил (survived).
    MutantSurvived,
    /// Тест не покрывает строку.
    UncoveredLine,
    /// Coverage gap.
    CoverageGap,
}

/// Одна найденная слепая зона.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlindZone {
    pub kind: BlindZoneKind,
    pub file: String,
    pub line: u32,
    /// Краткое описание.
    pub description: String,
}

impl BlindZone {
    pub fn new(kind: BlindZoneKind, file: &str, line: u32, description: &str) -> Self {
        Self {
            kind,
            file: file.to_string(),
            line,
            description: description.to_string(),
        }
    }
}

/// Реестр слепых зон для review-сессии.
#[derive(Debug, Default, Clone)]
pub struct ReviewBlind {
    pub zones: Vec<BlindZone>,
}

impl ReviewBlind {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, zone: BlindZone) {
        self.zones.push(zone);
    }

    /// Есть ли слепые зоны.
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let rb = ReviewBlind::new();
        assert!(rb.is_empty());
    }

    #[test]
    fn add_zone_makes_non_empty() {
        let mut rb = ReviewBlind::new();
        rb.add(BlindZone::new(
            BlindZoneKind::MutantSurvived,
            "src/main.rs",
            42,
            "survived mutant",
        ));
        assert!(!rb.is_empty());
        assert_eq!(rb.zones.len(), 1);
    }

    #[test]
    fn blind_zone_fields_correct() {
        let zone = BlindZone::new(
            BlindZoneKind::UncoveredLine,
            "src/core.rs",
            10,
            "not tested",
        );
        assert_eq!(zone.kind, BlindZoneKind::UncoveredLine);
        assert_eq!(zone.file, "src/core.rs");
        assert_eq!(zone.line, 10);
        assert_eq!(zone.description, "not tested");
    }

    #[test]
    fn default_is_empty() {
        let rb: ReviewBlind = Default::default();
        assert!(rb.is_empty());
    }

    #[test]
    fn multiple_zones_preserve_order() {
        let mut rb = ReviewBlind::new();
        rb.add(BlindZone::new(BlindZoneKind::CoverageGap, "a.rs", 1, "x"));
        rb.add(BlindZone::new(BlindZoneKind::MutantSurvived, "b.rs", 2, "y"));
        rb.add(BlindZone::new(BlindZoneKind::UncoveredLine, "c.rs", 3, "z"));
        assert_eq!(rb.zones.len(), 3);
        assert_eq!(rb.zones[0].file, "a.rs");
        assert_eq!(rb.zones[2].file, "c.rs");
    }

    #[test]
    fn kind_variants_distinct() {
        let k1 = BlindZoneKind::MutantSurvived;
        let k2 = BlindZoneKind::UncoveredLine;
        let k3 = BlindZoneKind::CoverageGap;
        assert_ne!(k1, k2);
        assert_ne!(k2, k3);
        assert_ne!(k1, k3);
    }
}
