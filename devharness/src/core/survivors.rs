//! P8-1: третья петля обратной связи — мутационные выжившие → очередь
//! наблюдений (`.harness/observations.md`).
//!
//! Почему здесь: гейт УЖЕ производит данные о слепых зонах — cargo-mutants
//! пишет `mutants.out/outcomes.json` с полными координатами каждого мутанта
//! (файл, строка, жанр, текст замены). Не хватало только мостка в очередь,
//! формат которой (`<файл>:<строка> | <подозрение> | red-тест: <идея>`)
//! `tickets.rs::import` уже понимает механически.
//!
//! Делает хук `collect_after_run` (вызывается ПОСЛЕ каждого прогона гейта —
//! per-file режим перезаписывает outcomes.json каждым запуском):
//! 1. парсит outcomes.json (прощающий: доступ по имени, unknown summary → WARN);
//! 2. MissedMutant'ы (и ТОЛЬКО они: таймаут = убит, unviable = не про тесты)
//!    → строки очереди с авто-идеей red-теста по жанру (§4 таксономии);
//! 3. seen-дедуп через `.harness/survivors-seen.json` (SHA256 file:line:col:mutation):
//!    «вечный» выживший в очереди одно строкой, повтор — только в killmap/survivors.log;
//! 4. каждый свежий выживший — строкой в `.harness/survivors.log` (дата | гейт |
//!    файл:строка | genre | мутация) — материал для дистилляции классов (P8-4);
//! 5. `.harness/killmap.json` (§5) — по ВСЕМ исходам прогона, включая caught;
//! 6. возвращает строку лога гейта: `survivors: 2 → observations.md (fn-value 1)`.
//!
//! Границы честности (§8 плана): инстанс (координаты) живёт в очереди/тикете и
//! НИКОГДА не идёт в промпт; авто-идея — триггер для человека, не тест; ни
//! killmap, ни очередь не влияют на вердикт гейта и kill-rate.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::killmap::{self, FileDelta};

/// Имя файла отпечатков увиденных выживших (в `.harness/`).
const SEEN_FILE: &str = "survivors-seen.json";
/// Журнал выживших (дата | гейт | файл:строка | genre | мутация) — для P8-4.
const LOG_FILE: &str = "survivors.log";
/// Очередь наблюдений (формат общий с ON-FLY ANALYSIS агентов).
const OBS_FILE: &str = "observations.md";

/// Один исход прогона из outcomes.json (scenario.Mutant + summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// «CaughtMutant» / «MissedMutant» / «TimeoutMutant» / «UnviableMutant».
    pub summary: String,
    /// Файл мутанта (относительный путь, как в дереве гейта).
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// Хвост `name` после «файл:строка:колонка: » — природа поломки.
    pub mutation: String,
    /// Жанр cargo-mutants («FnValue», «BinaryOperator», …).
    pub genre: String,
    /// Полное имя мутанта — идентификатор сценария.
    pub name: String,
}

impl Outcome {
    /// Выживший = MissedMutant и только: таймаут в статистике devh убит
    /// (killed = caught + timeout), unviable — не про тесты.
    pub fn is_survivor(&self) -> bool {
        self.summary == "MissedMutant"
    }

    /// Отпечаток для seen-дедупа (§3 плана: SHA256 file:line:col:mutation).
    pub fn dedup_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(format!("{}:{}:{}:{}", self.file, self.line, self.col, self.mutation).as_bytes());
        format!("{:x}", h.finalize())
    }

    /// Вклад в killmap: killed (ловят таймауты тоже) / missed.
    pub fn delta(&self) -> Option<FileDelta> {
        match self.summary.as_str() {
            "CaughtMutant" | "TimeoutMutant" => Some(FileDelta {
                killed: 1,
                missed: 0,
            }),
            "MissedMutant" => Some(FileDelta {
                killed: 0,
                missed: 1,
            }),
            _ => None,
        }
    }
}

