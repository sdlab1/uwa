//! T-232: agent_identity — обязательная декларация модели/рантайма агента.
//!
//! Модуль ЧИСТЫЙ (без файлов и окружения): правила идентичности v2 считаются
//! по переданным данным, поэтому проверяются юнит-тестами и не зависят от
//! состояния машины. Работа с файлами привязок и реестра остаётся в
//! `core::identity` и `core::identity_ledger`.
//!
//! Правила (fail-closed, молчаливых слияний нет):
//! 1. ОДИН канон = ОДИН ключ сессии. Второй ключ, закрепивший то же имя, —
//!    отказ, не тихое слияние.
//! 2. Агент БЕЗ декларации модели не получает задач: потолок C0, и роль lead
//!    ему недоступна.
//! 3. Модель, которой нет в таблице потолков, — C1 (явный fail-closed, НЕ
//!    дефолт вроде C5).
//! 4. Роль lead выдаётся ТОЛЬКО явной записью `role=lead` в реестре.
//!    Идентичность, слот и алиасы прав не дают.
//! 5. Смена модели фиксируется как событие `model_change` с меткой подозрения
//!    и понижением P очереди на 1 (не ниже P0).

use crate::core::model_ceiling;
use std::collections::BTreeMap;

/// Откуда взята декларация агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclSource {
    /// Флаг `--model` командной строки.
    Flag,
    /// Переменная окружения.
    Env,
    /// Запись реестра (`.harness/orch/registry.json`, поле `agents[].model`).
    Registry,
    /// Унаследовано от слота/сессии рантайма — ЯВНОЙ декларацией не считается.
    Slot,
}

impl DeclSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DeclSource::Flag => "флаг --model",
            DeclSource::Env => "окружение",
            DeclSource::Registry => "реестр",
            DeclSource::Slot => "слот (унаследовано)",
        }
    }

    /// Явная декларация. Унаследованная из слота — нет (fail-closed).
    pub fn is_explicit(self) -> bool {
        !matches!(self, DeclSource::Slot)
    }
}

/// Декларация агента: какая модель и рантайм фактически работают.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// Модель (каноническое имя из таблицы потолков).
    pub model: String,
    /// Рантайм (goose/kilo/opencode/…).
    pub runtime: String,
    /// Источник декларации.
    pub source: DeclSource,
}

impl Declaration {
    /// Декларация с указанием источника (флаг/env/реестр/слот).
    pub fn new(model: &str, runtime: &str, source: DeclSource) -> Self {
        Self {
            model: model.trim().to_string(),
            runtime: runtime.trim().to_string(),
            source,
        }
    }

    /// Декларация считается заявленной, если модель непуста и источник явный.
    pub fn is_declared(&self) -> bool {
        !self.model.is_empty() && self.source.is_explicit()
    }
}

/// Привязка имени (канона) к ключу сессии — данные для проверки правила 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyClaim {
    /// Ключ сессии (`session:…`, слот, tty и т.п.).
    pub key: String,
    /// Каноническое имя, закреплённое за этим ключом.
    pub canon: String,
}

impl KeyClaim {
    pub fn new(key: &str, canon: &str) -> Self {
        Self {
            key: key.to_string(),
            canon: canon.to_string(),
        }
    }
}

/// Один канон = один ключ. Повторная привязка ТЕМ ЖЕ ключом — норма (обновление
/// имени); ДРУГОЙ ключ на тот же канон — отказ: двумя ключами один канон
/// захватывают двое, и права слотов смешиваются.
pub fn check_one_canon_one_key(claims: &[KeyClaim], canon: &str, key: &str) -> Result<(), String> {
    let owners: Vec<&str> = claims
        .iter()
        .filter(|c| c.canon == canon)
        .map(|c| c.key.as_str())
        .collect();
    if owners.is_empty() || owners.iter().any(|k| *k == key) {
        return Ok(());
    }
    Err(format!(
        "канон '{canon}' уже закреплён другим ключом ({}), второй ключ '{key}' — отказ",
        owners.join(", ")
    ))
}

/// Все коллизии «канон ← несколько ключей» в наборе привязок.
/// Детерминированно: имена и ключи отсортированы; пустой список — инвариант ок.
pub fn canon_key_collisions(claims: &[KeyClaim]) -> Vec<(String, Vec<String>)> {
    let mut by_canon: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for c in claims {
        if c.canon.is_empty() {
            continue;
        }
        let keys = by_canon.entry(&c.canon).or_default();
        if !keys.contains(&c.key.as_str()) {
            keys.push(&c.key);
        }
    }
    by_canon
        .into_iter()
        .filter(|(_, keys)| keys.len() > 1)
        .map(|(canon, mut keys)| {
            keys.sort_unstable();
            (
                canon.to_string(),
                keys.into_iter().map(|k| k.to_string()).collect(),
            )
        })
        .collect()
}

