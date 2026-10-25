//! `devh mutate` — ЕДИНСТВЕННЫЙ вход для мутационного аудита.
//!
//! Почему отдельная подкоманда, а не скрипт: оператор и агент должны иметь
//! ОДНУ команду, которая делает всё (запустить, посмотреть живой прогресс,
//! получить список проблем, финальный вердикт) и не требует «руками» знать,
//! где какой бинарник. Внешний `tools/mutants-watch.sh` удалён (R2) —
//! наблюдение теперь родное, на inotify (`core::watch`).
//!
//! ## Подкоманды
//!
//! | команда | что делает | когда звать |
//! | --- | --- | --- |
//! | `devh mutate help` | канон: что запускать и когда | непонятно, что дальше |
//! | `devh mutate start <args>` | запустить аудит (cargo-mutants) ОТДЕЛЬНЫМ процессом | начать проверку зоны |
//! | `devh mutate status` | мгновенный снимок (легенда + пилюля + счётчики) | «что сейчас?» |
//! | `devh mutate watch` | живой CLI: строка обновляется на месте по СОБЫТИЯМ | смотреть прогресс в терминале |
//! | `devh mutate problems` | руководство к действию: что чинить | выжил мутант |
//! | `devh mutate verdict` | финальный вердикт, код возврата 0/1/2 | гейт перед мержем |
//! | `devh mutate stop` | остановить идущий аудит | «аудит не нужен / завис» |
//!
//! ## Почему аудит живёт отдельным процессом
//!
//! Прогон занимает десятки минут, а его убивает всё, что умеет «гасить
//! группу процессов» (таймаут tool'а, закрытый терминал). Поэтому
//! `start` поднимает СУПЕРВИЗОРА в отдельной сессии (`setsid`): он держит
//! cargo-mutants своим ребёнком, рисует живые кадры в `.harness/mutate/`,
//! пишет `problems.json` ПО МЕРЕ появления проблем и формирует вердикт.
//! Тул может умереть — аудит не умрёт.
//!
//! ## Почему счётчики из append-списков, а не из outcomes.json
//!
//! `outcomes.json` cargo-mutants переписывает целиком (`File::create`) —
//! читатель может поймать его на середине записи. Списки `caught.txt` /
//! `missed.txt` / `timeout.txt` / `unviable.txt` — append-only, читаются
//! всегда. Детали (файл:строка) берём и оттуда, и из `outcomes.json`:
//! в списках есть имя мутанта (`путь:строка:колонка: описание`), в JSON —
//! замена и жанр.
//!
//! ## Фоновый аудит не мутирует дерево агента (R11)
//!
//! `devh mutate start` работает в copy-режиме cargo-mutants (без
//! `--in-place`): исходники агента не трогаются. `--in-place` разрешён
//! только синхронному гейту внутри `devh run` (там своя worktree).
//!
//! ## Имена исходов
//!
//! Ровно те, что пишет cargo-mutants 27.1.0: `CaughtMutant`,
//! `MissedMutant`, `Timeout`, `Unviable` (см. `core::mutaudit`).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::early;
use crate::core::gates::{self, MutationStats};
use crate::core::mutaudit::{self, Audit};
use crate::core::rate::{self, RateKind};
use crate::core::tui;
use crate::core::watch::{Event, Watcher};
use crate::shell::color::colors;
use crate::shell::lock::OutputLock;
use crate::shell::proc::{group_alive, pid_alive};

/// Каталог артефактов аудита внутри `.harness/`.
const WORK: &str = "mutate";
const STATE_FILE: &str = "state.json";
const LIVE_FILE: &str = "live.log";
const CARGO_LOG: &str = "cargo.log";
const VERDICT_FILE: &str = "verdict.json";
const SUP_LOG: &str = "supervisor.log";
const PROBLEMS_FILE: &str = "problems.json";
/// Маркер запроса остановки: его ставит `devh mutate stop`, по нему
/// супервизор пишет состояние «stopped» (а не «failed»).
const STOP_MARKER: &str = "stop.requested";

/// Граница ожидания события (мс). Это НЕ «цикл со sleep»: рендер происходит
/// по событию ФС (inotify), а граница нужна лишь чтобы заметить завершение
/// процесса, если inotify молчит.
const POLL_MS: i32 = 1000;

/// Точка входа подкоманды `devh mutate`.
pub fn mutate(root: &Path, sub: &str, args: &[String]) -> anyhow::Result<()> {
    match sub {
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        "start" => start(root, args),
        "status" => status(root, args),
        "watch" => watch(root, args),
        "problems" => problems(root, args),
        "recheck" => recheck(root, args),
        "verdict" => verdict(root),
        "stop" => stop(root),
        // Внутренняя подкоманда: её запускает `start` (супервизор).
        "run" => supervise(root, args),
        other => {
            println!("{other}");
            help();
            anyhow::bail!("devh mutate: неизвестная подкоманда «{other}»");
        }
    }
}

/// Канон: что запускать и когда. Этот текст — источник истины: из него
/// генерируются `docs/mutation_workflow.md` и `AGENTS.md`.
fn help() {
    println!("{}", CANON);
}

/// Канон одной строкой-блоком (и в справке, и в документации).
pub const CANON: &str = r#"devh mutate — единственный вход в мутационный аудит

  devh mutate start <args>   запустить аудит (args = флаги cargo-mutants).
                              Процесс ОТДЕЛЬНЫЙ (setsid): тул/терминал могут
                              умереть — аудит продолжится. Печатает, что
                              запускать дальше. Фоновый аудит идёт в copy-режиме
                              (без --in-place): дерево агента не мутируется.
  devh mutate status          мгновенный снимок: легенда + процент-пилюля +
                              счётчики (✓/✗/⏱/⊘) + ETA + текущий мутант.
                              rc=1, если уже есть проблемы. Ничего не ждёт.
  devh mutate watch           живой CLI: строка обновляется на месте по
                              событиям ФС (inotify), пока идёт прогон
                              (--timeout N — ограничить секунды;
                               --until-problem — выйти на первой проблеме rc=1;
                               --batch-ms N — окно накопления проблем).
  devh mutate problems        руководство к действию: что чинить (файл:строка,
                              замена, жанр, исход). rc=1, если есть проблемы.
                              Если дерево изменилось после старта, снимок
                              помечается протухшим: не чини уже исправленное.
  devh mutate recheck <name>  перепроверить ОДНОГО мутанта по имени (секунды):
                              cargo-mutants -F, отдельный --output. Caught →
                              проблема закрыта в problems (overlay), иначе rc=1.
  devh mutate verdict         финальный вердикт: 0 = чисто, 1 = дыра
                              (выжил/таймаут/не компилится/неопознанный исход/
                              повреждённые детали/упавший инструмент),
                              2 = прогон ещё идёт / аудита нет.
  devh mutate stop            остановить идущий аудит (группа cargo-mutants).
  devh mutate help            эта справка.

Когда что:
  • закрыл зону кода      → start (ядро: --config devharness/cargo-mutants.toml)
  • перед мержем           → verdict (код возврата — гейт)
  • «что сейчас?»         → status
  • «что чинить?»         → problems
  • починил одного         → recheck <name>, затем перепрогон дельты
  • смотришь в терминал    → watch
  • дерево изменилось      → stop + перезапуск (next подскажет); verdict
    после старта               по старому снимку не выносится (rc=2)

Семантика исходов (cargo-mutants 27.1.0):
  ✓ CaughtMutant  — пойман: единственный результат, закрывает вопрос.
  ✗ MissedMutant  — выжил: дыра в тестах, чинить.
  ⏱ Timeout       — тест-фаза не уложилась: пойман не был, чинить.
  ⊘ Unviable      — мутант не компилируется: провал (ноль терпимости), чинить код.
  ○ Success (мутант) — собрался, но НЕ протестирован: поимки не было, чинить.
  ○ Success (baseline) — базовая сборка прошла: это НЕ мутант и не проблема.

Пока есть ✗/⏱/⊘/непротестированный/неопознанный исход/битые детали — вердикт
НЕ может быть «чисто». Руками cargo-mutants НЕ запускать — только через devh mutate.

Problem-first (синхронный гейт внутри devh run):
  • первая проблема (✗/⏱/⊘/неопознанный исход/собран-но-не-протестирован)
    ОСТАНАВЛИВАЕТ гейт и уходит в fix-промпт: файл, строка, замена, жанр;
  • остановка бьёт ТОЛЬКО по своей process group — фоновый devh mutate идёт дальше;
  • выключается: MUTATE_FIRST_PROBLEM=0 (по умолчанию включено)."#;
// ─────────────────────────── каталоги и файлы ───────────────────────────

fn work_dir(root: &Path) -> PathBuf {
    root.join(".harness").join(WORK)
}

/// Каталог прогона cargo-mutants (`mutants.out` или `<--output>/mutants.out`).
fn audit_dir(root: &Path, args: &[String]) -> PathBuf {
    let cmd = format!("cargo mutants {}", args.join(" "));
    root.join(early::mutants_out_dir(&cmd))
}

/// Состояние прогона (пишется супервизором).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// "running" | "done" | "failed" | "stopped".
    pub state: String,
    /// pid супервизора.
    pub pid: i32,
    /// Код возврата cargo-mutants (None — ещё идёт).
    pub exit: Option<i32>,
    /// Метка времени старта (unix secs).
    pub started: u64,
    /// Метка времени финала (unix secs) — None, пока идёт.
    pub finished: Option<u64>,
    /// Команда cargo-mutants (для отчёта).
    pub args: Vec<String>,
    /// Рабочий каталог прогона.
    pub cwd: String,
    /// Режим: "copy" (фоновый) — in-place только у синхронного гейта.
    pub mode: String,
    /// Группа процессов cargo-mutants (её гасит `devh mutate stop`).
    pub pgid: i32,
    /// F-1.8.1: отпечаток дерева на момент старта. Пусто — старый state.json;
    /// тогда stale-проверка выключена (нельзя доказать протухание).
    #[serde(default)]
    pub tree_stamp: String,
}

fn read_state(root: &Path) -> Option<State> {
    let text = fs::read_to_string(work_dir(root).join(STATE_FILE)).ok()?;
    serde_json::from_str::<State>(&text).ok()
}

