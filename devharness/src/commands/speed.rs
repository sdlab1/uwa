//! devh orch speed — единица скорости/эффективности агента.
//!
//! Отвечает на вопрос «сколько работы агент делает в единицу времени и
//! сколько это стоит по токенам». Ключевые правила честности:
//! * LOC — факт diff'а из git; код и WIP (невлитые чекпоинты) считаются
//!   ОТДЕЛЬНО;
//! * вес LOC = сложность задачи (C0 → 1.0 … C10 → 6.0): простой код не равен
//!   сложному;
//! * «размышления» (output + reasoning) — отдельно от входного контекста;
//! * рабочее время = интервалы запросов к модели (5-минутная точность), НЕ
//!   время жизни процесса: выключенная машина в счёт не идёт;
//! * в статистику по моделям попадают только агенты с идентификацией модели.
//!
//! Окна: за час, за 24 часа, за неделю (по дням, по часам, по дням недели).

use crate::core::throughput::{clip_intervals, total_minutes_rounded, WorkKind, BUCKET_MIN};
use crate::core::throughput_collect::{self, Collected};
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// Строка отчёта по агенту.
#[derive(Debug, Clone, Default)]
struct Row {
    agent: String,
    model: String,
    /// LOC кода (без WIP).
    loc: u64,
    /// LOC WIP (невлито).
    wip: u64,
    /// Взвешенный LOC (код + WIP).
    weighted: f64,
    /// «Размышления» (output + reasoning).
    thinking: u64,
    /// Все токены.
    tokens: u64,
    /// Реальное рабочее время (минуты).
    minutes: u64,
}

impl Row {
    /// Скорость: взвешенный LOC в час.
    fn speed(&self) -> f64 {
        if self.minutes == 0 {
            return 0.0;
        }
        self.weighted * 60.0 / self.minutes as f64
    }
    /// Эффективность: взвешенный LOC на 1000 токенов.
    fn efficiency(&self) -> f64 {
        if self.tokens == 0 {
            return 0.0;
        }
        self.weighted * 1000.0 / self.tokens as f64
    }
    /// Доля размышлений, %.
    fn think_share(&self) -> f64 {
        if self.tokens == 0 {
            return 0.0;
        }
        self.thinking as f64 / self.tokens as f64 * 100.0
    }
}

/// Агрегат за один bucket (день / час / день недели).
#[derive(Debug, Clone, Default)]
struct Agg {
    label: String,
    loc: u64,
    wip: u64,
    weighted: f64,
    thinking: u64,
    tokens: u64,
    minutes: u64,
}

impl Agg {
    fn speed(&self) -> f64 {
        if self.minutes == 0 {
            return 0.0;
        }
        self.weighted * 60.0 / self.minutes as f64
    }
}

fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dt_of(ts: u64) -> DateTime<Local> {
    Local
        .timestamp_opt(ts as i64, 0)
        .earliest()
        .unwrap_or_else(|| {
            Local
                .timestamp_opt(0, 0)
                .earliest()
                .unwrap_or_else(Local::now)
        })
}

fn day_start(ts: u64) -> u64 {
    let dt = dt_of(ts);
    let naive = dt
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| dt.naive_local());
    naive
        .and_local_timezone(Local)
        .earliest()
        .map(|d| d.timestamp().max(0) as u64)
        .unwrap_or(ts)
}

fn next_day(ts: u64) -> u64 {
    let dt = dt_of(ts);
    let naive = dt
        .date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap_or_else(|| dt.naive_local());
    naive
        .and_local_timezone(Local)
        .earliest()
        .map(|d| d.timestamp().max(0) as u64)
        .unwrap_or_else(|| ts.saturating_add(DAY))
}

