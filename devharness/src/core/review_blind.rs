//! `core/review_blind` — две связанные, но разные вещи про «слепоту»:
//!
//! 1. **Слепые зоны** (P8, третья петля): мутант выжил / строка не покрыта /
//!    coverage gap — реестр того, что тесты не видят.
//! 2. **Слепое ревью** (`REVIEW_BLIND`, E5/C2 + F-4): ревьюер видит только
//!    спеку, список файлов, дифф и правила — и **не видит** авторский
//!    пересказ. Якорение на «автор объяснил, значит всё в порядке» — главный
//!    механизм, которым self-review пропускает мусор.
//!
//! Канон: `docs/E_series.md` §C2, `docs/F_series.md` §F-4,
//! `docs/F_series_proto.md`, `docs/devh_third_loop.md` (P8).
//!
//! Модуль намеренно «глупый»: он не ходит в git и не знает про реестр —
//! только собирает текст промпта, проверяет, что в нём нет авторских
//! пересказов, и копит слепые зоны. Проверка нужна как guard: если кто-то
//! добавит в промпт секцию «итог автора» — это тихая дыра в ревью, и она
//! должна ловиться тестом.

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

/// Материалы ревью (всё, что ревьюер имеет право видеть).
#[derive(Debug, Default, Clone)]
pub struct BlindInput<'a> {
    /// Спека задачи (требования, критерии приёмки).
    pub spec: &'a str,
    /// Файлы, попавшие в зону ревью.
    pub files: &'a [String],
    /// Дифф (unified diff).
    pub diff: &'a str,
    /// Правила проекта (`RULES`), действующие для агента.
    pub rules: &'a [String],
}

/// Маркеры авторского пересказа: если они есть в промпте — ревью не слепое.
const AUTHOR_MARKERS: &[&str] = &[
    "итог автора",
    "summary автора",
    "автор объяснил",
    "по словам автора",
    "author summary",
];

/// Собрать слепой промпт ревью.
///
/// Порядок разделов фиксирован: спека → файлы → дифф → правила. Авторский
/// пересказ не добавляется НИКОГДА — это и есть суть слепоты.
pub fn build_prompt(input: &BlindInput<'_>) -> String {
    let mut out = String::new();
    out.push_str("## Спека\n\n");
    let spec = input.spec.trim();
    if spec.is_empty() {
        out.push_str("(спека не задана)\n");
    } else {
        out.push_str(spec);
        out.push('\n');
    }
    out.push_str("\n## Файлы\n\n");
    if input.files.is_empty() {
        out.push_str("(файлы не указаны)\n");
    } else {
        for f in input.files {
            out.push_str("- ");
            out.push_str(f.trim());
            out.push('\n');
        }
    }
    out.push_str("\n## Дифф\n\n");
    let diff = input.diff.trim();
    if diff.is_empty() {
        out.push_str("(диффа нет)\n");
    } else {
        out.push_str(diff);
        out.push('\n');
    }
    out.push_str("\n## Правила\n\n");
    if input.rules.is_empty() {
        out.push_str("(правила не заданы)\n");
    } else {
        for r in input.rules {
            out.push_str("- ");
            out.push_str(r.trim());
            out.push('\n');
        }
    }
    out
}

/// Найден ли в тексте авторский пересказ (слепота нарушена).
///
/// Проверка регистронезависимая: «Итог автора» и «итог автора» — одно и то же.
pub fn has_author_summary(text: &str) -> bool {
    let lower = text.to_lowercase();
    AUTHOR_MARKERS.iter().any(|m| lower.contains(m))
}

/// Слепой ли промпт: нет авторских пересказов.
pub fn is_blind(prompt: &str) -> bool {
    !has_author_summary(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        vec!["devharness/src/core/load.rs".to_string()]
    }

    fn rules() -> Vec<String> {
        vec!["R9: только свои пути".to_string()]
    }

    fn input<'a>(files: &'a [String], rules: &'a [String]) -> BlindInput<'a> {
        BlindInput {
            spec: "SPEC: посчитать load",
            files,
            diff: "+new line",
            rules,
        }
    }

    #[test]
    fn prompt_contains_all_sections() {
        let fs = files();
        let rs = rules();
        let p = build_prompt(&input(&fs, &rs));
        assert!(p.contains("## Спека"), "{p}");
        assert!(p.contains("devharness/src/core/load.rs"), "{p}");
        assert!(p.contains("+new"), "{p}");
        assert!(p.contains("R9: только свои пути"), "{p}");
    }

    #[test]
    fn prompt_has_no_author_summary() {
        let fs = files();
        let rs = rules();
        let p = build_prompt(&input(&fs, &rs));
        assert!(is_blind(&p), "в слепом промпте нет пересказа автора: {p}");
    }

    #[test]
    fn empty_inputs_are_explicit_not_blank() {
        let p = build_prompt(&BlindInput::default());
        assert!(p.contains("(спека не задана)"), "{p}");
        assert!(p.contains("(файлы не указаны)"), "{p}");
        assert!(p.contains("(диффа нет)"), "{p}");
        assert!(p.contains("(правила не заданы)"), "{p}");
        assert!(is_blind(&p));
    }

    #[test]
    fn author_summary_is_detected_case_insensitively() {
        assert!(has_author_summary("Итог автора: всё ок"));
        assert!(has_author_summary("AUTHOR SUMMARY: fine"));
        assert!(has_author_summary("по словам автора, всё хорошо"));
        assert!(!has_author_summary("спека без пересказа"));
    }

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
        rb.add(BlindZone::new(
            BlindZoneKind::MutantSurvived,
            "b.rs",
            2,
            "y",
        ));
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
