//! Метрики КАЧЕСТВА работы модели: комментарии, тщательность, течение процесса.
//!
//! Это НЕ «чёрная магия», а явная арифметика над наблюдаемыми числами:
//! каждая компонента — одна формула, записанная в коде и покрытая тестом.
//!
//! Что меряем (по одной задаче агента):
//! - `comment_lines / code_lines` — комментирует ли модель код и в какой мере;
//! - `deliberation_actions`     — эвристика «обдумывания»: сколько разных
//!   действий (проверки/чтения/правки) агент выполнил по задаче;
//! - `rework_rounds`            — круги переделки (упал → починил);
//! - `duration_secs`            — как протекала работа;
//! - `errors_final/warnings_final` — качество итогового кода.
//!
//! Результат агрегируется в ВЕКТОР компонент (`[f64; QUALITY_TERMS]`) и в
//! скалярный «коэффициент ума» (`capability_index`) — насколько модель
//! работает на данном уровне сложности (C0..C10).
//!
//! Данные идут в автотюн потолка (`core::autotune`): эмпирика может ТОЛЬКО
//! понизить потолок C, но не повысить его.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Число компонент вектора качества.
pub const QUALITY_TERMS: usize = 6;

/// Компоненты вектора качества (порядок фиксирован).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTerm {
    /// Качество кода: ошибки в финале (1 = без ошибок).
    Error = 0,
    /// Аккуратность: предупреждения в финале (1 = без варнингов).
    Warning = 1,
    /// Способность чинить: доля исправленных ошибок и скорость починки.
    Repair = 2,
    /// Комментируемость кода (плотность комментариев в «рабочей полосе»).
    Comment = 3,
    /// Тщательность обдумывания (эвристика по числу действий).
    Deliberation = 4,
    /// Течение процесса (эффективность, без крутых переделок).
    Process = 5,
}

impl QualityTerm {
    /// Все компоненты в фиксированном порядке.
    pub const ALL: [QualityTerm; QUALITY_TERMS] = [
        QualityTerm::Error,
        QualityTerm::Warning,
        QualityTerm::Repair,
        QualityTerm::Comment,
        QualityTerm::Deliberation,
        QualityTerm::Process,
    ];

    /// Человекочитаемое имя компоненты.
    pub fn name(self) -> &'static str {
        match self {
            QualityTerm::Error => "error",
            QualityTerm::Warning => "warning",
            QualityTerm::Repair => "repair",
            QualityTerm::Comment => "comment",
            QualityTerm::Deliberation => "deliberation",
            QualityTerm::Process => "process",
        }
    }

    /// Индекс компоненты в векторе.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Вектор компонент качества — массив float'ов фиксированного размера.
///
/// Хранится в raw-виде (без округления): округлённые значения теряют
/// различия между «почти одинаковыми» моделями, а нам нужен точный вклад
/// каждой компоненты в итоговый коэффициент.
pub type QualityVector = [f64; QUALITY_TERMS];

/// Вектор весов компонент (в сумме = 1.0).
///
/// Ошибки весят больше всего, комментарии — меньше всего: комментируемость
/// это стиль, а не ум; ошибки и варнинги — прямое качество кода.
pub const QUALITY_WEIGHTS: [f64; QUALITY_TERMS] = [0.30, 0.15, 0.25, 0.05, 0.10, 0.15];

/// Идеальная плотность комментариев (комментариев на строку кода).
pub const IDEAL_COMMENT_RATIO: f64 = 0.12;

/// Ширина «рабочей полосы» вокруг идеальной плотности.
pub const COMMENT_BAND: f64 = 0.12;

/// Сколько действий считается «полным обдумыванием».
pub const IDEAL_ACTIONS: f64 = 8.0;

/// Ожидаемая «трудность» задачи уровня C: во сколько раз больше ошибок,
/// варнингов и прогонов мы ожидаем от модели, работающей на этом уровне.
///
/// Это и есть «нормировка по уровню задачи»: 3 ошибки на C0 — это хуже,
/// чем 3 ошибки на C9 (там их и ожидают).
pub fn level_scale(c_level: u8) -> f64 {
    1.0 + c_level as f64
}