/// «файл:строка:колонка: описание» → (файл, строка, колонка, описание).
/// Прощающий: что не распарсилось — нули/пусто (caller использует поля
/// scenario.Mutant приоритетно, имя — только как фолбэк и для хвоста).
fn split_name(name: &str) -> (String, u32, u32, String) {
    let mut parts = name.splitn(4, ':');
    let file = parts.next().unwrap_or_default().to_string();
    let line = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let col = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let tail = parts.next().unwrap_or_default().trim().to_string();
    (file, line, col, tail)
}

fn as_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Разбор outcomes.json cargo-mutants 27.x. Прощающий (§10 риска): любой
/// объект с `scenario.Mutant.name` даёт исход; неизвестный/отсутствующий
/// summary считается предупреждением (events не теряются молча). Возвращает
/// (исходы, число предупреждений).
pub fn parse_outcomes(text: &str) -> (Vec<Outcome>, usize) {
    let mut warn = 0usize;
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return (Vec::new(), 1);
    };
    let Some(arr) = root.get("outcomes").and_then(Value::as_array) else {
        return (Vec::new(), 1);
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(m) = item.get("scenario").and_then(|s| s.get("Mutant")) else {
            warn = warn.saturating_add(1);
            continue;
        };
        let name = as_str(m, "name").unwrap_or_default();
        if name.is_empty() {
            warn = warn.saturating_add(1);
            continue;
        }
        let (nf, nl, nc, tail) = split_name(name);
        // file/span.start надёжнее разбора имени; фолбэк — голова имени.
        let file = as_str(m, "file")
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or(nf);
        let line = m
            .pointer("/span/start/line")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(nl);
        let col = m
            .pointer("/span/start/column")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(nc);
        let genre = as_str(m, "genre").unwrap_or("unknown").to_string();
        let summary = match as_str(item, "summary") {
            Some(s)
                if matches!(
                    s,
                    "CaughtMutant" | "MissedMutant" | "TimeoutMutant" | "UnviableMutant"
                ) =>
            {
                s.to_string()
            }
            _ => {
                // нет summary или новый вид: один WARN на исход, не паника.
                // В очередь/killmap такой исход не идёт (не мутационный вердикт).
                warn = warn.saturating_add(1);
                continue;
            }
        };
        out.push(Outcome {
            summary,
            file,
            line,
            col,
            mutation: tail,
            genre,
            name: name.to_string(),
        });
    }
    (out, warn)
}

/// Авто-идея red-теста по жанру (§4 таксономии; неизвестный жанр — дефолт
/// «прямой ассерт результата на этой строке»). Идея — триггер для человека
/// в тикете, не готовая строчка ассерта (§8.4).
pub fn auto_idea(genre: &str) -> &'static str {
    match genre {
        "FnValue" => {
            "ассерт ТОЧНОГО значения на этой строке (None/Some(x)/число/строка), \
             не is_some()/is_ok(); граничные значения — отдельным кейсом"
        }
        "BinaryOperator" | "ComparisonOperator" => {
            "кейсы на ОБА плеча условия и на границу (включая равенство)"
        }
        "MatchArmGuard" | "MatchArm" => "отдельный кейс на каждую ветку match; guard — отдельно",
        _ => "прямой ассерт результата на этой строке, без толерантности (жанр новый — уточни)",
    }
}

/// Короткая метка жанра для строки лога (`fn-value 1, binop 1`).
pub fn genre_tag(genre: &str) -> &'static str {
    match genre {
        "FnValue" => "fn-value",
        "BinaryOperator" => "binop",
        "ComparisonOperator" => "cmp",
        "MatchArm" => "match-arm",
        "MatchArmGuard" => "guard",
        _ => "other",
    }
}

