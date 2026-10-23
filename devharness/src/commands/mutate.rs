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
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::early;
use crate::core::gates::{self, MutationStats};
use crate::core::mutaudit::{self, Audit};
use crate::core::tui;
use crate::core::watch::{Event, Watcher};
use crate::shell::color::colors;
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
                              (--timeout N — ограничить секунды).
  devh mutate problems        руководство к действию: что чинить (файл:строка,
                              замена, жанр, исход). rc=1, если есть проблемы.
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
  • смотришь в терминал    → watch

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

/// Флаги cargo-mutants из состояния (или из аргументов — если состояния нет).
fn effective_args(root: &Path, args: &[String]) -> Vec<String> {
    match read_state(root) {
        Some(s) if !s.args.is_empty() => s.args.clone(),
        _ => args.iter().filter(|a| *a != "--").cloned().collect(),
    }
}

// ──────────────────────────────── start ────────────────────────────────

/// Запуск аудита: супервизор в отдельной сессии + подсказка «что дальше».
fn start(root: &Path, args: &[String]) -> anyhow::Result<()> {
    if let Some(st) = running(root) {
        println!(
            "аудит уже идёт (супервизор pid {}) — смотри: devh mutate status",
            st.pid
        );
        crate::commands::die(2);
    }
    let args: Vec<String> = args.iter().filter(|a| *a != "--").cloned().collect();
    if args.is_empty() {
        help();
        crate::commands::die(1);
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
        crate::commands::die(1);
    }
    // Тяжёлый вход: СВЕЖАЯ проверка нагрузки (не устаревший снимок).
    // Под контенцией мутации стоят ×2–4 (§9.10) — лучше не начинать вовсе.
    // Неизвестная нагрузка (нет /proc/loadavg) гейт НЕ блокирует.
    if let Some(load) = crate::core::load::load_average() {
        let env = std::env::var("HARNESS_MUTATE_MAX_LOAD").ok();
        if let Some(why) = crate::core::load::block_reason(env.as_deref(), load) {
            println!("devh mutate start: {why}");
            println!("аудит не запущен — дождись, пока соседний прогон освободит CPU");
            crate::commands::die(2);
        }
    }
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    // Старые артефакты — вон: списки append-only, иначе счётчики начнутся
    // не с нуля (а verdict будет считать мутанты прошлого прогона).
    let _ = fs::remove_dir_all(audit_dir(root, &args));
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
fn supervise(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let args: Vec<String> = args.iter().filter(|a| *a != "--").cloned().collect();
    if args.is_empty() {
        anyhow::bail!("devh mutate run: нет аргументов cargo-mutants");
    }
    let dir = work_dir(root);
    fs::create_dir_all(&dir)?;
    let adir = audit_dir(root, &args);
    let started = now_secs();
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
    let audit = snapshot(root, &eff);
    let aborted_state = aborted(root);
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
        crate::commands::die(2);
    }
    if stopped(root).is_some() {
        println!("⚠ аудит остановлен по требованию оператора — результата нет (вердикт = rc 2)");
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
            "exit": audit.exit,
            "malformed": audit.malformed,
            "details_missing": audit.details_missing,
            "details_mismatch": audit.details_mismatch,
            "tool_problem": audit.tool_problem,
            "blocks_clean": audit.blocks_clean_verdict() || aborted_state.is_some(),
            "problems": problem_json(&audit),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
        // Оборванный аудит: результата нет → вердикт выносить не по чему
        // (rc=2, среда не готова), даже если частичные данные уже накопились.
        if aborted_state.is_some() {
            crate::commands::die(2);
        }
        // Проблема видна СЕЙЧАС — даже если аудит ещё идёт (problem-first).
        if audit.blocks_clean_verdict() {
            crate::commands::die(1);
        }
        return Ok(());
    }
    // Оборванный аудит: НЕ печатаем кадр и «вердикт» — при неполных данных
    // kill-rate 100% и «чисто» были бы ложью (наблюдалось: 0/226 и
    // «вердикт: чисто» при мёртвом супервизоре). Результата нет — rc=2.
    if aborted_state.is_some() {
        println!("аудит оборвался — результата нет, вердикт не выносим (devh next: перезапуск)");
        crate::commands::die(2);
    }
    let cur = current_mutant(&audit_dir(root, &eff));
    println!("{}", tui::legend(&colors()));
    println!("{}", frame(&audit, &cur, elapsed_secs(root)));
    for l in report_lines(&audit) {
        println!("{l}");
    }
    // Проблема видна СЕЙЧАС — даже если аудит ещё идёт (problem-first).
    if audit.blocks_clean_verdict() {
        crate::commands::die(1);
    }
    Ok(())
}

// ───────────────────────────────── watch ─────────────────────────────────