/// Наблюдение о работе агента над одной задачей (сырые числа, без выводов).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualitySample {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Канонический адрес агента.
    pub agent: String,
    /// Модель агента.
    pub model: String,
    /// Задача (если известна).
    #[serde(default)]
    pub task_id: Option<String>,
    /// Уровень сложности задачи (C0..C10).
    #[serde(default)]
    pub c_level: u8,
    /// Строк добавлено в диффе.
    #[serde(default)]
    pub added_lines: u32,
    /// Из них строк-комментариев.
    #[serde(default)]
    pub comment_lines: u32,
    /// Из них строк кода.
    #[serde(default)]
    pub code_lines: u32,
    /// Сколько раз запускался `cargo check` по задаче.
    #[serde(default)]
    pub check_runs: u32,
    /// Ошибок в финальной сборке.
    #[serde(default)]
    pub errors_final: u32,
    /// Предупреждений в финальной сборке.
    #[serde(default)]
    pub warnings_final: u32,
    /// Финальная сборка прошла.
    #[serde(default)]
    pub build_ok: bool,
    /// Длительность работы над задачей, секунды.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// Эвристика: число различных действий агента (проверки/чтения/правки).
    #[serde(default)]
    pub deliberation_actions: u32,
    /// Эвристика: круги переделки (упал → починил → упал).
    #[serde(default)]
    pub rework_rounds: u32,
    /// Модель прошла принудительную идентификацию (ответила на вопрос).
    #[serde(default)]
    pub identified: bool,
}

impl QualitySample {
    /// Плотность комментариев: комментариев на строку кода.
    pub fn comment_ratio(&self) -> f64 {
        if self.code_lines == 0 {
            return 0.0;
        }
        self.comment_lines as f64 / self.code_lines as f64
    }
}

/// Путь к ledger'у качества: `.harness/orch/quality.jsonl`.
pub fn ledger_path(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root).join("quality.jsonl")
}

/// Дописать наблюдение в ledger (append-only). Ошибка не роняет гейт.
pub fn append_sample(root: &Path, sample: &QualitySample) -> std::io::Result<()> {
    let p = ledger_path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    let line = serde_json::to_string(sample)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Прочитать ledger (битые строки пропускаются).
pub fn load_samples(root: &Path) -> Vec<QualitySample> {
    let p = ledger_path(root);
    let Ok(f) = fs::File::open(p) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .filter_map(|l| l.ok())
        .filter_map(|l| serde_json::from_str::<QualitySample>(&l).ok())
        .collect()
}

/// Строка комментария? (по общему признаку для Rust/C/Python/shell).
pub fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with('#')
        || t.starts_with("--")
}

/// Метрики добавленного кода по тексту unified-диффа.
///
/// Возвращает `(добавлено, комментарии, код)`. Строки заголовков (`+++`)
/// не считаются.
pub fn diff_metrics(diff: &str) -> (u32, u32, u32) {
    let mut added = 0u32;
    let mut comments = 0u32;
    let mut code = 0u32;
    for line in diff.lines() {
        if !line.starts_with('+') {
            continue;
        }
        if line.starts_with("+++") {
            continue;
        }
        added = added.saturating_add(1);
        if is_comment_line(line.strip_prefix('+').unwrap_or(line)) {
            comments = comments.saturating_add(1);
        } else {
            code = code.saturating_add(1);
        }
    }
    (added, comments, code)
}

/// Маркеры действий агента для эвристики «обдумывания».
const ACTION_MARKERS: [&str; 12] = [
    "cargo check",
    "cargo test",
    "cargo build",
    "cargo clippy",
    "cargo fmt",
    "git diff",
    "git status",
    "grep ",
    "rg ",
    "sed ",
    "cat ",
    "python3",
];

/// Эвристика «обдумывания»: сколько РАЗНЫХ видов действий агент выполнял.
///
/// Не «сколько команд», а сколько видов: 20 одинаковых `cargo check` — это
/// не тщательность, это затык. Разные виды (сборка, тесты, чтение, поиск,
/// форматирование) — признак того, что модель проверяла разные срезы.
pub fn deliberation_actions(log: &str) -> u32 {
    let mut kinds: Vec<&str> = Vec::new();
    for marker in ACTION_MARKERS.iter() {
        if log.contains(marker) && !kinds.contains(marker) {
            kinds.push(marker);
        }
    }
    kinds.len() as u32
}