/// Строка очереди наблюдений (§3 плана). Обратная совместимость с
/// tickets.rs — по построению: содержит `red-тест:` (обязательное поле
/// очереди), поле 1 помечено `survivor:` (по нему import выбирает текст
/// спеки тикета), red-гейт помечает источник `[red-mutation]`.
pub fn queue_line(o: &Outcome, red_gate: bool) -> String {
    let marker = if red_gate { " [red-mutation]" } else { "" };
    format!(
        "{}:{} | survivor: {} [{}]{} | red-тест: {}\n",
        o.file,
        o.line,
        o.mutation,
        o.genre,
        marker,
        auto_idea(&o.genre)
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct Seen {
    version: u32,
    hashes: Vec<String>,
}

fn load_seen(obs: &Path) -> HashSet<String> {
    std::fs::read_to_string(obs.join(SEEN_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Seen>(&t).ok())
        .filter(|s| s.version == 1)
        .map(|s| s.hashes.into_iter().collect())
        .unwrap_or_default()
}

fn save_seen(obs: &Path, seen: &HashSet<String>) {
    let mut hashes: Vec<String> = seen.iter().cloned().collect();
    hashes.sort(); // детерминированный файл (diff'и между прогонами читаемы)
    let doc = Seen { version: 1, hashes };
    let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
    let p = obs.join(SEEN_FILE);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, &text).is_ok() {
        std::fs::rename(&tmp, &p).ok();
    }
}

fn append_line(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// Сегодня в YYYY-MM-DD (как в merge.rs — без внешних крейтов).
fn today_ymd() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    civil_from_days(now.div_euclid(86_400).wrapping_add(719_468))
}

fn civil_from_days(z: i64) -> String {
    let era = if z >= 0 { z } else { z.wrapping_sub(146_096) } / 146_097;
    let doe = z.wrapping_sub(era.wrapping_mul(146_097));
    let yoe = (doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096))
        / 365;
    let y = yoe.wrapping_add(era.wrapping_mul(400));
    let doy = doe.wrapping_sub(
        365_i64
            .wrapping_mul(yoe)
            .wrapping_add(yoe / 4)
            .wrapping_sub(yoe / 100),
    );
    let mp = (5_i64.wrapping_mul(doy).wrapping_add(2)) / 153;
    let d = doy
        .wrapping_sub(153_i64.wrapping_mul(mp).wrapping_add(2) / 5)
        .wrapping_add(1);
    let m = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    format!("{}-{:02}-{:02}", y.wrapping_add(i64::from(m <= 2)), m, d)
}

/// Хук ПОСЛЕ прогона гейта: разобрать outcomes.json текущего запуска,
/// накормить очередь/seen/log/killmap. Возвращает строку для лога гейта
/// (`survivors: N → observations.md (жанр-счётчики)`) — None, если свежих
/// выживших нет (killmap при этом обновлён, если в файле были исходы).
/// Блокировку очереди (GlobalLock) берёт вызывающий — §8.7 плана.
pub fn collect_after_run(wt: &Path, tool: &str, obs: &Path, gate: &str) -> Option<String> {
    let dir = wt.join(crate::core::early::mutants_out_dir(tool));
    let text = std::fs::read_to_string(dir.join("outcomes.json")).ok()?;
    let (outcomes, warn) = parse_outcomes(&text);
    if warn > 0 {
        eprintln!(
            ".. survivors: {warn} исход(ов) outcomes.json без разбираемого summary — пропущены"
        );
    }
    if outcomes.is_empty() {
        return None;
    }
    // killmap — по всем исходам прогона, включая caught (§5).
    let mut deltas: BTreeMap<String, FileDelta> = BTreeMap::new();
    for o in &outcomes {
        if let Some(d) = o.delta() {
            let e = deltas.entry(o.file.clone()).or_default();
            e.killed = e.killed.saturating_add(d.killed);
            e.missed = e.missed.saturating_add(d.missed);
        }
    }
    if !deltas.is_empty() {
        let mut km = killmap::Killmap::load(obs);
        km.apply_run(&deltas, &today_ymd());
        if let Err(e) = km.save(obs) {
            eprintln!(".. survivors: killmap не сохранён: {e}");
        }
    }
    // Очередь: только свежие MissedMutant'ы. Повторы («вечные» выжившие)
    // не дублируются — их рост виден в killmap и survivors.log (§3).
    let mut seen = load_seen(obs);
    let mut fresh: Vec<&Outcome> = Vec::new();
    for o in outcomes.iter().filter(|o| o.is_survivor()) {
        if seen.insert(o.dedup_hash()) {
            fresh.push(o);
        }
    }
    if fresh.is_empty() {
        return None;
    }
    save_seen(obs, &seen);
    let red = gate.starts_with("red-mutation");
    let today = today_ymd();
    let mut queue = String::new();
    let mut journal = String::new();
    let mut tags: BTreeMap<&str, u32> = BTreeMap::new();
    for o in &fresh {
        queue += &queue_line(o, red);
        journal += &format!(
            "{today} | {gate} | {}:{} | {} | {}\n",
            o.file, o.line, o.genre, o.mutation
        );
        let t = genre_tag(&o.genre);
        let n = tags.entry(t).or_insert(0);
        *n = n.saturating_add(1);
    }
    append_line(&obs.join(OBS_FILE), &queue).ok();
    append_line(&obs.join(LOG_FILE), &journal).ok();
    let tags_line = tags
        .iter()
        .map(|(t, n)| format!("{t} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "survivors: {} → observations.md ({tags_line})",
        fresh.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"outcomes":[
      {"scenario":{"Mutant":{"name":"devharness/src/core/load.rs:15:5: replace load_average -> Option<f64> with Some(0.0)","package":"devh","file":"devharness/src/core/load.rs","function":{"function_name":"load_average","return_type":"Option<f64>"},"span":{"start":{"line":15,"column":5},"end":{"line":15,"column":42}},"replacement":"Some(0.0)","genre":"FnValue"}},"summary":"MissedMutant","log_path":"l1","diff_path":"d1"},
      {"scenario":{"Mutant":{"name":"devharness/src/core/load.rs:22:5: replace max_load -> Option<u32> with None","file":"devharness/src/core/load.rs","span":{"start":{"line":22,"column":5},"end":{"line":22,"column":40}},"replacement":"None","genre":"FnValue"}},"summary":"CaughtMutant"},
      {"scenario":{"Mutant":{"name":"devharness/src/core/timing.rs:40:9: replace == with != ","file":"devharness/src/core/timing.rs","span":{"start":{"line":40,"column":9}},"replacement":"!=","genre":"BinaryOperator"}},"summary":"MissedMutant"},
      {"scenario":{"Mutant":{"name":"devharness/src/core/plan.rs:7:5: replace x with y","file":"devharness/src/core/plan.rs","span":{"start":{"line":7,"column":5}},"genre":"MatchArmGuard"}},"summary":"UnviableMutant"},
      {"scenario":{"Mutant":{"name":"devharness/src/core/gates.rs:9:5: replace a with b","file":"devharness/src/core/gates.rs","span":{"start":{"line":9,"column":5}},"genre":"FnValue"}},"summary":"TimeoutMutant"},
      {"scenario":{"Mutant":{"name":"devharness/src/core/x.rs:1:1: replace q with w","file":"devharness/src/core/x.rs","span":{"start":{"line":1,"column":1}},"genre":"FnValue"}},"summary":"Какой-тоНовыйВид"}
    ]}"#;

    #[test]
    fn parse_fixture_exact() {
        let (out, warn) = parse_outcomes(FIXTURE);
        assert_eq!(warn, 1, "неизвестный summary — один WARN, не паника");
        assert_eq!(out.len(), 5);
        let survivors: Vec<&Outcome> = out.iter().filter(|o| o.is_survivor()).collect();
        assert_eq!(survivors.len(), 2, "выживший = MissedMutant и только");
        let s0 = survivors.first().unwrap_or_else(|| unreachable!());
        assert_eq!(s0.file, "devharness/src/core/load.rs");
        assert_eq!((s0.line, s0.col), (15, 5));
        assert_eq!(s0.genre, "FnValue");
        assert_eq!(
            s0.mutation,
            "replace load_average -> Option<f64> with Some(0.0)"
        );
        assert_eq!(
            s0.name,
            "devharness/src/core/load.rs:15:5: replace load_average -> Option<f64> with Some(0.0)"
        );
        let s1 = survivors.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!(s1.file, "devharness/src/core/timing.rs");
        assert_eq!((s1.line, s1.col), (40, 9));
        assert_eq!(s1.genre, "BinaryOperator");
    }

    #[test]
    fn parse_missing_file_falls_back_to_name_head() {
        let j = r#"{"outcomes":[{"scenario":{"Mutant":{"name":"src/a.rs:3:1: replace f -> None","span":{"start":{"line":3,"column":1}}}},"summary":"MissedMutant"}]}"#;
        let (out, warn) = parse_outcomes(j);
        assert_eq!(warn, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out.first().map(|o| o.file.as_str()), Some("src/a.rs"));
        assert_eq!(
            out.first().map(|o| o.mutation.as_str()),
            Some("replace f -> None")
        );
    }

    #[test]
    fn parse_forgiving_on_garbage() {
        assert_eq!(parse_outcomes("не json"), (Vec::new(), 1));
        assert_eq!(parse_outcomes(r#"{"outcomes":[]}"#), (Vec::new(), 0));
        assert_eq!(parse_outcomes(r#"{"other":1}"#), (Vec::new(), 1));
    }

    #[test]
    fn queue_line_format_and_red_marker() {
        let (out, _) = parse_outcomes(FIXTURE);
        let s = out
            .iter()
            .find(|o| o.is_survivor())
            .unwrap_or_else(|| unreachable!());
        let line = queue_line(s, false);
        assert_eq!(
            line,
            "devharness/src/core/load.rs:15 | survivor: replace load_average -> Option<f64> with Some(0.0) [FnValue] | red-тест: ассерт ТОЧНОГО значения на этой строке (None/Some(x)/число/строка), не is_some()/is_ok(); граничные значения — отдельным кейсом\n"
        );
        assert!(
            queue_line(s, true).contains("[FnValue] [red-mutation]"),
            "RED-выжившие помечены источником"
        );
        // обязательное поле очереди — по построению (tickets.rs его требует)
        assert!(line.contains("red-тест:"));
    }

    #[test]
    fn auto_idea_by_genre_with_default() {
        assert!(auto_idea("FnValue").contains("ТОЧНОГО"));
        assert!(auto_idea("BinaryOperator").contains("ОБА плеча"));
        assert!(auto_idea("ComparisonOperator").contains("ОБА плеча"));
        assert!(auto_idea("MatchArmGuard").contains("ветку match"));
        assert!(auto_idea("MatchArm").contains("ветку match"));
        assert!(
            auto_idea("FunctionCall").contains("без толерантности"),
            "новый жанр — дефолт, не паника"
        );
        assert_eq!(genre_tag("BinaryOperator"), "binop");
        assert_eq!(genre_tag("Непонятный"), "other");
    }

    /// wt с mutants.out/outcomes.json (без --output в «шаблоне»).
    fn fake_run_dir(t: &Path, json: &str) {
        let dir = t.join("mutants.out");
        std::fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        std::fs::write(dir.join("outcomes.json"), json).unwrap_or_else(|_| unreachable!());
    }

    #[test]
    fn collect_after_run_queues_fresh_and_dedups() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        fake_run_dir(&wt, FIXTURE);

        let line = collect_after_run(&wt, "cargo mutants --in-place", &obs, "mutation")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(line, "survivors: 2 → observations.md (binop 1, fn-value 1)");

        let queue =
            std::fs::read_to_string(obs.join("observations.md")).unwrap_or_else(|_| unreachable!());
        let qlines: Vec<&str> = queue.lines().collect();
        assert_eq!(qlines.len(), 2, "ровно два выживших — по строке на каждый");
        for l in &qlines {
            assert!(l.contains("survivor:"), "{l}");
            assert!(l.contains("red-тест:"), "{l}");
        }
        assert!(queue.contains("devharness/src/core/load.rs:15 | survivor:"));
        assert!(queue.contains("devharness/src/core/timing.rs:40 | survivor:"));

        let seen = load_seen(&obs);
        assert_eq!(seen.len(), 2, "seen-дедуп: два отпечатка");
        let journal =
            std::fs::read_to_string(obs.join("survivors.log")).unwrap_or_else(|_| unreachable!());
        assert_eq!(journal.lines().count(), 2);
        assert!(
            journal.contains("| mutation | devharness/src/core/load.rs:15 | FnValue"),
            "{journal}"
        );

        let km = killmap::Killmap::load(&obs);
        let load = km
            .files
            .get("devharness/src/core/load.rs")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            (load.caught, load.missed, load.runs),
            (1, 1, 1),
            "caught+missed приращения, runs +1"
        );
        let gates = km
            .files
            .get("devharness/src/core/gates.rs")
            .unwrap_or_else(|| unreachable!());
        assert_eq!((gates.caught, gates.missed), (1, 0), "timeout = убит");
        assert!(
            !km.files.contains_key("devharness/src/core/plan.rs"),
            "unviable — не про тесты, в карту не идёт"
        );
        assert!(
            !km.files
                .get("devharness/src/core/load.rs")
                .map(|f| f.last.is_empty())
                .unwrap_or(true),
            "дата проставлена"
        );

        // второй collect на том же прогоне: дедуп → новых строк нет,
        // killmap считает прогон (runs=2), очередь не дублируется.
        let again = collect_after_run(&wt, "cargo mutants --in-place", &obs, "mutation");
        assert!(again.is_none(), "повтор — только счётчики, не очередь");
        let queue2 =
            std::fs::read_to_string(obs.join("observations.md")).unwrap_or_else(|_| unreachable!());
        assert_eq!(queue2.lines().count(), 2, "очередь не задвоена");
        let km2 = killmap::Killmap::load(&obs);
        assert_eq!(
            km2.files.get("devharness/src/core/load.rs").map(|f| f.runs),
            Some(2)
        );
        assert_eq!(load_seen(&obs).len(), 2);
    }

    #[test]
    fn collect_after_run_silent_without_survivors() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        let clean = r#"{"outcomes":[{"scenario":{"Mutant":{"name":"a.rs:1:1: replace x with y","file":"a.rs","span":{"start":{"line":1,"column":1}},"genre":"FnValue"}},"summary":"CaughtMutant"}]}"#;
        fake_run_dir(&wt, clean);
        assert!(collect_after_run(&wt, "cargo mutants", &obs, "mutation").is_none());
        assert!(
            !obs.join("observations.md").exists(),
            "чистый прогон очередь не трогает"
        );
        // а killmap обновлён (все исходы, включая caught)
        assert_eq!(
            killmap::Killmap::load(&obs)
                .files
                .get("a.rs")
                .map(|f| f.caught),
            Some(1)
        );
    }

    #[test]
    fn collect_after_run_absent_or_broken_outcomes_is_silent() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        // outcomes.json нет (пустой скоуп/Env-выход) — тишина, не паника
        assert!(collect_after_run(&wt, "cargo mutants", &obs, "mutation").is_none());
        // битый JSON — тоже тишина + WARN внутри парсера
        fake_run_dir(&wt, "{битый");
        assert!(collect_after_run(&wt, "cargo mutants", &obs, "mutation").is_none());
    }

    #[test]
    fn red_gate_survivor_is_marked() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        fake_run_dir(&wt, FIXTURE);
        let _ = collect_after_run(&wt, "cargo mutants", &obs, "red-mutation src/x.rs")
            .unwrap_or_else(|| unreachable!());
        let queue =
            std::fs::read_to_string(obs.join("observations.md")).unwrap_or_else(|_| unreachable!());
        assert!(
            queue.contains("[red-mutation]"),
            "RED-выживый помечен источником: {queue}"
        );
    }

    #[test]
    fn today_ymd_shape() {
        let d = today_ymd();
        assert_eq!(d.len(), 10, "{d}");
        assert_eq!(d.as_bytes().get(4), Some(&b'-'));
        assert!(d.starts_with("20"), "{d}");
    }
}
