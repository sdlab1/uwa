//! Доказуемые инварианты реестра оркестратора (Kani-цели).
//!
//! Три свойства реестра, от которых зависит честность выдачи задач и слияния.
//! Каждый инвариант — чистая функция над `Registry` (из `commands/orch.rs`),
//! поэтому он применим и в обычных тестах, и в `cargo kani` как формула.
//!
//! Инварианты:
//!
//! 1. [`zone_exclusivity`] — среди задач **в работе** (`Active`) нет двух с
//!    пересекающимися зонами (O5: «двойная работа запрещена»). Очередь и
//!    закрытые задачи не считаются: их по одной зоне держать можно.
//! 2. [`commit_proven`] — сданная задача (`Done`) имеет `commit_sha`, и этот
//!    SHA существует в git (T-237: 26 задач `done` ссылались на несуществующий
//!    SHA). И «SHA не задан», и «SHA не в git» — дыры; проверка существования
//!    идёт через предикат `sha_exists`, чтобы функция оставалась чистой
//!    (git — снаружи).
//! 3. [`terminal_not_active`] — задача с терминальным статусом (`Done` /
//!    `Failed` / `Revoked`) не сохраняет маркеры «взятия в работу»
//!    (`started_by` / `started_at`): иначе она выглядит активной для доски
//!    «в работе» и детекторов простоя — «статус не активен после done».
//!
//! Запуск (Kani — опционально, вне обычного CI):
//!
//! ```text
//! cargo install --locked kani-verifier && cargo kani setup
//! cargo kani --package devh --lib core::kani_specs
//! ```
//!
//! Дескрипторы `#[kani::proof]` здесь намеренно не добавляются: без
//! установленного `cargo kani` обычный `cargo test` не собрал бы их.
//! [`harness_plan`] отдаёт карту «инвариант → целевая функция → команда»:
//! подключение kani-цели — одно движение без изменения этого модуля.

use std::fmt;

use crate::commands::orch::{Registry, Status, Task};

/// Нарушение одного инварианта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Какой инвариант нарушен (идентификатор из [`INVARIANTS`]).
    pub invariant: &'static str,
    /// Задача-нарушитель (или пара задач для пересечения зон).
    pub task: String,
    /// Человекочитаемая причина с конкретикой.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.invariant, self.task, self.detail)
    }
}

/// Идентификатор инварианта «нет двух активных задач на одну зону».
pub const INV_ZONE_EXCLUSIVITY: &str = "zone_exclusivity";
/// Идентификатор инварианта «commit_sha сданной задачи существует в git».
pub const INV_COMMIT_PROVEN: &str = "commit_proven";
/// Идентификатор инварианта «терминальная задача не активна».
pub const INV_TERMINAL_NOT_ACTIVE: &str = "terminal_not_active";

/// Все инварианты в детерминированном порядке.
pub const INVARIANTS: [&str; 3] = [
    INV_ZONE_EXCLUSIVITY,
    INV_COMMIT_PROVEN,
    INV_TERMINAL_NOT_ACTIVE,
];

/// Задача в работе (взята исполнителем).
pub fn is_active(status: Status) -> bool {
    status == Status::Active
}

/// Задача закрыта (сдача, провал, отзыв) — больше не в работе.
pub fn is_terminal(status: Status) -> bool {
    matches!(status, Status::Done | Status::Failed | Status::Revoked)
}

/// Нормализация зоны: без окружающих пробелов и хвостового `/`.
pub fn normalize_zone(zone: &str) -> &str {
    zone.trim().trim_end_matches('/')
}

