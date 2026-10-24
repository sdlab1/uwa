//! Безопасная декомпозиция сложной задачи на подзадачи.
//!
//! Контракт `decompose(parent, constraints, proposal)` — детерминированный и
//! атомарный: либо возвращает проверенный набор простых дочерних задач, либо
//! структурированную ошибку. Никаких частичных результатов и побочных
//! эффектов (реестр не трогаем).

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Родительская (сложная) задача.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentTask {
    pub id: String,
    pub p: u8,
    pub c: u8,
    /// Зоны родителя: дочерние зоны обязаны лежать внутри.
    pub zones: Vec<String>,
}

/// Ограничения декомпозиции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraints {
    /// Максимум дочерних задач.
    pub max_children: usize,
    /// Потолок сложности C модели-исполнителя.
    pub model_ceiling_c: u8,
    /// Уже существующие ID реестра, на которые можно ссылаться в deps.
    pub existing_ids: Vec<String>,
}

/// Предложенная дочерняя задача.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProposal {
    pub id: String,
    pub title: String,
    pub zone: String,
    pub p: u8,
    pub c: u8,
    pub dod: Vec<String>,
    pub check: String,
    pub deps: Vec<String>,
}

/// Проверенная дочерняя задача (тот же набор полей, гарантированно валидный).
pub type ChildTask = ChildProposal;

/// Нарушение контракта декомпозиции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecompositionError {
    EmptyProposal,
    TooManyChildren { limit: usize, got: usize },
    EmptyId { index: usize },
    EmptyTitle { index: usize },
    DuplicateId { id: String },
    DuplicateTitle { title: String },
    EmptyZone { index: usize },
    ZoneEscape { index: usize, zone: String, parent_zones: Vec<String> },
    EmptyDod { index: usize },
    EmptyCheck { index: usize },
    ComplexityRaised { index: usize, field: &'static str, child: u8, parent: u8 },
    CeilingExceeded { index: usize, c: u8, ceiling: u8 },
    UnknownDep { index: usize, dep: String },
    Cycle { path: Vec<String> },
}

impl DecompositionError {
    /// Поле, которого касается нарушение (для машинного разбора).
    pub fn field(&self) -> &'static str {
        match self {
            DecompositionError::EmptyProposal => "proposal",
            DecompositionError::TooManyChildren { .. } => "children",
            DecompositionError::EmptyId { .. } => "id",
            DecompositionError::EmptyTitle { .. } => "title",
            DecompositionError::DuplicateId { .. } => "id",
            DecompositionError::DuplicateTitle { .. } => "title",
            DecompositionError::EmptyZone { .. } => "zone",
            DecompositionError::ZoneEscape { .. } => "zone",
            DecompositionError::EmptyDod { .. } => "dod",
            DecompositionError::EmptyCheck { .. } => "check",
            DecompositionError::ComplexityRaised { .. } => "complexity",
            DecompositionError::CeilingExceeded { .. } => "complexity",
            DecompositionError::UnknownDep { .. } => "deps",
            DecompositionError::Cycle { .. } => "deps",
        }
    }

    /// Конкретное правило контракта.
    pub fn rule(&self) -> &'static str {
        match self {
            DecompositionError::EmptyProposal => "декомпозиция не пуста",
            DecompositionError::TooManyChildren { .. } => "лимит подзадач",
            DecompositionError::EmptyId { .. } => "id не пуст",
            DecompositionError::EmptyTitle { .. } => "title не пуст",
            DecompositionError::DuplicateId { .. } => "id уникален",
            DecompositionError::DuplicateTitle { .. } => "title уникален",
            DecompositionError::EmptyZone { .. } => "zone не пуста",
            DecompositionError::ZoneEscape { .. } => "зона внутри родительской",
            DecompositionError::EmptyDod { .. } => "DoD не пуст",
            DecompositionError::EmptyCheck { .. } => "check не пуст",
            DecompositionError::ComplexityRaised { .. } => "P/C не выше родителя",
            DecompositionError::CeilingExceeded { .. } => "C не выше потолка модели",
            DecompositionError::UnknownDep { .. } => "deps только известные ID",
            DecompositionError::Cycle { .. } => "deps без циклов",
        }
    }
}