fn write_state(root: &Path, st: &State) -> anyhow::Result<()> {
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    let path = dir.join(STATE_FILE);
    let tmp = dir.join(".state.json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(st).unwrap_or_default())?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Есть ли живой прогон прямо сейчас.
fn running(root: &Path) -> Option<State> {
    let st = read_state(root)?;
    if st.state == "running" && pid_alive(st.pid) {
        Some(st)
    } else {
        None
    }
}

/// Прогон заявлен, но супервизор мёртв — аудит оборвался (не «чисто»).
fn aborted(root: &Path) -> Option<State> {
    let st = read_state(root)?;
    if st.state == "running" && !pid_alive(st.pid) {
        Some(st)
    } else {
        None
    }
}

/// Прогон остановлен по требованию оператора: результата НЕТ (часть
/// мутантов могла не дойти) — вердикт выносить не по чему (rc=2).
fn stopped(root: &Path) -> Option<State> {
    let st = read_state(root)?;
    if st.state == "stopped" {
        Some(st)
    } else {
        None
    }
}

/// Состояние аудита — для внешних потребителей (оракул `devh next`).
///
/// Один источник истины: и `verdict`, и `status`, и оракул смотрят в одно
/// и то же состояние, иначе оракул может предложить шаг, противоречащий
/// тому, что реально происходит с аудитом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditState {
    /// Аудит не запускался.
    None,
    /// Идёт прямо сейчас (супервизор жив).
    Running,
    /// Завершён — есть результат (можно выносить вердикт).
    Finished,
    /// Остановлен оператором — результата НЕТ (часть мутантов не дошла).
    Stopped,
    /// Супервизор умер, состояние осталось «running» — результата НЕТ.
    Aborted,
}

/// Текущее состояние аудита.
pub fn audit_state(root: &Path) -> AuditState {
    let Some(st) = read_state(root) else {
        return AuditState::None;
    };
    match st.state.as_str() {
        "running" => {
            if pid_alive(st.pid) {
                AuditState::Running
            } else {
                AuditState::Aborted
            }
        }
        "stopped" => AuditState::Stopped,
        "done" | "failed" => AuditState::Finished,
        _ => AuditState::None,
    }
}

/// Флаги cargo-mutants последнего прогона (пусто — прогона не было).
///
/// Оракул `devh next` перезапускает аудит ТОЙ ЖЕ командой: «перезапусти
/// аудит» без флагов — это неисполняемая команда.
pub fn state_args(root: &Path) -> Vec<String> {
    read_state(root).map(|s| s.args).unwrap_or_default()
}

/// F-1.8.1: отпечаток содержимого дерева (tracked + untracked, без ignored).
///
/// Ошибка git не ломает аудит: пустой отпечаток означает «протухание
/// доказать нельзя», а не «всё свежее». Молчаливое «свежее» здесь было бы
/// ложью ровно в тот момент, когда git сломан.
fn tree_stamp(root: &Path) -> String {
    let Ok(entries) = crate::shell::git::tree_entries(root) else {
        return String::new();
    };
    // Документация не влияет на cargo-mutants: правка docs/*.md во время
    // аудита не должна объявлять результат протухшим (ложный перепрогон).
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|e| audit_relevant(&e.path))
        .collect();
    crate::core::digest::tree_digest(&entries).unwrap_or_default()
}

/// Входит ли файл в отпечаток аудита. Артефакты самого аудита и
/// документация не влияют на cargo-mutants: иначе `mutants.out`/`.harness`
/// делали бы снимок протухшим в момент старта.
fn audit_relevant(path: &str) -> bool {
    if path.starts_with("docs/") || path.ends_with(".md") {
        return false;
    }
    !path
        .split('/')
        .any(|c| c == ".git" || c == "target" || c == ".harness" || c.starts_with("mutants.out"))
}

/// Дерево изменилось после старта аудита?
///
/// `true` — `problems.json`/`verdict` относятся к снимку ДО правок. Оракул
/// `devh next` не должен вести агента чинить уже исправленное. Пустой
/// отпечаток (старый state.json / git недоступен) — `false`: поведение
/// прежнее, без ложных «протухло».
pub fn audit_stale(root: &Path) -> bool {
    let Some(st) = read_state(root) else {
        return false;
    };
    if st.tree_stamp.is_empty() {
        return false;
    }
    let now = tree_stamp(root);
    !now.is_empty() && now != st.tree_stamp
}

/// Флаги cargo-mutants из состояния (или из аргументов — если состояния нет).
fn effective_args(root: &Path, args: &[String]) -> Vec<String> {
    match read_state(root) {
        Some(s) if !s.args.is_empty() => s.args.clone(),
        _ => args.iter().filter(|a| *a != "--").cloned().collect(),
    }
}

// ──────────────────────────────── start ────────────────────────────────

/// Запуск аудита: супервизор в отдельной сессии + подсказка «что дальше».
/// Чистая функция-гейт: можно ли запускать мутационный аудит.
///
/// Проверяет: (1) caller == lead (или lead не назначен — fail-open),
/// (2) явный охват (файлы/--in-diff), (3) cadence & load через core::rate.
/// Возвращает Ok(()) если можно, Err с текстом иначе.
pub fn mutate_start_gate(
    caller: &str,
    lead: Option<&str>,
    args: &[String],
    last_run_secs: Option<u64>,
    now_secs: u64,
    load_pct: u32,
) -> Result<(), String> {
    // Lead check.
    if let Some(lead_addr) = lead {
        if caller != lead_addr {
            return Err(format!(
                "мутационный аудит запускает только ведущий (lead={})",
                lead_addr
            ));
        }
    }
    // Scope check: нужен явный охват непроверенной зоны.
    let has_explicit_scope = args.iter().any(|a| a.starts_with("--in-diff="))
        || args
            .iter()
            .any(|a| !a.starts_with("-") && std::path::Path::new(a).exists());
    if !has_explicit_scope {
        return Err(
            "нужен явный охват непроверенной зоны: укажите файлы или --in-diff <дифф>".to_string(),
        );
    }
    // Cadence & load.
    let policy = rate::RatePolicy::default();
    let decision = rate::decide(
        RateKind::Mutation,
        now_secs,
        last_run_secs,
        load_pct,
        &policy,
    );
    match decision {
        rate::Decision::Allowed => Ok(()),
        rate::Decision::TooSoon { retry_in_secs } => Err(format!(
            "мутационный аудит: прошло менее 2 ч с прошлого (осталось {} с) — подожди",
            retry_in_secs
        )),
        rate::Decision::LoadTooHigh { load_pct, limit } => Err(format!(
            "system load {}% > лимит {}% — тяжёлые прогоны запрещены",
            load_pct, limit
        )),
    }
}