/// Зоны пересекаются: равны или одна — префикс-каталог другой.
///
/// Политика как у оркестратора: `devharness/src/core` пересекается с
/// `devharness/src/core/guards`, но `src/x` и `src/y` — нет. Граница `/`
/// обязательна: иначе `src/core2` считался бы частью `src/core`.
pub fn zones_overlap(a: &str, b: &str) -> bool {
    let a = normalize_zone(a);
    let b = normalize_zone(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// Пары пересечений между двумя наборами зон (в порядке обхода).
pub fn zone_pairs(a: &[String], b: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for x in a.iter() {
        for y in b.iter() {
            if zones_overlap(x, y) {
                out.push((normalize_zone(x).to_string(), normalize_zone(y).to_string()));
            }
        }
    }
    out
}

/// Инвариант 1: две задачи в работе не делят зону.
///
/// Порядок нарушений детерминирован: пары — по позиции задач в реестре,
/// внутри пары зоны — по порядку обхода; пара не дублируется (`a` раньше `b`).
pub fn zone_exclusivity(reg: &Registry) -> Vec<Violation> {
    let mut out = Vec::new();
    let active: Vec<&Task> = reg.tasks.iter().filter(|t| is_active(t.status)).collect();
    for (i, a) in active.iter().enumerate() {
        for b in active.iter().skip(i + 1) {
            let pairs = zone_pairs(&a.zones, &b.zones);
            if pairs.is_empty() {
                continue;
            }
            let detail = pairs
                .iter()
                .map(|(x, y)| format!("{x} ↔ {y}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Violation {
                invariant: INV_ZONE_EXCLUSIVITY,
                task: format!("{} + {}", a.id, b.id),
                detail: format!("общая зона активных задач: {detail}"),
            });
        }
    }
    out
}

/// Инвариант 2: у сданной задачи SHA коммита задан и существует в git.
///
/// `sha_exists` — предикат «SHA есть в git» (вызывает сторону реестра):
/// функция остаётся чистой и тестируемой без git.
pub fn commit_proven(reg: &Registry, sha_exists: impl Fn(&str) -> bool) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in reg.tasks.iter() {
        if t.status != Status::Done {
            continue;
        }
        let Some(sha) = t.commit_sha.as_ref().map(|s| s.trim()) else {
            out.push(Violation {
                invariant: INV_COMMIT_PROVEN,
                task: t.id.clone(),
                detail: "сдана без commit_sha — коммита нет, вливать нечего".to_string(),
            });
            continue;
        };
        if sha.is_empty() {
            out.push(Violation {
                invariant: INV_COMMIT_PROVEN,
                task: t.id.clone(),
                detail: "commit_sha пустой строкой".to_string(),
            });
            continue;
        }
        if !sha_exists(sha) {
            out.push(Violation {
                invariant: INV_COMMIT_PROVEN,
                task: t.id.clone(),
                detail: format!("commit_sha {sha} отсутствует в git"),
            });
        }
    }
    out
}

/// Инвариант 3: терминальная задача не сохраняет маркеры «в работе».
///
/// `close_task` меняет только `status`; если `started_by` / `started_at` не
/// сброшены, задача продолжает числиться взятой — доска «в работе», детектор
/// зависания и watchdog считают её активной.
pub fn terminal_not_active(reg: &Registry) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in reg.tasks.iter() {
        if !is_terminal(t.status) {
            continue;
        }
        if t.started_at.is_some() {
            out.push(Violation {
                invariant: INV_TERMINAL_NOT_ACTIVE,
                task: t.id.clone(),
                detail: format!(
                    "статус {} но started_at сохранён — задача числится взятой",
                    t.status.label()
                ),
            });
        }
        if t.started_by.is_some() {
            out.push(Violation {
                invariant: INV_TERMINAL_NOT_ACTIVE,
                task: t.id.clone(),
                detail: format!(
                    "статус {} но started_by сохранён — задача числится взятой",
                    t.status.label()
                ),
            });
        }
    }
    out
}

/// Все проверки сразу: (инвариант, нарушения).
///
/// Порядок результата детерминирован: инварианты по [`INVARIANTS`], внутри —
/// по позиции задач в реестре.
pub fn check_all(
    reg: &Registry,
    sha_exists: impl Fn(&str) -> bool,
) -> Vec<(&'static str, Vec<Violation>)> {
    vec![
        (INV_ZONE_EXCLUSIVITY, zone_exclusivity(reg)),
        (INV_COMMIT_PROVEN, commit_proven(reg, &sha_exists)),
        (INV_TERMINAL_NOT_ACTIVE, terminal_not_active(reg)),
    ]
}

/// План подключения kani: инвариант → чистая функция → команда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    /// Инвариант (ключ [`INVARIANTS`]).
    pub invariant: &'static str,
    /// Целевая функция модуля.
    pub target: &'static str,
    /// Как запускать проверку (Kani).
    pub cmd: &'static str,
}

