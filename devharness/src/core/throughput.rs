//! Единица скорости/эффективности агента.
//!
//! Что считаем (по каждому агенту и по каждой модели):
//! * **КОД** — взвешенный LOC сделанной работы. Вес = сложность задачи
//!   (C-уровень): C0 → 1.0, C5 → 3.5, C10 → 6.0. Простой код не равен
//!   сложному: 300 строк тривиальной правки и 300 строк оркестратора —
//!   разный труд.
//! * **WIP** — взвешенный LOC чекпоинтов (`devh/wip-*`), которые ещё не
//!   влиты в main. Считаются ОТДЕЛЬНО от кода: это работа «в руках».
//! * **THINKING** — output-токены модели (размышления + ответ), отдельно
//!   от токенов контекста.
//! * **ТОКЕНЫ** — все токены запроса (input + output + cache read/write).
//!
//! **Реальное рабочее время** — не «время жизни процесса», а время, когда
//! агент ДЕЛАЛ запросы к модели (активность по журналу запросов), с
//! точностью до 5 минут. Простой (компьютер выключен) и паузы без
//! запросов в счёт не идут: интервалы строятся по меткам активности и
//! дополнительно обрезаются `uptime` машины.
//!
//! Итоговые метрики:
//! * скорость = взвешенный LOC / реальное рабочее время;
//! * эффективность = взвешенный LOC на 1000 токенов;
//! * доля размышлений = thinking / все токены.

use std::collections::{BTreeMap, BTreeSet};

/// Точность бакета (минуты) — требование «точность 5 минут».
pub const BUCKET_MIN: u64 = 5;

/// Ширина бакета в секундах.
pub const BUCKET_SEC: u64 = BUCKET_MIN * 60;

/// Вид работы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    /// Код, дошедший до main (взвешенный по сложности).
    Code,
    /// Чекпоинт WIP (не влит).
    Wip,
    /// Размышления/ответ модели (output-токены).
    Thinking,
}

impl WorkKind {
    /// Короткая метка для таблиц.
    pub fn label(self) -> &'static str {
        match self {
            WorkKind::Code => "КОД",
            WorkKind::Wip => "WIP",
            WorkKind::Thinking => "THINK",
        }
    }
}

/// Одна выборка работы.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Агент-исполнитель.
    pub agent: String,
    /// Модель, породившая работу ("" — идентификация отсутствует).
    pub model: String,
    /// Вид работы.
    pub kind: WorkKind,
    /// LOC (added + deleted) — для Code/Wip.
    pub loc: u64,
    /// Вес по сложности.
    pub weight: f64,
    /// Thinking/output-токены — для Thinking.
    pub thinking: u64,
    /// Все токены запроса.
    pub total_tokens: u64,
}

impl Sample {
    /// Выборка кода с весом по сложности.
    pub fn code(ts: u64, agent: &str, model: &str, loc: u64, weight: f64) -> Self {
        Self {
            ts,
            agent: agent.to_string(),
            model: model.to_string(),
            kind: WorkKind::Code,
            loc,
            weight,
            thinking: 0,
            total_tokens: 0,
        }
    }

    /// Выборка WIP-чекпоинта (вес 1.0 — сложность задачи неизвестна).
    ///
    /// ВАЖНО: вид работы — именно `Wip`, а не `Code`: чекпоинт `devh/wip-*`
    /// ещё не влит в main, и его LOC не должен попадать в счёт кода.
    pub fn wip(ts: u64, agent: &str, model: &str, loc: u64) -> Self {
        Self {
            ts,
            agent: agent.to_string(),
            model: model.to_string(),
            kind: WorkKind::Wip,
            loc,
            weight: 1.0,
            thinking: 0,
            total_tokens: 0,
        }
    }

    /// Выборка размышлений/токенов.
    pub fn thinking(ts: u64, agent: &str, model: &str, thinking: u64, total: u64) -> Self {
        Self {
            ts,
            agent: agent.to_string(),
            model: model.to_string(),
            kind: WorkKind::Thinking,
            loc: 0,
            weight: 1.0,
            thinking,
            total_tokens: total,
        }
    }

    /// Взвешенный объём кода этой выборки.
    pub fn weighted_loc(&self) -> f64 {
        self.loc as f64 * self.weight
    }
}

/// Вес по сложности задачи: C0 → 1.0, C5 → 3.5, C10 → 6.0.
pub fn complexity_weight(c_level: Option<u8>) -> f64 {
    1.0 + 0.5 * f64::from(c_level.unwrap_or(0))
}

