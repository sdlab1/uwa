//! Реестр целей для Kani (формальная верификация).
//!
//! Цель: централизованный список функций/условий, которые проверяет `cargo kani`.
//! Используется для генерации harness-файлов и отслеживания покрытия.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Тип проверки (категория свойства).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CheckKind {
    /// Отсутствие паники (panic-free).
    NoPanic,
    /// Корректность арифметики (no overflow/underflow).
    Arithmetic,
    /// Инварианты структуры данных.
    Invariant,
    /// Постусловия функции.
    Postcondition,
    /// Предусловия (контракт входа).
    Precondition,
    /// Терминация (нет бесконечных циклов).
    Termination,
    /// Пользовательское свойство.
    Custom,
}

/// Одна цель верификации.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KaniTarget {
    /// Полный путь к функции (crate::module::function).
    pub function: String,
    /// Тип проверки.
    pub kind: CheckKind,
    /// Описание свойства (для отчётов).
    pub description: String,
    /// Дополнительные атрибуты (loop unwind, stubs и т.д.).
    #[serde(default)]
    pub attributes: Vec<String>,
    /// Ожидаемый результат: true = должно проходить, false = ожидаем counterexample.
    #[serde(default = "default_expected_pass")]
    pub expected_pass: bool,
}

fn default_expected_pass() -> bool {
    true
}

/// Реестр целей Kani.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KaniRegistry {
    pub targets: HashMap<String, KaniTarget>,
}

impl KaniRegistry {
    /// Создать пустой реестр.
    pub fn new() -> Self {
        Self {
            targets: HashMap::new(),
        }
    }

    /// Добавить цель (ключ = function).
    pub fn add(&mut self, target: KaniTarget) {
        self.targets.insert(target.function.clone(), target);
    }

    /// Получить цель по имени функции.
    pub fn get(&self, function: &str) -> Option<&KaniTarget> {
        self.targets.get(function)
    }

    /// Все цели заданного типа.
    pub fn by_kind(&self, kind: CheckKind) -> Vec<&KaniTarget> {
        self.targets
            .values()
            .filter(|t| t.kind == kind)
            .collect()
    }

    /// Генерация списка для `cargo kani` (функции с атрибутами).
    ///
    /// Возвращает список строк вида:
    /// `#[kani::proof] fn harness_<name>() { <function>(); }`
    pub fn generate_harnesses(&self) -> Vec<String> {
        self.targets
            .values()
            .map(|t| {
                let fn_name = t
                    .function
                    .split("::")
                    .last()
                    .unwrap_or("unknown");
                let attr = if t.attributes.is_empty() {
                    String::new()
                } else {
                    format!("{} ", t.attributes.join(" "))
                };
                format!(
                    "{attr}#[kani::proof]\nfn harness_{name}() {{\n    {func}();\n}}",
                    attr = attr,
                    name = fn_name,
                    func = t.function
                )
            })
            .collect()
    }

