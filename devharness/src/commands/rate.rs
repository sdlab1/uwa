//! `devh rate` — учёт запусков и гейт нагрузки.
//!
//! ## Зачем
//!
//! Тяжёлые прогоны (`devh test`, `devh mutate`) должны идти по отмашке
//   ведущего и на спокойной машине. `devh rate` — единый источник правды
//   о нагрузке: когда можно запускать, а когда — подождать.
//!
//! ## Подкоманды
//!
//! | подкоманда | что делает |
//! | --- | --- |
//! | `devh rate status` | мгновенный снимок: запуски за последний час, средняя длительность, CPU, рекомендация |
//! | `devh rate check` | гейт: rc=0 если можно запускать (CPU<=80%, нет запусков <15 мин назад), rc=2 если ждать |
//! | `devh rate limit` | показать/установить лимиты (build 15 мин, аудит 2 ч, CPU 80%) |
//! | `devh rate reset` | обнулить историю запусков (ручной инструмент) |
//!
//! ## Хранение
//!
//! История в `.harness/rate/history.jsonl` (append-only, одна строка на запуск):
//! ```json
//! {"ts":1792757359,"kind":"build","duration":845,"cpu_avg":72,"ok":true}
//! {"ts":1792757400,"kind":"mutate","duration":7200,"cpu_avg":85,"ok":false}
//! ```
//!
//! Лимиты в конфиге (`.harness/config` + env HARNESS_*):
//! - `RATE_BUILD_MAX_DURATION=900` (15 мин)
//! - `RATE_MUTATE_MAX_DURATION=7200` (2 ч)
//! - `RATE_CPU_MAX_PCT=80`
//! - `RATE_HISTORY_DAYS=7` (сколько дней хранить историю)

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::types::RunConfig;
use crate::shell::store::Store;

/// Путь к файлу истории.
fn history_path(store: &Store) -> PathBuf {
    store.dir.join("rate").join("history.jsonl")
}

/// Запись истории одного запуска.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    /// Unix timestamp.
    ts: u64,
    /// "build" | "test" | "mutate" | "audit".
    kind: String,
    /// Длительность в секундах.
    duration: u64,
    /// Средний CPU% за время запуска.
    cpu_avg: u32,
    /// Успешность (rc=0).
    ok: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Записать запись о завершённом запуске (append-only).
pub fn record_run(
    store: &Store,
    kind: &str,
    duration_secs: u64,
    cpu_avg: u32,
    ok: bool,
) -> anyhow::Result<()> {
    let path = history_path(store);
    fs::create_dir_all(path.parent().unwrap_or_else(|| unreachable!()))?;
    let rec = RunRecord {
        ts: now_secs(),
        kind: kind.to_string(),
        duration: duration_secs,
        cpu_avg,
        ok,
    };
    let line = serde_json::to_string(&rec).unwrap_or_default();
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

/// Статистика по истории.
#[derive(Debug, Clone)]
struct RateStats {
    runs_last_hour: u32,
    builds_last_hour: u32,
    mutates_last_hour: u32,
    avg_build_duration: u64,
    avg_mutate_duration: u64,
    cpu_avg_recent: u32,
    last_run_ts: Option<u64>,
}

fn load_stats(store: &Store) -> RateStats {
    let path = history_path(store);
    let text = fs::read_to_string(&path).unwrap_or_default();
    let now = now_secs();
    let hour_ago = now.saturating_sub(3600);

    let mut runs_last_hour = 0u32;
    let mut builds_last_hour = 0u32;
    let mut mutates_last_hour = 0u32;
    let mut build_durations: Vec<u64> = Vec::new();
    let mut mutate_durations: Vec<u64> = Vec::new();
    let mut cpu_vals: Vec<u32> = Vec::new();
    let mut last_run_ts = None;

    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<RunRecord>(line) else {
            continue;
        };
        if rec.ts >= hour_ago {
            runs_last_hour = runs_last_hour.saturating_add(1);
            match rec.kind.as_str() {
                "build" => builds_last_hour = builds_last_hour.saturating_add(1),
                "mutate" => mutates_last_hour = mutates_last_hour.saturating_add(1),
                _ => {}
            }
        }
        if rec.kind == "build" {
            build_durations.push(rec.duration);
        } else if rec.kind == "mutate" {
            mutate_durations.push(rec.duration);
        }
        cpu_vals.push(rec.cpu_avg);
        last_run_ts = Some(rec.ts.max(last_run_ts.unwrap_or(0)));
    }

    let avg_build = if build_durations.is_empty() {
        0
    } else {
        build_durations.iter().sum::<u64>() / build_durations.len() as u64
    };
    let avg_mutate = if mutate_durations.is_empty() {
        0
    } else {
        mutate_durations.iter().sum::<u64>() / mutate_durations.len() as u64
    };
    let cpu_avg = if cpu_vals.is_empty() {
        0
    } else {
        cpu_vals.iter().sum::<u32>() / cpu_vals.len() as u32
    };

    RateStats {
        runs_last_hour,
        builds_last_hour,
        mutates_last_hour,
        avg_build_duration: avg_build,
        avg_mutate_duration: avg_mutate,
        cpu_avg_recent: cpu_avg,
        last_run_ts,
    }
}