/// Причина выставленного потолка (различимая: молчания быть не должно).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingKind {
    /// Декларации нет — потолок C0.
    NotDeclared,
    /// Модель не в таблице потолков — fail-closed C1.
    UnknownModel,
    /// Модель в таблице — её потолок.
    Known(u8),
}

/// Вид потолка по декларации (None = декларации нет).
pub fn ceiling_kind(decl: Option<&Declaration>) -> CeilingKind {
    match decl {
        Some(d) if d.is_declared() => {
            if model_ceiling::ceiling_map().contains_key(d.model.as_str()) {
                CeilingKind::Known(model_ceiling::ceiling_for(&d.model))
            } else {
                CeilingKind::UnknownModel
            }
        }
        _ => CeilingKind::NotDeclared,
    }
}

/// Потолок агента: нет декларации → C0; неизвестная модель → C1; иначе таблица.
pub fn ceiling(decl: Option<&Declaration>) -> u8 {
    match ceiling_kind(decl) {
        CeilingKind::NotDeclared => 0,
        CeilingKind::UnknownModel => 1,
        CeilingKind::Known(c) => c,
    }
}

/// Может ли агент с такой декларацией получить задачу уровня `task_c`.
pub fn can_take_task(decl: Option<&Declaration>, task_c: u8) -> bool {
    ceiling(decl) >= task_c
}

/// Результат проверки прав ведущего.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadCheck {
    /// Ведущий: роль `lead` в реестре + декларация заявлена.
    Ok,
    /// Не ведущий, с причиной.
    Denied(String),
}

impl LeadCheck {
    pub fn is_ok(&self) -> bool {
        matches!(self, LeadCheck::Ok)
    }

    /// Причина отказа (для Ok — пустая строка).
    pub fn message(&self) -> &str {
        match self {
            LeadCheck::Ok => "",
            LeadCheck::Denied(m) => m,
        }
    }
}

/// Роль из реестра: никаких догадок из имени/слота/алиаса.
fn role_trim(registry_role: Option<&str>) -> Option<&str> {
    registry_role.map(str::trim).filter(|r| !r.is_empty())
}

/// Правило 4 + 2: lead — ТОЛЬКО явная запись роли в реестре, но и без
/// декларации модели прав нет вовсе.
pub fn lead_check(registry_role: Option<&str>, decl: Option<&Declaration>) -> LeadCheck {
    if decl.map(|d| d.is_declared()) != Some(true) {
        return LeadCheck::Denied("нет декларации модели/рантайма — прав lead нет".to_string());
    }
    match role_trim(registry_role) {
        Some("lead") => LeadCheck::Ok,
        Some(r) => LeadCheck::Denied(format!("роль '{r}' в реестре — не lead")),
        None => LeadCheck::Denied("роль в реестре не заявлена — прав lead нет".to_string()),
    }
}

/// Вердикт смены модели: событие, метка подозрения, понижение P.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChangeVerdict {
    /// Бывшая модель.
    pub old: String,
    /// Новая модель.
    pub new: String,
    /// Метка подозрения (O12-триггер деградации).
    pub suspicion: bool,
    /// На сколько уровней понизить P очереди (не ниже P0).
    pub p_downgrade: u8,
    /// Метка события журнала идентичностей.
    pub event: &'static str,
}

/// Правило 5: смена модели = подозрение + понижение P на 1.
pub fn model_change(old: &str, new: &str) -> ModelChangeVerdict {
    ModelChangeVerdict {
        old: old.trim().to_string(),
        new: new.trim().to_string(),
        suspicion: true,
        p_downgrade: 1,
        event: "model_change",
    }
}