/// Ключ модели: нормализация регистра и разделителей (как в оркестраторе).
pub fn model_key(model: &str) -> String {
    model
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

/// Агрегат по паре (агент, модель).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PairStats {
    pub agent: String,
    pub model: String,
    /// LOC кода (взвешенный — в weighted_loc).
    pub loc: u64,
    /// LOC WIP.
    pub wip_loc: u64,
    /// Взвешенный LOC (код + WIP).
    pub weighted_loc: f64,
    /// Thinking/output-токены.
    pub thinking: u64,
    /// Все токены.
    pub total_tokens: u64,
}

impl PairStats {
    /// Скорость: взвешенный LOC в час (при work_minutes = 0 → 0).
    pub fn speed_per_hour(&self, work_minutes: u64) -> f64 {
        if work_minutes == 0 {
            return 0.0;
        }
        self.weighted_loc * 60.0 / work_minutes as f64
    }

    /// Эффективность: взвешенный LOC на 1000 токенов.
    pub fn efficiency(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.weighted_loc * 1000.0 / self.total_tokens as f64
    }

    /// Доля размышлений в токенах.
    pub fn thinking_share(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.thinking as f64 / self.total_tokens as f64
    }
}

/// Свести выборки в агрегаты по паре (агент, модель).
pub fn aggregate_pairs(samples: &[Sample]) -> Vec<PairStats> {
    let mut map: BTreeMap<(String, String), PairStats> = BTreeMap::new();
    for s in samples {
        let key = (s.agent.clone(), model_key(&s.model));
        let e = map.entry(key).or_insert_with(|| PairStats {
            agent: s.agent.clone(),
            model: model_key(&s.model),
            ..Default::default()
        });
        match s.kind {
            WorkKind::Code => {
                e.loc = e.loc.saturating_add(s.loc);
                e.weighted_loc += s.weighted_loc();
            }
            WorkKind::Wip => {
                e.wip_loc = e.wip_loc.saturating_add(s.loc);
                e.weighted_loc += s.weighted_loc();
            }
            WorkKind::Thinking => {
                e.thinking = e.thinking.saturating_add(s.thinking);
                e.total_tokens = e.total_tokens.saturating_add(s.total_tokens);
            }
        }
    }
    map.into_values().collect()
}

/// Агрегат по агенту (все модели вместе).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentStats {
    pub agent: String,
    /// Идентифицированная модель агента ("" — идентификации нет).
    pub model: String,
    pub loc: u64,
    pub wip_loc: u64,
    pub weighted_loc: f64,
    pub thinking: u64,
    pub total_tokens: u64,
    /// Реальное рабочее время (минуты).
    pub work_minutes: u64,
}

impl AgentStats {
    pub fn speed_per_hour(&self) -> f64 {
        if self.work_minutes == 0 {
            return 0.0;
        }
        self.weighted_loc * 60.0 / self.work_minutes as f64
    }
    pub fn efficiency(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.weighted_loc * 1000.0 / self.total_tokens as f64
    }
    pub fn thinking_share(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.thinking as f64 / self.total_tokens as f64
    }
}

/// Свести пары (агент, модель) в агрегаты по агенту.
pub fn group_by_agent(
    pairs: &[PairStats],
    work_minutes: &BTreeMap<String, u64>,
    declared_model: &BTreeMap<String, String>,
) -> Vec<AgentStats> {
    let mut map: BTreeMap<String, AgentStats> = BTreeMap::new();
    for p in pairs {
        let e = map.entry(p.agent.clone()).or_insert_with(|| AgentStats {
            agent: p.agent.clone(),
            model: declared_model.get(&p.agent).cloned().unwrap_or_default(),
            work_minutes: work_minutes.get(&p.agent).copied().unwrap_or(0),
            ..Default::default()
        });
        e.loc = e.loc.saturating_add(p.loc);
        e.wip_loc = e.wip_loc.saturating_add(p.wip_loc);
        e.weighted_loc += p.weighted_loc;
        e.thinking = e.thinking.saturating_add(p.thinking);
        e.total_tokens = e.total_tokens.saturating_add(p.total_tokens);
    }
    map.into_values().collect()
}

/// Агрегат по модели (все агенты модели вместе).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelStats {
    pub model: String,
    pub agents: usize,
    pub loc: u64,
    pub wip_loc: u64,
    pub weighted_loc: f64,
    pub thinking: u64,
    pub total_tokens: u64,
    /// Рабочее время (сумма по агентам модели; агенты считаются один раз).
    pub work_minutes: u64,
}