fn print_stats(stats: &RateStats, cfg: &RunConfig) {
    println!("=== devh rate status ===");
    println!(
        "История: {}",
        history_path(&Store::new(Path::new("."))).display()
    );
    println!(
        "Запусков за час: {} (build: {}, mutate: {})",
        stats.runs_last_hour, stats.builds_last_hour, stats.mutates_last_hour
    );
    println!(
        "Среднее: build {:.1} мин, mutate {:.1} мин",
        stats.avg_build_duration as f64 / 60.0,
        stats.avg_mutate_duration as f64 / 60.0
    );
    println!("CPU (среднее за историю): {}%", stats.cpu_avg_recent);

    let max_build = cfg.rate.as_ref().map(|r| r.build_max_secs).unwrap_or(900);
    let max_mutate = cfg.rate.as_ref().map(|r| r.mutate_max_secs).unwrap_or(7200);
    let max_cpu = cfg.rate.as_ref().map(|r| r.cpu_max_pct).unwrap_or(80);

    println!("\nЛимиты:");
    println!(
        "  build:  {} мин (лимит {} мин)",
        max_build / 60,
        max_build / 60
    );
    println!(
        "  mutate: {} мин (лимит {} мин)",
        max_mutate / 60,
        max_mutate / 60
    );
    println!("  CPU:    {}% (лимит {}%)", stats.cpu_avg_recent, max_cpu);

    if let Some(last) = stats.last_run_ts {
        let age = now_secs().saturating_sub(last);
        println!("\nПоследний запуск: {} сек назад", age);
        if age < 900 {
            println!("⚠ Последний запуск < 15 мин назад — рекомендуется подождать");
        }
    } else {
        println!("\nЗапусков нет — можно стартовать");
    }
}

/// Подкоманда `status`.
fn rate_status(root: &Path, cfg: &RunConfig) -> anyhow::Result<()> {
    let store = Store::new(root);
    let stats = load_stats(&store);
    print_stats(&stats, cfg);
    Ok(())
}

/// Подкоманда `check` — гейт нагрузки (Ok = можно, Err = ждать).
fn rate_check(root: &Path, cfg: &RunConfig) -> anyhow::Result<()> {
    let store = Store::new(root);
    let stats = load_stats(&store);
    let now = now_secs();

    let max_cpu = cfg.rate.as_ref().map(|r| r.cpu_max_pct).unwrap_or(80);
    let min_gap = 900u64; // 15 мин между запусками

    let mut issues = Vec::new();

    if stats.cpu_avg_recent > max_cpu {
        issues.push(format!(
            "CPU среднее {}% > лимита {}%",
            stats.cpu_avg_recent, max_cpu
        ));
    }
    if let Some(last) = stats.last_run_ts {
        let gap = now.saturating_sub(last);
        if gap < min_gap {
            issues.push(format!("последний запуск {} сек назад (< 15 мин)", gap));
        }
    }
    if stats.builds_last_hour >= 3 {
        issues.push(format!(
            "build запусков за час: {} (рекомендуется ≤2)",
            stats.builds_last_hour
        ));
    }

    if issues.is_empty() {
        println!("✓ нагрузка OK — можно запускать");
        Ok(())
    } else {
        for i in &issues {
            println!("✗ {}", i);
        }
        println!("\nРекомендуется подождать и повторить devh rate check");
        Err(anyhow::anyhow!("нагрузка не позволяет запускать"))
    }
}