/// Рабочее время агента в окне [from, to] (минуты, округление до 5 мин).
///
/// Источники активности ОБЪЕДИНЯЮТСЯ, а не суммируются:
/// * окна запросов к модели (±3 мин вокруг запроса);
/// * интервалы работы по задачам [взял задачу → сдал/обновил] (потолок
///   MAX_TASK_WORK_SEC = 6 ч на задачу — иначе «забытая» задача съела бы
///   недели работы).
///
/// Объединение нужно, чтобы пересечения не считались дважды. Простой
/// (машина выключена) в счёт не идёт: ни один интервал тогда не активен.
fn work_minutes(activity: &[u64], intervals: &[(u64, u64)], from: u64, to: u64) -> u64 {
    let mut iv: Vec<(u64, u64)> = activity
        .iter()
        .copied()
        .filter(|t| *t >= from && *t <= to)
        .map(|t| {
            (
                t.saturating_sub(throughput_collect::ACTIVITY_WINDOW_SEC),
                t.saturating_add(throughput_collect::ACTIVITY_WINDOW_SEC),
            )
        })
        .collect();
    for (a, b) in intervals {
        if *b <= from || *a >= to {
            continue;
        }
        iv.push((*a, *b));
    }
    let clipped = clip_intervals(&iv, from, to);
    total_minutes_rounded(&clipped, BUCKET_MIN)
}