impl fmt::Display for DecompositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecompositionError::EmptyProposal => write!(f, "пустая декомпозиция"),
            DecompositionError::TooManyChildren { limit, got } => {
                write!(f, "подзадач {got}, лимит {limit}")
            }
            DecompositionError::EmptyId { index } => write!(f, "подзадача #{index}: пустой id"),
            DecompositionError::EmptyTitle { index } => {
                write!(f, "подзадача #{index}: пустой title")
            }
            DecompositionError::DuplicateId { id } => write!(f, "дубль id: {id}"),
            DecompositionError::DuplicateTitle { title } => write!(f, "дубль title: {title}"),
            DecompositionError::EmptyZone { index } => {
                write!(f, "подзадача #{index}: пустая zone")
            }
            DecompositionError::ZoneEscape {
                index,
                zone,
                parent_zones,
            } => write!(
                f,
                "подзадача #{index}: зона {zone} вне родительских {parent_zones:?}"
            ),
            DecompositionError::EmptyDod { index } => write!(f, "подзадача #{index}: пустой DoD"),
            DecompositionError::EmptyCheck { index } => {
                write!(f, "подзадача #{index}: пустой check")
            }
            DecompositionError::ComplexityRaised {
                index,
                field,
                child,
                parent,
            } => write!(
                f,
                "подзадача #{index}: {field}={child} выше родительского {parent}"
            ),
            DecompositionError::CeilingExceeded { index, c, ceiling } => write!(
                f,
                "подзадача #{index}: C={c} выше потолка модели {ceiling}"
            ),
            DecompositionError::UnknownDep { index, dep } => {
                write!(f, "подзадача #{index}: неизвестная зависимость {dep}")
            }
            DecompositionError::Cycle { path } => {
                write!(f, "цикл deps: {}", path.join(" → "))
            }
        }
    }
}

fn norm(zone: &str) -> &str {
    zone.trim().trim_end_matches('/')
}

/// Зона `child` внутри хотя бы одной родительской зоны.
pub fn zone_within(child: &str, parent_zones: &[String]) -> bool {
    let child = norm(child);
    if child.is_empty() {
        return false;
    }
    parent_zones.iter().any(|p| {
        let p = norm(p);
        !p.is_empty() && (child == p || child.strip_prefix(p).is_some_and(|r| r.starts_with('/')))
    })
}

fn find_cycle(children: &[ChildProposal]) -> Option<Vec<String>> {
    let ids: HashSet<&str> = children.iter().map(|c| c.id.as_str()).collect();
    let mut color: HashMap<&str, u8> = HashMap::new(); // 0 white, 1 gray, 2 black
    let deps: HashMap<&str, Vec<&str>> = children
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                c.deps
                    .iter()
                    .map(String::as_str)
                    .filter(|d| ids.contains(d))
                    .collect(),
            )
        })
        .collect();

    fn dfs<'a>(
        v: &'a str,
        deps: &HashMap<&'a str, Vec<&'a str>>,
        color: &mut HashMap<&'a str, u8>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        color.insert(v, 1);
        stack.push(v);
        if let Some(next) = deps.get(v) {
            for n in next {
                match color.get(n).copied().unwrap_or(0) {
                    0 => {
                        if let Some(c) = dfs(n, deps, color, stack) {
                            return Some(c);
                        }
                    }
                    1 => {
                        let start = stack.iter().position(|x| x == n).unwrap_or(0);
                        let mut path: Vec<String> =
                            stack[start..].iter().map(|s| s.to_string()).collect();
                        path.push(n.to_string());
                        return Some(path);
                    }
                    _ => {}
                }
            }
        }
        stack.pop();
        color.insert(v, 2);
        None
    }

    for c in children {
        if color.get(c.id.as_str()).copied().unwrap_or(0) == 0 {
            if let Some(path) = dfs(c.id.as_str(), &deps, &mut color, &mut Vec::new()) {
                return Some(path);
            }
        }
    }
    None
}