/// Понизить уровень P на `steps`, не ниже P0 (P — номер уровня, 0..5).
pub fn downgrade_p(p: u8, steps: u8) -> u8 {
    p.saturating_sub(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(model: &str) -> Declaration {
        Declaration::new(model, "goose", DeclSource::Registry)
    }

    #[test]
    fn one_canon_one_key_refuses_second_key() {
        let claims = vec![KeyClaim::new("session:a", "beam")];
        let err = check_one_canon_one_key(&claims, "beam", "session:b")
            .expect_err("второй ключ на тот же канон должен получить отказ");
        assert!(
            err.contains("session:a"),
            "в отказе должен быть виден чужой ключ: {err}"
        );
        assert!(
            err.contains("session:b"),
            "в отказе должен быть новый ключ: {err}"
        );
    }

    #[test]
    fn same_key_reclaim_is_ok() {
        let claims = vec![KeyClaim::new("session:a", "beam")];
        assert!(check_one_canon_one_key(&claims, "beam", "session:a").is_ok());
    }

    #[test]
    fn fresh_canon_is_ok() {
        let claims = vec![KeyClaim::new("session:a", "beam")];
        assert!(check_one_canon_one_key(&claims, "beam2", "session:a").is_ok());
    }

    #[test]
    fn no_declaration_ceiling_c0() {
        assert_eq!(ceiling(None), 0);
        assert_eq!(ceiling_kind(None), CeilingKind::NotDeclared);
        assert!(!can_take_task(None, 1));
    }

    #[test]
    fn empty_model_is_not_declared() {
        let empty = Declaration::new("  ", "goose", DeclSource::Registry);
        assert!(!empty.is_declared());
        assert_eq!(ceiling(Some(&empty)), 0);
    }

    #[test]
    fn unknown_model_ceiling_c1() {
        let decl = declared("totally-unknown-model");
        assert_eq!(ceiling_kind(Some(&decl)), CeilingKind::UnknownModel);
        assert_eq!(ceiling(Some(&decl)), 1);
    }

    #[test]
    fn known_model_uses_table_ceiling() {
        let decl = declared("gpt-4");
        assert_eq!(ceiling_kind(Some(&decl)), CeilingKind::Known(5));
        assert_eq!(ceiling(Some(&decl)), 5);
        assert!(can_take_task(Some(&decl), 5));
        assert!(!can_take_task(Some(&decl), 6));
    }

    #[test]
    fn slot_inherited_is_not_a_declaration() {
        let slot = Declaration::new("gpt-4", "goose", DeclSource::Slot);
        assert!(
            !slot.is_declared(),
            "унаследованное из слота — не декларация"
        );
        assert_eq!(ceiling(Some(&slot)), 0, "без явной декларации — C0");
    }

    #[test]
    fn lead_only_by_explicit_registry_role() {
        let decl = declared("kilo");
        assert!(lead_check(Some("lead"), Some(&decl)).is_ok());
        let denied = lead_check(Some("worker"), Some(&decl));
        assert!(!denied.is_ok());
        assert!(
            denied.message().contains("worker"),
            "в причине должна быть роль: {}",
            denied.message()
        );
        assert!(!lead_check(None, Some(&decl)).is_ok());
    }

    #[test]
    fn lead_denied_without_declaration() {
        assert!(!lead_check(Some("lead"), None).is_ok());
        let slot = Declaration::new("kilo", "goose", DeclSource::Slot);
        assert!(!lead_check(Some("lead"), Some(&slot)).is_ok());
    }

    #[test]
    fn model_change_is_suspected_and_downgrades_p() {
        let v = model_change("kilo", "opus");
        assert_eq!((v.old.as_str(), v.new.as_str()), ("kilo", "opus"));
        assert!(v.suspicion, "смена модели — триггер подозрения (O12)");
        assert_eq!(v.p_downgrade, 1);
        assert_eq!(v.event, "model_change");
        assert_eq!(downgrade_p(3, v.p_downgrade), 2);
        assert_eq!(downgrade_p(0, v.p_downgrade), 0, "ниже P0 не понижаем");
    }

    #[test]
    fn canon_collisions_list_sorted() {
        let claims = vec![
            KeyClaim::new("session:z", "beam"),
            KeyClaim::new("session:a", "beam"),
            KeyClaim::new("session:z", "beam"),
            KeyClaim::new("session:solo", "beam2"),
        ];
        let collisions = canon_key_collisions(&claims);
        assert_eq!(collisions.len(), 1);
        let (canon, keys) = &collisions[0];
        assert_eq!(canon, "beam");
        assert_eq!(
            keys,
            &vec!["session:a".to_string(), "session:z".to_string()],
            "ключи отсортированы, дублей нет"
        );
    }

    #[test]
    fn no_collisions_clean() {
        let claims = vec![
            KeyClaim::new("session:a", "beam"),
            KeyClaim::new("session:b", "beam2"),
        ];
        assert!(canon_key_collisions(&claims).is_empty());
    }
}