/// Строки по агентам за окно [from, to].
fn rows_for(col: &Collected, from: u64, to: u64) -> Vec<Row> {
    let mut map: BTreeMap<String, Row> = BTreeMap::new();
    for s in &col.samples {
        if s.ts < from || s.ts > to {
            continue;
        }
        let e = map.entry(s.agent.clone()).or_insert_with(|| Row {
            agent: s.agent.clone(),
            model: col
                .declared_model
                .get(&s.agent)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        });
        match s.kind {
            WorkKind::Code => {
                e.loc = e.loc.saturating_add(s.loc);
                e.weighted += s.weighted_loc();
            }
            WorkKind::Wip => {
                e.wip = e.wip.saturating_add(s.loc);
                e.weighted += s.weighted_loc();
            }
            WorkKind::Thinking => {
                e.thinking = e.thinking.saturating_add(s.thinking);
                e.tokens = e.tokens.saturating_add(s.total_tokens);
            }
        }
    }
    for (agent, row) in map.iter_mut() {
        if let Some(m) = col.observed_model.get(agent) {
            if !m.trim().is_empty() {
                row.model = m.clone();
            }
        }
    }
    let empty_iv: Vec<(u64, u64)> = Vec::new();
    let mut agents: BTreeSet<String> = BTreeSet::new();
    agents.extend(col.activity.keys().cloned());
    agents.extend(col.work_intervals.keys().cloned());
    for agent in agents {
        let activity: &[u64] = col
            .activity
            .get(&agent)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let intervals: &[(u64, u64)] = col.work_intervals.get(&agent).unwrap_or(&empty_iv);
        let minutes = work_minutes(activity, intervals, from, to);
        match map.get_mut(&agent) {
            Some(row) => row.minutes = minutes,
            None if minutes > 0 => {
                map.insert(
                    agent.clone(),
                    Row {
                        agent: agent.clone(),
                        model: col
                            .observed_model
                            .get(&agent)
                            .cloned()
                            .or_else(|| col.declared_model.get(&agent).cloned())
                            .unwrap_or_default(),
                        minutes,
                        ..Default::default()
                    },
                );
            }
            None => {}
        }
    }
    let mut rows: Vec<Row> = map.into_values().collect();
    rows.sort_by(|a, b| {
        b.weighted
            .partial_cmp(&a.weighted)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    rows
}

fn sum_rows(rows: &[Row]) -> Agg {
    let mut agg = Agg::default();
    for r in rows {
        agg.loc = agg.loc.saturating_add(r.loc);
        agg.wip = agg.wip.saturating_add(r.wip);
        agg.weighted += r.weighted;
        agg.thinking = agg.thinking.saturating_add(r.thinking);
        agg.tokens = agg.tokens.saturating_add(r.tokens);
        agg.minutes = agg.minutes.saturating_add(r.minutes);
    }
    agg
}

/// Агрегаты по дням (локальное время).
fn daily_aggs(col: &Collected, from: u64, to: u64) -> Vec<Agg> {
    let mut out = Vec::new();
    let mut cursor = day_start(from);
    while cursor <= to {
        let end = next_day(cursor).saturating_sub(1);
        let rows = rows_for(col, cursor, end);
        let mut agg = sum_rows(&rows);
        agg.label = dt_of(cursor).format("%d.%m").to_string();
        out.push(agg);
        let next = next_day(cursor);
        if next <= cursor {
            break;
        }
        cursor = next;
    }
    out
}

/// Агрегаты по часам суток за всё окно (час → сумма по всем дням).
fn hourly_aggs(col: &Collected, from: u64, to: u64) -> Vec<Agg> {
    let mut buckets: Vec<Agg> = (0..24)
        .map(|h| Agg {
            label: format!("{h:02}"),
            ..Default::default()
        })
        .collect();
    let mut cursor = from - from % HOUR;
    while cursor <= to {
        let end = cursor.saturating_add(HOUR).saturating_sub(1);
        let rows = rows_for(col, cursor, end);
        let hour = dt_of(cursor).hour() as usize;
        let slot = &mut buckets[hour.min(23)];
        slot.loc = slot.loc.saturating_add(sum_rows(&rows).loc);
        slot.wip = slot.wip.saturating_add(sum_rows(&rows).wip);
        slot.weighted += sum_rows(&rows).weighted;
        slot.thinking = slot.thinking.saturating_add(sum_rows(&rows).thinking);
        slot.tokens = slot.tokens.saturating_add(sum_rows(&rows).tokens);
        slot.minutes = slot.minutes.saturating_add(sum_rows(&rows).minutes);
        cursor = cursor.saturating_add(HOUR);
    }
    buckets
}

/// Агрегаты по дням недели (Пн..Вс) за всё окно.
fn weekday_aggs(col: &Collected, from: u64, to: u64) -> Vec<Agg> {
    const NAMES: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
    let mut buckets: Vec<Agg> = NAMES
        .iter()
        .map(|n| Agg {
            label: (*n).to_string(),
            ..Default::default()
        })
        .collect();
    let mut cursor = day_start(from);
    while cursor <= to {
        let end = next_day(cursor).saturating_sub(1);
        let rows = rows_for(col, cursor, end);
        let idx = dt_of(cursor).weekday().num_days_from_monday() as usize;
        let slot = &mut buckets[idx.min(6)];
        let s = sum_rows(&rows);
        slot.loc = slot.loc.saturating_add(s.loc);
        slot.wip = slot.wip.saturating_add(s.wip);
        slot.weighted += s.weighted;
        slot.thinking = slot.thinking.saturating_add(s.thinking);
        slot.tokens = slot.tokens.saturating_add(s.tokens);
        slot.minutes = slot.minutes.saturating_add(s.minutes);
        let next = next_day(cursor);
        if next <= cursor {
            break;
        }
        cursor = next;
    }
    buckets
}

/// Агрегаты по моделям (только агенты с идентификацией).
///
/// Модель агента — «эффективная»: наблюдаемая в рантайме (журнал запросов),
/// а если её нет — объявленная в реестре. Если реестр устарел, работа агента
/// ушла бы в чужую модель; расхождение показывается отдельным предупреждением
/// (`model_mismatches`), чтобы ведущий поправил идентификацию.
/// Агент без идентификации (ни объявленной, ни наблюдаемой модели) в
/// статистику по моделям НЕ идёт.
fn model_aggs(col: &Collected, from: u64, to: u64) -> Vec<(String, Agg, Vec<String>)> {
    let rows = rows_for(col, from, to);
    let mut map: BTreeMap<String, (Agg, Vec<String>)> = BTreeMap::new();
    for r in rows {
        let declared = col
            .declared_model
            .get(&r.agent)
            .cloned()
            .unwrap_or_default();
        let observed = col
            .observed_model
            .get(&r.agent)
            .cloned()
            .unwrap_or_default();
        let effective = if !observed.trim().is_empty() {
            observed
        } else {
            declared
        };
        if effective.trim().is_empty() {
            continue; // без идентификации модели в статистику по моделям не идём
        }
        let key = crate::core::throughput::model_key(&effective);
        let slot = map.entry(key.clone()).or_insert_with(|| {
            (
                Agg {
                    label: key.clone(),
                    ..Default::default()
                },
                Vec::new(),
            )
        });
        slot.0.loc = slot.0.loc.saturating_add(r.loc);
        slot.0.wip = slot.0.wip.saturating_add(r.wip);
        slot.0.weighted += r.weighted;
        slot.0.thinking = slot.0.thinking.saturating_add(r.thinking);
        slot.0.tokens = slot.0.tokens.saturating_add(r.tokens);
        slot.0.minutes = slot.0.minutes.saturating_add(r.minutes);
        if !slot.1.contains(&r.agent) {
            slot.1.push(r.agent.clone());
        }
    }
    map.into_iter()
        .map(|(model, (agg, agents))| (model, agg, agents))
        .collect()
}

/// Расхождения «модель в реестре» vs «модель, наблюдаемая в рантайме».
///
/// Это сигнал качества идентификации: если агент объявил модель, а журнал
/// рантайма показывает другую — идентификация подозрительна, и метрики по
/// этой модели нельзя принимать за чистые.
fn model_mismatches(col: &Collected) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for (agent, declared) in &col.declared_model {
        if declared.trim().is_empty() {
            continue;
        }
        let observed = match col.observed_model.get(agent) {
            Some(o) if !o.trim().is_empty() => o,
            _ => continue,
        };
        let d = crate::core::throughput::model_key(declared);
        let o = crate::core::throughput::model_key(observed);
        if d != o {
            out.push((agent.clone(), declared.clone(), observed.clone()));
        }
    }
    out.sort();
    out
}

fn pad(s: &str, w: usize) -> String {
    let len = s.chars().count();
    if len >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - len))
    }
}