/// Эвристика «кругов переделки»: пары «ошибка → успех» по задаче.
pub fn rework_rounds(samples: &[QualitySample]) -> u32 {
    let mut by_task: BTreeMap<String, Vec<&QualitySample>> = BTreeMap::new();
    for s in samples {
        if let Some(t) = s.task_id.as_ref() {
            by_task.entry(t.clone()).or_default().push(s);
        }
    }
    let mut rounds = 0u32;
    for (_, mut list) in by_task {
        list.sort_by_key(|s| s.ts);
        let mut failed = false;
        for s in list {
            if !s.build_ok {
                failed = true;
            } else if failed {
                rounds = rounds.saturating_add(1);
                failed = false;
            }
        }
    }
    rounds
}

/// Компонента «качество кода по ошибкам»: 1 = без ошибок, 0 и меньше — хуже.
pub fn error_score(avg_errors: f64) -> f64 {
    1.0 / (1.0 + avg_errors.max(0.0))
}

/// Компонента «аккуратность по варнингам»: 1 = без предупреждений.
pub fn warning_score(avg_warnings: f64) -> f64 {
    1.0 / (1.0 + avg_warnings.max(0.0))
}

/// Компонента «способность чинить»: доля исправленных ошибок, с поправкой на
/// скорость починки (медленная починка даёт меньше).
pub fn repair_score(fix_rate: f64, fix_latency_avg: Option<f64>, ref_secs: f64) -> f64 {
    let latency = fix_latency_avg.unwrap_or(0.0);
    let coef = fix_rate / (1.0 + latency / ref_secs.max(1.0));
    coef.clamp(0.0, 1.0)
}

/// Компонента «комментируемость»: плотность комментариев в рабочей полосе
/// вокруг идеальной. Слишком мало — код без объяснений, слишком много —
/// шум. Вне полосы — 0.
pub fn comment_score(ratio: f64) -> f64 {
    let d = (ratio - IDEAL_COMMENT_RATIO).abs();
    if d >= COMMENT_BAND {
        return 0.0;
    }
    1.0 - d / COMMENT_BAND
}

/// Компонента «тщательность обдумывания» (эвристика): насыщается при
/// `IDEAL_ACTIONS` видах действий.
pub fn deliberation_score(actions: u32) -> f64 {
    let a = actions as f64;
    (1.0 + a).ln() / (1.0 + IDEAL_ACTIONS).ln()
}

/// Компонента «течение процесса»: эффективность (финальная сборка прошла)
/// минус штраф за круги переделки.
pub fn process_score(build_ok: bool, rework: u32) -> f64 {
    let base = if build_ok { 1.0 } else { 0.25 };
    base / (1.0 + rework as f64)
}

/// Агрегация наблюдений в вектор компонент.
///
/// `c_level` — фильтр по уровню задачи (None = все уровни). Метрики
/// нормируются на `level_scale(c_level)`: ошибки на сложной задаче
/// ожидаемы и не должны штрафоваться так же, как на простой.
pub fn quality_vector(samples: &[QualitySample], c_level: Option<u8>) -> QualityVector {
    let selected: Vec<&QualitySample> = samples
        .iter()
        .filter(|s| c_level.map(|c| s.c_level == c).unwrap_or(true))
        .collect();
    if selected.is_empty() {
        return [0.0; QUALITY_TERMS];
    }
    let n = selected.len() as f64;
    let mut err_num = 0.0f64;
    let mut warn_num = 0.0f64;
    let mut ok = 0.0f64;
    let mut clean = 0.0f64;
    let mut actions = 0.0f64;
    let mut comments = 0.0f64;
    for s in &selected {
        let scale = level_scale(s.c_level);
        err_num += s.errors_final as f64 / scale;
        warn_num += s.warnings_final as f64 / scale;
        if s.build_ok {
            ok += 1.0;
            if s.warnings_final == 0 {
                clean += 1.0;
            }
        }
        actions += s.deliberation_actions as f64;
        comments += s.comment_ratio();
    }
    let avg_errors = err_num / n;
    let avg_warnings = warn_num / n;
    let success_rate = ok / n;
    let warn_clean_rate = clean / n;
    let avg_actions = actions / n;
    let avg_ratio = comments / n;
    let failures = selected.iter().filter(|s| !s.build_ok).count();
    let rework = rework_rounds(&selected.iter().map(|s| (*s).clone()).collect::<Vec<_>>());
    let fix_rate = success_rate;
    let latency = selected
        .iter()
        .filter_map(|s| s.duration_secs)
        .fold(None::<f64>, |acc, v| Some(acc.unwrap_or(0.0) + v))
        .map(|sum| sum / n);
    let mut v: QualityVector = [0.0; QUALITY_TERMS];
    v[QualityTerm::Error.index()] = error_score(avg_errors);
    v[QualityTerm::Warning.index()] = warning_score(avg_warnings);
    // Не было ни одной ошибки — чинить было нечего: это не «плохая починка»,
    // а идеальный результат (иначе образцовая модель штрафуется за чистоту).
    let repair = if failures == 0 {
        1.0
    } else {
        repair_score(fix_rate, latency, 300.0)
    };
    v[QualityTerm::Repair.index()] = repair;
    v[QualityTerm::Comment.index()] = comment_score(avg_ratio);
    v[QualityTerm::Deliberation.index()] = deliberation_score(avg_actions.round() as u32);
    v[QualityTerm::Process.index()] = process_score(success_rate > 0.5, rework);
    v
}

