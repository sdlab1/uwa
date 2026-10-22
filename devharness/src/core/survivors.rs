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
//! 2. Непойманные (MissedMutant И Timeout — Ф-3: пойман = только CaughtMutant;
//!    таймаут поимкой НЕ считается) → строки очереди с авто-идеей red-теста
//!    по жанру (§4 таксономии); unviable — не про тесты, это провал кода;
//! 3. seen-дедуп через `.harness/survivors-seen.json` (SHA256 file:line:col:mutation):
//!    «вечный» выживший в очереди одно строкой, повтор — только в killmap/survivors.log;
//! 4. каждый свежий непойманный — строкой в `.harness/survivors.log` (дата | гейт |
//!    файл:строка | ВИД исхода | genre | мутация) — материал для дистилляции
//!    классов (P8-4); старый 5-полевой формат читается как есть;
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
use crate::core::mutaudit::{S_CAUGHT, S_MISSED, S_SUCCEEDED, S_TIMEOUT, S_UNVIABLE};

/// Имя файла отпечатков увиденных выживших (в `.harness/`).
const SEEN_FILE: &str = "survivors-seen.json";
/// Журнал выживших (дата | гейт | файл:строка | genre | мутация) — для P8-4.
const LOG_FILE: &str = "survivors.log";
/// Очередь наблюдений (формат общий с ON-FLY ANALYSIS агентов).
const OBS_FILE: &str = "observations.md";