fn fmt_num(v: f64) -> String {
    format!("{v:.1}")
}

fn fmt_k(v: u64) -> String {
    if v >= 1_000_000 {
        format!("{:.1}M", v as f64 / 1_000_000.0)
    } else if v >= 1000 {
        format!("{:.1}k", v as f64 / 1000.0)
    } else {
        v.to_string()
    }
}

fn fmt_time(minutes: u64) -> String {
    format!("{}ч{:02}м", minutes / 60, minutes % 60)
}

/// Точка входа: `devh orch speed [--agent X] [--json] [--hours N]`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut agent_filter: Option<String> = None;
    let mut json = false;
    let mut hours_override: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    agent_filter = Some(v.clone());
                }
            }
            "--hours" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    hours_override = v.parse::<u64>().ok();
                }
            }
            "--json" => json = true,
            other => {
                if let Some(v) = other.strip_prefix("--agent=") {
                    agent_filter = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--hours=") {
                    hours_override = v.parse::<u64>().ok();
                }
            }
        }
        i += 1;
    }

    let now = now_ts();
    let since = now.saturating_sub(WEEK);
    let mut col = throughput_collect::collect(root, since);
    if let Some(agent) = agent_filter.as_ref() {
        filter_agent(&mut col, agent);
    }

    let hour_from = now.saturating_sub(HOUR);
    let day_from = now.saturating_sub(DAY);
    let rows_hour = rows_for(&col, hour_from, now);
    let rows_day = rows_for(&col, day_from, now);
    let rows_week = rows_for(&col, since, now);
    let daily = daily_aggs(&col, since, now);
    let hourly = hourly_aggs(&col, since, now);
    let weekday = weekday_aggs(&col, since, now);
    let models = model_aggs(&col, since, now);

    if json {
        let out = serde_json::json!({
            "agent_filter": agent_filter,
            "generated_at": now,
            "hour": rows_hour.iter().map(row_json).collect::<Vec<_>>(),
            "day": rows_day.iter().map(row_json).collect::<Vec<_>>(),
            "week": rows_week.iter().map(row_json).collect::<Vec<_>>(),
            "week_total": agg_json(&sum_rows(&rows_week)),
            "daily": daily.iter().map(agg_json).collect::<Vec<_>>(),
            "hourly": hourly.iter().map(agg_json).collect::<Vec<_>>(),
            "weekday": weekday.iter().map(agg_json).collect::<Vec<_>>(),
            "models": models.iter().map(|(name, agg, agents)| serde_json::json!({
                "model": name,
                "agents": agents,
                "agg": agg_json(agg),
            })).collect::<Vec<_>>(),
            "sources": col.sources.iter().map(|s| serde_json::json!({
                "name": s.name, "state": s.state, "detail": s.detail
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    print_report(
        &rows_hour,
        &rows_day,
        &rows_week,
        &daily,
        &hourly,
        &weekday,
        &models,
        &col,
        hours_override,
    );
    Ok(())
}

fn filter_agent(col: &mut Collected, agent: &str) {
    let wanted = agent.trim().to_lowercase();
    col.samples
        .retain(|s| s.agent.to_lowercase() == wanted || s.agent.to_lowercase().contains(&wanted));
    let keys: Vec<String> = col.activity.keys().cloned().collect();
    for k in keys {
        if k.to_lowercase() != wanted && !k.to_lowercase().contains(&wanted) {
            col.activity.remove(&k);
        }
    }
    let wkeys: Vec<String> = col.work_intervals.keys().cloned().collect();
    for k in wkeys {
        if k.to_lowercase() != wanted && !k.to_lowercase().contains(&wanted) {
            col.work_intervals.remove(&k);
        }
    }
    col.declared_model
        .retain(|k, _| k.to_lowercase() == wanted || k.to_lowercase().contains(&wanted));
    col.observed_model
        .retain(|k, _| k.to_lowercase() == wanted || k.to_lowercase().contains(&wanted));
}

fn row_json(r: &Row) -> serde_json::Value {
    serde_json::json!({
        "agent": r.agent,
        "model": r.model,
        "loc": r.loc,
        "wip": r.wip,
        "weighted_loc": r.weighted,
        "thinking": r.thinking,
        "tokens": r.tokens,
        "work_minutes": r.minutes,
        "speed_per_hour": r.speed(),
        "efficiency_per_1k": r.efficiency(),
        "thinking_share": r.think_share(),
    })
}

fn agg_json(a: &Agg) -> serde_json::Value {
    serde_json::json!({
        "label": a.label,
        "loc": a.loc,
        "wip": a.wip,
        "weighted_loc": a.weighted,
        "thinking": a.thinking,
        "tokens": a.tokens,
        "work_minutes": a.minutes,
        "speed_per_hour": a.speed(),
    })
}

#[allow(clippy::too_many_arguments)]
fn print_report(
    rows_hour: &[Row],
    rows_day: &[Row],
    rows_week: &[Row],
    daily: &[Agg],
    hourly: &[Agg],
    weekday: &[Agg],
    models: &[(String, Agg, Vec<String>)],
    col: &Collected,
    hours_override: Option<u64>,
) {
    let c = crate::shell::color::colors();
    println!(
        "{}{}=== СКОРОСТЬ / ЭФФЕКТИВНОСТЬ АГЕНТОВ ==={}",
        c.b, c.c, c.z
    );
    println!(
        "{}взвешенный LOC (вес = сложность C) · реальное рабочее время (5-мин бакета) · токены раздельно: контекст / размышления{}",
        c.d, c.z
    );

    let hour_label = match hours_override {
        Some(h) => format!("за последние {h} ч"),
        None => "за час (60 мин)".to_string(),
    };
    print_agents(&format!("{hour_label}"), rows_hour, &c);
    print_agents("за 24 часа", rows_day, &c);
    print_agents("за неделю", rows_week, &c);

    // По дням.
    println!("\n{}{}── за неделю: по дням ──{}", c.b, c.y, c.z);
    println!(
        "{}date       LOC(взв)     WIP     время      LOC/ч      токены  think%{}",
        c.d, c.z
    );
    for a in daily {
        println!(
            "{}   {:>9} {:>9}  {:>7}  {:>9}  {:>9}  {:>5.1}%{}",
            pad(&a.label, 8),
            fmt_num(a.weighted),
            fmt_k(a.wip),
            fmt_time(a.minutes),
            fmt_num(a.speed()),
            fmt_k(a.tokens),
            if a.tokens == 0 {
                0.0
            } else {
                a.thinking as f64 / a.tokens as f64 * 100.0
            },
            c.z
        );
    }

    // По часам суток.
    println!(
        "\n{}{}── за неделю: по часам суток (локальное время) ──{}",
        c.b, c.y, c.z
    );
    println!(
        "{}час     LOC(взв)     WIP     время      LOC/ч      токены{}",
        c.d, c.z
    );
    for a in hourly {
        if a.weighted == 0.0 && a.minutes == 0 && a.tokens == 0 {
            continue;
        }
        println!(
            "{}   {:>9} {:>9}  {:>7}  {:>9}  {:>9}{}",
            a.label,
            fmt_num(a.weighted),
            fmt_k(a.wip),
            fmt_time(a.minutes),
            fmt_num(a.speed()),
            fmt_k(a.tokens),
            c.z
        );
    }

    // По дням недели.
    println!("\n{}{}── по дням недели (за неделю) ──{}", c.b, c.y, c.z);
    println!(
        "{}день     LOC(взв)     WIP     время      LOC/ч      токены{}",
        c.d, c.z
    );
    for a in weekday {
        println!(
            "{}   {:>9} {:>9}  {:>7}  {:>9}  {:>9}{}",
            pad(&a.label, 4),
            fmt_num(a.weighted),
            fmt_k(a.wip),
            fmt_time(a.minutes),
            fmt_num(a.speed()),
            fmt_k(a.tokens),
            c.z
        );
    }

    // По моделям.
    println!(
        "\n{}{}── по моделям (только агенты с идентификацией) ──{}",
        c.b, c.y, c.z
    );
    if models.is_empty() {
        println!(
            "{}нет агентов с идентификацией модели — статистика по моделям пуста{}",
            c.d, c.z
        );
    } else {
        println!(
            "{}модель                        агенты   LOC(взв)     WIP     время      LOC/ч   эфф/1k  think%{}",
            c.d, c.z
        );
        for (name, agg, agents) in models {
            let share = if agg.tokens == 0 {
                0.0
            } else {
                agg.thinking as f64 / agg.tokens as f64 * 100.0
            };
            let eff = if agg.tokens == 0 {
                0.0
            } else {
                agg.weighted * 1000.0 / agg.tokens as f64
            };
            println!(
                "{} {:>5}   {:>9} {:>9}  {:>7}  {:>9}  {:>7}  {:>5.1}%{}",
                pad(name, 28),
                agents.len(),
                fmt_num(agg.weighted),
                fmt_k(agg.wip),
                fmt_time(agg.minutes),
                fmt_num(agg.speed()),
                fmt_num(eff),
                share,
                c.z
            );
            println!("{}    агенты: {}{}", c.d, agents.join(", "), c.z);
        }
    }

    // Расхождение идентификации модели.
    let mism = model_mismatches(col);
    if !mism.is_empty() {
        println!(
            "\n{}{}── ⚠ расхождение идентификации модели (реестр ≠ рантайм) ──{}",
            c.b, c.r, c.z
        );
        for (agent, declared, observed) in &mism {
            println!(
                "{}  {}: в реестре «{}», в рантайме «{}»{}",
                c.r, agent, declared, observed, c.z
            );
        }
    }

    // Источники.
    println!("\n{}{}── источники данных ──{}", c.b, c.y, c.z);
    for s in &col.sources {
        let mark = if s.state == "ok" { "✓" } else { "·" };
        println!("{} {} {} — {}{}", c.g, mark, s.name, s.detail, c.z);
    }
    println!(
        "{}LOC — факт diff'а из git; время — объединение запросов к модели и интервалов работы по задачам (простой не считается); модели — только с идентификацией.{}",
        c.d, c.z
    );
}

fn print_agents(title: &str, rows: &[Row], c: &crate::shell::color::Colors) {
    println!("\n{}{}── {title} ──{}", c.b, c.y, c.z);
    println!(
        "{}агент            модель                       LOC(взв)     WIP    размышл     токены     время      LOC/ч   эфф/1k  think%{}",
        c.d, c.z
    );
    if rows.is_empty() {
        println!("{}нет данных{}", c.d, c.z);
        return;
    }
    for r in rows {
        println!(
            "{} {:>7}  {:>9} {:>9}  {:>9} {:>9}  {:>9} {:>9}  {:>7}  {:>5.1}%{}",
            pad(&r.agent, 16),
            pad(&short_model(&r.model), 27),
            fmt_num(r.weighted),
            fmt_k(r.wip),
            fmt_k(r.thinking),
            fmt_k(r.tokens),
            fmt_time(r.minutes),
            fmt_num(r.speed()),
            fmt_num(r.efficiency()),
            r.think_share(),
            c.z
        );
    }
}

fn short_model(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        return "— (идентификации нет)".to_string();
    }
    if m.chars().count() <= 27 {
        return m.to_string();
    }
    let tail: String = m.chars().skip(m.chars().count() - 24).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(
        ts: u64,
        agent: &str,
        kind: WorkKind,
        loc: u64,
        weight: f64,
    ) -> crate::core::throughput::Sample {
        crate::core::throughput::Sample {
            ts,
            agent: agent.to_string(),
            model: "m".to_string(),
            kind,
            loc,
            weight,
            thinking: 0,
            total_tokens: 0,
        }
    }

    #[test]
    fn rows_for_filters_window() {
        let now = 1_000_000u64;
        let mut col = Collected::default();
        col.samples
            .push(sample(now - 10, "a", WorkKind::Code, 100, 2.0));
        col.samples
            .push(sample(now - 100_000, "a", WorkKind::Code, 500, 2.0));
        let rows = rows_for(&col, now - 3600, now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].loc, 100);
        assert_eq!(rows[0].weighted, 200.0);
    }

    #[test]
    fn work_minutes_uses_activity_not_process_life() {
        let act = vec![1000u64, 1060, 1120];
        // один час активности -> 5-минутные бакеты
        let m = work_minutes(&act, &[], 0, 7200);
        assert!(m > 0 && m % 5 == 0, "минуты кратны 5: {m}");
        assert!(m <= 15, "не больше суммы окон: {m}");
        // без активности — ноль
        assert_eq!(work_minutes(&[], &[], 0, 7200), 0);
    }

    #[test]
    fn work_minutes_unions_task_intervals_without_double_count() {
        // Запрос к модели внутри интервала задачи — время не удваивается.
        let act = vec![3600u64];
        let iv = vec![(0u64, 7200u64)];
        let m = work_minutes(&act, &iv, 0, 7200);
        assert_eq!(m, 120, "2 часа работы по задаче, а не 125: {m}");
        // Только интервал задачи (запросов не было) — тоже рабочее время.
        assert_eq!(work_minutes(&[], &iv, 0, 7200), 120);
        // Интервал задачи обрезается окном отчёта.
        assert_eq!(work_minutes(&[], &iv, 0, 3600), 60);
    }

    #[test]
    fn model_mismatches_detect_declared_vs_observed() {
        let mut col = Collected::default();
        col.declared_model
            .insert("a".to_string(), "minimax-m3.1".to_string());
        col.observed_model
            .insert("a".to_string(), "minimax-m3.1".to_string());
        col.declared_model
            .insert("b".to_string(), "kilo".to_string());
        col.observed_model
            .insert("b".to_string(), "deepseek-v3".to_string());
        col.declared_model
            .insert("c".to_string(), "laguna-s".to_string());
        let mism = model_mismatches(&col);
        assert_eq!(mism.len(), 1, "только расхождение: {mism:?}");
        assert_eq!(mism[0].0, "b".to_string());
    }

    #[test]
    fn model_aggs_skip_unidentified_agents() {
        let mut col = Collected::default();
        col.declared_model
            .insert("a".to_string(), "minimax-m3.1".to_string());
        col.samples.push(sample(100, "a", WorkKind::Code, 100, 1.0));
        col.samples.push(sample(100, "b", WorkKind::Code, 100, 1.0));
        let models = model_aggs(&col, 0, 1000);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].0, "minimax-m3-1");
        assert_eq!(models[0].2, vec!["a".to_string()]);
    }

    #[test]
    fn model_aggs_prefer_observed_model_over_stale_registry() {
        let mut col = Collected::default();
        col.declared_model
            .insert("a".to_string(), "minimax-m3.1".to_string());
        col.observed_model
            .insert("a".to_string(), "stepfun-ai/step-5-preview".to_string());
        col.samples.push(sample(100, "a", WorkKind::Code, 100, 1.0));
        let models = model_aggs(&col, 0, 1000);
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].0, "stepfun-ai-step-5-preview",
            "работа не уходит в чужую модель"
        );
        // ... и расхождение видно отдельно.
        assert_eq!(model_mismatches(&col).len(), 1);
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_k(999), "999");
        assert_eq!(fmt_k(1500), "1.5k");
        assert_eq!(fmt_k(2_000_000), "2.0M");
        assert_eq!(fmt_time(65), "1ч05м");
        assert_eq!(fmt_num(12.34), "12.3");
    }
}
