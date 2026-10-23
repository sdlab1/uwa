//! Таксономия классов слепоты тестов (P8).
//!
//! Жанр cargo-mutants -> Класс слепоты (blind:*) -> Правило для RULES/промпта.
//!
//! Базовый набор из 6 классов (расширяется при дистилляции человеком).

use std::collections::HashMap;
use serde::{Serialize, Deserialize};

/// Класс слепоты тестов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlindClass {
    /// `FnValue` — точные значения возврата (None/Some/число/строка), не `is_some()`/`is_ok()`.
    FnValue,
    /// `BinaryOperator`/`ComparisonOperator` — плечи условий и границы (оба плеча + равенство).
    Boundary,
    /// `MatchArmGuard`/`MatchArm` — непокрытые ветки match; guard — отдельно.
    MatchArms,
    /// Делегат/обёртка — контракт-тест на результат, не на факт делегирования.
    Wrapper,
    /// Чтение env/config — три кейса: установлено, сброшено, мусор на границе.
    EnvVar,
    /// Чистая функция с операторами — один proptest с инвариантом вместо пяти эрзац-юнитов.
    PureFn,
}

/// Жанр мутанта из `outcomes.json` (cargo-mutants `scenario.Mutant.genre`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MutantGenre {
    FnValue,
    BinaryOperator,
    ComparisonOperator,
    MatchArmGuard,
    MatchArm,
    Unknown,
}

impl From<&str> for MutantGenre {
    fn from(s: &str) -> Self {
        match s {
            "FnValue" => MutantGenre::FnValue,
            "BinaryOperator" => MutantGenre::BinaryOperator,
            "ComparisonOperator" => MutantGenre::ComparisonOperator,
            "MatchArmGuard" => MutantGenre::MatchArmGuard,
            "MatchArm" => MutantGenre::MatchArm,
            _ => MutantGenre::Unknown,
        }
    }
}

/// Маппинг жанра -> класс (базовый, расширяется через RULES).
pub fn genre_to_class(genre: MutantGenre) -> BlindClass {
    match genre {
        MutantGenre::FnValue => BlindClass::FnValue,
        MutantGenre::BinaryOperator | MutantGenre::ComparisonOperator => BlindClass::Boundary,
        MutantGenre::MatchArmGuard | MutantGenre::MatchArm => BlindClass::MatchArms,
        MutantGenre::Unknown => BlindClass::FnValue, // дефолт: «прямой ассерт результата»
    }
}

/// Класс -> авто-идея red-теста (подсказка для `observations.md`).
pub fn class_to_red_idea(class: BlindClass) -> &'static str {
    match class {
        BlindClass::FnValue => "ассерт точного значения (None/Some(x)/число/строка), не is_some()/is_ok(); граничные значения отдельно",
        BlindClass::Boundary => "кейсы на оба плеча каждого нового if/сравнения + граница (включая равенство)",
        BlindClass::MatchArms => "кейс на каждую ветку match; guard — отдельно",
        BlindClass::Wrapper => "прямой контракт-тест на результат обёртки, не на факт делегирования",
        BlindClass::EnvVar => "тест с установленным значением, сброшенным и на границе (мусор -> дефолт)",
        BlindClass::PureFn => "один proptest с инвариантом вместо пяти эрзац-юнитов",
    }
}

/// Класс -> тег для RULES (`blind:*`).
pub fn class_to_tag(class: BlindClass) -> &'static str {
    match class {
        BlindClass::FnValue => "blind:fn-value",
        BlindClass::Boundary => "blind:boundary",
        BlindClass::MatchArms => "blind:match-arms",
        BlindClass::Wrapper => "blind:wrapper",
        BlindClass::EnvVar => "blind:env-var",
        BlindClass::PureFn => "blind:pure-fn",
    }
}

/// Все известные теги (для `--rules` фильтрации).
pub fn all_tags() -> Vec<&'static str> {
    vec![
        "blind:fn-value",
        "blind:boundary",
        "blind:match-arms",
        "blind:wrapper",
        "blind:env-var",
        "blind:pure-fn",
    ]
}