/// Живой CLI: строка обновляется на месте по событиям ФС.
fn watch(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let mut timeout: u64 = 0;
    let mut i = 0;
    while i < args.len() {
        if args.get(i) == Some(&"--timeout".to_string()) {
            if let Some(v) = args.get(i.saturating_add(1)) {
                timeout = v.parse::<u64>().unwrap_or(0);
            }
            i = i.saturating_add(2);
        } else {
            i = i.saturating_add(1);
        }
    }
    let eff = effective_args(root, args);
    let adir = audit_dir(root, &eff);
    let start = now_secs();
    // Наблюдаем и каталог прогона, и каталог артефактов: супервизор пишет
    // туда финальные verdict/state — по этому событию watch замечает финал
    // сразу, а не по таймауту ожидания.
    let mut watcher = match Watcher::new(&[&adir, root, &work_dir(root)]) {
        Ok(w) => w,
        Err(e) => {
            println!("devh mutate watch: inotify недоступен ({e})");
            crate::commands::die(2);
        }
    };
    let mut first = true;
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
        if !audit.alive {
            println!();
            // Мёртвый супервизор с незавершённым состоянием — результата
            // нет: «вердикт: чисто» по частичным данным был бы ложью.
            if aborted(root).is_some() {
                println!(
                    "⚠ аудит оборвался: результата нет, вердикт не выносим \
                     (devh next: перезапуск)"
                );
                crate::commands::die(2);
            }
            for l in report_lines(&audit) {
                println!("{l}");
            }
            // Проблема — немедленно код возврата 1 (problem-first), даже
            // если аудит только что закончился.
            if audit.blocks_clean_verdict() {
                crate::commands::die(1);
            }
            return Ok(());
        }
        if timeout > 0 && now_secs().saturating_sub(start) >= timeout {
            println!();
            println!("таймаут ожидания (--timeout {timeout}s) — аудит продолжается");
            return Ok(());
        }
        // Ждём СОБЫТИЯ (изменение файла прогона), а не такта.
        match watcher.wait(POLL_MS) {
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
                crate::commands::die(3);
            }
        }
    }
}

// ─────────────────────────────── problems ───────────────────────────────

/// Руководство к действию: что именно чинить.
fn problems(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let eff = effective_args(root, args);
    let audit = snapshot(root, &eff);
    let aborted_state = aborted(root);
    if audit.total.is_none() && audit.done == 0 && !audit.alive && aborted_state.is_none() {
        println!("аудит не запущен — нечего чинить (devh mutate start <args>)");
        crate::commands::die(2);
    }
    // Проблемы уровня инструмента/деталей — тоже проблемы: чинить нечего,
    // но и «проблем нет» сказать нельзя.
    let mut extra: Vec<String> = Vec::new();
    if stopped(root).is_some() {
        extra.push(
            "⚠ аудит остановлен по требованию оператора — результата нет (вердикт = rc 2)"
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
            crate::commands::die(1);
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
    crate::commands::die(1);
}

// ──────────────────────────────── verdict ────────────────────────────────

/// Финальный вердикт. Код возврата — гейт: 0 чисто, 1 дыра, 2 не закончен.
fn verdict(root: &Path) -> anyhow::Result<()> {
    let eff = effective_args(root, &[]);
    let audit = snapshot(root, &eff);
    if audit.alive {
        println!("прогон ещё идёт — финальный вердикт не выносим (devh mutate status)");
        crate::commands::die(2);
    }
    // Остановленный прогон: часть мутантов могла не дойти → результата нет.
    if stopped(root).is_some() {
        println!("аудит остановлен по требованию оператора — результата нет, вердикт не выносим");
        crate::commands::die(2);
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
        crate::commands::die(2);
    }
    if audit.total.is_none() && audit.done == 0 {
        println!("аудит не запущен — нечего вердиктовать (devh mutate start <args>)");
        crate::commands::die(2);
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
        crate::commands::die(1);
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
    match gates::judge_mutation_stats(&stats, min_kill) {
        gates::Verdict::Pass => {}
        gates::Verdict::Fail { tail, .. } => {
            println!("вердикт: ПРОВАЛ — {tail}");
            crate::commands::die(1);
        }
        gates::Verdict::EnvBlocked { missing } => {
            println!("вердикт: ПРОВАЛ — {missing}");
            crate::commands::die(1);
        }
        gates::Verdict::SpecBlock { tail, .. } => {
            println!("вердикт: ПРОВАЛ — {tail}");
            crate::commands::die(1);
        }
    }
    // Детали аудита — тоже блок «чистого» вердикта: без них доказать
    // «чисто» нечем.
    if audit.malformed {
        println!("вердикт: ПРОВАЛ — детали аудита повреждены (outcomes.json не читается)");
        crate::commands::die(1);
    }
    if audit.details_missing {
        println!("вердикт: ПРОВАЛ — детали аудита отсутствуют (outcomes.json нет)");
        crate::commands::die(1);
    }
    if audit.details_mismatch {
        println!("вердикт: ПРОВАЛ — детали аудита расходятся со счётчиками");
        crate::commands::die(1);
    }
    if let Some(t) = &audit.tool_problem {
        println!("вердикт: ПРОВАЛ — {t}");
        crate::commands::die(1);
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

/// Сколько проблем видно ПРЯМО СЕЙЧАС (живой список + детали JSON).
///
/// Для оракула `devh next`: проблема, найденная в процессе прогона, —
/// уже следующий шаг (problem-first).
pub fn live_problem_count(root: &Path) -> u32 {
    let eff = effective_args(root, &[]);
    snapshot(root, &eff).bad.len() as u32
}

/// Вердикт по завершённому аудиту: `Some(true)` — чисто, `Some(false)` —
/// дыра, `None` — вердикт выносить нельзя (идёт/оборвался/остановлен).
///
/// Контракт ОДНОЗНАЧЕН: `true` = чисто. Оракул `devh next` читает
/// `Some(false)` как «дыра, надо чинить» — инверсия здесь делала бы
/// чистый аудит «дырой», а дырявый — «чистым».
pub fn verdict_state(root: &Path) -> Option<bool> {
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
            crate::commands::die(2);
        }
        println!("нечего останавливать: аудит не идёт (devh mutate status)");
        crate::commands::die(2);
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