fn start(root: &Path, args: &[String]) -> anyhow::Result<()> {
    if let Some(st) = running(root) {
        println!(
            "аудит уже идёт (супервизор pid {}) — смотри: devh mutate status",
            st.pid
        );
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    let args: Vec<String> = args.iter().filter(|a| *a != "--").cloned().collect();
    if args.is_empty() {
        help();
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    // T-034: машинный гейт.
    let reg = crate::commands::orch::load(root);
    let (lead, _lead_src) = crate::commands::orch::lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let last_audit = if let Some(st) = running(root) {
        Some(st.started)
    } else {
        let state_path = work_dir(root).join(STATE_FILE);
        if let Ok(text) = std::fs::read_to_string(&state_path) {
            serde_json::from_str::<State>(&text).ok().map(|s| s.started)
        } else {
            None
        }
    };
    let load_pct = crate::core::load::load_average()
        .map(|l| l as u32)
        .unwrap_or(0);
    match mutate_start_gate(
        &caller,
        lead.as_deref(),
        &args,
        last_audit,
        crate::core::identity::unix_now(),
        load_pct,
    ) {
        Ok(()) => {}
        Err(msg) => {
            eprintln!("devh mutate start: {}", msg);
            crate::commands::die(crate::commands::ExitCode::EnvError);
        }
    }
    // R11: фоновый аудит не мутирует дерево агента.
    if args
        .iter()
        .any(|a| a == "--in-place" || a.starts_with("--in-place="))
    {
        println!(
            "devh mutate start: --in-place запрещён для фонового аудита — он мутирует\n\
             дерево агента (R11). Оставьте флаги как есть: cargo-mutants сам работает\n\
             в copy-режиме. In-place гейт живёт внутри devh run (там своя worktree)."
        );
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    // Тяжёлый вход: СВЕЖАЯ проверка нагрузки (не устаревший снимок).
    // Под контенцией мутации стоят ×2–4 (§9.10) — лучше не начинать вовсе.
    // Неизвестная нагрузка (нет /proc/loadavg) гейт НЕ блокирует.
    if let Some(load) = crate::core::load::load_average() {
        let env = std::env::var("HARNESS_MUTATE_MAX_LOAD").ok();
        if let Some(why) = crate::core::load::block_reason(env.as_deref(), load) {
            println!("devh mutate start: {why}");
            println!("аудит не запущен — дождись, пока соседний прогон освободит CPU");
            crate::commands::die(crate::commands::ExitCode::EnvError);
        }
    }
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    // P5 R10: два прогона с ОДНИМ output не должны убивать артефакты друг
    // друга. Лок = проверка «каталог не переписывается прямо сейчас»;
    // на время прогона его держит супервизор (см. supervise). Держатель
    // этого короткого лока — сам start: уборка файлов + spawn + state.
    let _output_lock =
        match OutputLock::try_acquire(root, &audit_dir(root, &args), "devh mutate start") {
            Ok(g) => g,
            Err(holder) => {
                println!("devh mutate start: output занят — {holder}");
                println!(
                    "  тот же каталог аудита уже переписывается — дождись конца чужого прогона"
                );
                crate::commands::die(crate::commands::ExitCode::EnvError);
            }
        };
    // Старые артефакты харнесса — вон (каталог самого прогона убирает
    // супервизор ПОД локом: см. supervise). Списки append-only, иначе
    // счётчики начнутся не с нуля (а verdict считал бы мутанты прошлого).
    let _ = fs::remove_file(dir.join(LIVE_FILE));
    let _ = fs::remove_file(dir.join(VERDICT_FILE));
    let _ = fs::remove_file(dir.join(PROBLEMS_FILE));
    let _ = fs::remove_file(dir.join(CARGO_LOG));
    let _ = fs::remove_file(dir.join(STOP_MARKER));

    // Супервизор: setsid → своя сессия, таймаут tool'а его не достаёт.
    let exe = std::env::current_exe()?;
    let sup_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(SUP_LOG))?;
    let sup_err = sup_log.try_clone()?;
    let mut cmd = Command::new(exe);
    cmd.arg("mutate")
        .arg("run")
        .arg("--")
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(sup_log))
        .stderr(Stdio::from(sup_err));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // Пишем состояние СРАЗУ: `status` должен видеть «идёт» ещё до первого
    // кадра (cargo-mutants стартует не мгновенно).
    write_state(
        root,
        &State {
            state: "running".to_string(),
            pid: child.id() as i32,
            exit: None,
            started: now_secs(),
            finished: None,
            args: args.clone(),
            cwd: root.display().to_string(),
            mode: "copy".to_string(),
            pgid: 0,
            tree_stamp: tree_stamp(root),
        },
    )?;
    println!("аудит запущен (супервизор pid {})", child.id());
    println!("  cargo-mutants: {}", args.join(" "));
    println!("  режим:         copy (дерево агента не мутируется)");
    println!("  лог прогона:   {}", dir.join(CARGO_LOG).display());
    println!("  живой кадр:   {}", dir.join(LIVE_FILE).display());
    println!();
    println!("дальше:");
    println!("  devh mutate status    # снимок прямо сейчас");
    println!("  devh mutate problems  # что чинить (rc=1 — есть проблемы)");
    println!("  devh mutate watch     # живой CLI в этом терминале");
    println!("  devh mutate verdict   # финальный вердикт (код возврата = гейт)");
    Ok(())
}

// ────────────────────────── супервизор (внутренний) ──────────────────────────

/// Супервизор: держит cargo-mutants ребёнком, рисует кадры, пишет вердикт.
/// Взять лок output для прогона. `start` держит тот же лок первые мс
/// (проверка + уборка файлов + запись state + spawn) — это окно ждём
/// коротким ретраем; если лок занят дольше — там чужой прогон.
fn acquire_output_for_run(root: &Path, output: &Path) -> anyhow::Result<OutputLock> {
    const RETRIES: usize = 50;
    let mut last = String::new();
    for i in 0..RETRIES {
        match OutputLock::try_acquire(root, output, "cargo-mutants") {
            Ok(g) => return Ok(g),
            Err(holder) => {
                last = holder;
                if i + 1 < RETRIES {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    anyhow::bail!("output занят — {last}")
}

fn supervise(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let args: Vec<String> = args.iter().filter(|a| *a != "--").cloned().collect();
    if args.is_empty() {
        anyhow::bail!("devh mutate run: нет аргументов cargo-mutants");
    }
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    let adir = audit_dir(root, &args);
    // P5 R10: лок на каталог output держится ВЕСЬ прогон (flock снимается
    // сам при смерти супервизора). Start держал тот же лок первые мс —
    // короткое окно ждём, дальше занятость = чужой прогон.
    let _output_lock = match acquire_output_for_run(root, &adir) {
        Ok(g) => g,
        Err(e) => {
            // Start уже объявил «running» — этот прогон НЕ идёт: гасим
            // метку, иначе status/verdict ждали бы вечно.
            write_state(
                root,
                &State {
                    state: "failed".to_string(),
                    pid: 0,
                    exit: Some(3),
                    started: now_secs(),
                    finished: Some(now_secs()),
                    args: args.clone(),
                    cwd: root.display().to_string(),
                    mode: "copy".to_string(),
                    pgid: 0,
                    tree_stamp: String::new(),
                },
            )?;
            return Err(e);
        }
    };
    // Старый прогон — вон ПОД локом: параллельный `devh mutate start` того
    // же output уже получил rc=2, а читатели (status/problems) не должны
    // видеть полуразобранный каталог.
    let _ = fs::remove_dir_all(&adir);
    let started = now_secs();
    // F-1.8.1: аудит копирует дерево СЕЙЧАС. Отпечаток фиксируем до запуска
    // cargo-mutants: если дерево потом изменится, результат — снимок ДО правок.
    let tree_stamp = tree_stamp(root);
    // Старый вердикт прошлого прогона — в нём «чисто/дыра» для ДРУГОГО
    // аудита. Убираем на старте: пока новый вердикт не записан, его нет
    // (fail-closed), а не «протухший зелёный».
    let _ = fs::remove_file(dir.join(VERDICT_FILE));
    write_state(
        root,
        &State {
            state: "running".to_string(),
            pid: std::process::id() as i32,
            exit: None,
            started,
            finished: None,
            args: args.clone(),
            cwd: root.display().to_string(),
            mode: "copy".to_string(),
            pgid: 0,
            tree_stamp: tree_stamp.clone(),
        },
    )?;

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(CARGO_LOG))?;
    let err = log.try_clone()?;
    let mut cmd = Command::new("cargo");
    cmd.arg("mutants")
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    // Своя группа процессов: `devh mutate stop` гасит её целиком, и
    // cargo-mutants не получает Ctrl-C из терминала агента.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            write_state(
                root,
                &State {
                    state: "failed".to_string(),
                    pid: 0,
                    exit: Some(127),
                    started,
                    finished: Some(now_secs()),
                    args: args.clone(),
                    cwd: root.display().to_string(),
                    mode: "copy".to_string(),
                    pgid: 0,
                    tree_stamp: tree_stamp.clone(),
                },
            )?;
            anyhow::bail!("devh mutate: cargo mutants не запустился ({e})");
        }
    };
    let pgid = child.id() as i32;
    if let Some(mut st) = read_state(root) {
        st.pgid = pgid;
        let _ = write_state(root, &st);
    }

    // Наблюдатель: каталог прогона + его родитель (cargo-mutants создаёт
    // mutants.out ПОСЛЕ старта) + завершение процесса. Каталог артефактов
    // (`dir`) НЕ наблюдаем: супервизор сам пишет туда live.log/state.json —
    // иначе каждое событие порождает новый кадр, кадр — новое событие
    // (обратная связь: live.log рос на мегабайты в секунду).
    let mut watcher = match Watcher::new(&[&adir, root]) {
        Ok(w) => w,
        Err(e) => {
            anyhow::bail!("devh mutate: inotify недоступен ({e}) — аудит без наблюдения");
        }
    };
    watcher.watch_child(child);

    // Problem-first: помним, какие проблемы УЖЕ доложили (по отпечатку),
    // чтобы не повторяться, но обновить список, когда придут детали
    // (замена/жанр) из outcomes.json.
    let mut reported: BTreeMap<String, String> = BTreeMap::new();
    let mut exited = false;
    let mut stop_sent = false;
    while !exited {
        // Запрос остановки (`devh mutate stop` положил маркер) проверяем на
        // КАЖДОЙ итерации, включая таймауты: каталог артефактов больше не
        // под наблюдением (см. выше), поэтому события о маркере не будет.
        // Супервизор САМ гасит своего ребёнка: он владеет завершением
        // cargo-mutants. Группа ребёнка отдельная (setpgid), супервизор в
        // неё не входит.
        if !stop_sent && dir.join(STOP_MARKER).exists() {
            stop_sent = true;
            // Гасим ТОЛЬКО группу ребёнка: группа супервизора — он сам
            // (setsid), её трогать нельзя, иначе не запишется финальный
            // статус.
            if pgid > 1 && pgid != std::process::id() as i32 {
                // -pgid sends to process group; pgid > 1 so 0 - pgid cannot overflow i32.
                let neg_pgid = 0i32 - pgid;
                unsafe {
                    libc::kill(neg_pgid, libc::SIGTERM);
                }
            }
        }
        let events = match watcher.wait(POLL_MS) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        for ev in events {
            match ev {
                Event::Exited => {
                    exited = true;
                }
                Event::Changed(_) | Event::DirCreated(_) => {
                    // Кадр рисуем по СОБЫТИЮ, а не по таймеру.
                    let audit = snapshot(root, &args);
                    write_frame(root, &args, &dir, false)?;
                    // Problem-first: проблема попадает в отчёт СРАЗУ.
                    report_new_problems(&dir, &audit, &mut reported)?;
                }
                Event::Timeout => {}
            }
        }
    }
    let exit = watcher.exit_code();

    // Финальный кадр + полный отчёт — в live.log (чтобы «история» прогона
    // осталась в одном месте) и в verdict.json (машиночитаемо).
    write_frame(root, &args, &dir, true)?;
    let audit = snapshot(root, &args);
    write_problems(&dir, &audit)?;
    let stopped = dir.join(STOP_MARKER).exists();
    let verdict = serde_json::json!({
        "exit": exit,
        "stopped": stopped,
        "caught": audit.counts.caught,
        "missed": audit.counts.missed,
        "timeout": audit.counts.timeout,
        "unviable": audit.counts.unviable,
        "unknown": audit.unknown,
        "succeeded": audit.succeeded,
        "total": audit.total,
        "done": audit.done,
        "kill_rate": audit.kill_rate(),
        "hole": audit.has_hole(),
        "malformed": audit.malformed,
        "details_missing": audit.details_missing,
        "details_mismatch": audit.details_mismatch,
        "tool_problem": audit.tool_problem,
        "problems": problem_json(&audit),
    });
    // Вердикт — машинночитаемый артефакт: пишем АТОМАРНО (tmp+rename),
    // иначе читатель (оракул `devh next`, внешний скрипт) поймает файл на
    // середине записи — ровно та же беда, что с outcomes.json у
    // cargo-mutants (там File::create перетирает файл целиком).
    if let Err(e) = write_verdict_fail_closed(&dir, &verdict) {
        eprintln!(".. {e}");
    }
    let state = if stopped {
        "stopped".to_string()
    } else {
        match exit {
            Some(0) => "done".to_string(),
            Some(_) => "failed".to_string(),
            None => "stopped".to_string(),
        }
    };
    write_state(
        root,
        &State {
            state,
            pid: 0,
            exit,
            started,
            finished: Some(now_secs()),
            args: args.clone(),
            cwd: root.display().to_string(),
            mode: "copy".to_string(),
            pgid,
            tree_stamp,
        },
    )?;
    Ok(())
}

// ────────────────────────────── снимок и рендер ──────────────────────────────

/// Текущий снимок аудита (счётчики — live, детали — из списков и JSON).
fn snapshot(root: &Path, args: &[String]) -> Audit {
    let dir = audit_dir(root, args);
    let st = read_state(root);
    let alive = running(root).is_some();
    let exit = st.as_ref().and_then(|s| s.exit);
    mutaudit::load(&dir, alive, exit)
}

/// Кадр строки прогресса (легенда + строка) — то же, что рисует гейт.
fn frame(audit: &Audit, cur: &str, elapsed_secs: f64) -> String {
    let c = colors();
    let last = audit
        .bad
        .last()
        .map(|b| {
            let sym = match b.summary.as_str() {
                mutaudit::S_MISSED => "✗",
                mutaudit::S_UNVIABLE => "⊘",
                mutaudit::S_TIMEOUT => "⏱",
                _ => "?",
            };
            format!("{sym} {}", b.describe())
        })
        .unwrap_or_default();
    tui::line(tui::LineArgs {
        done: audit.done,
        total: audit.total,
        n: &audit.counts,
        avg_secs: audit.avg_secs,
        elapsed_secs,
        last: &last,
        cur,
        c: &c,
    })
}

/// Сколько идёт прогон (по времени старта из state.json).
///
/// После финала время ФИКСИРУЕТСЯ (`finished`): иначе «сколько шёл аудит»
/// продолжало бы расти после конца прогона.
fn elapsed_secs(root: &Path) -> f64 {
    match read_state(root) {
        Some(st) if st.started > 0 => {
            let end = st.finished.unwrap_or_else(now_secs);
            end.saturating_sub(st.started) as f64
        }
        _ => 0.0,
    }
}

/// Текущий мутант: самый свежий .diff в каталоге прогона.
fn current_mutant(dir: &Path) -> String {
    let diff_dir = dir.join("diff");
    let Ok(rd) = fs::read_dir(&diff_dir) else {
        return String::new();
    };
    let mut best: Option<(SystemTime, String)> = None;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("diff") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let newer = match &best {
            Some((t, _)) => modified > *t,
            None => true,
        };
        if newer {
            best = Some((modified, name.to_string()));
        }
    }
    best.map(|(_, n)| decode_diff_name(&n)).unwrap_or_default()
}