impl ModelStats {
    pub fn speed_per_hour(&self) -> f64 {
        if self.work_minutes == 0 {
            return 0.0;
        }
        self.weighted_loc * 60.0 / self.work_minutes as f64
    }
    pub fn efficiency(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.weighted_loc * 1000.0 / self.total_tokens as f64
    }
    pub fn thinking_share(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.thinking as f64 / self.total_tokens as f64
    }
}

/// Свести пары в агрегаты по модели. Без идентификации модели агент в
/// статистику по моделям НЕ попадает (требование: «только при наличии
/// идентификации модели»).
pub fn group_by_model(
    pairs: &[PairStats],
    work_minutes: &BTreeMap<String, u64>,
    declared_model: &BTreeMap<String, String>,
) -> Vec<ModelStats> {
    let mut map: BTreeMap<String, ModelStats> = BTreeMap::new();
    let mut seen_agents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in pairs {
        let model = declared_model
            .get(&p.agent)
            .map(|m| model_key(m))
            .unwrap_or_default();
        if model.is_empty() {
            continue; // идентификации нет — в статистику по моделям не идём
        }
        let e = map.entry(model.clone()).or_insert_with(|| ModelStats {
            model: model.clone(),
            ..Default::default()
        });
        e.loc = e.loc.saturating_add(p.loc);
        e.wip_loc = e.wip_loc.saturating_add(p.wip_loc);
        e.weighted_loc += p.weighted_loc;
        e.thinking = e.thinking.saturating_add(p.thinking);
        e.total_tokens = e.total_tokens.saturating_add(p.total_tokens);
        let agents = seen_agents.entry(model).or_default();
        if agents.insert(p.agent.clone()) {
            e.agents = agents.len();
            e.work_minutes = e
                .work_minutes
                .saturating_add(work_minutes.get(&p.agent).copied().unwrap_or(0));
        }
    }
    map.into_values().collect()
}

/// Бакет серии.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bucket {
    /// Начало бакета (unix, выровнено по BUCKET_SEC).
    pub start: u64,
    pub loc: u64,
    pub wip_loc: u64,
    pub weighted_loc: f64,
    pub thinking: u64,
    pub total_tokens: u64,
}

/// Выровнять время по началу бакета.
pub fn bucket_start(ts: u64, bucket_sec: u64) -> u64 {
    if bucket_sec == 0 {
        return ts;
    }
    ts - ts % bucket_sec
}

/// Разложить выборки по бакетам в диапазоне [from, to].
pub fn bucketize(samples: &[Sample], from: u64, to: u64, bucket_sec: u64) -> Vec<Bucket> {
    let mut map: BTreeMap<u64, Bucket> = BTreeMap::new();
    let mut t = bucket_start(from, bucket_sec);
    while t <= to {
        map.insert(
            t,
            Bucket {
                start: t,
                ..Default::default()
            },
        );
        t = t.saturating_add(bucket_sec);
    }
    for s in samples {
        if s.ts < from || s.ts > to {
            continue;
        }
        let b = map
            .entry(bucket_start(s.ts, bucket_sec))
            .or_insert_with(|| Bucket {
                start: bucket_start(s.ts, bucket_sec),
                ..Default::default()
            });
        match s.kind {
            WorkKind::Code => {
                b.loc = b.loc.saturating_add(s.loc);
                b.weighted_loc += s.weighted_loc();
            }
            WorkKind::Wip => {
                b.wip_loc = b.wip_loc.saturating_add(s.loc);
                b.weighted_loc += s.weighted_loc();
            }
            WorkKind::Thinking => {
                b.thinking = b.thinking.saturating_add(s.thinking);
                b.total_tokens = b.total_tokens.saturating_add(s.total_tokens);
            }
        }
    }
    map.into_values().collect()
}

/// Бакеты реального рабочего времени агента (по активности).
///
/// Активность = метки запросов к модели. Каждый запрос «занимает» окно
/// ±`window_sec`; окна объединяются и округляются до бакета.
pub fn work_buckets(activity: &[u64], window_sec: u64, bucket_sec: u64) -> BTreeSet<u64> {
    let mut set: BTreeSet<u64> = BTreeSet::new();
    for ts in activity {
        let lo = ts.saturating_sub(window_sec);
        let hi = ts.saturating_add(window_sec);
        let mut b = bucket_start(lo, bucket_sec);
        while b <= hi {
            set.insert(b);
            b = b.saturating_add(bucket_sec);
        }
    }
    set
}

