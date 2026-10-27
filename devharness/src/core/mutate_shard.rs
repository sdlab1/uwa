//! Разбиение мутационного аудита на ШАРДЫ (T-644).
//!
//! Зачем: один прогон 510 мутантов идёт часами и занимает машину целиком.
//! Шарды дают три вещи:
//!   1. **план** — что и в каком порядке проверять (детерминированно);
//!   2. **экономию** — файлы, покрытые ledger'ом (`.harness/mutate/coverage.json`)
//!      и НЕ менявшиеся с прошлого аудита, в план не попадают: «не проверять
//!      опять всё заново» (директива пользователя);
//!   3. **оценку** — ожидаемое число мутантов и времени по шарду, чтобы
//!      понимать, во что ввязываться.
//!
//! Шарды идут ПОСЛЕДОВАТЕЛЬНО (--run): параллельные прогоны на одной машине
//! дерут CPU (замер: ×2–4 медленнее) и мешают соседним агентам.

use std::path::Path;

use crate::core::mut_history::{self, Coverage};

/// Оценка числа мутантов в файле, о котором ledger ничего не знает.
pub const DEFAULT_MUTANTS_PER_FILE: u32 = 25;
/// Оценка минут на один мутант (тёплый кэш, по замерам §9.10).
pub const DEFAULT_MINUTES_PER_MUTANT: f64 = 0.25;
/// Сколько файлов класть в один шард по умолчанию.
pub const DEFAULT_MAX_FILES_PER_SHARD: usize = 4;
/// Сколько шардов готовить по умолчанию.
pub const DEFAULT_MAX_SHARDS: usize = 8;

/// Один шард аудита.
#[derive(Debug, Clone, PartialEq)]
pub struct Shard {
    /// Номер шарда (с 1).
    pub index: usize,
    /// Файлы шарда (детерминированный порядок).
    pub files: Vec<String>,
    /// Ожидаемое число мутантов.
    pub expected_mutants: u32,
    /// Оценка времени, минут.
    pub est_minutes: f64,
}

/// План шардов по зоне.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShardPlan {
    /// Шарды к прогону (пустой — если файлов нет или всё покрыто).
    pub shards: Vec<Shard>,
    /// Файлы, пропущенные как покрытые (аудит был, файл не менялся).
    pub covered: Vec<String>,
    /// Файлы, которые менялись после аудита (в план входят).
    pub changed: Vec<String>,
    /// Файлы, никогда не аудировавшиеся (в план входят).
    pub fresh: Vec<String>,
    /// Файлы, не попавшие в план из-за лимита шардов.
    pub overflow: Vec<String>,
}

/// Параметры планирования.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanOpts {
    /// Максимум файлов в шарде.
    pub max_files_per_shard: usize,
    /// Максимум шардов.
    pub max_shards: usize,
    /// Оценка мутантов в неизвестном файле.
    pub mutants_per_file: u32,
    /// Оценка минут на мутант.
    pub minutes_per_mutant: f64,
}

impl Default for PlanOpts {
    fn default() -> Self {
        Self {
            max_files_per_shard: DEFAULT_MAX_FILES_PER_SHARD,
            max_shards: DEFAULT_MAX_SHARDS,
            mutants_per_file: DEFAULT_MUTANTS_PER_FILE,
            minutes_per_mutant: DEFAULT_MINUTES_PER_MUTANT,
        }
    }
}

/// Оценка числа мутантов файла (по ledger'у; неизвестен — дефолт).
pub fn expected_mutants(cov: &Coverage, file: &str, fallback: u32) -> u32 {
    cov.files
        .get(file)
        .map(|c| c.mutants)
        .filter(|n| *n > 0)
        .unwrap_or(fallback)
}

/// Построить план шардов.
///
/// Детерминированно: файлы сортируются, шарды набираются по порядку.
/// Покрытые файлы (аудит был, содержимое не менялось) в план НЕ входят.
pub fn plan(root: &Path, files: &[String], opts: &PlanOpts) -> ShardPlan {
    let cov = mut_history::load_coverage(root);
    let scope = mut_history::split_scope(root, files);
    let mut plan = ShardPlan {
        covered: scope.covered.clone(),
        changed: scope.changed.clone(),
        fresh: scope.fresh.clone(),
        ..ShardPlan::default()
    };
    // Порядок детерминирован: сначала свежие, потом изменённые; внутри — сортировка.
    let mut pending: Vec<String> = scope.pending();
    pending.sort();
    let max_files = opts.max_files_per_shard.max(1);
    let mut current: Vec<String> = Vec::new();
    let mut current_mutants: u32 = 0;
    for file in pending {
        let est = expected_mutants(&cov, &file, opts.mutants_per_file);
        if current.len() >= max_files {
            plan.shards.push(Shard {
                index: plan.shards.len().saturating_add(1),
                files: std::mem::take(&mut current),
                expected_mutants: current_mutants,
                est_minutes: current_mutants as f64 * opts.minutes_per_mutant,
            });
            current_mutants = 0;
        }
        if plan.shards.len() >= opts.max_shards {
            // Лимит шардов: остальные файлы — в overflow (показываем, не теряем).
            plan.overflow.push(file);
            plan.overflow.extend(current.drain(..));
            continue;
        }
        current.push(file);
        current_mutants = current_mutants.saturating_add(est);
    }
    if !current.is_empty() && plan.shards.len() < opts.max_shards {
        plan.shards.push(Shard {
            index: plan.shards.len().saturating_add(1),
            files: current,
            expected_mutants: current_mutants,
            est_minutes: current_mutants as f64 * opts.minutes_per_mutant,
        });
    }
    plan.overflow.sort();
    plan
}