/// `devharness__src__core__plan.rs_line_19_col_5` → `devharness/src/core/plan.rs:19`.
fn decode_diff_name(raw: &str) -> String {
    let base = raw.strip_suffix(".diff").unwrap_or(raw);
    let (path, line) = match base.split_once("_line_") {
        Some((p, rest)) => {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            (p, num)
        }
        None => (base, String::new()),
    };
    let path = path.replace("__", "/");
    if line.is_empty() {
        path
    } else {
        format!("{path}:{line}")
    }
}

/// Один кадр в live.log (перезапись строки на месте) + в stdout (live).
fn write_frame(root: &Path, args: &[String], dir: &Path, final_frame: bool) -> anyhow::Result<()> {
    let audit = snapshot(root, args);
    let cur = current_mutant(&audit_dir(root, args));
    let line = frame(&audit, &cur, elapsed_secs(root));
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LIVE_FILE))?;
    if final_frame {
        // Финал: легенда + строка + отчёт, обычными переводами строк.
        writeln!(out, "{}", tui::legend(&colors()))?;
        writeln!(out, "{line}")?;
        for l in report_lines(&audit) {
            writeln!(out, "{l}")?;
        }
    } else {
        write!(out, "\r\x1b[K{line}")?;
    }
    Ok(())
}

/// Строки отчёта: что чинить дальше.
fn report_lines(audit: &Audit) -> Vec<String> {
    let mut v = Vec::new();
    if audit.alive {
        v.push("статус: прогон идёт".to_string());
    } else if audit.testable() == 0 {
        // kill_rate() при 0 тестируемых даёт условные 100% («не упало ни
        // одного из нуля») — это НЕ измеренный процент: тестов не было,
        // а судья в этой ситуации велит Fail («testable 0»). Печатаем
        // правду, а не условность, чтобы отчёт не читался как успех.
        v.push(format!(
            "статус: прогон завершён (rc {:?}), kill-rate не измерен (0 тестируемых)",
            audit.exit
        ));
    } else {
        v.push(format!(
            "статус: прогон завершён (rc {:?}), kill-rate {:.1}% ({} тестируемых)",
            audit.exit,
            audit.kill_rate(),
            audit.testable()
        ));
    }
    for l in problem_blocks(audit) {
        v.push(l);
    }
    if audit.succeeded > 0 {
        v.push(format!(
            "⚠ без тестовой фазы: {} мутант(ов) собраны, но НЕ протестированы — \
             поимка не подтверждена (это не killed и не missed)",
            audit.succeeded
        ));
    }
    if audit.unknown > 0 {
        v.push(format!(
            "⚠ неизвестный исход мутанта: {} — поимка не подтверждена",
            audit.unknown
        ));
    }
    if let Some(t) = &audit.tool_problem {
        v.push(format!("⚠ инструмент: {t}"));
    }
    if audit.malformed {
        v.push("⚠ детали аудита повреждены: outcomes.json не читается".to_string());
    }
    if audit.details_missing {
        v.push("⚠ детали аудита отсутствуют: outcomes.json нет, а мутанты были".to_string());
    }
    if audit.details_mismatch {
        v.push("⚠ детали аудита расходятся со счётчиками (outcomes.json ≠ списки)".to_string());
    }
    if !audit.alive {
        // `blocks_clean_verdict()` = «БЛОКИРУЕТ чистый вердикт» (true = дыра).
        // Поэтому «чисто» — это ровно НЕ блокирует.
        if audit.blocks_clean_verdict() {
            v.push("вердикт: НЕ чисто — есть непойманные мутанты (см. проблемы выше)".to_string());
        } else {
            v.push("вердикт: чисто — все мутанты пойманы".to_string());
        }
    }
    v
}

/// Блоки проблем — «руководство к действию» (файл:строка, замена, жанр).
fn problem_blocks(audit: &Audit) -> Vec<String> {
    audit.bad_lines()
}

/// Отпечаток проблемы: по нему видно, что проблема НОВАЯ (или что у неё
/// появились детали — замена/жанр из outcomes.json).
fn fingerprint(b: &mutaudit::Bad) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        b.summary, b.file, b.line, b.name, b.replacement, b.genre
    )
}

/// Машиночитаемый список проблем (для problems.json / --json).
fn problem_json(audit: &Audit) -> Vec<serde_json::Value> {
    audit
        .bad
        .iter()
        .map(|b| {
            serde_json::json!({
                "summary": b.summary,
                "file": b.file,
                "line": b.line,
                "name": b.name,
                "replacement": b.replacement,
                "genre": b.genre,
            })
        })
        .collect()
}

/// Записать `problems.json` (живой список проблем) — атомарно.
fn write_problems(dir: &Path, audit: &Audit) -> anyhow::Result<()> {
    let json = serde_json::json!({
        "updated": now_secs(),
        "count": audit.bad.len(),
        "problems": problem_json(audit),
    });
    write_json_atomic(dir, PROBLEMS_FILE, &json)
}

/// Атомарная запись JSON-артефакта: временный файл + rename в том же
/// каталоге. Читатель (devh next, внешний скрипт) никогда не видит файл
/// на середине записи.
fn write_json_atomic(dir: &Path, name: &str, value: &serde_json::Value) -> anyhow::Result<()> {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp"));
    fs::write(
        &tmp,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    )?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Записать машинночитаемый вердикт АТОМАРНО и fail-closed.
///
/// Ошибка записи = артефакт неполон: читать из него «чисто» нельзя. Старый
/// вердикт (прошлого прогона) при этом ГАСИМ — иначе читатель увидит
/// чужой вердикт и примет его за результат текущего прогона (ровно та же
/// беда, что с перезаписью outcomes.json у cargo-mutants).
fn write_verdict_fail_closed(dir: &Path, verdict: &serde_json::Value) -> anyhow::Result<()> {
    match write_json_atomic(dir, VERDICT_FILE, verdict) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(dir.join(VERDICT_FILE));
            Err(anyhow::anyhow!(
                "вердикт не записан ({e}) — артефакт неполон"
            ))
        }
    }
}

/// Problem-first: доложить ТОЛЬКО новые проблемы (и обновить список, когда
/// у уже известной проблемы появились детали — замена/жанр).
fn report_new_problems(
    dir: &Path,
    audit: &Audit,
    reported: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut changed = false;
    for b in &audit.bad {
        let fp = fingerprint(b);
        match reported.get(&fp) {
            Some(prev) if *prev == b.action() => {}
            _ => {
                reported.insert(fp.clone(), b.action());
                changed = true;
                let mut out = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(LIVE_FILE))?;
                writeln!(out, "!! {}", b.action())?;
            }
        }
    }
    if changed {
        write_problems(dir, audit)?;
    }
    Ok(())
}

// ──────────────────────────────── status ────────────────────────────────