    /// Сохранить реестр в JSON (для CI/отчётов).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Загрузить реестр из JSON.
    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

/// Встроенные цели для devh (базовый набор).
pub fn builtin_registry() -> KaniRegistry {
    let mut reg = KaniRegistry::new();

    // core::complexity
    reg.add(KaniTarget {
        function: "crate::core::complexity::Complexity::from_metrics".into(),
        kind: CheckKind::Arithmetic,
        description: "log2/cycles/deps не переполняются, результат в 0..10".into(),
        attributes: vec!["#[kani::unwind(10)]".into()],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::core::complexity::Complexity::to_p_level".into(),
        kind: CheckKind::Invariant,
        description: "C0/1->P0, C2/3->P1, C4/5->P2, C6/7->P3, C8/9->P4, C10->P5".into(),
        attributes: vec![],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::core::complexity::filter_by_complexity".into(),
        kind: CheckKind::Postcondition,
        description: "Ok <=> task_c <= ceiling".into(),
        attributes: vec![],
        expected_pass: true,
    });

    // core::rate
    reg.add(KaniTarget {
        function: "crate::commands::rate::load_stats".into(),
        kind: CheckKind::NoPanic,
        description: "Парсинг JSONL не паникует на любом вводе".into(),
        attributes: vec!["#[kani::unwind(20)]".into()],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::commands::rate::rate_check".into(),
        kind: CheckKind::Invariant,
        description: "cpu_avg_recent > max_cpu => Err; recent_run < 900 => Err".into(),
        attributes: vec![],
        expected_pass: true,
    });

    // core::chaos_state
    reg.add(KaniTarget {
        function: "crate::core::chaos_state::check_invariants".into(),
        kind: CheckKind::Invariant,
        description: "status в {open,active,done}; fold(events) == status; нет .tmp; нет маркеров".into(),
        attributes: vec!["#[kani::unwind(15)]".into()],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::core::chaos_state::recover_state".into(),
        kind: CheckKind::Postcondition,
        description: "После recover: status=open, нет .tmp, нет маркеров, events содержит aborted".into(),
        attributes: vec![],
        expected_pass: true,
    });

    // core::taxonomy
    reg.add(KaniTarget {
        function: "crate::core::taxonomy::genre_to_class".into(),
        kind: CheckKind::Invariant,
        description: "Все жанры маппятся в валидный BlindClass".into(),
        attributes: vec![],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::core::taxonomy::class_to_tag".into(),
        kind: CheckKind::Invariant,
        description: "Все теги начинаются с 'blind:'".into(),
        attributes: vec![],
        expected_pass: true,
    });

    // core::coverage_index
    reg.add(KaniTarget {
        function: "crate::core::coverage_index::parse_llvm_cov_json".into(),
        kind: CheckKind::NoPanic,
        description: "Битый JSON -> пустой Vec, не паника".into(),
        attributes: vec!["#[kani::unwind(30)]".into()],
        expected_pass: true,
    });
    reg.add(KaniTarget {
        function: "crate::core::coverage_index::compute_test_files_hash".into(),
        kind: CheckKind::NoPanic,
        description: "Хэш не паникует на пустом/несуществующем дереве".into(),
        attributes: vec!["#[kani::unwind(10)]".into()],
        expected_pass: true,
    });

    // core::agent_router
    reg.add(KaniTarget {
        function: "crate::core::agent_router::filter_tasks_by_model_ceiling".into(),
        kind: CheckKind::Invariant,
        description: "Задачи без c_level всегда видимы; C > ceiling -> скрыты с причиной".into(),
        attributes: vec!["#[kani::unwind(10)]".into()],
        expected_pass: true,
    });

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_can_add_and_get() {
        let mut reg = KaniRegistry::new();
        reg.add(KaniTarget {
            function: "crate::foo::bar".into(),
            kind: CheckKind::NoPanic,
            description: "test".into(),
            attributes: vec![],
            expected_pass: true,
        });
        assert!(reg.get("crate::foo::bar").is_some());
        assert!(reg.get("crate::foo::baz").is_none());
    }

    #[test]
    fn by_kind_filters_correctly() {
        let mut reg = KaniRegistry::new();
        reg.add(KaniTarget {
            function: "a".into(),
            kind: CheckKind::NoPanic,
            description: "".into(),
            attributes: vec![],
            expected_pass: true,
        });
        reg.add(KaniTarget {
            function: "b".into(),
            kind: CheckKind::Invariant,
            description: "".into(),
            attributes: vec![],
            expected_pass: true,
        });
        assert_eq!(reg.by_kind(CheckKind::NoPanic).len(), 1);
        assert_eq!(reg.by_kind(CheckKind::Invariant).len(), 1);
        assert_eq!(reg.by_kind(CheckKind::Arithmetic).len(), 0);
    }

    #[test]
    fn generate_harnesses_produces_rust_code() {
        let mut reg = KaniRegistry::new();
        reg.add(KaniTarget {
            function: "crate::mymod::myfunc".into(),
            kind: CheckKind::NoPanic,
            description: "".into(),
            attributes: vec!["#[kani::unwind(5)]".into()],
            expected_pass: true,
        });
        let harnesses = reg.generate_harnesses();
        assert_eq!(harnesses.len(), 1);
        let h = &harnesses[0];
        assert!(h.contains("harness_myfunc"));
        assert!(h.contains("crate::mymod::myfunc"));
        assert!(h.contains("kani::proof"));
        assert!(h.contains("kani::unwind(5)"));
    }

    #[test]
    fn json_roundtrip() {
        let reg = builtin_registry();
        let json = reg.to_json();
        let back = KaniRegistry::from_json(&json).unwrap_or_else(|| unreachable!());
        assert_eq!(back.targets.len(), reg.targets.len());
    }

    #[test]
    fn builtin_registry_not_empty() {
        let reg = builtin_registry();
        assert!(!reg.targets.is_empty(), "встроенный реестр не пуст");
        // Проверяем наличие ключевых функций
        assert!(reg.get("crate::core::complexity::Complexity::from_metrics").is_some());
        assert!(reg.get("crate::commands::rate::rate_check").is_some());
        assert!(reg.get("crate::core::chaos_state::check_invariants").is_some());
    }

    #[test]
    fn all_builtin_targets_have_description() {
        let reg = builtin_registry();
        for t in reg.targets.values() {
            assert!(!t.description.is_empty(), "функция {} без описания", t.function);
        }
    }

    #[test]
    fn all_builtin_targets_have_valid_function_path() {
        let reg = builtin_registry();
        for t in reg.targets.values() {
            assert!(t.function.starts_with("crate::"), "путь должен начинаться с crate::");
        }
    }
}