/// Проверить предложение и вернуть дочерние задачи (атомарно: всё или ошибка).
pub fn decompose(
    parent: &ParentTask,
    constraints: &Constraints,
    proposal: &[ChildProposal],
) -> Result<Vec<ChildTask>, DecompositionError> {
    if proposal.is_empty() {
        return Err(DecompositionError::EmptyProposal);
    }
    if proposal.len() > constraints.max_children {
        return Err(DecompositionError::TooManyChildren {
            limit: constraints.max_children,
            got: proposal.len(),
        });
    }

    let known: HashSet<&str> = constraints
        .existing_ids
        .iter()
        .map(String::as_str)
        .chain(proposal.iter().map(|c| c.id.as_str()))
        .collect();

    let mut ids: HashSet<&str> = HashSet::new();
    let mut titles: HashSet<&str> = HashSet::new();
    for (index, child) in proposal.iter().enumerate() {
        let id = child.id.trim();
        if id.is_empty() {
            return Err(DecompositionError::EmptyId { index });
        }
        if constraints.existing_ids.iter().any(|e| e == id) || !ids.insert(id) {
            return Err(DecompositionError::DuplicateId {
                id: id.to_string(),
            });
        }
        let title = child.title.trim();
        if title.is_empty() {
            return Err(DecompositionError::EmptyTitle { index });
        }
        if !titles.insert(title) {
            return Err(DecompositionError::DuplicateTitle {
                title: title.to_string(),
            });
        }
        if child.zone.trim().is_empty() {
            return Err(DecompositionError::EmptyZone { index });
        }
        if !zone_within(&child.zone, &parent.zones) {
            return Err(DecompositionError::ZoneEscape {
                index,
                zone: child.zone.clone(),
                parent_zones: parent.zones.clone(),
            });
        }
        if child.p > parent.p {
            return Err(DecompositionError::ComplexityRaised {
                index,
                field: "P",
                child: child.p,
                parent: parent.p,
            });
        }
        if child.c > parent.c {
            return Err(DecompositionError::ComplexityRaised {
                index,
                field: "C",
                child: child.c,
                parent: parent.c,
            });
        }
        if child.c > constraints.model_ceiling_c {
            return Err(DecompositionError::CeilingExceeded {
                index,
                c: child.c,
                ceiling: constraints.model_ceiling_c,
            });
        }
        if child.dod.iter().all(|d| d.trim().is_empty()) {
            return Err(DecompositionError::EmptyDod { index });
        }
        if child.check.trim().is_empty() {
            return Err(DecompositionError::EmptyCheck { index });
        }
        for dep in &child.deps {
            if !known.contains(dep.trim()) {
                return Err(DecompositionError::UnknownDep {
                    index,
                    dep: dep.clone(),
                });
            }
        }
    }

    if let Some(path) = find_cycle(proposal) {
        return Err(DecompositionError::Cycle { path });
    }

    Ok(proposal.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> ParentTask {
        ParentTask {
            id: "T-100".to_string(),
            p: 3,
            c: 6,
            zones: vec!["devharness/src/core".to_string()],
        }
    }

    fn constraints() -> Constraints {
        Constraints {
            max_children: 4,
            model_ceiling_c: 5,
            existing_ids: vec!["T-001".to_string()],
        }
    }

    fn child(id: &str, title: &str) -> ChildProposal {
        ChildProposal {
            id: id.to_string(),
            title: title.to_string(),
            zone: "devharness/src/core/x.rs".to_string(),
            p: 1,
            c: 3,
            dod: vec!["сделано".to_string()],
            check: "cargo test -p devh --lib x".to_string(),
            deps: Vec::new(),
        }
    }

    #[test]
    fn normal_decomposition_succeeds() {
        let proposal = vec![child("T-101", "часть 1"), child("T-102", "часть 2")];
        let out = decompose(&parent(), &constraints(), &proposal)
            .unwrap_or_else(|e| panic!("ожидался успех: {e}"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "T-101");
    }

    #[test]
    fn empty_proposal_is_rejected() {
        assert_eq!(
            decompose(&parent(), &constraints(), &[]),
            Err(DecompositionError::EmptyProposal)
        );
    }

    #[test]
    fn too_complex_child_is_rejected() {
        let mut c = child("T-101", "часть");
        c.c = 9;
        assert!(matches!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::ComplexityRaised { field: "C", .. })
        ));
        let mut c = child("T-101", "часть");
        c.p = 4;
        assert!(matches!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::ComplexityRaised { field: "P", .. })
        ));
    }

    #[test]
    fn model_ceiling_is_enforced() {
        let mut c = child("T-101", "часть");
        c.c = 5;
        let mut cons = constraints();
        cons.model_ceiling_c = 4;
        assert_eq!(
            decompose(&parent(), &cons, &[c]),
            Err(DecompositionError::CeilingExceeded {
                index: 0,
                c: 5,
                ceiling: 4
            })
        );
    }

    #[test]
    fn zone_escape_is_rejected() {
        let mut c = child("T-101", "часть");
        c.zone = "devharness/src/shell/msg.rs".to_string();
        assert!(matches!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::ZoneEscape { .. })
        ));
    }

    #[test]
    fn dep_cycle_is_rejected() {
        let mut a = child("T-101", "a");
        let mut b = child("T-102", "b");
        a.deps = vec!["T-102".to_string()];
        b.deps = vec!["T-101".to_string()];
        match decompose(&parent(), &constraints(), &[a, b]) {
            Err(DecompositionError::Cycle { path }) => {
                assert!(path.len() >= 3, "{path:?}");
            }
            other => panic!("ожидался Cycle, а не {other:?}"),
        }
    }

    #[test]
    fn unknown_dep_is_rejected() {
        let mut c = child("T-101", "часть");
        c.deps = vec!["T-999".to_string()];
        assert_eq!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::UnknownDep {
                index: 0,
                dep: "T-999".to_string()
            })
        );
    }

    #[test]
    fn existing_id_dep_is_allowed() {
        let mut c = child("T-101", "часть");
        c.deps = vec!["T-001".to_string()];
        assert!(decompose(&parent(), &constraints(), &[c]).is_ok());
    }

    #[test]
    fn sub_task_limit_is_enforced() {
        let proposal: Vec<_> = (0..5)
            .map(|i| child(&format!("T-10{i}"), &format!("часть {i}")))
            .collect();
        assert_eq!(
            decompose(&parent(), &constraints(), &proposal),
            Err(DecompositionError::TooManyChildren { limit: 4, got: 5 })
        );
    }

    #[test]
    fn duplicate_ids_and_titles_are_rejected() {
        let a = child("T-101", "часть");
        let b = child("T-101", "другая");
        assert!(matches!(
            decompose(&parent(), &constraints(), &[a, b]),
            Err(DecompositionError::DuplicateId { .. })
        ));
        let a = child("T-101", "часть");
        let b = child("T-102", "часть");
        assert!(matches!(
            decompose(&parent(), &constraints(), &[a, b]),
            Err(DecompositionError::DuplicateTitle { .. })
        ));
    }

    #[test]
    fn empty_dod_and_check_are_rejected() {
        let mut c = child("T-101", "часть");
        c.dod = vec!["  ".to_string()];
        assert!(matches!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::EmptyDod { .. })
        ));
        let mut c = child("T-101", "часть");
        c.check = " ".to_string();
        assert!(matches!(
            decompose(&parent(), &constraints(), &[c]),
            Err(DecompositionError::EmptyCheck { .. })
        ));
    }

    #[test]
    fn decomposition_is_deterministic() {
        let proposal = vec![child("T-101", "a"), child("T-102", "b")];
        let first = decompose(&parent(), &constraints(), &proposal)
            .unwrap_or_else(|e| panic!("{e}"));
        let second = decompose(&parent(), &constraints(), &proposal)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first, second);
        assert_eq!(first, proposal);
    }

    #[test]
    fn error_exposes_field_and_rule() {
        let mut c = child("T-101", "часть");
        c.zone = "elsewhere".to_string();
        let err = decompose(&parent(), &constraints(), &[c]).unwrap_err();
        assert_eq!(err.field(), "zone");
        assert!(err.rule().contains("родительской"));
        assert!(!err.to_string().is_empty());
    }
}