/// Правило-класс для RULES_LOG (текст, который дописывается при дистилляции).
pub fn class_to_rule_text(class: BlindClass) -> &'static str {
    match class {
        BlindClass::FnValue => "Новая pub-fn с логикой получает прямой тест с точным ассертом (значение/None/Some(x)/граница), не is_some/is_ok. Признак в триаже: жанр FnValue.",
        BlindClass::Boundary => "Новое if/сравнение/булево покрывается кейсами на оба плеча и на границу (равенство). Признак: BinaryOperator/ComparisonOperator.",
        BlindClass::MatchArms => "Каждая ветка match и каждый guard — отдельный кейс.",
        BlindClass::Wrapper => "Делегирующая pub-fn с добавленной логикой — контракт-тест на результат, не на делегирование. Признак: FnValue в тонких модулях.",
        BlindClass::EnvVar => "Чтение env/config — три кейса: установлено, сброшено, мусор на границе.",
        BlindClass::PureFn => "Чистая функция с операторами — proptest с инвариантом, не пять юнитов.",
    }
}

/// Группировка выживших по классу для `devh survivors --rules`.
pub fn group_survivors_by_class(
    survivors: &[(String, MutantGenre)], // (file:line:mutation, genre)
) -> HashMap<BlindClass, Vec<(String, MutantGenre)>> {
    let mut groups: HashMap<BlindClass, Vec<(String, MutantGenre)>> = HashMap::new();
    for (location, genre) in survivors {
        let class = genre_to_class(*genre);
        groups.entry(class).or_default().push((location.clone(), *genre));
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genre_fn_value_maps_to_fn_value_class() {
        assert_eq!(genre_to_class(MutantGenre::FnValue), BlindClass::FnValue);
    }

    #[test]
    fn genre_binary_operator_maps_to_boundary() {
        assert_eq!(genre_to_class(MutantGenre::BinaryOperator), BlindClass::Boundary);
        assert_eq!(genre_to_class(MutantGenre::ComparisonOperator), BlindClass::Boundary);
    }

    #[test]
    fn genre_match_arms_map_to_match_arms() {
        assert_eq!(genre_to_class(MutantGenre::MatchArmGuard), BlindClass::MatchArms);
        assert_eq!(genre_to_class(MutantGenre::MatchArm), BlindClass::MatchArms);
    }

    #[test]
    fn genre_unknown_defaults_to_fn_value() {
        assert_eq!(genre_to_class(MutantGenre::Unknown), BlindClass::FnValue);
    }

    #[test]
    fn class_to_red_idea_returns_non_empty() {
        for class in [
            BlindClass::FnValue,
            BlindClass::Boundary,
            BlindClass::MatchArms,
            BlindClass::Wrapper,
            BlindClass::EnvVar,
            BlindClass::PureFn,
        ] {
            assert!(!class_to_red_idea(class).is_empty(), "{class:?}");
        }
    }

    #[test]
    fn class_to_tag_is_blind_prefix() {
        for class in [
            BlindClass::FnValue,
            BlindClass::Boundary,
            BlindClass::MatchArms,
            BlindClass::Wrapper,
            BlindClass::EnvVar,
            BlindClass::PureFn,
        ] {
            let tag = class_to_tag(class);
            assert!(tag.starts_with("blind:"), "{class:?} -> {tag}");
        }
    }

    #[test]
    fn all_tags_has_six_entries() {
        assert_eq!(all_tags().len(), 6);
    }

    #[test]
    fn class_to_rule_text_non_empty() {
        for class in [
            BlindClass::FnValue,
            BlindClass::Boundary,
            BlindClass::MatchArms,
            BlindClass::Wrapper,
            BlindClass::EnvVar,
            BlindClass::PureFn,
        ] {
            assert!(!class_to_rule_text(class).is_empty(), "{class:?}");
        }
    }

    #[test]
    fn group_survivors_by_class_groups_correctly() {
        let survivors = vec![
            ("file.rs:10:mut".to_string(), MutantGenre::FnValue),
            ("file.rs:20:mut".to_string(), MutantGenre::BinaryOperator),
            ("other.rs:5:mut".to_string(), MutantGenre::MatchArm),
        ];
        let groups = group_survivors_by_class(&survivors);
        assert_eq!(groups.get(&BlindClass::FnValue).map(|v| v.len()), Some(1));
        assert_eq!(groups.get(&BlindClass::Boundary).map(|v| v.len()), Some(1));
        assert_eq!(groups.get(&BlindClass::MatchArms).map(|v| v.len()), Some(1));
        assert_eq!(groups.get(&BlindClass::Wrapper), None);
    }

    #[test]
    fn class_to_tag_unique() {
        let tags = all_tags();
        let mut sorted = tags.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(tags.len(), sorted.len(), "теги уникальны");
    }
}