/// Один исход прогона из outcomes.json (scenario.Mutant + summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Каноническое имя исхода: `CaughtMutant` / `MissedMutant` / `Timeout` /
    /// `Unviable` (легаси-имена приводятся к канону при разборе).
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
    /// Выживший = MissedMutant (дыра в тестах: тесты не поймали мутанта).
    pub fn is_survivor(&self) -> bool {
        self.summary == S_MISSED
    }

    /// Таймаут = ТОЖЕ не пойман (Ф-3): тест-фаза не уложилась — вопрос
    /// открыт, значит это такой же предмет для red-теста, как и выживший.
    pub fn is_timeout(&self) -> bool {
        self.summary == S_TIMEOUT
    }

    /// Мутант без тестовой фазы (собран, но НЕ протестирован, `--check`).
    ///
    /// Это НЕ дыра покрытия: тесты не запускались, поймать было нечего.
    /// В очередь слепых мест он НЕ идёт — иначе прогон без тестов залил бы
    /// очередь тысячами строк и «зажёг» seen-дедуп на всех местах (а
    /// настоящие выжившие на тех же местах потом не попали бы в очередь).
    /// Вердикт при этом мутант блокирует: поимка не подтверждена — см.
    /// `gates::judge_mutation_stats` (ветка `succeeded`).
    pub fn is_succeeded(&self) -> bool {
        self.summary == S_SUCCEEDED
    }

    /// Непойманный мутант = руководство к действию (выживший ИЛИ таймаут).
    ///
    /// Осознанно НЕ включает `succeeded` (см. `is_succeeded`) и неопознанные
    /// исходы: те — не «слепота тестов», а respectively «тестов не было» и
    /// «инструмент сказал не-понятно». Их считает вердикт.
    pub fn is_action(&self) -> bool {
        self.is_survivor() || self.is_timeout()
    }

    /// Отпечаток для seen-дедупа (§3 плана: SHA256 file:line:col:mutation).
    pub fn dedup_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(format!("{}:{}:{}:{}", self.file, self.line, self.col, self.mutation).as_bytes());
        format!("{:x}", h.finalize())
    }

    /// Вклад в killmap. Ф-3: пойман = ТОЛЬКО `CaughtMutant`. Таймаут — не
    /// поимка, он учитывается ОТДЕЛЬНО (и в hot-spot попадает: зона с
    /// зависаниями — такая же слепота тестов). Unviable — не про тесты.
    pub fn delta(&self) -> Option<FileDelta> {
        match self.summary.as_str() {
            S_CAUGHT => Some(FileDelta {
                killed: 1,
                missed: 0,
                timeout: 0,
            }),
            S_MISSED => Some(FileDelta {
                killed: 0,
                missed: 1,
                timeout: 0,
            }),
            S_TIMEOUT => Some(FileDelta {
                killed: 0,
                missed: 0,
                timeout: 1,
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
            // baseline-сценарий (`Success`) — НЕ мутант: не проблема и не
            // предупреждение (раньше он попадал в warn и пугал оператора).
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
        let summary = match as_str(item, "summary").and_then(crate::core::mutaudit::canon_summary) {
            Some(canon) => canon.to_string(),
            None => {
                // Нераспознанный summary МУТАНТА: один WARN на исход, не
                // паника. В очередь не идёт (исход неизвестен) — но он виден
                // в логе гейта и в отчёте нативного CLI как проблема.
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
    // Таймаут — тоже непойманный (Ф-3): помечаем вид исхода, чтобы триаж
    // видел, что чинить: «выжил» (тест не ловит) vs «завис» (тест виснет).
    let kind = if o.is_timeout() { " [timeout]" } else { "" };
    format!(
        "{}:{} | survivor: {} [{}]{kind}{} | red-тест: {}\n",
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
pub(crate) fn today_ymd() -> String {
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
    // killmap — по всем исходам прогона, включая caught (§5). red-mutation
    // меряет качество СПЕКИ, не кода (§8.6): в кодовую карту hot-spot'ов
    // такие промахи не идут — иначе spec-проблема маскируется под слепое
    // место модуля. Красные выжившие живут в журнале (метка red-mutation)
    // и в очереди, а дистиллируются отдельными классами.
    let mut deltas: BTreeMap<String, FileDelta> = BTreeMap::new();
    let red = is_red(gate);
    if !red {
        for o in &outcomes {
            if let Some(d) = o.delta() {
                let e = deltas.entry(o.file.clone()).or_default();
                e.killed = e.killed.saturating_add(d.killed);
                e.missed = e.missed.saturating_add(d.missed);
                // Ф-3: таймаут — отдельный счётчик (не поимка, не выживший).
                e.timeout = e.timeout.saturating_add(d.timeout);
            }
        }
    }
    if !deltas.is_empty() {
        let mut km = killmap::Killmap::load(obs);
        km.apply_run(&deltas, &today_ymd());
        if let Err(e) = km.save(obs) {
            eprintln!(".. survivors: killmap не сохранён: {e}");
        }
    }
    // Очередь: только свежие НЕПОЙМАННЫЕ (выжившие + таймауты). Повторы
    // («вечные» выжившие) не дублируются — их рост виден в killmap и
    // survivors.log (§3).
    let mut seen = load_seen(obs);
    let mut fresh: Vec<&Outcome> = Vec::new();
    for o in outcomes.iter().filter(|o| o.is_action()) {
        if seen.insert(o.dedup_hash()) {
            fresh.push(o);
        }
    }
    if fresh.is_empty() {
        return None;
    }
    save_seen(obs, &seen);
    let today = today_ymd();
    let mut queue = String::new();
    let mut journal = String::new();
    let mut tags: BTreeMap<&str, u32> = BTreeMap::new();
    for o in &fresh {
        queue += &queue_line(o, red);
        journal += &format!(
            "{today} | {gate} | {}:{} | {} | {} | {}\n",
            o.file, o.line, o.summary, o.genre, o.mutation
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

// ---------------------------------------------------------------------------
// P8-4: дистилляция — фактура для человека (группировка журнала, классы RULES,
// тишина и пенсия) + строка сессии в RULES_LOG.
//
// Слои: `Shot` (журнал) → `group_by_genre` (повторы для глаз) и
// `rule_blocks`/`rule_classes` (маппинг жанр→класс по тексту тега `blind:*` —
// человек подтверждает, тулинг только считает, §6). Ни один выход здесь не
// идёт в промпт агента и не влияет на вердикт гейта (§8): класс называет
// человек, пенсия — строка с обоснованием.
// ---------------------------------------------------------------------------

/// Порог пенсии по умолчанию: класс молчит K сессий (§6).
pub const SILENCE_DEFAULT: u32 = 10;
/// Бюджет активного слоя RULES (§6): сверх — сначала пенсия.
pub const RULES_BUDGET: usize = 12;
/// Маркер строки хука в логе гейта — по нему merge находит выстрелы сессии.
pub const HOOK_MARK: &str = "survivors:";
/// Хвост строки хука с метками жанров (`… → observations.md (fn-value 1)`).
pub const HOOK_TAIL: &str = "→ observations.md";
/// Журнал выживших (`.harness/survivors.log`) — путь для команд и тестов.
pub fn log_path(obs: &Path) -> std::path::PathBuf {
    obs.join(LOG_FILE)
}

/// Жанр → aliases (полное имя из outcomes.json + короткая метка лога).
/// Сопоставление по токенам, поэтому «MatchArmGuard» не даёт ложного
/// «MatchArm» (§4: MatchArm внутри MatchArmGuard).
const GENRE_ALIASES: &[(&str, &[&str])] = &[
    (
        "MatchArmGuard",
        &["matcharmguard", "match-arm-guard", "guard"],
    ),
    (
        "ComparisonOperator",
        &["comparisonoperator", "comparison-operator", "cmp"],
    ),
    (
        "BinaryOperator",
        &["binaryoperator", "binary-operator", "binop"],
    ),
    ("FnValue", &["fnvalue", "fn-value"]),
    ("MatchArm", &["matcharm", "match-arm", "match-arms"]),
];

/// Один выстрел из `.harness/survivors.log`:
/// «дата | гейт | файл:строка | жанр | мутация».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub date: String,
    pub gate: String,
    pub file: String,
    pub line: u32,
    pub genre: String,
    pub mutation: String,
    /// Вид исхода (`MissedMutant` / `Timeout`). В старых журналах (5 полей)
    /// — пусто: вид тогда был один (выживший).
    pub summary: String,
}

impl Shot {
    /// Поток выстрела (§8.6): red-mutation меряет качество спеки, не кода.
    /// Гейт-метка — `red-mutation` (label гейта в run.rs; в журнале может
    /// быть с хвостом «red-mutation src/x.rs»).
    pub fn is_red(&self) -> bool {
        is_red(&self.gate)
    }
}

/// Поток гейта: красные выжившие отдельный (§8.6).
pub fn is_red(gate: &str) -> bool {
    gate.starts_with("red-mutation")
}

/// не наш формат, молча пропускаем (журнал дописывают люди и инструменты).
///
/// Формат строки — 6 полей: «дата | гейт | файл:строка | вид | жанр |
/// мутация». Старые журналы (5 полей, без вида) читаются как есть: вид
/// неизвестен → пусто.
pub fn parse_journal(text: &str) -> Vec<Shot> {
    let mut out: Vec<Shot> = Vec::new();
    for raw in text.lines() {
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let parts: Vec<&str> = l.split('|').collect();
        if parts.len() < 5 {
            continue;
        }
        let date = parts
            .first()
            .copied()
            .unwrap_or_default()
            .trim()
            .to_string();
        let gate = parts.get(1).copied().unwrap_or_default().trim().to_string();
        let at = parts.get(2).copied().unwrap_or_default().trim().to_string();
        // Формат строки: 6 полей «дата | гейт | файл:строка | вид | жанр |
        // мутация». Старый — 5 полей (без вида). Различаем ПО СОДЕРЖИМОМУ
        // поля 3: там стоит имя исхода (новый формат) или жанр (старый).
        // Так строки со «|» внутри мутации не путаются.
        let f3 = parts.get(3).copied().unwrap_or_default().trim();
        let new_fmt = is_summary_token(f3);
        let (summary, genre, mutation) = if new_fmt {
            (
                f3.to_string(),
                parts.get(4).copied().unwrap_or_default().trim().to_string(),
                join_mutation(parts.get(5..).unwrap_or_default()),
            )
        } else {
            (
                String::new(),
                f3.to_string(),
                join_mutation(parts.get(4..).unwrap_or_default()),
            )
        };
        if !looks_like_date(&date) || genre.is_empty() {
            continue;
        }
        let Some((file, num)) = at.rsplit_once(':') else {
            continue;
        };
        let file = file.trim();
        if file.is_empty() {
            continue;
        }
        out.push(Shot {
            date,
            gate,
            file: file.to_string(),
            line: num.trim().parse::<u32>().unwrap_or(0),
            genre,
            mutation,
            summary,
        });
    }
    out
}

/// Дата YYYY-MM-DD (без внешних крейтов; тот же вид, что печатает merge).
fn looks_like_date(d: &str) -> bool {
    d.len() == 10
        && d.as_bytes().get(4) == Some(&b'-')
        && d.as_bytes().get(7) == Some(&b'-')
        && d.chars().take(4).all(|c| c.is_ascii_digit())
        && d.chars().skip(5).take(2).all(|c| c.is_ascii_digit())
        && d.chars().skip(8).all(|c| c.is_ascii_digit())
}

/// Склеить хвост журнала (мутация) из полей, разделённых «|».
///
/// Внутри мутации «|» — часть текста, поэтому склеиваем как есть, но
/// внешние пробелы убираем: разделитель «| » оставлял бы лишний пробел
/// в начале описания замены.
fn join_mutation(parts: &[&str]) -> String {
    parts.join("|").trim().to_string()
}

/// Похоже ли поле на имя исхода мутанта (новый формат журнала)?
fn is_summary_token(s: &str) -> bool {
    matches!(
        s,
        S_CAUGHT | S_MISSED | S_TIMEOUT | S_UNVIABLE | "TimeoutMutant" | "UnviableMutant"
    )
}

/// Базовое имя файла: в RULES координаты инстансов запрещены (§8), а для
/// глаза человека «load.rs ×3» информативнее полного пути.
fn basename(file: &str) -> String {
    file.rsplit('/').next().unwrap_or(file).to_string()
}

/// Группа «жанр × файлы»: сколько выстрелов, где, когда последний, примеры.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreGroup {
    pub genre: String,
    pub tag: &'static str,
    pub total: u32,
    /// Имена файлов (basename) и число выстрелов, по убыванию.
    pub files: Vec<(String, u32)>,
    /// Самая свежая дата выстрела группы.
    pub last: String,
    /// До трёх различных хвостов мутаций (свежие first).
    pub examples: Vec<String>,
    /// Поток: red-mutation (§8.6) — отдельные группы, не смешиваются с кодовыми.
    pub red: bool,
}

/// Группировка выстрелов по жанру (§6). Код и red-mutation (§8.6) — разные
/// группы; внутри потока: total desc, затем жанр asc.
pub fn group_by_genre(shots: &[Shot]) -> Vec<GenreGroup> {
    // Поток × жанр (§8.6): red-mutation дистиллируется отдельными классами
    // (blind:red-asserts), поэтому в группировке не смешивается с кодовыми.
    let mut by: BTreeMap<(bool, &str), Vec<&Shot>> = BTreeMap::new();
    for s in shots {
        by.entry((s.is_red(), s.genre.as_str()))
            .or_default()
            .push(s);
    }
    let mut out: Vec<GenreGroup> = by
        .into_iter()
        .map(|((red, genre), list)| {
            let mut files: BTreeMap<String, u32> = BTreeMap::new();
            for s in &list {
                let b = basename(&s.file);
                let e = files.entry(b).or_insert(0);
                *e = e.saturating_add(1);
            }
            let mut files: Vec<(String, u32)> = files.into_iter().collect();
            files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut recent: Vec<&Shot> = list.clone();
            recent.sort_by(|a, b| b.date.cmp(&a.date));
            let mut examples: Vec<String> = Vec::new();
            for s in recent {
                if examples.len() >= 3 {
                    break;
                }
                if !examples.iter().any(|e| e == &s.mutation) {
                    examples.push(s.mutation.clone());
                }
            }
            GenreGroup {
                genre: genre.to_string(),
                tag: genre_tag(genre),
                red,
                total: list.len() as u32,
                files,
                last: list
                    .iter()
                    .map(|s| s.date.clone())
                    .max()
                    .unwrap_or_default(),
                examples,
            }
        })
        .collect();
    // Код первым, потом красный поток; внутри — по числу выстрелов.
    out.sort_by(|a, b| {
        a.red
            .cmp(&b.red)
            .then_with(|| b.total.cmp(&a.total))
            .then_with(|| a.genre.cmp(&b.genre))
    });
    out
}

/// «load.rs ×3, timing.rs ×1» — только basename'ы (§8: без координат).
fn files_inline(files: &[(String, u32)]) -> String {
    files
        .iter()
        .map(|(f, n)| format!("{f} ×{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Однострочная сводка: «fn-value 4 (load.rs ×3), red:fn-value 1 (gates.rs ×1)».
/// Красный поток помечен префиксом `red:` (§8.6).
pub fn digest_line(groups: &[GenreGroup]) -> Option<String> {
    if groups.is_empty() {
        return None;
    }
    let parts: Vec<String> = groups
        .iter()
        .map(|g| {
            format!(
                "{} {} ({})",
                if g.red {
                    format!("red:{}", g.tag)
                } else {
                    g.tag.to_string()
                },
                g.total,
                files_inline(&g.files)
            )
        })
        .collect();
    Some(parts.join(", "))
}

/// Человекочитаемые строки режима по умолчанию (§6: «вот что повторялось»).
pub fn group_lines(shots: &[Shot]) -> Vec<String> {
    let groups = group_by_genre(shots);
    let mut out: Vec<String> = Vec::new();
    if let Some(d) = digest_line(&groups) {
        out.push(format!("вот что повторялось: {d}"));
    }
    for g in &groups {
        out.push(format!(
            "- {}{} ({}) ×{} — {}; последний {}",
            if g.red { "[red-mutation] " } else { "" },
            g.tag,
            g.genre,
            g.total,
            files_inline(&g.files),
            if g.last.is_empty() {
                "?"
            } else {
                g.last.as_str()
            }
        ));
        for ex in &g.examples {
            out.push(format!("    напр. {ex}"));
        }
    }
    out
}

/// Токен для сопоставления жанров: [a-z0-9-], поэтому `blind:match-arms`
/// разбивается на «blind» + «match-arms» и не пересекается с «MatchArm».
fn tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Жанры, названные в тексте тега класса (§4: «Признак: жанр FnValue»).
/// Маппинг подтверждает человек — тулинг лишь ищет известные имена.
pub fn genres_in(text: &str) -> Vec<String> {
    let ts = tokens(text);
    GENRE_ALIASES
        .iter()
        .filter(|(_, aliases)| aliases.iter().any(|a| ts.iter().any(|t| t == a)))
        .map(|(g, _)| g.to_string())
        .collect()
}

/// Тег класса из строки RULES (`- `blind:fn-value` …`). Упоминание «blind:»
/// без имени — не класс.
fn tag_in_line(line: &str) -> Option<String> {
    if let Some(start) = line.find("`blind:") {
        let rest = line.get(start.checked_add(1)?..)?;
        if let Some(end) = rest.find('`') {
            if let Some(t) = rest[..end].trim().strip_prefix("blind:") {
                let tag = t.trim().trim_matches(|c: char| c == '*' || c == '.');
                if !tag.is_empty() {
                    return Some(tag.to_string());
                }
            }
        }
    }
    let start = line.find("blind:")?;
    let rest = line.get(start.checked_add("blind:".len())?..)?;
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | ')' | ';'))
        .unwrap_or(rest.len());
    let tag = rest[..end]
        .trim()
        .trim_matches(|c: char| c == '*' || c == '.');
    if tag.is_empty() {
        return None;
    }
    Some(tag.to_string())
}

/// Блоки `blind:*` из RULES: [(тег, текст блока)]. Блок = строка тега +
/// продолжения до пустой строки/заголовка/нового пункта (§4).
pub fn rule_blocks(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if let Some(tag) = tag_in_line(line) {
            if let Some(prev) = cur.take() {
                out.push(prev);
            }
            cur = Some((tag, line.trim().to_string()));
            continue;
        }
        let t = line.trim();
        let stop = t.is_empty() || t.starts_with('#') || t.starts_with("- ") || t.starts_with("-`");
        if stop {
            if let Some(prev) = cur.take() {
                out.push(prev);
            }
            continue;
        }
        if let Some((_, block)) = cur.as_mut() {
            block.push('\n');
            block.push_str(t);
        }
    }
    if let Some(prev) = cur.take() {
        out.push(prev);
    }
    out
}

/// Класс RULES с фактурой выстрелов и тишины. Пенсия — когда тишина ≥ K и
/// маппинг жанров тега непустой (без жанров в теге пенсия не предлагается:
/// это решение человека, а не догадка тулинга).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRow {
    pub tag: String,
    pub genres: Vec<String>,
    pub shots: u32,
    pub last: String,
    pub silence: u32,
    pub pension: bool,
    /// Поток класса (§8.6): тег, назвавший в тексте «red-mutation», считает
    /// только красные выстрелы; иначе — кодовые. Маппинг подтверждает человек.
    pub red: bool,
}

/// Сколько сессий прошло после последнего выстрела (даты RULES_LOG'а позже
/// даты выстрела). Нет выстрела — молчат все сессии.
fn silence_sessions(last_shot: &str, sessions: &[String]) -> u32 {
    if last_shot.is_empty() {
        return sessions.len() as u32;
    }
    sessions.iter().filter(|d| d.as_str() > last_shot).count() as u32
}

/// Даты сессий из RULES_LOG («## Session N — title (YYYY-MM-DD)»).
pub fn session_dates(rules_log: &str) -> Vec<String> {
    rules_log
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if !t.starts_with("## Session") {
                return None;
            }
            let s = t.rfind('(')?;
            let e = t.rfind(')')?;
            if e <= s {
                return None;
            }
            let d = t.get(s.checked_add(1)?..e)?.trim();
            looks_like_date(d).then(|| d.to_string())
        })
        .collect()
}

/// Маппинг классов RULES на фактуру журнала. `sessions` — даты сессий из
/// RULES_LOG (для тишины), `k` — порог пенсии.
///
/// Поток (§8.6): класс, чей тег назвал в тексте «red-mutation», считает
/// ТОЛЬКО красные выстрелы; остальные — только кодовые. Красный и кодовой
/// выстрел одного жанра не складываются: первая петля меряет спеки, вторая —
/// код, и дистиллируются они разными классами (blind:red-asserts и т.п.).
pub fn rule_classes(
    rules_text: &str,
    shots: &[Shot],
    sessions: &[String],
    k: u32,
) -> Vec<ClassRow> {
    rule_blocks(rules_text)
        .into_iter()
        .map(|(tag, block)| {
            // «red-mutation»/«red-mut» — явное заявление класса на поток.
            let red = tokens(&block)
                .iter()
                .any(|t| t == "red-mutation" || t == "red-mut");
            let genres = genres_in(&block);
            let hits: Vec<&Shot> = shots
                .iter()
                .filter(|s| s.is_red() == red && genres.iter().any(|g| g == &s.genre))
                .collect();
            let last = hits
                .iter()
                .map(|s| s.date.clone())
                .max()
                .unwrap_or_default();
            let silence = silence_sessions(&last, sessions);
            ClassRow {
                tag,
                // без названных в теге жанров пенсия не предлагается: это
                // решение человека, а не догадка тулинга по пустому тексту.
                pension: !genres.is_empty() && silence >= k,
                genres,
                shots: hits.len() as u32,
                last,
                silence,
                red,
            }
        })
        .collect()
}

/// Потоки журнала одной строкой: «код — fn-value 4 (load.rs ×3); red-mutation —
/// fn-value 1 (gates.rs ×1)». Только basename'ы (§8). None — журнал пуст.
/// Итог режима `--rules`: человек видит материал ОБОИХ потоков, даже если
/// классы названы только на один (§8.6).
pub fn streams_line(shots: &[Shot]) -> Option<String> {
    let mut code: BTreeMap<&str, Vec<&Shot>> = BTreeMap::new();
    let mut red: BTreeMap<&str, Vec<&Shot>> = BTreeMap::new();
    for s in shots {
        if s.is_red() {
            red.entry(s.genre.as_str()).or_default().push(s);
        } else {
            code.entry(s.genre.as_str()).or_default().push(s);
        }
    }
    if code.is_empty() && red.is_empty() {
        return None;
    }
    fn part(map: &BTreeMap<&str, Vec<&Shot>>) -> String {
        map.iter()
            .map(|(g, list)| {
                let mut files: BTreeMap<String, u32> = BTreeMap::new();
                for s in list {
                    let e = files.entry(basename(&s.file)).or_insert(0);
                    *e = e.saturating_add(1);
                }
                let names: String = files
                    .iter()
                    .map(|(f, n)| format!("{f} ×{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {} ({})", genre_tag(g), list.len(), names)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    let mut out = format!("потоки (§8.6): код — {}", part(&code));
    if !red.is_empty() {
        out.push_str(&format!("; red-mutation — {}", part(&red)));
    }
    Some(out)
}

/// Строки режима `--rules` (§6): жанры класса, выстрелы, тишина, пенсия,
/// бюджет активного слоя. Без координат инстансов (§8).
pub fn rules_lines(rows: &[ClassRow]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if rows.is_empty() {
        return out;
    }
    out.push(format!(
        "активный слой RULES: {} классов (бюджет ≤{})",
        rows.len(),
        RULES_BUDGET
    ));
    for r in rows {
        let g = if r.genres.is_empty() {
            "не назван — маппинг подтверждает человек".to_string()
        } else {
            r.genres.join(", ")
        };
        let mark = if r.red { "[red-mutation] " } else { "" };
        out.push(format!(
            "- {mark}`blind:{}` — жанры: {}; выстрелов {}, последний {}",
            r.tag,
            g,
            r.shots,
            if r.last.is_empty() {
                "нет"
            } else {
                r.last.as_str()
            }
        ));
        if r.genres.is_empty() {
            out.push("    (допиши в тег «Признак: <жанр>» — тогда считается тишина)".to_string());
        } else if r.pension {
            out.push(format!(
                "    на пенсию: обоснование — {} сессий без выстрелов класса",
                r.silence
            ));
        } else {
            out.push(format!("    тишина {} сессий", r.silence));
        }
    }
    let over = rows.len().saturating_sub(RULES_BUDGET);
    if over > 0 {
        out.push(format!(
            "!!! активный слой больше бюджета на {over}: сначала пенсия (§6)"
        ));
    }
    out
}

/// Метки жанров из строки хука: «survivors: 2 → observations.md (fn-value 1)».
pub fn tags_in_gate_line(line: &str) -> Vec<(String, u32)> {
    let Some(s) = line.rfind('(') else {
        return Vec::new();
    };
    let Some(e) = line.rfind(')') else {
        return Vec::new();
    };
    if e <= s {
        return Vec::new();
    }
    let Some(inner) = s.checked_add(1).and_then(|m| line.get(m..e)) else {
        return Vec::new();
    };
    inner
        .split(',')
        .filter_map(|part| {
            let mut it = part.split_whitespace();
            let tag = it.next()?;
            let n = it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
            Some((tag.to_string(), n))
        })
        .collect()
}

/// Сумма меток жанров по одному списку логов → «fn-value 1, boundary 2».
fn sum_tags(logs: &[String]) -> BTreeMap<String, u32> {
    let mut by: BTreeMap<String, u32> = BTreeMap::new();
    for log in logs {
        for l in log.lines() {
            if !l.contains(HOOK_MARK) || !l.contains(HOOK_TAIL) {
                continue;
            }
            for (tag, n) in tags_in_gate_line(l) {
                let e = by.entry(tag).or_insert(0);
                *e = e.saturating_add(n);
            }
        }
    }
    by
}

/// Строки сессии для RULES_LOG (§6): код и red-mutation (§8.6) — разными
/// строками, с помёткой красного потока. Пустой Vec — выживших не было
/// (журнал RULES_LOG не засоряем).
pub fn session_survivors_lines(code_logs: &[String], red_logs: &[String]) -> Vec<String> {
    let by_code = sum_tags(code_logs);
    let by_red = sum_tags(red_logs);
    let mut out: Vec<String> = Vec::new();
    if !by_code.is_empty() {
        let list = by_code
            .iter()
            .map(|(t, n)| format!("{t} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "- survivors: {list} — выжившие на триаже в .harness/observations.md, классы называет человек"
        ));
    }
    if !by_red.is_empty() {
        let list = by_red
            .iter()
            .map(|(t, n)| format!("{t} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "- survivors [red-mutation]: {list} — отдельный поток (§8.6): качество спеки, классы вроде blind:red-asserts"
        ));
    }
    out
}

/// Файл накопителя сессии item'а: код и red-mutation — разные (§8.6).
fn session_file(obs: &Path, id: &crate::core::types::ItemId, red: bool) -> std::path::PathBuf {
    let name = if red { "redsurv" } else { "surv" };
    obs.join("logs")
        .join(format!("{}-{name}.log", id.dir_name()))
}

/// Накопитель сессии: строка выживших дописывается в per-item файл и
/// ПЕРЕЖИВАЕТ финальный (зелёный) раунд.
///
/// Зачем: в семантике v2 выживший мутант ВСЕГДА роняет гейт — значит
/// зелёный финал возможен только после починки тестов, и лог последнего
/// (зелёного) прогона не содержит ни одной строки выживших. Урок сессии
/// («здесь тесты были слепы») без накопителя стирался бы ровно тогда, когда
/// работа доведена до конца, — и RULES_LOG на merge не смог бы его записать.
pub fn append_session_line(obs: &Path, id: &crate::core::types::ItemId, gate: &str, line: &str) {
    let path = session_file(obs, id, is_red(gate));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    writeln!(f, "{line}").ok();
}

/// Строки сессии для RULES_LOG: накопители item'а — `<id>-surv.log` (код) и
/// `<id>-redsurv.log` (red-mutation, §8.6). Пусто — в сессии выживших не было.
pub fn session_line(
    store: &crate::shell::store::Store,
    id: &crate::core::types::ItemId,
) -> Vec<String> {
    let code: Vec<String> = std::fs::read_to_string(session_file(&store.dir, id, false))
        .ok()
        .into_iter()
        .collect();
    let red: Vec<String> = std::fs::read_to_string(session_file(&store.dir, id, true))
        .ok()
        .into_iter()
        .collect();
    session_survivors_lines(&code, &red)
}

/// `devh survivors --reset`: очистить seen-дедуп (ручной инструмент §6).
/// true — файл был и удалён; false — не было (тоже не ошибка).
pub fn reset_seen(obs: &Path) -> bool {
    let p = obs.join(SEEN_FILE);
    if p.is_file() {
        return std::fs::remove_file(&p).is_ok();
    }
    false
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
        // Ф-3: в очередь идут выжившие И таймауты (2 + 1 = 3).
        assert_eq!(
            line, "survivors: 3 → observations.md (binop 1, fn-value 2)",
            "таймаут — actionable, в счётчике жанров тот же fn-value"
        );

        let queue =
            std::fs::read_to_string(obs.join("observations.md")).unwrap_or_else(|_| unreachable!());
        let qlines: Vec<&str> = queue.lines().collect();
        assert_eq!(qlines.len(), 3, "по строке на каждый непойманный");
        for l in &qlines {
            assert!(l.contains("survivor:"), "{l}");
            assert!(l.contains("red-тест:"), "{l}");
        }
        assert!(queue.contains("devharness/src/core/load.rs:15 | survivor:"));
        assert!(queue.contains("devharness/src/core/timing.rs:40 | survivor:"));
        assert!(
            queue.contains("devharness/src/core/gates.rs:9 | survivor:")
                && queue.contains("[timeout]"),
            "таймаут в очереди помечен видом исхода: {queue}"
        );

        let seen = load_seen(&obs);
        assert_eq!(seen.len(), 3, "seen-дедуп: три отпечатка");
        let journal =
            std::fs::read_to_string(obs.join("survivors.log")).unwrap_or_else(|_| unreachable!());
        assert_eq!(journal.lines().count(), 3);
        assert!(
            journal
                .contains("| mutation | devharness/src/core/load.rs:15 | MissedMutant | FnValue"),
            "новый формат журнала: 6 полей, вид исхода явным полем: {journal}"
        );
        assert!(
            journal.contains("| mutation | devharness/src/core/gates.rs:9 | Timeout | FnValue"),
            "таймаут в журнале — со своим видом: {journal}"
        );

        let km = killmap::Killmap::load(&obs);
        let load = km
            .files
            .get("devharness/src/core/load.rs")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            (load.caught, load.missed, load.timeout, load.runs),
            (1, 1, 0, 1),
            "caught+missed приращения, runs +1"
        );
        let gates = km
            .files
            .get("devharness/src/core/gates.rs")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            (gates.caught, gates.missed, gates.timeout),
            (0, 0, 1),
            "Ф-3: таймаут — НЕ поимка (caught=0), отдельный счётчик"
        );
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
        assert_eq!(queue2.lines().count(), 3, "очередь не задвоена");
        let km2 = killmap::Killmap::load(&obs);
        assert_eq!(
            km2.files.get("devharness/src/core/load.rs").map(|f| f.runs),
            Some(2)
        );
        assert_eq!(load_seen(&obs).len(), 3);
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

    /// Мутант без тестовой фазы (`Success` на мутанте) — НЕ дыра покрытия:
    /// тесты не запускались. В очередь он не идёт (иначе прогон без тестов
    /// залил бы очередь и «сжёг» seen-дедуп на всех местах), но вердикт его
    /// блокирует — см. gates::judge_mutation_stats.
    #[test]
    fn succeeded_mutants_do_not_flood_the_queue() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        let json = r#"{"outcomes":[
          {"scenario":{"Mutant":{"name":"a.rs:1:1: replace x with y","file":"a.rs","span":{"start":{"line":1,"column":1}},"genre":"FnValue"}},"summary":"Success"},
          {"scenario":{"Mutant":{"name":"b.rs:2:1: replace p with q","file":"b.rs","span":{"start":{"line":2,"column":1}},"genre":"FnValue"}},"summary":"Success"},
          {"scenario":{"Mutant":{"name":"c.rs:3:1: replace m with n","file":"c.rs","span":{"start":{"line":3,"column":1}},"genre":"FnValue"}},"summary":"MissedMutant"}
        ]}"#;
        fake_run_dir(&wt, json);
        let line = collect_after_run(&wt, "cargo mutants", &obs, "mutation")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            line, "survivors: 1 → observations.md (fn-value 1)",
            "без тестовой фазы — не в очереди"
        );
        let queue =
            std::fs::read_to_string(obs.join("observations.md")).unwrap_or_else(|_| unreachable!());
        assert_eq!(queue.lines().count(), 1, "только настоящий выживший");
        assert!(queue.contains("c.rs:3"), "{queue}");
        assert_eq!(
            load_seen(&obs).len(),
            1,
            "seen-дедуп не сожжён на местах без тестовой фазы"
        );
        // killmap: без тестовой фазы — не ни поимка, ни дыра покрытия.
        let km = killmap::Killmap::load(&obs);
        assert!(!km.files.contains_key("a.rs"), "{:?}", km.files);
        assert!(!km.files.contains_key("b.rs"), "{:?}", km.files);
    }

    /// Неопознанный исход мутанта — не «слепота тестов»: в очередь не идёт,
    /// но считается предупреждением (и блокирует вердикт гейта).
    #[test]
    fn unknown_summary_is_warned_but_not_queued() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = t.path().join("wt");
        let obs = t.path().join(".harness");
        std::fs::create_dir_all(&obs).unwrap_or_else(|_| unreachable!());
        let json = r#"{"outcomes":[
          {"scenario":{"Mutant":{"name":"a.rs:1:1: replace x with y","file":"a.rs","span":{"start":{"line":1,"column":1}},"genre":"FnValue"}},"summary":"Какой-тоНовыйВид"}
        ]}"#;
        fake_run_dir(&wt, json);
        assert!(
            collect_after_run(&wt, "cargo mutants", &obs, "mutation").is_none(),
            "неопознанный исход — не выживший: в очередь не идёт"
        );
        assert!(
            !obs.join("observations.md").exists(),
            "очередь не создаётся неопознанным исходом"
        );
        let (out, warn) = parse_outcomes(json);
        assert_eq!(warn, 1, "неопознанный исход — один WARN");
        assert!(out.is_empty());
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
        // §8.6: красный прогон не пишет в кодовую карту hot-spot'ов —
        // иначе spec-проблема маскируется под слепое место модуля.
        let km = killmap::Killmap::load(&obs);
        assert!(
            km.files.is_empty(),
            "red-прогон не идёт в killmap: {:?}",
            km.files
        );
    }

    #[test]
    fn today_ymd_shape() {
        let d = today_ymd();
        assert_eq!(d.len(), 10, "{d}");
        assert_eq!(d.as_bytes().get(4), Some(&b'-'));
        assert!(d.starts_with("20"), "{d}");
    }

    // ---- P8-4: дистилляция (§6) ----

    /// Журнал с датами: 3 код-выстрела FnValue (load.rs ×3, из них два «вечных» на
    /// 15-й строке) + 1 binop в timing.rs + 1 red-mutation выстрел FnValue —
    /// отдельный поток (§8.6).
    const JOURNAL: &str = concat!(
        "2026-10-10 | mutation | devharness/src/core/load.rs:15 | FnValue | replace load_average -> Option<f64> with Some(0.0)\n",
        "2026-10-12 | mutation | devharness/src/core/load.rs:22 | FnValue | replace max_load -> Option<u32> with None\n",
        "2026-10-14 | mutation | devharness/src/core/timing.rs:40 | BinaryOperator | replace == with !=\n",
        "2026-10-15 | red-mutation load.rs | devharness/src/core/load.rs:15 | FnValue | replace load_average -> Option<f64> with Some(0.0)\n",
        "2026-10-16 | mutation | devharness/src/core/load.rs:15 | FnValue | replace load_average -> Option<f64> with Some(1.5)\n",
        "мусорная строка без полей\n",
        "2026-10-13 | mutation | load.rs | FnValue | без координат — не наш формат\n",
    );

    const RULES: &str = concat!(
        "# Отчёт\n\n",
        "## Железные правила\n1. CI зелёный до коммита.\n\n",
        "## Классы слепоты тестов (из мутационных выживших, P8)\n",
        "- `blind:fn-value` Новая pub-fn с логикой получает прямой тест с точным ассертом\n",
        "  (значение/None/Some(x)/граница), не is_some/is_ok. Признак в триаже: жанр FnValue.\n",
        "- `blind:boundary` Новое if/сравнение/булево покрывается кейсами на оба плеча\n",
        "  и на границу (равенство). Признак: BinaryOperator/ComparisonOperator.\n",
        "- `blind:wrapper` Делегирующая pub-fn с добавленной логикой — контракт-тест на\n",
        "  результат, не на делегирование. Признак: FnValue в тонких модулях.\n",
        "- `blind:pure-fn` Чистая функция с операторами — proptest с инвариантом, не пять юнитов.\n",
        "- `blind:red-asserts` RED-выстрел = спека без проверяемого ассерта: red-тест пишется\n",
        "  из спеки до кода (поток red-mutation). Признак в триаже: жанр FnValue из red-гейта.\n",
    );

    /// Журнал в НОВОМ формате (6 полей): вид исхода — отдельное поле.
    const JOURNAL_NEW: &str = concat!(
        "2026-10-20 | mutation | devharness/src/core/load.rs:15 | MissedMutant | FnValue | replace load_average -> Option<f64> with Some(0.0)\n",
        "2026-10-21 | mutation | devharness/src/core/gates.rs:9 | Timeout | FnValue | replace a with b\n",
        "2026-10-21 | mutation | devharness/src/core/plan.rs:3 | Unviable | FnValue | replace c with d\n",
    );

    #[test]
    fn parse_journal_reads_new_six_field_format() {
        // Новый формат: 6 полей, вид исхода — ПОЛЕ 3. Старый (5 полей)
        // читается как есть (вид неизвестен → пусто, не выдумываем).
        let shots = parse_journal(JOURNAL_NEW);
        assert_eq!(shots.len(), 3, "{shots:?}");
        let s = shots.first().unwrap_or_else(|| unreachable!());
        assert_eq!(s.summary, "MissedMutant");
        assert_eq!(s.file, "devharness/src/core/load.rs");
        assert_eq!(s.line, 15);
        assert_eq!(s.genre, "FnValue");
        let t = shots.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!(t.summary, "Timeout", "таймаут — свой вид исхода");
        let old = parse_journal(JOURNAL);
        assert_eq!(old.len(), 5);
        assert!(
            old.iter().all(|s| s.summary.is_empty()),
            "старый 5-полевой журнал: вид неизвестен"
        );
    }

    #[test]
    fn journal_grouping_counts_files_and_examples() {
        let shots = parse_journal(JOURNAL);
        assert_eq!(shots.len(), 5, "мусор и строка без координат пропущены");
        let groups = group_by_genre(&shots);
        assert_eq!(groups.len(), 3, "два код-жанра + red-группа (§8.6)");
        let fnv = groups.first().unwrap_or_else(|| unreachable!());
        assert_eq!(fnv.tag, "fn-value");
        assert!(!fnv.red, "первая группа — код: красный поток идёт отдельно");
        assert_eq!(fnv.total, 3);
        assert_eq!(fnv.files.len(), 1, "группировка жанр × файл");
        assert_eq!(fnv.files.first().map(|(f, _)| f.as_str()), Some("load.rs"));
        assert_eq!(fnv.files.first().map(|(_, n)| *n), Some(3));
        assert_eq!(fnv.last, "2026-10-16", "самая свежая дата группы");
        assert_eq!(fnv.examples.len(), 3, "примеров не больше трёх");
        assert!(
            fnv.examples
                .first()
                .map(|e| e.contains("Some(1.5)"))
                .unwrap_or(false),
            "свежие примеры first: {:?}",
            fnv.examples
        );
        let binop = groups.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!((binop.tag, binop.total), ("binop", 1));
        assert_eq!(
            binop.files.first().map(|(f, _)| f.as_str()),
            Some("timing.rs")
        );
        assert_eq!(
            digest_line(&groups).as_deref(),
            Some("fn-value 3 (load.rs ×3), binop 1 (timing.rs ×1), red:fn-value 1 (load.rs ×1)")
        );
        // §8.6: красный поток — своя группа (жанр тот же, но не суммируется с
        // кодовым) и префикс `red:` в сводке.
        let red = groups.get(2).unwrap_or_else(|| unreachable!());
        assert_eq!((red.tag, red.total), ("fn-value", 1));
        assert!(red.red, "группа из red-mutation-выстрела");
        assert_eq!(red.last, "2026-10-15");
        assert_eq!(
            red.files.first().map(|(f, n)| (f.as_str(), *n)),
            Some(("load.rs", 1))
        );
        assert!(
            !groups
                .iter()
                .any(|g| !g.red && g.tag == "fn-value" && g.total == 4),
            "код и red-выстрелы одного жанра не складываются: {groups:?}"
        );
        let lines = group_lines(&shots);
        assert!(
            lines
                .first()
                .map(|l| l.starts_with("вот что повторялось: fn-value 3 (load.rs ×3)"))
                .unwrap_or(false),
            "{lines:?}"
        );
        // граница честности §8: в выводе только basename'ы, без координат.
        assert!(
            !lines.join("\n").contains(".rs:"),
            "строки вывода без file:line: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("- [red-mutation] fn-value (FnValue) ×1")),
            "красная группа помечена в выводе: {lines:?}"
        );
    }

    #[test]
    fn rules_mode_silence_and_pension() {
        let shots = parse_journal(JOURNAL);
        // 12 сессий после последнего выстрела (10-16) + 2 до него.
        let mut sessions: Vec<String> = vec!["2026-10-01".into(), "2026-10-09".into()];
        sessions.extend((17..=28).map(|d| format!("2026-10-{d:02}")));
        let rows = rule_classes(RULES, &shots, &sessions, 10);
        assert_eq!(rows.len(), 5, "пять blind-тегов в RULES");
        let by = |tag: &str| rows.iter().find(|r| r.tag == tag).cloned();
        let fnv = by("fn-value").unwrap_or_else(|| unreachable!());
        assert_eq!(fnv.genres, vec!["FnValue".to_string()]);
        assert!(!fnv.red, "кодовый класс считает кодовый поток");
        // красный выстрел того же жанра НЕ идёт в счёт кодового класса (§8.6)
        assert_eq!((fnv.shots, fnv.last.as_str()), (3, "2026-10-16"));
        assert_eq!(fnv.silence, 12, "сессии после последнего выстрела");
        assert!(fnv.pension, "тишина ≥ K → пенсия");
        let bnd = by("boundary").unwrap_or_else(|| unreachable!());
        assert!(bnd.genres.iter().any(|g| g == "BinaryOperator"), "{bnd:?}");
        assert!(
            bnd.genres.iter().any(|g| g == "ComparisonOperator"),
            "{bnd:?}"
        );
        assert_eq!(
            (bnd.shots, bnd.last.as_str()),
            (1, "2026-10-14"),
            "binop-выстрел журнала"
        );
        assert_eq!(bnd.silence, 12, "сессии после выстрела класса");
        assert!(bnd.pension);
        let pure = by("pure-fn").unwrap_or_else(|| unreachable!());
        assert_eq!(
            pure.silence,
            sessions.len() as u32,
            "без жанров в теге тишина — весь журнал сессий"
        );
        assert!(
            pure.genres.is_empty(),
            "в теге не назван жанр — маппинг за человеком: {pure:?}"
        );
        assert!(
            !pure.pension,
            "без маппинга пенсия не предлагается: {pure:?}"
        );
        // §8.6: класс, назвавший в тексте «red-mutation», считает только
        // красные выстрелы — свой поток, свои даты и своя пенсия.
        let rr = by("red-asserts").unwrap_or_else(|| unreachable!());
        assert!(rr.red, "класс объявил поток словом: {rr:?}");
        assert_eq!((rr.shots, rr.last.as_str()), (1, "2026-10-15"));
        assert_eq!(rr.silence, 12, "сессии после красного выстрела");
        assert!(rr.pension, "тишина ≥ K — пенсия и у красного класса");
        let out = rules_lines(&rows).join("\n");
        assert!(
            out.contains("на пенсию: обоснование — 12 сессий без выстрелов класса"),
            "{out}"
        );
        // Тот же K, но меньше порога не достигнут («тишина K сессий»).
        let alive = rule_classes(RULES, &shots, &sessions, 13);
        let out2 = rules_lines(&alive).join("\n");
        assert!(out2.contains("тишина 12 сессий"), "{out2}");
        assert!(
            alive.iter().all(|r| !r.pension),
            "порог 13 не достигнут: {alive:?}"
        );
        assert!(
            out.contains("активный слой RULES: 5 классов (бюджет ≤12)"),
            "{out}"
        );
        assert!(
            out.contains("[red-mutation] `blind:red-asserts`"),
            "красный класс помечен потоком в --rules: {out}"
        );
        assert!(out.contains("маппинг подтверждает человек"), "{out}");
        // §8: слой RULES не получает координат инстансов.
        assert!(!out.contains(".rs"), "{out}");
    }

    #[test]
    fn rules_silence_without_any_shot_pensions_named_genre() {
        let rows = rule_classes(
            "- `blind:fn-value` тест, признак: жанр FnValue.\n",
            &[],
            &["2026-10-01".to_string(), "2026-10-08".to_string()],
            2,
        );
        let r = rows.first().unwrap_or_else(|| unreachable!());
        assert_eq!(
            (r.shots, r.silence),
            (0, 2),
            "нет выстрела — молчат все сессии"
        );
        assert!(r.pension, "тишина ≥ K → пенсия даже без единого выстрела");
    }

    #[test]
    fn rules_budget_overflow_is_flagged() {
        let mut text = String::from("## Классы\n");
        for i in 0..15 {
            text.push_str(&format!(
                "- `blind:class-{i}` чего-то там, признаком служит жанр FnValue.\n\n"
            ));
        }
        let rows = rule_classes(&text, &[], &[], 10);
        assert_eq!(rows.len(), 15);
        let out = rules_lines(&rows).join("\n");
        assert!(
            out.contains("активный слой больше бюджета на 3"),
            "сверх бюджета — сначала пенсия: {out}"
        );
    }

    #[test]
    fn empty_journal_gives_no_data_and_no_panic() {
        assert!(parse_journal("").is_empty());
        assert!(group_lines(&[]).is_empty());
        assert!(digest_line(&[]).is_none());
        assert!(rules_lines(&[]).is_empty());
        assert!(session_survivors_lines(&[], &[]).is_empty());
        assert!(streams_line(&[]).is_none());
        // классы без журнала: тишина = все сессии, пенсии нет (K не достигнут)
        let rows = rule_classes(RULES, &[], &["2026-10-01".into()], 10);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| !r.pension), "одна сессия молчания ≠ K");
    }

    #[test]
    fn parse_journal_keeps_pipes_in_mutation() {
        let shots = parse_journal(
            "2026-10-12 | mutation | load.rs:20 | FnValue | replace a with b | лишний | хвост\n",
        );
        assert_eq!(shots.len(), 1);
        assert_eq!(
            shots.first().map(|s| s.mutation.as_str()),
            Some("replace a with b | лишний | хвост")
        );
    }

    #[test]
    fn session_lines_survive_a_later_green_round() {
        // Накопитель сессии: строка выживших ПЕРЕЖИВАЕТ финальный (зелёный)
        // раунд. В семантике v2 выживший всегда роняет гейт — значит зелёный
        // финал возможен только после починки тестов, и лог последнего
        // (зелёного) прогона не содержит ни строки выживших. Без накопителя
        // урок сессии стирался бы ровно тогда, когда работа доведена до конца.
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = crate::shell::store::Store::new(t.path());
        let id = crate::core::types::ItemId(1);
        let obs = store.dir.clone();
        append_session_line(
            &obs,
            &id,
            "mutation",
            "survivors: 1 → observations.md (fn-value 1)",
        );
        // финальный зелёный раунд перезаписал лог гейта — там выживших нет
        std::fs::write(
            store.log_file(&id, "mutate"),
            "MUTATE: Mutants 10, killed 10 (100%)\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let lines = session_line(&store, &id);
        assert_eq!(lines.len(), 1, "урок сессии дожил до merge: {lines:?}");
        assert!(lines[0].contains("fn-value 1"), "{lines:?}");
        assert!(lines[0].starts_with("- survivors: "), "{lines:?}");

        // red-поток — отдельный файл и отдельная строка (§8.6)
        append_session_line(
            &obs,
            &id,
            "red-mutation",
            "survivors: 2 → observations.md (binop 1, fn-value 1)",
        );
        let lines = session_line(&store, &id);
        assert_eq!(lines.len(), 2, "код и red — разными строками: {lines:?}");
        let rl = lines.get(1).unwrap_or_else(|| unreachable!());
        assert!(rl.starts_with("- survivors [red-mutation]: "), "{rl}");
        assert!(rl.contains("binop 1") && rl.contains("fn-value 1"), "{rl}");

        // item без выживших — строк нет (RULES_LOG не засоряем)
        let other = crate::core::types::ItemId(2);
        assert!(session_line(&store, &other).is_empty());
    }

    #[test]
    fn session_survivors_lines_split_streams() {
        let code = vec![
            String::from("MUTATE: Mutants 10, killed 9 (90%)\nsurvivors: 1 → observations.md (fn-value 1)\n"),
            String::from(
                "survivors: 2 → observations.md (binop 1, fn-value 1)\n.. survivors: 1 исход(ов) outcomes.json без разбираемого summary — пропущены\n",
            ),
        ];
        let red = vec![String::from(
            "survivors: 1 → observations.md (fn-value 1)\n",
        )];
        let lines = session_survivors_lines(&code, &red);
        assert_eq!(
            lines.len(),
            2,
            "код и red — разными строками (§8.6): {lines:?}"
        );
        let cl = lines.first().unwrap_or_else(|| unreachable!());
        assert!(cl.starts_with("- survivors: "), "{cl}");
        assert!(cl.contains("fn-value 2"), "сумма по логам: {cl}");
        assert!(cl.contains("binop 1"), "{cl}");
        assert!(!cl.contains("исход"), "WARN хука — не выстрел: {cl}");
        // потоки не смешиваются: жанр красной строки — свой счёт
        let rl = lines.get(1).unwrap_or_else(|| unreachable!());
        assert!(rl.starts_with("- survivors [red-mutation]: "), "{rl}");
        assert!(rl.contains("fn-value 1"), "{rl}");
        assert!(
            !rl.contains("binop"),
            "красный счёт — только красные выстрелы: {rl}"
        );
        // без выживших строк нет; пустые скобки тоже не выстрел
        assert!(session_survivors_lines(&[], &[]).is_empty());
        assert!(
            session_survivors_lines(&["MUTATE: Mutants 10, killed 10 (100%)".into()], &[])
                .is_empty()
        );
        assert!(
            session_survivors_lines(&["survivors: 0 → observations.md ()".into()], &[]).is_empty()
        );
        // только red-логи — только красная строка
        let only_red = session_survivors_lines(&[], &red);
        assert_eq!(only_red.len(), 1, "{only_red:?}");
        assert!(only_red
            .first()
            .map(|l| l.contains("[red-mutation]"))
            .unwrap_or(false));
    }

    #[test]
    fn streams_line_splits_code_and_red() {
        let shots = parse_journal(JOURNAL);
        let s = streams_line(&shots).unwrap_or_else(|| unreachable!());
        assert!(s.starts_with("потоки (§8.6): код — "), "{s}");
        assert!(s.contains("binop 1 (timing.rs ×1)"), "{s}");
        assert!(s.contains("fn-value 3 (load.rs ×3)"), "{s}");
        assert!(
            s.contains("; red-mutation — fn-value 1 (load.rs ×1)"),
            "{s}"
        );
        // §8: только basename'ы — ни путей, ни координат
        assert!(!s.contains("devharness"), "{s}");
        assert!(!s.contains(".rs:"), "{s}");
    }

    #[test]
    fn session_dates_from_rules_log() {
        let log = "# Отчёт\n\n## Session 40 — Титул (2026-10-01)\n- commit: abc\n\n## Session 41 — Другое (2026-10-15)\n";
        assert_eq!(
            session_dates(log),
            vec!["2026-10-01".to_string(), "2026-10-15".to_string()]
        );
        assert!(session_dates("нет сессий тут").is_empty());
        assert!(session_dates("## Session 42 — без даты (особая)").is_empty());
    }

    #[test]
    fn genres_in_tag_text_and_no_false_match() {
        assert_eq!(
            genres_in("Признак в триаже: жанр FnValue."),
            vec!["FnValue".to_string()]
        );
        assert_eq!(
            genres_in("MatchArmGuard и MatchArm"),
            vec!["MatchArmGuard".to_string(), "MatchArm".to_string()]
        );
        assert_eq!(
            genres_in("`blind:match-arms` класс"),
            vec!["MatchArm".to_string()]
        );
        assert_eq!(
            genres_in("`blind:fn-value` класс"),
            vec!["FnValue".to_string()]
        );
        assert!(genres_in("никуда не годный текст").is_empty());
        // «guard» — короткий alias, но «blind:match-arms» его не содержит
        assert!(!genres_in("`blind:match-arms`")
            .iter()
            .any(|g| g == "MatchArmGuard"));
    }

    #[test]
    fn rule_blocks_split_by_tag_and_headings() {
        let blocks = rule_blocks(RULES);
        assert_eq!(blocks.len(), 5, "{blocks:?}");
        assert_eq!(blocks.first().map(|(t, _)| t.as_str()), Some("fn-value"));
        assert!(
            blocks
                .first()
                .map(|(_, b)| b.contains("Признак в триаже: жанр FnValue"))
                .unwrap_or(false),
            "продолжение блока собрано: {:?}",
            blocks.first()
        );
        // тег без имени классом не считается
        assert!(rule_blocks("- про слепоту: blind: без имени\n").is_empty());
    }

    #[test]
    fn reset_seen_removes_file_and_is_silent_without_it() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let obs = t.path();
        assert!(!reset_seen(obs), "нет файла — не ошибка");
        std::fs::write(obs.join("survivors-seen.json"), "{\"version\":1}")
            .unwrap_or_else(|_| unreachable!());
        assert!(reset_seen(obs), "файл был — удалён");
        assert!(!obs.join("survivors-seen.json").exists());
        assert!(!reset_seen(obs), "повторно удалять нечего");
    }
}