/// Карта «инвариант → цель → команда» (пометки «как запускать»).
pub fn harness_plan() -> Vec<Harness> {
    vec![
        Harness {
            invariant: INV_ZONE_EXCLUSIVITY,
            target: "zone_exclusivity",
            cmd: "cargo kani --package devh --lib core::kani_specs::zone_exclusivity",
        },
        Harness {
            invariant: INV_COMMIT_PROVEN,
            target: "commit_proven",
            cmd: "cargo kani --package devh --lib core::kani_specs::commit_proven",
        },
        Harness {
            invariant: INV_TERMINAL_NOT_ACTIVE,
            target: "terminal_not_active",
            cmd: "cargo kani --package devh --lib core::kani_specs::terminal_not_active",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: Status, zones: &[&str], sha: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            agent: "beam".to_string(),
            title: format!("задача {id}"),
            complexity: 2,
            c_level: Some(2),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status,
            zones: zones.iter().map(|s| s.to_string()).collect(),
            fail_level: None,
            note: String::new(),
            created: 1,
            updated: 1,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: sha.map(|s| s.to_string()),
            override_reason: None,
        }
    }

    fn taken(mut t: Task) -> Task {
        t.started_by = Some("beam".to_string());
        t.started_at = Some(100);
        t
    }

    fn reg(tasks: Vec<Task>) -> Registry {
        Registry {
            tasks,
            ..Registry::default()
        }
    }

    #[test]
    fn two_active_tasks_sharing_zone_are_violation() {
        let r = reg(vec![
            task("T-1", Status::Active, &["devharness/src/core"], None),
            task("T-2", Status::Active, &["devharness/src/core/guards"], None),
        ]);
        let v = zone_exclusivity(&r);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].invariant, INV_ZONE_EXCLUSIVITY);
        assert_eq!(v[0].task, "T-1 + T-2");
        assert!(v[0].detail.contains("devharness/src/core"));
        assert!(v[0]
            .detail
            .contains("devharness/src/core ↔ devharness/src/core/guards"));
    }

    #[test]
    fn exact_same_zone_is_violation() {
        let r = reg(vec![
            task("T-1", Status::Active, &["devharness/src/core/mod.rs"], None),
            task("T-2", Status::Active, &["devharness/src/core/mod.rs"], None),
        ]);
        assert_eq!(zone_exclusivity(&r).len(), 1);
    }

    #[test]
    fn closed_tasks_and_disjoint_zones_are_clean() {
        let r = reg(vec![
            task("T-1", Status::Active, &["devharness/src/core"], None),
            task("T-2", Status::Active, &["devharness/tests"], None),
            task("T-3", Status::Active, &["devharness/src/core/guards"], None),
            // Закрытая задача пересекается с активной — это не двойная работа.
            task(
                "T-4",
                Status::Done,
                &["devharness/src/core"],
                Some("abc123"),
            ),
        ]);
        let v = zone_exclusivity(&r);
        assert!(v.iter().any(|x| x.task == "T-1 + T-3"));
        assert!(!v.iter().any(|x| x.task == "T-1 + T-2"));
        assert!(!v.iter().any(|x| x.task.contains("T-4")));
    }

    #[test]
    fn zones_overlap_boundaries_and_empty() {
        assert!(zones_overlap("src/core", "src/core/guards"));
        assert!(zones_overlap("src/core/", "src/core"));
        assert!(zones_overlap("src/core", "src/core"));
        assert!(!zones_overlap("src/core", "src/core2"));
        assert!(!zones_overlap("src/x", "src/y"));
        assert!(!zones_overlap("", "src/core"));
        assert!(!zones_overlap("   ", "src/core"));
    }

    #[test]
    fn commit_proven_requires_sha_and_git_existence() {
        let r = reg(vec![
            task("T-1", Status::Done, &["a"], Some("deadbeef")),
            task("T-2", Status::Done, &["a"], None),
            task("T-3", Status::Done, &["a"], Some("   ")),
            task("T-4", Status::Done, &["a"], Some("missing")),
            // В работе без SHA — норма.
            task("T-5", Status::Active, &["a"], None),
        ]);
        let v = commit_proven(&r, |_: &str| true);
        assert_eq!(v.len(), 2);
        assert!(v
            .iter()
            .any(|x| x.task == "T-2" && x.detail.contains("без commit_sha")));
        assert!(v
            .iter()
            .any(|x| x.task == "T-3" && x.detail.contains("пустой")));
        // SHA, которого нет в git:
        let v = commit_proven(&r, |sha| sha != "missing");
        assert_eq!(v.len(), 3);
        assert!(v
            .iter()
            .any(|x| x.task == "T-4" && x.detail.contains("отсутствует в git")));
        // Всё целиком сдано и коммит есть:
        let ok = reg(vec![task("T-1", Status::Done, &["a"], Some("deadbeef"))]);
        assert!(commit_proven(&ok, |_| true).is_empty());
    }

    #[test]
    fn terminal_task_keeps_taken_markers_is_violation() {
        let clean = task("T-1", Status::Done, &["a"], Some("abc123"));
        assert!(terminal_not_active(&reg(vec![clean])).is_empty());
        let stale = taken(task("T-2", Status::Done, &["a"], Some("abc123")));
        let v = terminal_not_active(&reg(vec![stale]));
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|x| x.invariant == INV_TERMINAL_NOT_ACTIVE));
        assert!(v.iter().any(|x| x.detail.contains("started_at")));
        assert!(v.iter().any(|x| x.detail.contains("started_by")));
        // Активная задача с маркерами — норма.
        assert!(
            terminal_not_active(&reg(vec![taken(task("T-3", Status::Active, &["a"], None))]))
                .is_empty()
        );
    }

    #[test]
    fn empty_registry_is_clean_for_all_invariants() {
        let r = Registry::default();
        let all = check_all(&r, |_| false);
        assert_eq!(all.len(), INVARIANTS.len());
        assert!(all.iter().all(|(_, v)| v.is_empty()));
    }

    #[test]
    fn checks_are_deterministic() {
        let r = reg(vec![
            taken(task("T-1", Status::Done, &["a", "b"], None)),
            task("T-2", Status::Active, &["b"], None),
            task("T-3", Status::Active, &["b/c"], Some("sha-1")),
        ]);
        let first = check_all(&r, |_: &str| false);
        let second = check_all(&r, |_: &str| false);
        assert_eq!(first, second);
        let order: Vec<&str> = first.iter().map(|(k, _)| *k).collect();
        assert_eq!(order, INVARIANTS.to_vec());
        assert_eq!(first[0].1.len(), 1); // T-2 + T-3 делят зону b.
        assert!(!first[1].1.is_empty()); // T-1 сдана без SHA.
        assert!(!first[2].1.is_empty()); // T-1 сохранила маркеры «в работе».
    }

    #[test]
    fn harness_plan_covers_all_invariants_deterministically() {
        let plan = harness_plan();
        assert_eq!(plan.len(), INVARIANTS.len());
        assert!(plan.iter().all(|h| h.cmd.starts_with("cargo kani")));
        assert_eq!(harness_plan(), plan);
        for inv in INVARIANTS.iter() {
            assert!(plan.iter().any(|h| h.invariant == *inv));
        }
    }

    #[test]
    fn violation_display_is_readable() {
        let r = reg(vec![
            task("T-1", Status::Active, &["src/core"], None),
            task("T-2", Status::Active, &["src/core"], None),
        ]);
        let shown = zone_exclusivity(&r)[0].to_string();
        assert!(shown.contains(INV_ZONE_EXCLUSIVITY));
        assert!(shown.contains("T-1 + T-2"));
        assert!(shown.contains("↔"));
    }

    /// Живая сверка РЕАЛЬНОГО реестра (O7-доклад): тот же инвариант, что
    /// ловит двойную работу в проде, прогоняется по `.harness/orch/registry.json`.
    /// Запуск только явный (`--ignored`): в worktree реестра нет, корень
    /// задаётся через `DEVH_LIVE_REGISTRY_ROOT`. Зелёный = аудит работает и
    /// детерминирован (два прогона идентичны); найденные нарушения печатаются
    /// и тест НЕ валят — это долги реестра, их лечит владелец зоны, не модуль.
    #[test]
    #[ignore = "нужен живой реестр: DEVH_LIVE_REGISTRY_ROOT=<корень с .harness/orch/registry.json>"]
    fn live_registry_audit_is_deterministic_and_reports_violations() {
        let root = match std::env::var("DEVH_LIVE_REGISTRY_ROOT") {
            Ok(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        };
        if !root.join(".harness/orch/registry.json").exists() {
            println!(
                "skip: реестр не найден в {} — аудит не на чем гонять",
                root.display()
            );
            return;
        }
        let audit = |r: &crate::commands::orch::Registry| {
            (
                zone_exclusivity(r),
                commit_proven(r, |sha| {
                    std::process::Command::new("git")
                        .args(["rev-parse", "--verify", &format!("{sha}^{{commit}}")])
                        .current_dir(&root)
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                }),
                terminal_not_active(r),
            )
        };
        let reg = crate::commands::orch::load(&root);
        let first = audit(&reg);
        let second = audit(&reg);
        assert_eq!(
            first, second,
            "аудит живого реестра должен быть детерминирован"
        );
        let (zones, commits, taken) = &first;
        println!(
            "ЖИВОЙ РЕЕСТР {}: zone_exclusivity={} (O5 двойная работа), commit_proven={} (T-237 несуществующие SHA), terminal_not_active={}",
            root.display(),
            zones.len(),
            commits.len(),
            taken.len()
        );
        for (label, set) in [
            ("zone_exclusivity", zones),
            ("commit_proven", commits),
            ("terminal_not_active", taken),
        ] {
            for v in set.iter() {
                println!("  {label}: {v}");
            }
        }
    }
}
