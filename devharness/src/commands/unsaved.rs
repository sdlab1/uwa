//! `devh orch unsaved` — сторож несохранённой работы: печать + тревога.
//!
//! Печатает worktree'ы с незакоммиченными правками (объём, возраст
//! последнего коммита) и тревогу (`HARNESS_COLOR=always` — цветная рамка),
//! когда объём >= `HARNESS_UNSAVED_MIN_LOC` И возраст >=
//! `HARNESS_UNSAVED_MAX_MIN`. Тревога уходит письмом ведущему и tech-lead'ам
//! не чаще `HARNESS_UNSAVED_ALARM_EVERY_MIN` на worktree.
//!
//! Авто-проверка (без писем): `auto_check` вызывается из `devh next` —
//! оракул, который агент запускает каждым ходом; печать идёт в stderr,
//! чтобы не ломать контракт `devh next` («ровно одна команда» в stdout).
//!
//! Read-only: модуль ничего не пишет в репозиторий, кроме файла паузы
//! тревог (`.harness/orch/unsaved_state.json`).

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::unsaved_watch::{self, Candidate, Limits};
use crate::shell::color::colors;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Файл паузы тревог — в ГЛАВНОМ репо: все worktree делят одну паузу
/// (в копии `.harness` своего worktree сторож считал бы тревогу первой
/// каждый раз и слал бы письма на каждый ход).
fn state_path(root: &Path) -> std::path::PathBuf {
    main_root(root).join(".harness").join("orch").join("unsaved_state.json")
}

/// Корень главного репо из любого worktree (фоллбэк — сам root).
fn main_root(root: &Path) -> std::path::PathBuf {
    crate::shell::git::main_root_from_cwd(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Пауза тревог: читаем метки «последняя тревога по worktree».
fn last_alarms(root: &Path) -> BTreeMap<String, u64> {
    let text = std::fs::read_to_string(state_path(root)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

fn remember_alarm(root: &Path, path: &str, now: u64) {
    let mut map = last_alarms(root);
    map.insert(path.to_string(), now);
    if let Some(p) = state_path(root).parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(
        state_path(root),
        serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string()),
    );
}

/// Email-адреса получателей тревоги: ведущий + все tech-lead'ы (без себя).
fn alarm_recipients(root: &Path) -> Vec<String> {
    let me = crate::core::identity::current().name;
    let text = std::fs::read_to_string(main_root(root).join(".harness").join("orch").join("registry.json"))
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let mut out: Vec<String> = Vec::new();
    if let Some(lead) = v.get("lead").and_then(|l| l.as_str()) {
        if !lead.is_empty() {
            out.push(lead.to_string());
        }
    }
    if let Some(agents) = v.get("agents").and_then(|a| a.as_array()) {
        for a in agents {
            let role = a.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if role == "tech_lead" && !name.is_empty() && !out.contains(&name.to_string()) {
                out.push(name.to_string());
            }
        }
    }
    out.retain(|a| a != &me);
    out
}

fn warn_box(lines: &[String]) {
    let c = colors();
    let r = c.r;
    let y = c.y;
    let z = c.z;
    let width = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(20)
        .max(20);
    eprintln!("{r}+{:-^width$}+{z}", " СТОРОЖ НЕСОХРАНЁННОЙ РАБОТЫ ");
    for l in lines {
        eprintln!("{r}|{z} {y}{l}{z}");
    }
    eprintln!("{r}+{:-^width$}+{z}", "");
}

/// Печать таблицы кандидатов (всегда печатаем: объём + возраст).
fn print_table(cands: &[Candidate], now: u64) {
    let c = colors();
    let g = c.g;
    let y = c.y;
    let b = c.b;
    let z = c.z;
    if cands.is_empty() {
        println!("{g}несохранённой работы нет: все worktree'ы чисты{z}");
        return;
    }
    println!("{b}worktree{z}                          {b}LOC{z}   {b}последний коммит{z}");
    for cand in cands {
        let age = unsaved_watch::age_min(now, cand.last_commit_at);
        let age_txt = if cand.last_commit_at.is_none() {
            "НЕТ КОММИТОВ".to_string()
        } else {
            format!("{age} мин назад")
        };
        println!(
            "{y}{wt:<32}{z} {loc:>6}   {age_txt}",
            wt = cand.worktree.display(),
            loc = cand.loc
        );
    }
}

/// Отправка тревог (rate-limit по worktree). Возвращает число отправленных.
fn send_alarms(root: &Path, alarms: &[(Candidate, String)], limits: &Limits, now: u64) -> usize {
    let mut sent = 0usize;
    let root = main_root(root);
    let recipients = alarm_recipients(&root);
    if recipients.is_empty() {
        return 0;
    }
    let state = last_alarms(&root);
    for (cand, reason) in alarms {
        let key = cand.worktree.display().to_string();
        let quiet_until = state.get(&key).copied().unwrap_or(0);
        if now < quiet_until.saturating_add(limits.alarm_every_min.saturating_mul(60)) {
            continue;
        }
        let body = format!(
            "СТОРОЖ НЕСОХРАНЁННОЙ РАБОТЫ: {reason}\n\
             Порог: LOC >= {}, возраст >= {} мин. Забери работу: devh orch unsaved\n\
             Пауза тревог по этому worktree: {} мин.",
            limits.min_loc, limits.max_age_min, limits.alarm_every_min
        );
        for to in &recipients {
            let _ = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                to.clone(),
                "--subject".to_string(),
                "ТРЕВОГА: несохранённая работа (LOC без коммита)".to_string(),
                "--body".to_string(),
                body.clone(),
            ]);
        }
        remember_alarm(&root, &key, now);
        sent = sent.saturating_add(1);
    }
    sent
}

/// Точка входа `devh orch unsaved`.
pub fn run(root: &Path, _args: &[String]) -> anyhow::Result<()> {
    let limits = unsaved_watch::limits_from_env();
    let now = unix_now();
    let root_buf = root.to_path_buf();
    let cands = unsaved_watch::scan(&root_buf, now);
    print_table(&cands, now);
    let alarms = unsaved_watch::alarms(&root_buf, &limits, now);
    if alarms.is_empty() {
        println!(
            "тревоги нет (порог LOC>= {}, возраст>= {} мин)",
            limits.min_loc, limits.max_age_min
        );
        return Ok(());
    }
    let lines: Vec<String> = alarms.iter().map(|(_, r)| r.clone()).collect();
    warn_box(&lines);
    let sent = send_alarms(root, &alarms, &limits, now);
    println!(
        "тревога отправлена: {} worktree (получатели: ведущий + tech-lead'ы), пауза {} мин",
        sent, limits.alarm_every_min
    );
    Ok(())
}

/// Авто-проверка из `devh next`: только печать в stderr, без писем.
pub fn auto_check(root: &Path) {
    let limits = unsaved_watch::limits_from_env();
    let now = unix_now();
    let root_buf = root.to_path_buf();
    let alarms = unsaved_watch::alarms(&root_buf, &limits, now);
    if alarms.is_empty() {
        return;
    }
    let lines: Vec<String> = alarms.iter().map(|(_, r)| r.clone()).collect();
    warn_box(&lines);
    eprintln!(
        "забери работу в коммит/WIP: devh orch unsaved (порог LOC>= {}, возраст>= {} мин)",
        limits.min_loc, limits.max_age_min
    );
}