/// Объединение интервалов (пересечения склеиваются).
pub fn merge_intervals(intervals: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = intervals.iter().copied().filter(|(a, b)| b > a).collect();
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => {
                if b > last.1 {
                    last.1 = b;
                }
            }
            _ => out.push((a, b)),
        }
    }
    out
}

/// Обрезать интервалы по [from, to].
pub fn clip_intervals(intervals: &[(u64, u64)], from: u64, to: u64) -> Vec<(u64, u64)> {
    merge_intervals(
        &intervals
            .iter()
            .map(|(a, b)| ((*a).max(from), (*b).min(to)))
            .filter(|(a, b)| b > a)
            .collect::<Vec<_>>(),
    )
}

/// Суммарная длительность интервалов в минутах (с округлением до бакета).
pub fn total_minutes(intervals: &[(u64, u64)], bucket_min: u64) -> u64 {
    let secs: u64 = merge_intervals(intervals).iter().map(|(a, b)| b - a).sum();
    secs / 60 / bucket_min * bucket_min
}

/// Суммарная длительность интервалов в минутах с округлением до БЛИЖАЙШЕГО
/// бакета (точность 5 минут).
///
/// Округление к ближайшему, а не вниз: одиночный запрос к модели (окно
/// ±3 мин) — это 5 минут работы, а не 0. Округление вниз обнуляло бы
/// короткие сессии и завышало скорость.
pub fn total_minutes_rounded(intervals: &[(u64, u64)], bucket_min: u64) -> u64 {
    if bucket_min == 0 {
        return 0;
    }
    let secs: u64 = merge_intervals(intervals).iter().map(|(a, b)| b - a).sum();
    let bucket_secs = bucket_min.saturating_mul(60);
    let buckets = secs.saturating_add(bucket_secs / 2) / bucket_secs;
    buckets.saturating_mul(bucket_min)
}