/// Скалярный «коэффициент ума» модели: взвешенная сумма компонент (0..1).
///
/// 1.0 — образцовая работа, 0.0 — нулевая.
pub fn capability_index(v: &QualityVector) -> f64 {
    let mut sum = 0.0f64;
    let mut wsum = 0.0f64;
    for i in 0..QUALITY_TERMS {
        let w = QUALITY_WEIGHTS.get(i).copied().unwrap_or(0.0);
        sum += w * v.get(i).copied().unwrap_or(0.0);
        wsum += w;
    }
    if wsum <= 0.0 {
        return 0.0;
    }
    (sum / wsum).clamp(0.0, 1.0)
}

/// Потолок C по коэффициенту ума (0..1 → C0..C10).
pub fn ceiling_from_capability(index: f64) -> u8 {
    let raw = (index.clamp(0.0, 1.0) * crate::core::complexity::Complexity::MAX as f64).round();
    raw.clamp(0.0, crate::core::complexity::Complexity::MAX as f64) as u8
}

/// Агрегация по агенту: (ключ → вектор) по всем наблюдениям.
pub fn vectors_by_agent(samples: &[QualitySample]) -> BTreeMap<String, QualityVector> {
    let mut by: BTreeMap<String, Vec<&QualitySample>> = BTreeMap::new();
    for s in samples {
        by.entry(s.agent.clone()).or_default().push(s);
    }
    by.into_iter()
        .map(|(k, v)| {
            let owned: Vec<QualitySample> = v.into_iter().cloned().collect();
            (k, quality_vector(&owned, None))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(c_level: u8, errors: u32, warnings: u32, ok: bool) -> QualitySample {
        QualitySample {
            ts: 1,
            agent: "beam".into(),
            model: "mimo".into(),
            task_id: Some("T-1".into()),
            c_level,
            added_lines: 100,
            comment_lines: 10,
            code_lines: 90,
            check_runs: 2,
            errors_final: errors,
            warnings_final: warnings,
            build_ok: ok,
            duration_secs: Some(60.0),
            deliberation_actions: 6,
            rework_rounds: 0,
            identified: true,
        }
    }

    #[test]
    fn diff_metrics_counts_comments_and_code() {
        let diff = "--- a/x\n+++ b/x\n@@ -1 +1,3 @@\n+// комментарий\n+let x = 1;\n+/* блок */\n";
        let (added, comments, code) = diff_metrics(diff);
        assert_eq!(added, 3);
        assert_eq!(comments, 2);
        assert_eq!(code, 1);
    }

    #[test]
    fn diff_metrics_ignores_headers() {
        let diff = "+++ b/file.rs\n--- a/file.rs\n+code();\n";
        let (added, _, _) = diff_metrics(diff);
        assert_eq!(added, 1);
    }

    #[test]
    fn comment_ratio_and_score() {
        let mut s = sample(1, 0, 0, true);
        s.code_lines = 100;
        s.comment_lines = 12;
        assert!((s.comment_ratio() - 0.12).abs() < 1e-9);
        assert!((comment_score(s.comment_ratio()) - 1.0).abs() < 1e-9);
        s.comment_lines = 0;
        assert_eq!(comment_score(s.comment_ratio()), 0.0);
    }

    #[test]
    fn too_many_comments_is_not_better() {
        let many = comment_score(0.9);
        assert_eq!(many, 0.0, "шум комментариев не считается умом");
    }

    #[test]
    fn deliberation_counts_distinct_kinds_not_repeats() {
        let log = "cargo check\ncargo check\ncargo check\ngrep foo\ncargo test";
        assert_eq!(deliberation_actions(log), 3);
    }

    #[test]
    fn error_and_warning_scores_are_separate() {
        assert!(error_score(4.0) < warning_score(0.0));
        let v = quality_vector(&[sample(0, 4, 0, false)], None);
        assert!(v[QualityTerm::Error.index()] < v[QualityTerm::Warning.index()]);
    }

    #[test]
    fn level_normalization_softens_hard_tasks() {
        let easy = quality_vector(&[sample(0, 4, 0, false)], None);
        let hard = quality_vector(&[sample(9, 4, 0, false)], None);
        assert!(
            hard[QualityTerm::Error.index()] > easy[QualityTerm::Error.index()],
            "на C9 те же 4 ошибки ожидаемее, чем на C0"
        );
    }

    #[test]
    fn capability_index_is_bounded() {
        let perfect: QualityVector = [1.0; QUALITY_TERMS];
        let zero: QualityVector = [0.0; QUALITY_TERMS];
        assert!((capability_index(&perfect) - 1.0).abs() < 1e-9);
        assert_eq!(capability_index(&zero), 0.0);
        let mid = [0.5; QUALITY_TERMS];
        assert!((capability_index(&mid) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn capability_index_weights_errors_most() {
        let mut with_errors = [1.0; QUALITY_TERMS];
        with_errors[QualityTerm::Error.index()] = 0.0;
        let mut with_comments = [1.0; QUALITY_TERMS];
        with_comments[QualityTerm::Comment.index()] = 0.0;
        assert!(capability_index(&with_errors) < capability_index(&with_comments));
    }

    #[test]
    fn ceiling_from_capability_maps_to_c_scale() {
        assert_eq!(ceiling_from_capability(0.0), 0);
        assert_eq!(ceiling_from_capability(1.0), 10);
        assert_eq!(ceiling_from_capability(0.55), 6);
    }

    #[test]
    fn rework_rounds_counts_fail_then_success() {
        let mut a = sample(1, 2, 0, false);
        a.ts = 10;
        let mut b = sample(1, 0, 0, true);
        b.ts = 20;
        let mut c = sample(1, 1, 0, false);
        c.ts = 30;
        let mut d = sample(1, 0, 0, true);
        d.ts = 40;
        let all = vec![a, b, c, d];
        assert_eq!(rework_rounds(&all), 2);
    }

    #[test]
    fn process_score_penalizes_rework() {
        assert!(process_score(true, 0) > process_score(true, 3));
        assert!(process_score(true, 0) > process_score(false, 0));
    }

    #[test]
    fn ledger_roundtrip_keeps_raw_numbers() {
        let dir = std::env::temp_dir().join(format!("devh-quality-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = ledger_path(&dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("dir");
        }
        let s = sample(3, 2, 1, false);
        append_sample(&dir, &s).expect("append");
        let loaded = load_samples(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.first().map(|x| x.c_level), Some(3));
        assert_eq!(loaded.first().map(|x| x.errors_final), Some(2));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vectors_by_agent_separates_agents() {
        let mut a = sample(1, 0, 0, true);
        a.agent = "beam".into();
        let mut b = sample(1, 9, 9, false);
        b.agent = "beam2".into();
        let v = vectors_by_agent(&[a, b]);
        assert_eq!(v.len(), 2);
        let x = v.get("beam").expect("beam");
        let y = v.get("beam2").expect("beam2");
        assert!(x[QualityTerm::Error.index()] > y[QualityTerm::Error.index()]);
    }
}