/// Текстовый отчёт плана (для `--plan` и письма).
pub fn render_plan(plan: &ShardPlan) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "мутационный аудит: план шардов — {}, в очереди {}, изменённых {}, покрытых (пропуск) {}, лишних (лимит) {}\n",
        plan.shards.len(),
        plan.fresh.len(),
        plan.changed.len(),
        plan.covered.len(),
        plan.overflow.len()
    ));
    for shard in &plan.shards {
        out.push_str(&format!(
            "  шард {}: файлов {}, ожидается мутантов ~{}, время ~{:.1} мин\n",
            shard.index,
            shard.files.len(),
            shard.expected_mutants,
            shard.est_minutes
        ));
        for f in &shard.files {
            out.push_str(&format!("      {f}\n"));
        }
    }
    if !plan.covered.is_empty() {
        out.push_str(&format!(
            "  пропущено (уже покрыто, не менялось): {}\n",
            plan.covered.join(", ")
        ));
    }
    if !plan.overflow.is_empty() {
        out.push_str(&format!(
            "  не вошло (лимит шардов): {}\n",
            plan.overflow.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "devh-mut-shard-{}-{}",
            std::process::id(),
            crate::core::digest::sha256_hex(
                format!("{:?}", std::time::SystemTime::now()).as_bytes()
            )
        ));
        let _ = fs::create_dir_all(&base);
        base
    }

    fn opts() -> PlanOpts {
        PlanOpts {
            max_files_per_shard: 2,
            max_shards: 2,
            mutants_per_file: 10,
            minutes_per_mutant: 0.1,
        }
    }

    /// План детерминирован: тот же вход — тот же выход (порядок файлов).
    #[test]
    fn plan_is_deterministic() {
        let root = tmpdir();
        let files: Vec<String> = vec!["b.rs".into(), "a.rs".into(), "c.rs".into()];
        let p1 = plan(&root, &files, &opts());
        let p2 = plan(&root, &files, &opts());
        assert_eq!(p1, p2, "план детерминирован");
        assert_eq!(p1.shards[0].files, vec!["a.rs".to_string(), "b.rs".to_string()]);
        assert_eq!(p1.shards[1].files, vec!["c.rs".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Покрытый файл (аудит был, не менялся) в план не попадает.
    #[test]
    fn covered_files_are_skipped() {
        let root = tmpdir();
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let sha = mut_history::file_sha(&root.join("a.rs"));
        let mut cov = Coverage::default();
        cov.files.insert(
            "a.rs".to_string(),
            mut_history::FileCoverage {
                mutants: 7,
                caught: 7,
                missed: 0,
                timeout: 0,
                unviable: 0,
                at: 1,
                sha: sha.clone(),
            },
        );
        mut_history::save_coverage(&root, &cov).expect("save");
        let files = vec!["a.rs".to_string()];
        let p = plan(&root, &files, &opts());
        assert!(p.shards.is_empty(), "покрытый файл не планируем");
        assert_eq!(p.covered, vec!["a.rs".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Файл, менявшийся после аудита, — в плане (нужна перепроверка дельты).
    #[test]
    fn changed_file_is_planned() {
        let root = tmpdir();
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let mut cov = Coverage::default();
        cov.files.insert(
            "a.rs".to_string(),
            mut_history::FileCoverage {
                mutants: 7,
                caught: 7,
                missed: 0,
                timeout: 0,
                unviable: 0,
                at: 1,
                sha: "старый-sha".to_string(),
            },
        );
        mut_history::save_coverage(&root, &cov).expect("save");
        let p = plan(&root, &vec!["a.rs".to_string()], &opts());
        assert_eq!(p.shards.len(), 1, "изменённый файл планируем");
        assert_eq!(p.changed, vec!["a.rs".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Лимит шардов: лишние файлы — в overflow, не теряются молча.
    #[test]
    fn shard_limit_is_respected() {
        let root = tmpdir();
        let files: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let p = plan(&root, &files, &opts());
        assert_eq!(p.shards.len(), 2, "шардов не больше лимита");
        assert!(!p.overflow.is_empty(), "лишние файлы видны");
        let planned: usize = p.shards.iter().map(|s| s.files.len()).sum();
        assert_eq!(planned + p.overflow.len(), 10, "все файлы учтены");
        let _ = fs::remove_dir_all(&root);
    }

    /// Оценка мутантов берётся из ledger'а (известного файла).
    #[test]
    fn estimate_uses_coverage_ledger() {
        let root = tmpdir();
        let mut cov = Coverage::default();
        cov.files.insert(
            "a.rs".to_string(),
            mut_history::FileCoverage {
                mutants: 42,
                ..Default::default()
            },
        );
        assert_eq!(expected_mutants(&cov, "a.rs", 10), 42);
        assert_eq!(expected_mutants(&cov, "b.rs", 10), 10, "неизвестный — дефолт");
        let _ = fs::remove_dir_all(&root);
    }

    /// Отчёт плана содержит файлы и оценку.
    #[test]
    fn render_mentions_files_and_estimate() {
        let root = tmpdir();
        let files: Vec<String> = vec!["a.rs".into()];
        let p = plan(&root, &files, &opts());
        let text = render_plan(&p);
        assert!(text.contains("a.rs"), "отчёт: {text}");
        assert!(text.contains("мутантов"), "отчёт: {text}");
        let _ = fs::remove_dir_all(&root);
    }
}