/// Подкоманда `limit` — показать/установить лимиты.
fn rate_limit(root: &Path, args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        // показать текущие
        let cfg = crate::commands::load_config(root).unwrap_or_default();
        let max_build = cfg.rate.as_ref().map(|r| r.build_max_secs).unwrap_or(900);
        let max_mutate = cfg.rate.as_ref().map(|r| r.mutate_max_secs).unwrap_or(7200);
        let max_cpu = cfg.rate.as_ref().map(|r| r.cpu_max_pct).unwrap_or(80);
        let history_days = cfg.rate.as_ref().map(|r| r.history_days).unwrap_or(7);
        println!("Текущие лимиты:");
        println!(
            "  RATE_BUILD_MAX_DURATION={} ({} мин)",
            max_build,
            max_build / 60
        );
        println!(
            "  RATE_MUTATE_MAX_DURATION={} ({} мин)",
            max_mutate,
            max_mutate / 60
        );
        println!("  RATE_CPU_MAX_PCT={}", max_cpu);
        println!("  RATE_HISTORY_DAYS={}", history_days);
        Ok(())
    } else {
        // установить (через env — для демо; реально через devh init / конфиг)
        let mut set = Vec::new();
        for arg in args {
            if let Some((k, v)) = arg.split_once('=') {
                set.push(format!("{}={}", k.to_uppercase(), v));
            }
        }
        if set.is_empty() {
            return Err(anyhow::anyhow!(
                "devh rate limit: формат KEY=VALUE, напр. RATE_CPU_MAX_PCT=70"
            ));
        }
        println!("Установка (требует перезапуск или devh init):");
        for s in set {
            println!("  export HARNESS_{}", s);
        }
        Ok(())
    }
}

/// Подкоманда `reset` — обнулить историю.
fn rate_reset(root: &Path) -> anyhow::Result<()> {
    let store = Store::new(root);
    let path = history_path(&store);
    if path.exists() {
        fs::remove_file(&path)?;
        println!("История удалена: {}", path.display());
    } else {
        println!("История пуста");
    }
    Ok(())
}

/// Точка входа подкоманды `devh rate`.
pub fn rate(root: &Path, sub: &str, args: &[String]) -> anyhow::Result<()> {
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    match sub {
        "status" => rate_status(root, &cfg),
        "check" => rate_check(root, &cfg),
        "limit" => rate_limit(root, args),
        "reset" => rate_reset(root),
        "help" | "--help" | "-h" => {
            println!(
                "devh rate — учёт запусков и гейт нагрузки

  devh rate status          — мгновенный снимок (запуски/CPU/лимиты)
  devh rate check           — гейт: rc=0 можно, rc=2 ждать
  devh rate limit [K=V ...] — показать/установить лимиты
  devh rate reset           — обнулить историю
  devh rate help            — эта справка

Лимиты (конфиг .harness/config + HARNESS_*):
  RATE_BUILD_MAX_DURATION=900     (15 мин)
  RATE_MUTATE_MAX_DURATION=7200   (2 ч)
  RATE_CPU_MAX_PCT=80
  RATE_HISTORY_DAYS=7
"
            );
            Ok(())
        }
        other => {
            println!("devh rate: неизвестная подкоманда «{other}»");
            println!("devh rate help");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rate_record_and_load() {
        let tmp = tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(tmp.path());
        record_run(&store, "build", 100, 50, true).unwrap_or_else(|_| unreachable!());
        record_run(&store, "mutate", 5000, 85, true).unwrap_or_else(|_| unreachable!());
        let stats = load_stats(&store);
        assert_eq!(stats.runs_last_hour, 2);
        assert_eq!(stats.builds_last_hour, 1);
        assert_eq!(stats.mutates_last_hour, 1);
        assert_eq!(stats.avg_build_duration, 100);
        assert_eq!(stats.avg_mutate_duration, 5000);
    }

    #[test]
    fn rate_check_passes_when_ok() {
        let tmp = tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(tmp.path());
        let cfg = RunConfig::default();
        // нет истории — check проходит
        assert!(rate_check(tmp.path(), &cfg).is_ok());
    }

    #[test]
    fn rate_check_fails_on_cpu() {
        let tmp = tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(tmp.path());
        record_run(&store, "build", 100, 95, true).unwrap_or_else(|_| unreachable!());
        let cfg = RunConfig::default();
        assert!(rate_check(tmp.path(), &cfg).is_err());
    }

    #[test]
    fn rate_check_fails_on_recent_run() {
        let tmp = tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(tmp.path());
        record_run(&store, "build", 100, 50, true).unwrap_or_else(|_| unreachable!());
        let cfg = RunConfig::default();
        // запуск только что был — gap < 15 мин
        assert!(rate_check(tmp.path(), &cfg).is_err());
    }
}