/// Мгновенный снимок. Ничего не ждёт: читает файлы и печатает кадр.
fn status(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let eff = effective_args(root, args);
    let mut audit = snapshot(root, &eff);
    filter_rechecked(root, &mut audit);
    let aborted_state = aborted(root);
    let stale = audit_stale(root);
    // Диагноз ПЕРЕД «аудит не запущен»: аудит стартовал, а супервизор умер.
    // Сказать «не запущен» здесь = солгать оператору (он потом скажет
    // «а я его запускал!»).
    if let Some(st) = aborted_state.as_ref() {
        println!(
            "⚠ аудит оборвался: супервизор (pid {}) мёртв, состояние не завершено — результата нет",
            st.pid
        );
    }
    if audit.total.is_none() && audit.done == 0 && !audit.alive && aborted_state.is_none() {
        // Аудита нет вообще — это не «чисто», это «нечего смотреть».
        println!("аудит не запущен (нет mutants.out) — devh mutate start <args>");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    if stopped(root).is_some() {
        println!("⚠ аудит остановлен по требованию оператора — результата нет (вердикт = rc 2)");
    }
    if stale {
        println!(
            "⚠ дерево изменилось после старта аудита — это снимок ДО правок; \
             уже исправленные проблемы могут выглядеть живыми (нужен перепрогон)"
        );
    }
    if args.iter().any(|a| a == "--json") {
        let json = serde_json::json!({
            "done": audit.done,
            "total": audit.total,
            "caught": audit.counts.caught,
            "missed": audit.counts.missed,
            "timeout": audit.counts.timeout,
            "unviable": audit.counts.unviable,
            "unknown": audit.unknown,
            "succeeded": audit.succeeded,
            "kill_rate": audit.kill_rate(),
            "alive": audit.alive,
            "aborted": aborted_state.is_some(),
            "stale": stale,
            "exit": audit.exit,
            "malformed": audit.malformed,
            "details_missing": audit.details_missing,
            "details_mismatch": audit.details_mismatch,
            "tool_problem": audit.tool_problem,
            "blocks_clean": audit.blocks_clean_verdict() || aborted_state.is_some() || stale,
            "problems": problem_json(&audit),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
        // Оборванный аудит: результата нет → вердикт выносить не по чему
        // (rc=2, среда не готова), даже если частичные данные уже накопились.
        if aborted_state.is_some() {
            crate::commands::die(crate::commands::ExitCode::EnvError);
        }
        // Проблема видна СЕЙЧАС — даже если аудит ещё идёт (problem-first).
        if audit.blocks_clean_verdict() {
            crate::commands::die(crate::commands::ExitCode::ActionError);
        }
        return Ok(());
    }
    // Оборванный аудит: НЕ печатаем кадр и «вердикт» — при неполных данных
    // kill-rate 100% и «чисто» были бы ложью (наблюдалось: 0/226 и
    // «вердикт: чисто» при мёртвом супервизоре). Результата нет — rc=2.
    if aborted_state.is_some() {
        println!("аудит оборвался — результата нет, вердикт не выносим (devh next: перезапуск)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    let cur = current_mutant(&audit_dir(root, &eff));
    println!("{}", tui::legend(&colors()));
    println!("{}", frame(&audit, &cur, elapsed_secs(root)));
    for l in report_lines(&audit) {
        println!("{l}");
    }
    // Проблема видна СЕЙЧАС — даже если аудит ещё идёт (problem-first).
    if audit.blocks_clean_verdict() {
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    Ok(())
}

// ───────────────────────────────── watch ─────────────────────────────────

/// Опции `watch` (чистый парсер — тестируется без inotify).
#[derive(Debug, Default, PartialEq, Eq)]
struct WatchOpts {
    timeout: u64,
    until_problem: bool,
    batch_ms: u64,
}

fn parse_watch_opts(args: &[String]) -> WatchOpts {
    let mut o = WatchOpts::default();
    let mut i = 0;
    while i < args.len() {
        match args.get(i).map(String::as_str) {
            Some("--timeout") => {
                if let Some(v) = args.get(i.saturating_add(1)) {
                    o.timeout = v.parse().unwrap_or(0);
                }
                i = i.saturating_add(2);
            }
            Some("--batch-ms") => {
                if let Some(v) = args.get(i.saturating_add(1)) {
                    o.batch_ms = v.parse().unwrap_or(0);
                }
                i = i.saturating_add(2);
            }
            Some("--until-problem") => {
                o.until_problem = true;
                i = i.saturating_add(1);
            }
            _ => i = i.saturating_add(1),
        }
    }
    o
}

/// Живой CLI: строка обновляется на месте по событиям ФС.
///
/// `--until-problem` превращает наблюдение в блокирующий канал
/// «проблема → работа»: выход rc=1, как только в снимке появилась проблема.
/// `--batch-ms N` даёт окно накопления (события ФС, не sleep-цикл): похожие
/// проблемы приходят одной пачкой. Без флага поведение прежнее — смотреть до
/// финала/таймаута.
fn watch(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let opts = parse_watch_opts(args);
    let eff = effective_args(root, args);
    let adir = audit_dir(root, &eff);
    let start = now_secs();
    let first_audit = snapshot(root, &eff);
    if first_audit.total.is_none()
        && first_audit.done == 0
        && !first_audit.alive
        && aborted(root).is_none()
    {
        println!("аудит не запущен (нет mutants.out) — devh mutate start <args>");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    // Наблюдаем и каталог прогона, и каталог артефактов: супервизор пишет
    // туда финальные verdict/state — по этому событию watch замечает финал
    // сразу, а не по таймауту ожидания.
    let mut watcher = match Watcher::new(&[&adir, root, &work_dir(root)]) {
        Ok(w) => w,
        Err(e) => {
            println!("devh mutate watch: inotify недоступен ({e})");
            crate::commands::die(crate::commands::ExitCode::EnvError);
        }
    };
    let mut first = true;
    let mut problem_deadline: Option<Instant> = None;
    let mut last_problem_count = 0usize;
    loop {
        let audit = snapshot(root, &eff);
        let cur = current_mutant(&adir);
        let line = frame(&audit, &cur, elapsed_secs(root));
        if first {
            println!("{}", tui::legend(&colors()));
            first = false;
        }
        // Живой кадр: стираем строку ПЕРЕД записью (иначе кадр стирает сам
        // себя — грабля, из-за которой оператор видел «пустую» консоль).
        print!("\r\x1b[K{line}");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();

        // Problem-first: блокирующий канал для агента. Выходим не по
        // таймеру, а когда проблемы стабильны `batch_ms` (или сразу).
        if opts.until_problem && !audit.bad.is_empty() {
            let n = audit.bad.len();
            let now = Instant::now();
            if problem_deadline.is_none() || n != last_problem_count {
                problem_deadline = Some(now + Duration::from_millis(opts.batch_ms));
            }
            last_problem_count = n;
            let due = opts.batch_ms == 0 || problem_deadline.map(|d| now >= d).unwrap_or(false);
            if due {
                println!();
                println!("проблем: {n}");
                for l in problem_blocks(&audit) {
                    println!("  {l}");
                }
                crate::commands::die(crate::commands::ExitCode::ActionError);
            }
        }

        if !audit.alive {
            println!();
            // Мёртвый супервизор с незавершённым состоянием — результата
            // нет: «вердикт: чисто» по частичным данным был бы ложью.
            if aborted(root).is_some() {
                println!(
                    "⚠ аудит оборвался: результата нет, вердикт не выносим \
                     (devh next: перезапуск)"
                );
                crate::commands::die(crate::commands::ExitCode::EnvError);
            }
            if stopped(root).is_some() {
                println!("⚠ аудит остановлен — результата нет (вердикт = rc 2)");
                crate::commands::die(crate::commands::ExitCode::EnvError);
            }
            for l in report_lines(&audit) {
                println!("{l}");
            }
            // Проблема — немедленно код возврата 1 (problem-first), даже
            // если аудит только что закончился.
            if audit.blocks_clean_verdict() {
                crate::commands::die(crate::commands::ExitCode::ActionError);
            }
            return Ok(());
        }
        if opts.timeout > 0 && now_secs().saturating_sub(start) >= opts.timeout {
            println!();
            println!(
                "таймаут ожидания (--timeout {}s) — аудит продолжается",
                opts.timeout
            );
            return Ok(());
        }
        // Ждём СОБЫТИЯ (изменение файла прогона), а не такта. Если задано
        // окно накопления — просыпаемся не позже его дедлайна.
        let wait_ms = problem_deadline
            .map(|d| {
                d.saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(POLL_MS as u128) as i32
            })
            .unwrap_or(POLL_MS)
            .max(1);
        match watcher.wait(wait_ms) {
            Ok(events) => {
                let changed = events
                    .iter()
                    .any(|e| matches!(e, Event::Changed(_) | Event::DirCreated(_)));
                if !changed {
                    continue;
                }
            }
            Err(e) => {
                println!("\nнаблюдатель отвалился: {e}");
                crate::commands::die(crate::commands::ExitCode::InternalError);
            }
        }
    }
}

// ─────────────────────────────── problems ───────────────────────────────

/// Руководство к действию: что именно чинить.
fn problems(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let eff = effective_args(root, args);
    let mut audit = snapshot(root, &eff);
    filter_rechecked(root, &mut audit);
    let aborted_state = aborted(root);
    if audit.total.is_none() && audit.done == 0 && !audit.alive && aborted_state.is_none() {
        println!("аудит не запущен — нечего чинить (devh mutate start <args>)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    // Проблемы уровня инструмента/деталей — тоже проблемы: чинить нечего,
    // но и «проблем нет» сказать нельзя.
    let mut extra: Vec<String> = Vec::new();
    let stale = audit_stale(root);
    if stopped(root).is_some() {
        extra.push(
            "⚠ аудит остановлен по требованию оператора — результата нет (вердикт = rc 2)"
                .to_string(),
        );
    }
    if stale {
        extra.push(
            "⚠ дерево изменилось после старта аудита — часть проблем может быть уже \
             исправлена (снимок ДО правок); сверься с текущим кодом, не чини вслепую"
                .to_string(),
        );
    }
    if let Some(st) = aborted_state {
        extra.push(format!(
            "⚠ аудит оборвался: супервизор (pid {}) мёртв, состояние не завершено — \
             результата нет (вердикт = rc 2), перезапустить тем же прогоном",
            st.pid
        ));
    }
    if let Some(t) = &audit.tool_problem {
        extra.push(format!("⚠ инструмент: {t}"));
    }
    if audit.succeeded > 0 {
        extra.push(format!(
            "⚠ без тестовой фазы: {} мутант(ов) собраны, но НЕ протестированы — \
             поимка не подтверждена (это не killed и не missed)",
            audit.succeeded
        ));
    }
    if audit.unknown > 0 {
        extra.push(format!(
            "⚠ неизвестный исход мутанта: {} — поимка не подтверждена",
            audit.unknown
        ));
    }
    if audit.malformed {
        extra.push("⚠ детали аудита повреждены: outcomes.json не читается".to_string());
    }
    if audit.details_missing {
        extra.push("⚠ детали аудита отсутствуют: outcomes.json нет, а мутанты были".to_string());
    }
    if audit.details_mismatch {
        extra.push("⚠ детали аудита расходятся со счётчиками (outcomes.json ≠ списки)".to_string());
    }
    if args.iter().any(|a| a == "--json") {
        let json = serde_json::json!({
            "count": audit.bad.len(),
            "stale": stale,
            "problems": problem_json(&audit),
            "tool_problem": audit.tool_problem,
            "malformed": audit.malformed,
            "details_missing": audit.details_missing,
            "details_mismatch": audit.details_mismatch,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
        if !audit.bad.is_empty() || !extra.is_empty() {
            crate::commands::die(crate::commands::ExitCode::ActionError);
        }
        return Ok(());
    }
    if audit.bad.is_empty() && extra.is_empty() {
        println!(
            "проблем нет: поймано {caught}, выжило {missed}, таймаут {timeout}, \
             не компилится {unviable}",
            caught = audit.counts.caught,
            missed = audit.counts.missed,
            timeout = audit.counts.timeout,
            unviable = audit.counts.unviable,
        );
        return Ok(());
    }
    println!("проблем: {}", audit.bad.len());
    for l in problem_blocks(&audit) {
        println!("  {l}");
    }
    for l in extra {
        println!("  {l}");
    }
    crate::commands::die(crate::commands::ExitCode::ActionError);
}

// ──────────────────────────────── recheck ────────────────────────────────

const RECHECK_FILE: &str = "rechecked.json";

/// Убрать из `--re`-флагов и `--output` старые значения: recheck задаёт свои.
fn recheck_args(base: &[String], name: &str, out: &Path) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < base.len() {
        let a = &base[i];
        let takes = matches!(
            a.as_str(),
            "--output" | "-o" | "--re" | "-F" | "--exclude-re" | "-E" | "--list" | "--in-diff"
        );
        if takes {
            i = i.saturating_add(2);
            continue;
        }
        if a.starts_with("--output=")
            || a.starts_with("--re=")
            || a.starts_with("--exclude-re=")
            || a == "--iterate"
            || a == "--in-place"
        {
            i = i.saturating_add(1);
            continue;
        }
        args.push(a.clone());
        i = i.saturating_add(1);
    }
    args.push("--re".to_string());
    args.push(format!("^{}$", regex::escape(name)));
    args.push("--output".to_string());
    args.push(out.display().to_string());
    args
}

/// Исход ОДНОГО мутанта в каталоге output прогона (`<--output>/mutants.out`).
///
/// `dir` считает вызывающий через [`audit_dir`] — единую формулу
/// (`early::mutants_out_dir`), тут только чтение. `replacement`/`genre`
/// берутся из `outcomes.json` через [`mutaudit::load`]: append-списки дают
/// только имена. Возвращает `(пойман, исход)`; `None` — имени в output нет.
///
/// Пойманный мутант в списки плохих НЕ попадает (`BAD_LISTS` — это
/// missed/timeout/unviable) — его ищем в `caught.txt`.
fn recheck_find(dir: &Path, name: &str, exit: Option<i32>) -> Option<(bool, mutaudit::Bad)> {
    let (qfile, qline, qdesc) = mutaudit::parse_list_name(name);
    let audit = mutaudit::load(dir, false, exit);
    let found = audit.bad.into_iter().find(|b| {
        // Точное имя (детали из outcomes.json) либо — пока JSON не дописан —
        // тот же мутант по файлу/строке/описанию из append-списка.
        b.name == name
            || (b.file == qfile && b.line == qline && (b.name == qdesc || b.name.is_empty()))
    });
    if let Some(b) = found {
        return Some((false, b));
    }
    let caught_txt = fs::read_to_string(dir.join("caught.txt")).unwrap_or_default();
    if !caught_txt.lines().any(|l| l.trim() == name) {
        return None;
    }
    Some((
        true,
        mutaudit::Bad {
            summary: mutaudit::S_CAUGHT.to_string(),
            file: qfile,
            line: qline,
            name: name.to_string(),
            replacement: String::new(),
            genre: String::new(),
        },
    ))
}

/// Три РАЗНЫХ причины «исхода нет» — одна строка «исход не найден» врали бы
/// оператору (каша из «инструмент не записал прогон» и «другое имя»).
///
/// «исход не найден» — только когда output СОЗДАН и `outcomes.json` есть,
/// а имени в нём нет.
fn recheck_miss_reason(dir: &Path, name: &str, rc: Option<i32>, log: &str) -> String {
    if !dir.is_dir() {
        return format!(
            "recheck: инструмент не создал output ({}) — cargo-mutants не записал прогон (rc {rc:?})\n{log}",
            dir.display()
        );
    }
    if !dir.join("outcomes.json").exists() {
        return format!(
            "recheck: в {} нет outcomes.json — прогон не дописан (rc {rc:?})\n{log}",
            dir.display()
        );
    }
    format!(
        "recheck: исход для {name:?} не найден в {} — такого мутанта в прогоне нет (rc {rc:?})\n{log}",
        dir.display()
    )
}

/// `devh mutate recheck <name>`: один мутант по имени, отдельный output.
/// Результат — overlay `rechecked.json`; вердикт по-прежнему требует
/// перепрогона (recheck не доказывает «чисто» для всего аудита).
fn recheck(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let name = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .next()
        .cloned()
        .unwrap_or_default();
    if name.trim().is_empty() {
        println!("devh mutate recheck <имя мутанта> — имя из devh mutate problems");
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    let base = effective_args(root, &[]);
    if base.is_empty() {
        println!("аудит не запускался — нет флагов для recheck (devh mutate start <args>)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    let out = work_dir(root).join("recheck");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out)?;
    let args = recheck_args(&base, &name, &out);
    println!(".. recheck: cargo mutants {}", args.join(" "));
    let res = Command::new("timeout")
        .arg("600")
        .arg("cargo")
        .arg("mutants")
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()?;
    let stdout = String::from_utf8_lossy(&res.stdout);
    let stderr = String::from_utf8_lossy(&res.stderr);
    let rc = res.status.code();
    // Каталог прогона — ЕДИНАЯ формула (`--output <out>` → `<out>/mutants.out`):
    // раньше тут читали сам `&out` — там никогда ничего не было, и recheck
    // всегда отвечал «исход не найден» (rc=2), фича не работала ни разу.
    let dir = audit_dir(root, &args);
    let log = format!("{}\n{}", tail(&stdout, 12), tail(&stderr, 12));
    let Some((caught, found)) = recheck_find(&dir, &name, rc) else {
        println!("{}", recheck_miss_reason(&dir, &name, rc, &log));
        crate::commands::die(crate::commands::ExitCode::EnvError);
    };
    let json = serde_json::json!({
        "updated": now_secs(),
        "tree_stamp": tree_stamp(root),
        "name": found.name,
        "summary": found.summary,
        "file": found.file,
        "line": found.line,
        "replacement": found.replacement,
        "genre": found.genre,
        "caught": caught,
    });
    write_json_atomic(&work_dir(root), RECHECK_FILE, &json)?;
    if caught {
        println!(
            "recheck: ✓ {} пойман на текущем дереве — проблема закрыта",
            found.name
        );
        Ok(())
    } else {
        println!("recheck: {}", mutaudit::bad_line(&found));
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Overlay: убрать из списка проблемы, перепроверенные как Caught на
/// ТЕКУЩЕМ дереве. Старый recheck (другое дерево) не применяется.
fn filter_rechecked(root: &Path, audit: &mut Audit) {
    let Ok(text) = fs::read_to_string(work_dir(root).join(RECHECK_FILE)) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if v.get("tree_stamp").and_then(|x| x.as_str()) != Some(tree_stamp(root).as_str()) {
        return;
    }
    if v.get("caught").and_then(|x| x.as_bool()) != Some(true) {
        return;
    }
    let Some(name) = v.get("name").and_then(|x| x.as_str()) else {
        return;
    };
    audit.bad.retain(|b| b.name != name);
}

// ──────────────────────────────── verdict ────────────────────────────────

/// Финальный вердикт. Код возврата — гейт: 0 чисто, 1 дыра, 2 не закончен.
fn verdict(root: &Path) -> anyhow::Result<()> {
    let eff = effective_args(root, &[]);
    let audit = snapshot(root, &eff);
    if audit.alive {
        println!("прогон ещё идёт — финальный вердикт не выносим (devh mutate status)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    // Остановленный прогон: часть мутантов могла не дойти → результата нет.
    if stopped(root).is_some() {
        println!("аудит остановлен по требованию оператора — результата нет, вердикт не выносим");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    // Супервизор мёртв, а состояние — «running»: результата НЕТ. Проверка
    // ДО любой арифметики: упавший аудит с частичными данными не должен
    // выглядеть «чисто» (раньше это вылезало, если mutants.out успел
    // накопить данные — вердикт считался по неполной выборке).
    if let Some(st) = aborted(root) {
        println!(
            "аудит оборвался (супервизор pid {} мёртв, состояние не завершено) — \
             результата нет, вердикт не выносим",
            st.pid
        );
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    if audit.total.is_none() && audit.done == 0 {
        println!("аудит не запущен — нечего вердиктовать (devh mutate start <args>)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    if audit_stale(root) {
        println!(
            "вердикт: не выносим — дерево изменилось после старта аудита, \
             результат относится к старому снимку; перепрогони тем же прогоном"
        );
        crate::commands::die(crate::commands::ExitCode::EnvError);
    }
    for l in report_lines(&audit) {
        println!("{l}");
    }
    // Неопознанный исход мутанта — БЛОКЕР «чистого» вердикта: поимка не
    // подтверждена (инструмент отработал непостижимо). Общий судья считает
    // такие мутанты в знаменателе, но сам по себе вердикт не выносит —
    // поэтому блокируем ЯВНО, до общей арифметики kill-rate.
    if audit.unknown > 0 {
        println!(
            "вердикт: ПРОВАЛ — неизвестный исход мутанта: {} — поимка не подтверждена",
            audit.unknown
        );
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    // Канонический судья — тот же, что у синхронного гейта (F-3/R5):
    // расхождение двух входов = «красивый» отчёт в одном и честный провал
    // в другом.
    let stats = MutationStats {
        // Мутантов «с исходом» = все, кто получил исход: пойманные + дыры +
        // те, у кого тестовой фазы не было + неопознанные. Ровно то же
        // число, что cargo-mutants печатает как «N mutants tested».
        mutants: audit
            .done
            .saturating_add(audit.succeeded)
            .saturating_add(audit.unknown),
        killed: audit.killed(),
        missed: audit.counts.missed,
        timeout: audit.counts.timeout,
        unviable: audit.counts.unviable,
        // «succeeded» (мутант без тестовой фазы) cargo-mutants НЕ пишет ни в
        // один append-список: он виден только в outcomes.json — и мы
        // считаем его ЯВНО, чтобы вердикт говорил правду («succeeded N > 0:
        // поимка не подтверждена»), а не «детали расходятся со счётчиками».
        succeeded: audit.succeeded,
        // Неопознанный исход мутанта — тоже только из outcomes.json.
        unknown: audit.unknown,
    };
    let min_kill = min_kill_pct(root);
    // F-3 (MUT_BLIND): 0 мутантов без честной причины ≠ Pass. Причина
    // пустоты (exclude_re/skip/пустой diff) — из лога этого прогона:
    // найдена → «нечего убивать» (Pass при любом режиме).
    let policy = gates::MutPolicy {
        min_kill,
        blind: blind_mode(root),
        zero_ok: std::fs::read_to_string(work_dir(root).join(CARGO_LOG))
            .map(|t| gates::parse_no_mutants_reason(&t).is_some())
            .unwrap_or(false),
    };
    match gates::judge_mutation_stats(&stats, &policy) {
        gates::Verdict::Pass => {}
        gates::Verdict::Fail { tail, .. } => {
            println!("вердикт: ПРОВАЛ — {tail}");
            crate::commands::die(crate::commands::ExitCode::ActionError);
        }
        gates::Verdict::EnvBlocked { missing } => {
            println!("вердикт: ПРОВАЛ — {missing}");
            crate::commands::die(crate::commands::ExitCode::ActionError);
        }
        gates::Verdict::SpecBlock { tail, .. } => {
            println!("вердикт: ПРОВАЛ — {tail}");
            crate::commands::die(crate::commands::ExitCode::ActionError);
        }
    }
    // Детали аудита — тоже блок «чистого» вердикта: без них доказать
    // «чисто» нечем.
    if audit.malformed {
        println!("вердикт: ПРОВАЛ — детали аудита повреждены (outcomes.json не читается)");
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    if audit.details_missing {
        println!("вердикт: ПРОВАЛ — детали аудита отсутствуют (outcomes.json нет)");
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    if audit.details_mismatch {
        println!("вердикт: ПРОВАЛ — детали аудита расходятся со счётчиками");
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    if let Some(t) = &audit.tool_problem {
        println!("вердикт: ПРОВАЛ — {t}");
        crate::commands::die(crate::commands::ExitCode::ActionError);
    }
    println!("вердикт: чисто");
    Ok(())
}

/// Порог kill-rate из конфига (тот же, что у гейта).
fn min_kill_pct(root: &Path) -> u32 {
    crate::commands::load_config(root)
        .ok()
        .and_then(|c| c.mutate.map(|m| m.min_kill))
        .unwrap_or(90)
}

/// F-3 (MUT_BLIND): режим слепого гейта из конфига (код-дефолт off).
fn blind_mode(root: &Path) -> crate::core::gates::BlindMode {
    crate::commands::load_config(root)
        .ok()
        .and_then(|c| c.mutate.map(|m| m.blind))
        .unwrap_or_default()
}

/// Сколько проблем видно ПРЯМО СЕЙЧАС (живой список + детали JSON).
///
/// Для оракула `devh next`: проблема, найденная в процессе прогона, —
/// уже следующий шаг (problem-first).
pub fn live_problem_count(root: &Path) -> u32 {
    let eff = effective_args(root, &[]);
    let mut audit = snapshot(root, &eff);
    filter_rechecked(root, &mut audit);
    audit.bad.len() as u32
}

/// Вердикт по завершённому аудиту: `Some(true)` — чисто, `Some(false)` —
/// дыра, `None` — вердикт выносить нельзя (идёт/оборвался/остановлен).
///
/// Контракт ОДНОЗНАЧЕН: `true` = чисто. Оракул `devh next` читает
/// `Some(false)` как «дыра, надо чинить» — инверсия здесь делала бы
/// чистый аудит «дырой», а дырявый — «чистым».
pub fn verdict_state(root: &Path) -> Option<bool> {
    if audit_stale(root) {
        return None;
    }
    let eff = effective_args(root, &[]);
    let audit = snapshot(root, &eff);
    verdict_flag(&audit, stopped(root).is_some(), aborted(root).is_some())
}

/// Чистая функция решения: без файловой системы, без скрытых состояний.
///
/// `blocks_clean_verdict()` = «блокирует чистый вердикт» (true = дыра),
/// поэтому чистота — это ЕГО ОТСУТСТВИЕ.
fn verdict_flag(audit: &Audit, stopped: bool, aborted: bool) -> Option<bool> {
    if audit.alive || stopped || aborted {
        return None;
    }
    Some(!audit.blocks_clean_verdict())
}

// ──────────────────────────────── stop ────────────────────────────────

/// Остановить идущий аудит: гасим ГРУППУ cargo-mutants (не pid наугад).
///
/// Без sleep-цикла: супервизор сам увидит завершение ребёнка (труба
/// завершения в `core::watch`) и запишет состояние «stopped».
fn stop(root: &Path) -> anyhow::Result<()> {
    let Some(_st) = running(root) else {
        if let Some(st) = aborted(root) {
            // Супервизор мёртв, а cargo-mutants — жив: это СИРОТА. Владельца
            // у неё больше нет (гасить должен был супервизор), поэтому
            // гасит вызывающий — иначе мутатор продолжит крутиться.
            let pgid = st.pgid;
            if pgid > 1 && group_alive(pgid) {
                // -pgid sends to process group; pgid > 1 so 0 - pgid cannot overflow i32.
                let neg_pgid = 0i32 - pgid;
                unsafe {
                    libc::kill(neg_pgid, libc::SIGTERM);
                }
                println!(
                    "супервизор мёртв (pid {}) — сирота cargo-mutants (группа {pgid}) \
                     погашена",
                    st.pid
                );
            } else {
                println!(
                    "аудит уже мёртв (супервизор pid {} не в списке процессов) — состояние \
                     осталось «running»",
                    st.pid
                );
            }
            crate::commands::die(crate::commands::ExitCode::EnvError);
        }
        println!("нечего останавливать: аудит не идёт (devh mutate status)");
        crate::commands::die(crate::commands::ExitCode::EnvError);
    };
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    // Маркер: по нему супервизор запишет «stopped», а не «failed», и САМ
    // погасит своего ребёнка (он владеет завершением cargo-mutants).
    let _ = fs::write(dir.join(STOP_MARKER), b"stop\n");
    // Пока супервизор ЖИВ — остановкой владеет только он. Второй kill из
    // `stop` был гонкой: ребёнок мог быть убит, а супервизор — не успеть
    // записать финальный статус («stopped» превратился бы в «failed»).
    println!("остановка запрошена (супервизор погасит cargo-mutants сам)");
    println!("финальный статус: devh mutate status");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_name_decodes_to_path_and_line() {
        assert_eq!(
            decode_diff_name("devharness__src__core__plan.rs_line_19_col_5.diff"),
            "devharness/src/core/plan.rs:19"
        );
        // без координат — просто путь
        assert_eq!(
            decode_diff_name("devharness__src__core__plan.rs.diff"),
            "devharness/src/core/plan.rs"
        );
    }

    #[test]
    fn canon_mentions_every_subcommand() {
        for sub in [
            "start", "status", "watch", "problems", "verdict", "stop", "help",
        ] {
            assert!(CANON.contains(sub), "канон не упоминает {sub}");
        }
        // правило unviable=провал в самом каноне
        assert!(CANON.contains("Unviable"));
        // и запрет ручного cargo-mutants
        assert!(CANON.contains("Руками cargo-mutants НЕ запускать"));
    }

    /// Контракт `verdict_state`: `Some(true)` — ЧИСТО, `Some(false)` — дыра.
    ///
    /// Инверсия (Some(true) = «не чисто») делала бы ровно наоборот: чистый
    /// аудит оракул `devh next` считал бы дырой, а дырявый — чистым.
    #[test]
    fn verdict_flag_true_means_clean() {
        let mut clean = Audit::default();
        clean.counts.caught = 3;
        clean.done = 3;
        assert_eq!(
            verdict_flag(&clean, false, false),
            Some(true),
            "пойманные мутанты без дыр = чисто"
        );

        let mut hole = Audit::default();
        hole.counts.caught = 2;
        hole.counts.missed = 1;
        hole.done = 3;
        assert_eq!(
            verdict_flag(&hole, false, false),
            Some(false),
            "выживший = дыра"
        );

        // Идёт / остановлен / оборвался — вердикт выносить НЕЛЬЗЯ.
        let mut alive = Audit::default();
        alive.alive = true;
        assert_eq!(verdict_flag(&alive, false, false), None, "идущий аудит");
        assert_eq!(verdict_flag(&clean, true, false), None, "остановлен");
        assert_eq!(verdict_flag(&clean, false, true), None, "оборвался");
    }

    /// Ошибка записи вердикта — fail-closed: артефакт неполон, и старый
    /// вердикт прошлого прогона гасится (иначе читатель принял бы его за
    /// результат текущего).
    #[test]
    fn verdict_write_failure_is_fail_closed() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        // Старый «чистый» вердикт ПРОШЛОГО прогона.
        fs::write(d.path().join(VERDICT_FILE), r#"{"hole":false}"#)
            .unwrap_or_else(|_| unreachable!());
        // Ломаем запись: tmp-путь занимает каталог → fs::write падает.
        fs::create_dir_all(d.path().join(".verdict.json.tmp")).unwrap_or_else(|_| unreachable!());
        let err = write_verdict_fail_closed(d.path(), &serde_json::json!({"hole": true}))
            .expect_err("запись должна упасть");
        assert!(
            err.to_string().contains("вердикт"),
            "неинформативная ошибка: {err}"
        );
        assert!(
            !d.path().join(VERDICT_FILE).exists(),
            "старый вердикт оставлен — читатель примет его за результат текущего прогона"
        );
    }

    /// Отчёт не должен называть «чисто» прогон с дырами — и наоборот.
    #[test]
    fn report_lines_name_the_real_verdict() {
        let mut clean = Audit::default();
        clean.counts.caught = 2;
        clean.done = 2;
        let out = report_lines(&clean);
        assert!(
            out.iter().any(|l| l.contains("вердикт: чисто")),
            "чистый аудит назван чистым: {out:?}"
        );

        let mut hole = Audit::default();
        hole.counts.caught = 1;
        hole.counts.missed = 1;
        hole.done = 2;
        let out = report_lines(&hole);
        assert!(
            out.iter().any(|l| l.contains("вердикт: НЕ чисто")),
            "дырявый аудит назван не-чистым: {out:?}"
        );
        assert!(
            !out.iter().any(|l| l.contains("вердикт: чисто")),
            "дырявый аудит НЕ может быть назван чистым: {out:?}"
        );

        // Идущий прогон: финального вердикта нет вовсе.
        let mut alive = Audit::default();
        alive.alive = true;
        let out = report_lines(&alive);
        assert!(
            !out.iter().any(|l| l.contains("вердикт:")),
            "у идущего прогона финального вердикта нет: {out:?}"
        );
    }

    /// Ревью T-004: при 0 тестируемых kill_rate() — условные 100%, но это
    /// НЕ измеренный процент (судья велит Fail). Отчёт обязан печатать
    /// «не измерен», а не «100.0%».
    #[test]
    fn report_lines_says_unmeasured_when_nothing_testable() {
        let mut all_unviable = Audit::default();
        all_unviable.counts.unviable = 3;
        all_unviable.done = 3;
        let out = report_lines(&all_unviable);
        assert!(
            out.iter()
                .any(|l| l.contains("kill-rate не измерен (0 тестируемых)")),
            "0 тестируемых ≠ 100%: {out:?}"
        );
        assert!(
            !out.iter().any(|l| l.contains("kill-rate 100")),
            "«kill-rate 100» при нуле тестируемых врёт: {out:?}"
        );
        // Обычный случай — прежний формат с процентом.
        let mut normal = Audit::default();
        normal.counts.caught = 9;
        normal.counts.missed = 1;
        normal.done = 10;
        let out = report_lines(&normal);
        assert!(
            out.iter()
                .any(|l| l.contains("kill-rate 90.0% (10 тестируемых)")),
            "процент печатается как раньше: {out:?}"
        );
    }

    #[test]
    fn recheck_args_strip_cache_and_set_exact_filter() {
        let base: Vec<String> = [
            "--config",
            "devharness/cargo-mutants.toml",
            "--iterate",
            "--in-diff",
            "d.diff",
            "--output",
            "old.out",
            "-F",
            "old",
            "--test-tool",
            "nextest",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = Path::new("/tmp/recheck");
        let got = recheck_args(&base, "src/a.rs:1:5: replace x with None", out);
        assert!(!got.iter().any(|a| a == "--iterate" || a == "--in-diff"));
        assert!(!got.iter().any(|a| a == "old.out" || a == "old"));
        assert!(got
            .iter()
            .any(|a| a == r"^src/a\.rs:1:5: replace x with None$"));
        assert!(got.iter().any(|a| a == "/tmp/recheck"));
    }

    /// Имя сценария, как его пишут `problems` и append-списки.
    const RC_NAME: &str = "devharness/src/core/plan.rs:7:5: replace x with y";

    /// Фейковый output прогона: `<out>/mutants.out/{outcomes.json,список}` —
    /// ровно то, что настоящий cargo-mutants пишет по `--output <out>`.
    /// Путь считается ЕДИНОЙ формулой [`audit_dir`] — как в `recheck`.
    fn fake_recheck_out(root: &Path, name: &str, summary: &str) -> PathBuf {
        let out = root.join("out");
        let dir = audit_dir(root, &["--output".to_string(), out.display().to_string()]);
        fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        let list = match summary {
            mutaudit::S_CAUGHT => "caught.txt",
            mutaudit::S_UNVIABLE => "unviable.txt",
            mutaudit::S_TIMEOUT => "timeout.txt",
            _ => "missed.txt",
        };
        fs::write(dir.join(list), format!("{name}\n")).unwrap_or_else(|_| unreachable!());
        let json = format!(
            r#"{{"outcomes":[{{"scenario":{{"Mutant":{{"name":"{name}","file":"devharness/src/core/plan.rs","span":{{"start":{{"line":7,"column":5}}}},"replacement":"None","genre":"FnValue"}}}},"summary":"{summary}","phase_results":[{{"phase":"Test","duration":2.5}}]}}]}}"#
        );
        fs::write(dir.join("outcomes.json"), json).unwrap_or_else(|_| unreachable!());
        dir
    }

    /// Главный баг-фикс: recheck читает `<--output>/mutants.out`, а не сам
    /// `--output` (в нём никогда ничего не было → «исход не найден» всегда).
    #[test]
    fn recheck_finds_name_in_out_muts_out() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let dir = fake_recheck_out(d.path(), RC_NAME, mutaudit::S_MISSED);
        assert_eq!(
            dir,
            d.path().join("out").join("mutants.out"),
            "каталог считается формулой audit_dir, а не второй формулой"
        );
        let (caught, found) = recheck_find(&dir, RC_NAME, Some(2)).expect("имя найдено");
        assert!(!caught, "выживший — не пойман");
        assert_eq!(found.summary, mutaudit::S_MISSED);
        assert_eq!(found.name, RC_NAME);
        assert_eq!(found.file, "devharness/src/core/plan.rs");
        assert_eq!(found.line, 7);
        // replacement/genre — ИЗ outcomes.json, а не пустые из списка.
        assert_eq!(found.replacement, "None");
        assert_eq!(found.genre, "FnValue");
    }

    /// Пойманный мутант не попадает ни в один список плохих — только в
    /// caught.txt: раньше `caught` в recheck был недостижим.
    #[test]
    fn recheck_finds_caught_from_caught_txt() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let dir = fake_recheck_out(d.path(), RC_NAME, mutaudit::S_CAUGHT);
        let (caught, found) = recheck_find(&dir, RC_NAME, Some(0)).expect("имя найдено");
        assert!(caught, "caught.txt → пойман");
        assert_eq!(found.summary, mutaudit::S_CAUGHT);
        assert_eq!(found.name, RC_NAME);
        assert_eq!(found.file, "devharness/src/core/plan.rs");
    }

    /// JSON ещё не дописан: мутант узнаётся по файлу/строке/описанию из
    /// append-списка (замены не будет, но «исход не найден» — ложь).
    #[test]
    fn recheck_finds_by_place_when_outcomes_json_missing() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let dir = fake_recheck_out(d.path(), RC_NAME, mutaudit::S_MISSED);
        fs::remove_file(dir.join("outcomes.json")).unwrap_or_else(|_| unreachable!());
        let (caught, found) = recheck_find(&dir, RC_NAME, Some(2)).expect("имя найдено");
        assert!(!caught);
        assert_eq!(found.summary, mutaudit::S_MISSED);
        assert_eq!(found.line, 7);
        assert!(found.replacement.is_empty(), "без JSON деталей нет");
    }

    /// Три причины «исхода нет» — одна строка «исход не найден» врали бы
    /// оператору. «исход не найден» — только когда output создан и дописан.
    #[test]
    fn recheck_miss_reason_separates_three_causes() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let dir = d.path().join("out").join("mutants.out");

        // 1) Каталога нет — инструмент его не создал.
        let m = recheck_miss_reason(&dir, RC_NAME, Some(66), "log");
        assert!(m.contains("инструмент не создал output"), "{m}");
        assert!(m.contains("rc Some(66)"), "код возврата сохранён: {m}");

        // 2) Каталог есть, outcomes.json нет — прогон не дописан.
        fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        let m = recheck_miss_reason(&dir, RC_NAME, Some(2), "log");
        assert!(m.contains("нет outcomes.json"), "{m}");

        // 3) Output дописан, но имени нет — только тут «исход не найден».
        fs::write(dir.join("outcomes.json"), r#"{"outcomes":[]}"#)
            .unwrap_or_else(|_| unreachable!());
        let m = recheck_miss_reason(&dir, RC_NAME, Some(2), "log");
        assert!(m.contains("исход для"), "{m}");
        assert!(
            !m.contains("не создал") && !m.contains("outcomes.json"),
            "{m}"
        );
    }

    #[test]
    fn rechecked_overlay_closes_only_caught_on_same_tree() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness/mutate"))
            .unwrap_or_else(|_| unreachable!());
        let write = |stamp: &str, caught: bool| {
            fs::write(
                d.path().join(".harness/mutate/rechecked.json"),
                format!(r#"{{"tree_stamp":"{stamp}","caught":{caught},"name":"src/a.rs:1:5: x"}}"#),
            )
            .unwrap_or_else(|_| unreachable!());
        };
        let mut audit = Audit::default();
        audit.bad = vec![
            mutaudit::Bad {
                name: "src/a.rs:1:5: x".to_string(),
                ..mutaudit::Bad::default()
            },
            mutaudit::Bad {
                name: "src/b.rs:2:5: y".to_string(),
                ..mutaudit::Bad::default()
            },
        ];
        // Не тот отпечаток — overlay не применяется.
        write("other", true);
        let mut a = audit.clone();
        filter_rechecked(d.path(), &mut a);
        assert_eq!(a.bad.len(), 2);
        // Текущий отпечаток (не-git дерево → пустой) + caught — одна снята.
        write("", true);
        let mut a = audit.clone();
        filter_rechecked(d.path(), &mut a);
        assert_eq!(a.bad.len(), 1);
        assert_eq!(
            a.bad.first().map(|b| b.name.as_str()),
            Some("src/b.rs:2:5: y")
        );
        // caught=false — не снимает.
        write("", false);
        let mut a = audit;
        filter_rechecked(d.path(), &mut a);
        assert_eq!(a.bad.len(), 2);
    }

    #[test]
    fn watch_opts_parse_until_problem_and_batch() {
        let args: Vec<String> = ["--until-problem", "--batch-ms", "250", "--timeout", "5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse_watch_opts(&args);
        assert!(o.until_problem);
        assert_eq!(o.batch_ms, 250);
        assert_eq!(o.timeout, 5);
        assert_eq!(parse_watch_opts(&[]), WatchOpts::default());
        // битые числа не включают «вечный» таймаут/окно
        let bad: Vec<String> = ["--batch-ms", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_watch_opts(&bad).batch_ms, 0);
    }

    #[test]
    fn audit_dir_follows_output_flag() {
        let root = Path::new("/tmp/x");
        assert_eq!(audit_dir(root, &[]), Path::new("/tmp/x/mutants.out"));
        assert_eq!(
            audit_dir(root, &["--output".to_string(), "/tmp/o".to_string()]),
            Path::new("/tmp/o/mutants.out")
        );
    }

    #[test]
    fn mutate_start_gate_rejects_non_lead() {
        let args = vec!["src/main.rs".to_string()];
        let res = mutate_start_gate("worker", Some("lead"), &args, None, 1_000_000, 50);
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .contains("мутационный аудит запускает только ведущий"));
    }

    #[test]
    fn mutate_start_gate_fail_open_without_lead() {
        let args = vec!["src/main.rs".to_string()];
        let res = mutate_start_gate("worker", None, &args, None, 1_000_000, 50);
        assert!(res.is_ok(), "без lead — fail-open");
    }

    #[test]
    fn mutate_start_gate_rejects_no_scope() {
        let args: Vec<String> = vec![];
        let res = mutate_start_gate("lead", Some("lead"), &args, None, 1_000_000, 50);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("нужен явный охват"));
    }

    #[test]
    fn mutate_start_gate_rejects_too_soon() {
        let args = vec!["src/main.rs".to_string()];
        let last = 1_000_000u64 - 100; // 100 с назад < 7200
        let res = mutate_start_gate("lead", Some("lead"), &args, Some(last), 1_000_000, 50);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("менее 2 ч"));
    }

    #[test]
    fn mutate_start_gate_rejects_high_load() {
        let args = vec!["src/main.rs".to_string()];
        let res = mutate_start_gate("lead", Some("lead"), &args, None, 1_000_000, 90);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("system load"));
    }

    #[test]
    fn mutate_start_gate_allows_valid() {
        let args = vec!["src/main.rs".to_string()];
        let res = mutate_start_gate("lead", Some("lead"), &args, None, 1_000_000, 50);
        assert!(res.is_ok());
    }

    #[test]
    fn state_roundtrips_through_json() {
        let st = State {
            state: "running".to_string(),
            pid: 42,
            exit: None,
            started: 1,
            finished: None,
            args: vec!["--config".to_string(), "x.toml".to_string()],
            cwd: "/tmp".to_string(),
            mode: "copy".to_string(),
            pgid: 42,
            tree_stamp: "abc".to_string(),
        };
        let text = serde_json::to_string(&st).unwrap_or_default();
        let back: State = serde_json::from_str(&text).unwrap_or_else(|_| unreachable!());
        assert_eq!(back, st);
    }

    #[test]
    fn old_state_file_without_new_fields_still_reads() {
        // Совместимость: старый state.json (без cwd/mode/pgid) читается.
        let json = r#"{"state":"done","pid":1,"exit":0,"started":5,"args":["--x"]}"#;
        let st: State = serde_json::from_str(json).unwrap_or_else(|_| unreachable!());
        assert_eq!(st.state, "done");
        assert!(st.cwd.is_empty());
        assert_eq!(st.pgid, 0);
        assert!(st.finished.is_none());
        assert!(st.tree_stamp.is_empty());
    }

    /// F-1.8.1: правка файла после старта делает снимок протухшим, а старый
    /// state.json без отпечатка не объявляет ложное «протухло».
    #[test]
    fn audit_stale_detects_tree_change_and_tolerates_old_state() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(d.path())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false),
                "git {args:?}"
            );
        };
        run(&["init", "-q"]);
        fs::create_dir_all(d.path().join(".git/info")).unwrap_or_else(|_| unreachable!());
        fs::write(d.path().join(".git/info/exclude"), ".harness/\n")
            .unwrap_or_else(|_| unreachable!());
        fs::write(d.path().join("a.rs"), "fn a() {}\n").unwrap_or_else(|_| unreachable!());
        run(&["add", "a.rs"]);

        let stamp = tree_stamp(d.path());
        assert!(!stamp.is_empty(), "git есть — отпечаток обязан быть");
        write_state(
            d.path(),
            &State {
                state: "running".to_string(),
                pid: 1,
                exit: None,
                started: 1,
                finished: None,
                args: vec![],
                cwd: d.path().display().to_string(),
                mode: "copy".to_string(),
                pgid: 0,
                tree_stamp: stamp,
            },
        )
        .unwrap_or_else(|_| unreachable!());
        assert!(!audit_stale(d.path()), "дерево не менялось");

        fs::write(d.path().join("a.rs"), "fn a() { }\n").unwrap_or_else(|_| unreachable!());
        assert!(audit_stale(d.path()), "правка файла = протухший снимок");

        // Старый state.json без tree_stamp — stale-проверка не выдумывает
        // протухание (совместимость), даже если дерево уже другое.
        let old = r#"{"state":"done","pid":1,"exit":0,"started":5,"args":["--x"]}"#;
        fs::write(d.path().join(".harness/mutate/state.json"), old)
            .unwrap_or_else(|_| unreachable!());
        assert!(!audit_stale(d.path()), "пустой отпечаток ≠ протухло");
    }

    #[test]
    fn problem_fingerprint_changes_with_details() {
        let mut b = mutaudit::Bad {
            summary: mutaudit::S_MISSED.to_string(),
            file: "devharness/src/core/plan.rs".to_string(),
            line: 7,
            name: "replace x with y".to_string(),
            ..mutaudit::Bad::default()
        };
        let before = fingerprint(&b);
        b.replacement = "None".to_string();
        let after = fingerprint(&b);
        assert_ne!(before, after, "появление замены — новая деталь проблемы");
    }
}
