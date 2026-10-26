//! Скорость агентов — через КАНОНИЧЕСКИЙ сборщик `core/throughput*`.
//!
//! Панель НЕ изобретает свой счётчик: числа те же, что у `devh orch speed`
//! (взвешенный LOC, WIP, thinking, токены, реальное рабочее время), потому
//! что источник один — `core::throughput_collect::collect`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::core::throughput::{self, WorkKind};
use crate::core::throughput_collect;

/// Строка скорости по агенту.
#[derive(Debug, Clone, Default)]
pub struct SpeedRow {
    /// Агент.
    pub agent: String,
    /// Модель (наблюдаемая, иначе заявленная).
    pub model: String,
    /// LOC кода (взвешенный вес по сложности — отдельно).
    pub loc: u64,
    /// LOC невлитой работы (WIP).
    pub wip_loc: u64,
    /// Взвешенный LOC (вес = 1.0 + 0.5*C).
    pub weighted_loc: f64,
    /// Токены «размышлений».
    pub thinking: u64,
    /// Все токены.
    pub total_tokens: u64,
    /// Реальное рабочее время (минуты, 5-мин бакеты).
    pub work_minutes: u64,
    /// Скорость: взвешенный LOC в час.
    pub speed_per_hour: f64,
    /// Эффективность: взвешенный LOC на 1000 токенов.
    pub efficiency: f64,
    /// Доля «размышлений», 0..1.
    pub thinking_share: f64,
}

/// TTL кэша скорости (секунды): сбор по истории git и журналам рантаймов тяжёлый.
const CACHE_TTL_SECS: u64 = 300;

/// Кэш скорости: (время, строки).
fn cache() -> &'static Mutex<Option<(u64, Vec<SpeedRow>)>> {
    static CACHE: OnceLock<Mutex<Option<(u64, Vec<SpeedRow>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Скорость агентов из кэша (пересчёт не чаще раза в 5 минут).
pub fn cached(root: &Path, since: u64) -> Vec<SpeedRow> {
    let now = super::now_secs();
    if let Ok(map) = cache().lock() {
        if let Some((ts, rows)) = map.as_ref() {
            if now.saturating_sub(*ts) < CACHE_TTL_SECS {
                return rows.clone();
            }
        }
    }
    let rows = collect(root, since);
    if let Ok(mut map) = cache().lock() {
        *map = Some((now, rows.clone()));
    }
    rows
}

/// Собрать скорость агентов за окно `since` (unix-секунды).
pub fn collect(root: &Path, since: u64) -> Vec<SpeedRow> {
    let col = throughput_collect::collect(root, since);
    let pairs = throughput::aggregate_pairs(&col.samples);
    let work_minutes = work_minutes(&col, since);
    let stats = throughput::group_by_agent(&pairs, &work_minutes, &col.declared_model);
    let mut out: Vec<SpeedRow> = stats
        .into_iter()
        .map(|s| {
            let model = col
                .observed_model
                .get(&s.agent)
                .filter(|m| !m.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| s.model.clone());
            SpeedRow {
                agent: s.agent.clone(),
                model,
                loc: s.loc,
                wip_loc: s.wip_loc,
                weighted_loc: s.weighted_loc,
                thinking: s.thinking,
                total_tokens: s.total_tokens,
                work_minutes: s.work_minutes,
                speed_per_hour: s.speed_per_hour(),
                efficiency: s.efficiency(),
                thinking_share: s.thinking_share(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.weighted_loc
            .partial_cmp(&a.weighted_loc)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    out
}

/// Реальное рабочее время по агентам (интервалы задач + бакета активности).
fn work_minutes(col: &throughput_collect::Collected, since: u64) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    let now = super::now_secs();
    for (agent, intervals) in &col.work_intervals {
        let clipped = throughput::clip_intervals(intervals, since, now);
        let minutes = throughput::total_minutes_rounded(&clipped, throughput::BUCKET_MIN);
        if minutes > 0 {
            out.insert(agent.clone(), minutes);
        }
    }
    // Активность без интервалов (агент работал, но задача не закрыта).
    for (agent, marks) in &col.activity {
        if out.contains_key(agent) {
            continue;
        }
        let marks: Vec<u64> = marks.iter().copied().filter(|t| *t >= since).collect();
        if marks.is_empty() {
            continue;
        }
        let buckets = throughput::work_buckets(&marks, now - since, throughput::BUCKET_MIN);
        out.insert(
            agent.clone(),
            (buckets.len() as u64).saturating_mul(throughput::BUCKET_MIN),
        );
    }
    out
}

/// Вид работы для подписи (используется в заголовках разделов).
pub fn kind_label(kind: WorkKind) -> &'static str {
    kind.label()
}