/// Метка «N ч NN мин».
pub fn human_minutes(minutes: u64) -> String {
    format!("{} ч {} мин", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(h: u64) -> u64 {
        h * 3600
    }

    #[test]
    fn complexity_weight_scales_with_level() {
        assert_eq!(complexity_weight(Some(0)), 1.0);
        assert_eq!(complexity_weight(Some(2)), 2.0);
        assert_eq!(complexity_weight(Some(5)), 3.5);
        assert_eq!(complexity_weight(Some(10)), 6.0);
        assert_eq!(complexity_weight(None), 1.0);
    }

    #[test]
    fn model_key_normalizes() {
        assert_eq!(model_key("MiniMax-M3.1"), "minimax-m3-1");
        assert_eq!(
            model_key("nvidia/nvidia/nemotron-3"),
            "nvidia-nvidia-nemotron-3"
        );
        assert_eq!(model_key("  "), "");
    }

    #[test]
    fn aggregate_pairs_splits_by_agent_and_model() {
        let s = vec![
            Sample::code(ts(1), "a", "m1", 100, 2.0),
            Sample::code(ts(1), "a", "m1", 50, 1.0),
            Sample::wip(ts(2), "a", "m1", 30),
            Sample::thinking(ts(2), "a", "m1", 500, 2000),
            Sample::code(ts(2), "b", "m2", 10, 1.0),
        ];
        let pairs = aggregate_pairs(&s);
        assert_eq!(pairs.len(), 2);
        let a = pairs.iter().find(|p| p.agent == "a").expect("a");
        assert_eq!(a.loc, 150);
        assert_eq!(a.wip_loc, 30);
        assert_eq!(a.weighted_loc, 280.0); // 100*2 + 50*1 + 30*1
        assert_eq!(a.thinking, 500);
        assert_eq!(a.total_tokens, 2000);
    }

    #[test]
    fn efficiency_and_thinking_share() {
        let p = PairStats {
            agent: "a".into(),
            model: "m".into(),
            loc: 100,
            wip_loc: 0,
            weighted_loc: 200.0,
            thinking: 250,
            total_tokens: 1000,
        };
        assert_eq!(p.efficiency(), 200.0); // 200 LOC / 1000 токенов
        assert_eq!(p.thinking_share(), 0.25);
        let empty = PairStats::default();
        assert_eq!(empty.efficiency(), 0.0);
        assert_eq!(empty.speed_per_hour(0), 0.0);
    }

    #[test]
    fn speed_scales_with_work_time() {
        let p = PairStats {
            weighted_loc: 600.0,
            ..Default::default()
        };
        assert_eq!(p.speed_per_hour(60), 600.0);
        assert_eq!(p.speed_per_hour(120), 300.0);
    }

    #[test]
    fn group_by_model_skips_unidentified_agents() {
        let pairs = vec![
            PairStats {
                agent: "a".into(),
                model: "m1".into(),
                weighted_loc: 100.0,
                total_tokens: 1000,
                ..Default::default()
            },
            PairStats {
                agent: "b".into(),
                model: "m2".into(),
                weighted_loc: 50.0,
                ..Default::default()
            },
        ];
        let mut declared = BTreeMap::new();
        declared.insert("a".to_string(), "m1".to_string());
        let models = group_by_model(&pairs, &BTreeMap::new(), &declared);
        assert_eq!(
            models.len(),
            1,
            "без идентификации — в статистику по моделям не идём"
        );
        assert_eq!(models[0].model, "m1");
        assert_eq!(models[0].agents, 1);
    }

    #[test]
    fn work_buckets_use_five_minute_granularity() {
        // Два запроса рядом (в одном бакете) и один далеко.
        let act = vec![ts(1) + 10, ts(1) + 60, ts(2)];
        let b = work_buckets(&act, 120, BUCKET_SEC);
        // первый час: запросы покрывают бакеты 00:55 и 01:00 (окно ±2 мин),
        // второй час — 01:55 и 02:00
        assert!(b.contains(&bucket_start(ts(1), BUCKET_SEC)));
        assert!(b.contains(&bucket_start(ts(1) + 60, BUCKET_SEC)));
        assert!(b.contains(&bucket_start(ts(2), BUCKET_SEC)));
        assert_eq!(b.len(), 4, "окно ±2 мин задевает соседний бакет: {b:?}");
    }

    #[test]
    fn intervals_merge_and_clip() {
        let iv = vec![(10, 20), (15, 30), (40, 50)];
        assert_eq!(merge_intervals(&iv), vec![(10, 30), (40, 50)]);
        let clipped = clip_intervals(&iv, 12, 45);
        assert_eq!(clipped, vec![(12, 30), (40, 45)]);
        // 18 + 5 = 23 сек -> 0 минут при бакете 5
        assert_eq!(total_minutes(&clipped, 5), 0);
        assert_eq!(total_minutes(&[(0, 3600)], 5), 60);
    }

    #[test]
    fn bucketize_places_samples() {
        let s = vec![
            Sample::code(61, "a", "m", 10, 1.0),
            Sample::code(3599, "a", "m", 20, 2.0),
            Sample::thinking(120, "a", "m", 5, 50),
        ];
        let b = bucketize(&s, 0, 3600, 300);
        assert_eq!(b.len(), 13); // 0..3600 шагами 300
        assert_eq!(b[0].loc, 10);
        assert_eq!(b[0].thinking, 5);
        assert_eq!(b[0].total_tokens, 50);
        assert_eq!(b[11].loc, 20);
        assert_eq!(b[11].weighted_loc, 40.0);
    }

    #[test]
    fn total_minutes_rounds_to_nearest_bucket() {
        // одиночный запрос: окно ±3 мин = 6 мин -> 5 мин (и вниз, и к ближайшему)
        let iv = vec![(1000, 1360)];
        assert_eq!(total_minutes(&iv, 5), 5);
        assert_eq!(total_minutes_rounded(&iv, 5), 5);
        // короткая сессия 3 мин: вниз — 0, к ближайшему — 5
        let short = vec![(1000, 1180)];
        assert_eq!(
            total_minutes(&short, 5),
            0,
            "вниз — обнуляет короткую сессию"
        );
        assert_eq!(total_minutes_rounded(&short, 5), 5);
        // 12 минут -> 10 (до 12.5) и 15 (после)
        assert_eq!(total_minutes_rounded(&[(0, 720)], 5), 10);
        assert_eq!(total_minutes_rounded(&[(0, 780)], 5), 15);
    }

    #[test]
    fn human_minutes_formats() {
        assert_eq!(human_minutes(0), "0 ч 0 мин");
        assert_eq!(human_minutes(65), "1 ч 5 мин");
        assert_eq!(human_minutes(120), "2 ч 0 мин");
    }
}
