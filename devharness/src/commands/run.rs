use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::gates::{
    self, parse_review_verdict, tail_of, Gate, Raw, RedPhase, ReviewVerdict, Verdict,
};
use crate::core::runstate;
use crate::core::state::{self, ClaimAction, Event};
use crate::core::types::{CrapCfg, ItemId, MutateCfg, RunConfig};
use crate::prompt;
use crate::shell::color::colors;
use crate::shell::git;
use crate::shell::lock::{GlobalLock, ItemRunLock, MutateLock};
use crate::shell::proc;
use crate::shell::store::{item_rel, Store};
use crate::shell::worktree;
use crate::timing::Timing;

const RED_TIMEOUT: Duration = Duration::from_secs(300);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(3600);
const MUTATE_TIMEOUT: Duration = Duration::from_secs(3600);
const BENCH_TIMEOUT: Duration = Duration::from_secs(1800);

struct PidGuard<'a> {
    store: &'a Store,
    id: ItemId,
}

impl Drop for PidGuard<'_> {
    fn drop(&mut self) {
        self.store.remove_pid(&self.id);
    }
}

fn die(store: &Store, id: &ItemId, code: i32) -> ! {
    store.remove_pid(id);
    crate::commands::die(code)
}

/// Зафиксировать фазу прогона в durable-маркере (для оракула `devh next`).
///
/// Пишет ТОЛЬКО харнесс — тот, кто знает вердикт гейта. Ошибка записи не
/// должна ронять прогон: без маркера оракул предложит `devh run`
/// (безопасный ответ: перепроверить гейты).
fn mark_phase(store: &Store, id: &ItemId, wt: &Path, phase: runstate::Phase) {
    mark_phase_named(store, id, wt, phase, "", 1);
}

/// То же, но с именем упавшего гейта и кодом (для `GateFailed`).
fn mark_phase_named(
    store: &Store,
    id: &ItemId,
    wt: &Path,
    phase: runstate::Phase,
    gate: &str,
    code: i32,
) {
    let head = git::short_head(wt);
    // Дайджест дерева: вердикт гейта относится к ЭТОМУ дереву, а не к
    // коммиту. Без него незакоммиченная правка после зелёных гейтов не
    // протухала бы, и оракул предложил бы `commit` по чужому вердикту.
    let digest = git::tree_digest(wt).unwrap_or_default();
    if digest.is_empty() && matches!(phase, runstate::Phase::GatesOk | runstate::Phase::Committed) {
        // Дайджест не посчитан → свежесть содержимого НЕИЗВЕСТНА. Оракул
        // такой вердикт не доверяет (пустой дайджест = «перепроверь»), но
        // оператор должен знать, почему оракул просит прогон заново.
        eprintln!(
            "devh: дайджест дерева не посчитан — вердикт будет считаться протухшим \
             (оракул предложит перепроверку гейтов)"
        );
    }
    let st = match phase {
        runstate::Phase::GatesOk => runstate::RunState::gates_ok(&head),
        runstate::Phase::Committed => runstate::RunState::committed(&head),
        runstate::Phase::GateFailed => runstate::RunState::gate_failed(gate, code, &head),
        runstate::Phase::InProgress => runstate::RunState::in_progress(&head),
    }
    .with_digest(&digest);
    if let Err(e) = runstate::write_or_invalidate(store, id, &st) {
        eprintln!("devh: маркер прогона не записан ({e}) — оракул перепроверит гейты заново");
    }
}

/// Fail-closed маркер «прогон идёт» — пишется ДО любого fallible шага.
///
/// Старый терминальный вердикт (gates_ok/committed) не должен пережить
/// начатый прогон: если прогон упадёт на preflight'е (claim, worktree),
/// оракул иначе предложил бы `commit`/`merge` по зелёному свету от
/// ПРЕДЫДУЩЕГО прогона. Маркер без дерева (head/digest пустые) — вердикта
/// нет, оракул предложит `devh run`.
fn mark_phase_start(store: &Store, id: &ItemId) {
    let st = runstate::RunState::in_progress("");
    if let Err(e) = runstate::write_or_invalidate(store, id, &st) {
        eprintln!("devh: маркер прогона не записан ({e}) — оракул перепроверит гейты заново");
    }
}

/// Имя гейта по вердикту (для маркера сдачи и для сообщений).
fn verdict_gate_name(v: &Verdict) -> String {
    match v {
        Verdict::Pass => "pass".to_string(),
        Verdict::Fail { gate, .. } => gate.name().to_string(),
        Verdict::EnvBlocked { .. } => "env".to_string(),
        Verdict::SpecBlock { gate, .. } => gate.name().to_string(),
    }
}

/// Конфиг для `devh run`: последовательный прогон включает стабильный
/// reuse-путь (P1a — тёплый инкремент вместо 19-мин cold-build'а).
/// Явный `HARNESS_WT_REUSE=0` остаётся аварийным люком: значение «выключено»
/// пользователем не переопределяем силой.
fn cfg_for_run(cfg: &RunConfig) -> RunConfig {
    let explicit_off = std::env::var("HARNESS_WT_REUSE")
        .map(|v| matches!(v.trim(), "0" | "false" | "no" | "off"))
        .unwrap_or(false);
    if cfg.wt_reuse || !explicit_off {
        RunConfig {
            wt_reuse: true,
            ..cfg.clone()
        }
    } else {
        cfg.clone()
    }
}

pub fn run(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();
    let cfg = cfg_for_run(cfg);
    let cfg = &cfg;
    require_gates(cfg)?;
    let (lock, _pid) = lock_run(store, id)?;
    let agent = resolve_agent(store, cfg, id);
    let mut timing = Timing::start();
    timing.flush(store, id);

    // Fail-closed ДО любого fallible шага: старый терминальный маркер
    // (gates_ok/committed) не должен пережить начатый прогон — иначе
    // ранний сбой (claim, worktree) оставил бы оракулу зелёный свет от
    // ПРЕДЫДУЩЕГО прогона.
    mark_phase_start(store, id);
    claim_item(store, cfg, id)?;
    let (wt, fresh) = ensure_worktree(store, cfg, id, &mut timing)?;
    // Fail-closed маркер: пока прогон идёт — вердикта НЕТ. Если финальная
    // запись (GatesOk/Committed/GateFailed) не удастся, оракул увидит «прогон
    // не завершён» и предложит `devh run` (перепроверить гейты), а НЕ
    // `commit`/`merge` по маркеру ПРЕДЫДУЩЕГО прогона.
    mark_phase(store, id, &wt, runstate::Phase::InProgress);
    let red = red_pre_phase(store, cfg, id, &root, &wt, &agent, fresh, &mut timing)?;
    no_crap_notice(store, cfg, id);
    timing.stage("red-pre", store, id);

    let mut round: u32 = 0;
    let mut next_prompt = prompt::agent_prompt(store, cfg, &root, &wt, id, &agent);
    loop {
        if let Err(e) = run_agent(
            store,
            &wt,
            id,
            &next_prompt,
            &agent,
            Duration::from_secs(cfg.agent_timeout as u64),
            format!("agent-r{}", round.saturating_add(1)),
            &mut timing,
            cfg,
        ) {
            eprintln!(
                "{}FAIL агент (код выше; timeout={}s; лог: {}){} ({e})",
                c.r,
                cfg.agent_timeout,
                store.log_file(id, "agent").display(),
                c.z
            );
            die(store, id, 1);
        }

        // P7/A1: единый роутинг вердикта — класс отказа (Fix/Env/Spec) и
        // бюджет решают судьбу раунда; код выхода даёт чистый decide_round.
        match gate_ladder(store, cfg, &wt, id, &mut timing, round) {
            Ok(()) => break,
            Err(stop) => {
                let kind = gates::failure_kind(&stop.verdict);
                match gates::decide_round(round, cfg.max_fix, kind) {
                    gates::RoundDecision::Continue => {
                        let Verdict::Fail { gate, tail } = stop.verdict else {
                            unreachable!("класс Fix несёт Fail-вердикт")
                        };
                        round = round.saturating_add(1);
                        println!(
                            ".. гейт {} упал — раунд фикса {}/{} (fix-раунд {})",
                            gate.name(),
                            round,
                            cfg.max_fix,
                            round
                        );
                        next_prompt = prompt::fix_prompt(&gate, &tail, cfg, store, id);
                    }
                    gates::RoundDecision::GiveUp { code } => {
                        give_up(store, id, &stop, kind, code, cfg.max_fix, &wt)
                    }
                }
            }
        }
    }
    // Гейты зелёные — но маркер «gates ok» ставим ПОСЛЕ red-пост-гейта и
    // RED-MUT: иначе маркер врал бы оракулу (`devh commit`), когда
    // обязательная стадия после лестницы гейтов упала. До red-стадий
    // маркера нет — оракул предложит `devh run` (безопасно: перепроверить).
    println!(
        "{}OK гейты: verify{}{}{}",
        c.g,
        if cfg.mutate.is_some() {
            " +mutation"
        } else {
            ""
        },
        if cfg.bench.is_some() { " +bench" } else { "" },
        c.z
    );

    red_post(store, cfg, id, &wt, &red, &mut timing)?;
    red_mutate_phase(store, cfg, id, &wt, &red, &mut timing)?;
    // Все обязательные стадии прошли — теперь вердикт «гейты зелёные» честен.
    mark_phase(store, id, &wt, runstate::Phase::GatesOk);
    commit_push(store, cfg, id, &wt, round, &mut timing, &root)?;
    finalize(cfg, id, &wt, &timing)?;
    drop(lock);
    Ok(())
}

/// Конфиг-предусловия прогона: VERIFY — иначе нечем проверить работу;
/// MUTATE-гейт ОБЯЗАТЕЛЕН («пропуск» запрещён — прогон без мутационного
/// гейта не отличает «зелёный» от «недоказанный»).
fn require_gates(cfg: &RunConfig) -> anyhow::Result<()> {
    if cfg.verify.is_empty() {
        anyhow::bail!("devh: VERIFY не задан (.harness/config)");
    }
    if cfg.mutate.is_none() {
        anyhow::bail!(
            "devh: MUTATE-гейт ОБЯЗАТЕЛЕН, «пропуск» запрещён — пропиши MUTATE в .harness/config (gobot: cd gobot + mix mutants/muex; rust: cargo-mutants)"
        );
    }
    Ok(())
}

/// Захват item'а: run.lock (не два прогона одновременно) + pid-файл
/// (`devh ps`). Оба guard'а живут весь вызов run() — поэтому возвращаются.
fn lock_run<'a>(store: &'a Store, id: &ItemId) -> anyhow::Result<(ItemRunLock, PidGuard<'a>)> {
    let lock = ItemRunLock::acquire(&store.item_dir(id)).ok_or_else(|| {
        anyhow::anyhow!(
            "devh: {} сейчас в работе (run.lock занят) — run отклонён",
            id.dir_name()
        )
    })?;
    store.write_pid(id, std::process::id())?;
    Ok((lock, PidGuard { store, id: *id }))
}

/// Агент: per-item override (файл `agent` внутри item), иначе дефолт конфига.
fn resolve_agent(store: &Store, cfg: &RunConfig, id: &ItemId) -> String {
    let mut agent = cfg.agent.clone();
    if let Some(a) = store.read(item_rel(id, "agent")) {
        if !a.trim().is_empty() {
            println!(".. агент (per-item): {}", a.trim());
            agent = a.trim().to_string();
        }
    }
    agent
}

/// Claim/resume под GlobalLock: WIP-лимит (MAX_ACTIVE), перевод в Active;
/// уже active — OK resume (повторный run продолжает прерванную работу).
fn claim_item(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let _g = GlobalLock::acquire(&store.dir).map_err(|e| anyhow::anyhow!("flock: {e}"))?;
    let prev_st = store.status_of(id);
    match state::claim_action(prev_st) {
        ClaimAction::RejectDone => anyhow::bail!(
            "devh: {} уже done — создай новый: devh new \"...\"",
            id.dir_name()
        ),
        ClaimAction::Resume => println!(
            "{}OK resume: {} уже active — продолжаем{}",
            c.g,
            id.dir_name(),
            c.z
        ),
        ClaimAction::Claim => {
            let active = store.count_active();
            if !state::can_claim(active, cfg.max_active) {
                anyhow::bail!(
                    "devh: WIP-лимит — уже {active} active (MAX_ACTIVE={}). Заверши их (devh merge / abort) прежде чем брать новое.",
                    cfg.max_active
                );
            }
            store.apply_event(id, Event::Claimed)?;
        }
    }
    Ok(())
}

/// Worktree: ensure (+bootstrap) — свежий либо тёплый reuse (P1a); стадия
/// тайминга «worktree(+bootstrap)» (контракт `devh ps`).
fn ensure_worktree(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    timing: &mut Timing,
) -> anyhow::Result<(PathBuf, bool)> {
    let c = colors();
    let (wt, fresh) = worktree::ensure(store, cfg, id)?;
    if fresh {
        println!(
            "{}OK worktree: {} (ветка {}){}",
            c.g,
            wt.display(),
            id.branch(),
            c.z
        );
    } else {
        println!("{}OK resume worktree: {}{}", c.g, wt.display(), c.z);
    }
    timing.stage("worktree(+bootstrap)", store, id);
    Ok((wt, fresh))
}

/// Факты о red.cmd, нужные driver'у после RED-pre-фазы (пост-гейт и
/// RED-MUT смотрят на существование/обход). Классификация исхода
/// (Proven/Resumed/Skipped) не вынесена в энум: все три ведут в один
/// агент-цикл, развилки по классу нет — поле было бы dead_code, а факты
/// классификации видны в логе («OK red» / «OK resume» / «⚠ RED пропущен»).
struct RedInfo {
    file: PathBuf,
    has_red: bool,
    no_red: bool,
}

/// RED-pre: записанный обход / автор red.cmd агентом / линковка к VERIFY +
/// доказательство «падает на чистом HEAD» (resume-грязное дерево скипает).
/// Падает rc=1 (Spec/FAIL) или rc=2 (env-блокер) — до любого fix-раунда.
#[allow(clippy::too_many_arguments)]
fn red_pre_phase(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    root: &Path,
    wt: &Path,
    agent: &str,
    fresh: bool,
    timing: &mut Timing,
) -> anyhow::Result<RedInfo> {
    let c = colors();
    let no_red = store
        .read(item_rel(id, "no-red"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let red_file = store.item_dir(id).join("red.cmd");
    let mut has_red = red_file.is_file();

    if no_red {
        let reason = store.read(item_rel(id, "no-red")).unwrap_or_default();
        println!(
            "{}⚠  RED пропущен (записанный обход): {}{}",
            c.y,
            reason.trim(),
            c.z
        );
    } else {
        let max_attempt = cfg.max_fix.max(1);
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            if !has_red {
                if attempt == 1 {
                    println!(
                        "{}.. red: red.cmd отсутствует — поручаю агенту написать его первой задачей{}",
                        c.y, c.z
                    );
                } else {
                    println!(
                        "{}.. red: раннер red.cmd не совпадает с раннером VERIFY — поручаю переписать (попытка {attempt}/{max_attempt}){}",
                        c.y, c.z
                    );
                }
                let mut author = prompt::red_author_prompt(store, cfg, root, wt, id, agent);
                if attempt > 1 {
                    author += &format!(
                        "\nКОРРЕКЦИЯ: red.cmd обязан запускать тот же раннер/бинарь, что и VERIFY ({}), а сейчас там: {}. Перепиши файл строго этим раннером.\n",
                        cfg.verify.trim(),
                        std::fs::read_to_string(&red_file)
                            .map(|s| gates::runner_signatures(&s).join(" | "))
                            .unwrap_or_else(|_| "<нет файла>".into())
                    );
                }
                if let Err(e) = run_agent(
                    store,
                    wt,
                    id,
                    &author,
                    agent,
                    Duration::from_secs(cfg.agent_timeout as u64),
                    "red-author",
                    timing,
                    cfg,
                ) {
                    eprintln!(
                        "{}FAIL агент на фазе RED (лог: {}){} ({e})",
                        c.r,
                        store.log_file(id, "agent").display(),
                        c.z
                    );
                    die(store, id, 1);
                }
                has_red = red_file.is_file();
                if !has_red && attempt >= max_attempt {
                    eprintln!(
                        "{}FAIL: RED-пруф ОБЯЗАТЕЛЕН (в первую очередь). Агент не написал {} — команду, падающую на чистом HEAD и зеленеющую после работы; либо записанный обход: echo \"<причина>\" > {}{}",
                        c.r,
                        red_file.display(),
                        store.item_dir(id).join("no-red").display(),
                        c.z
                    );
                    die(store, id, 1);
                }
                if !has_red {
                    continue;
                }
            }
            let src = std::fs::read_to_string(&red_file).unwrap_or_default();
            if gates::red_links_verify(&src, &cfg.verify) {
                break;
            }
            if attempt >= max_attempt {
                eprintln!(
                    "{}FAIL: RED-LINK: раннер red.cmd ({}) не совпадает с раннером VERIFY ({}) — red обязан гнать тот же тестовый вход{}",
                    c.r,
                    gates::runner_signatures(&src).join(" | "),
                    gates::runner_signatures(&cfg.verify).join(" | "),
                    c.z
                );
                eprintln!(
                    "      поправь {} (или строку RED: в спеке, либо выровняй VERIFY) и повтори: devh run {}{}",
                    red_file.display(),
                    id,
                    c.z
                );
                die(store, id, 1);
            }
            let _ = std::fs::remove_file(&red_file);
            has_red = false;
        }
        if !fresh && git::tree_dirty(wt) {
            println!(
                "{}OK resume: RED pre-check пропущен — дерево уже содержит работу; доказательство RED было на чистом HEAD первой попытки, позеленение проверит пост-гейт{}",
                c.g, c.z
            );
        } else {
            // P7 (red.expect): подпись причины — маркер `# red-expect:` в
            // red.cmd → строка спеки `RED-EXPECT:` → ключ RED_EXPECT.
            let red_src = std::fs::read_to_string(&red_file).unwrap_or_default();
            let expect = resolve_red_expect(store, cfg, id, &red_src);
            let red_gate = |expect: &Option<String>| Gate::Red {
                cmd: red_file.display().to_string(),
                phase: RedPhase::Pre,
                expect: expect.clone(),
            };
            let mut raw = run_red(&red_file, wt, false);
            std::fs::write(store.log_file(id, "red"), &raw.log).ok();
            let mut verdict = gates::judge(&red_gate(&expect), &raw, cfg);
            // P7: контеншен-флейк — на перегруженной машине (load≈11 было)
            // red.cmd может умереть таймаутом и не попасть под подпись;
            // одна повторная попытка под nice лечит большую часть.
            if matches!(verdict, Verdict::SpecBlock { .. }) {
                println!(
                    "{}.. red: подпись не совпала — повтор под nice (контеншен?){}",
                    c.y, c.z
                );
                raw = run_red(&red_file, wt, true);
                std::fs::write(store.log_file(id, "red"), &raw.log).ok();
                verdict = gates::judge(&red_gate(&expect), &raw, cfg);
            }
            match verdict {
                Verdict::Pass => println!(
                    "{}OK red: {}{}",
                    c.g,
                    gates::red_proof_line(&raw.log, expect.as_deref()),
                    c.z
                ),
                Verdict::SpecBlock { tail, .. } => {
                    eprintln!(
                        "{}FAIL (red): БЛОКЕР СПЕКИ — правь red.cmd или RED-EXPECT; fix-раунд агента тут не лечит{}",
                        c.r, c.z
                    );
                    eprintln!("{tail}");
                    eprintln!("      лог: {}{}", store.log_file(id, "red").display(), c.z);
                    die(store, id, 1);
                }
                Verdict::EnvBlocked { missing } => {
                    eprintln!(
                        "{}FAIL (red): env-блокер — {}. Блокер СРЕДЫ, не работа агента (rc=2). Лог: {}{}",
                        c.r,
                        missing,
                        store.log_file(id, "red").display(),
                        c.z
                    );
                    die(store, id, 2);
                }
                Verdict::Fail { .. } => {
                    eprintln!(
                        "{}FAIL: red.cmd уже зелёный на чистом HEAD — это не баг. Почини red.cmd или удали.{}",
                        c.r, c.z
                    );
                    die(store, id, 1);
                }
            }
        }
    }
    Ok(RedInfo {
        file: red_file,
        has_red,
        no_red,
    })
}

/// Обход CRAP (файл `no-crap`) — заметка пользователю; решение гейт примет сам.
fn no_crap_notice(store: &Store, cfg: &RunConfig, id: &ItemId) {
    let c = colors();
    let no_crap = store
        .read(item_rel(id, "no-crap"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if no_crap && cfg.crap.is_some() {
        let reason = store.read(item_rel(id, "no-crap")).unwrap_or_default();
        println!(
            "{}⚠  CRAP пропущен (записанный обход): {}{}",
            c.y,
            reason.trim(),
            c.z
        );
    }
}

/// RED-пост-гейт: тот же red.cmd, что падал на чистом HEAD, обязан
/// позеленеть — иначе «фикс» ничего не починил. Стадия «red-post».
fn red_post(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    wt: &Path,
    red: &RedInfo,
    timing: &mut Timing,
) -> anyhow::Result<()> {
    let c = colors();
    if !red.no_red && red.has_red {
        let raw = run_red(&red.file, wt, false);
        let verdict = gates::judge(
            &Gate::Red {
                cmd: red.file.display().to_string(),
                phase: RedPhase::Post,
                expect: None,
            },
            &raw,
            cfg,
        );
        if verdict == Verdict::Pass {
            println!("{}OK red: позеленел{}", c.g, c.z);
        } else {
            eprintln!(
                "{}FAIL: Red-пост-гейт: red.cmd не позеленел — фикс не доказан{}",
                c.r, c.z
            );
            mark_phase_named(store, id, wt, runstate::Phase::GateFailed, "red-post", 1);
            die(store, id, 1);
        }
    }
    timing.stage("red-post", store, id);
    Ok(())
}

/// RED-MUT: red-тест обязан ловить мутантов в изменённых файлах — иначе он
/// не проверяет изменение, а лишь присутствует. Стадия «redmut».
fn red_mutate_phase(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    wt: &Path,
    red: &RedInfo,
    timing: &mut Timing,
) -> anyhow::Result<()> {
    let c = colors();
    if let (Some(m), true) = (&cfg.red_mutate, !red.no_red && red.has_red) {
        if !m.tool.contains("{test}") {
            eprintln!(
                "{}FAIL: RED-MUT: в RED_MUTATE нет шаблона {{test}} — нечем подставить команду red ({}){}",
                c.r,
                red.file.display(),
                c.z
            );
            eprintln!(
                "      пример: RED_MUTATE=\"bash scripts/mutate.sh '{{test}}'\" — прогон мутантов, которые ловятся только red-тестом; и повтори: devh run {}{}",
                id, c.z
            );
            die(store, id, 1);
        }
        let red_cmd = std::fs::read_to_string(&red.file).unwrap_or_default();
        let rep = mutate_gate(
            m,
            wt,
            &store.dir,
            Some(red_cmd.trim()),
            "red-mutation",
            "RED_MUTATE",
            Gate::Mutate,
        )
        .unwrap_or_else(|e| {
            eprintln!("{}{e}{}", c.r, c.z);
            mark_phase_named(
                store,
                id,
                wt,
                runstate::Phase::GateFailed,
                "red-mutation",
                1,
            );
            die(store, id, 1);
        });
        let mpath = store.log_file(id, "redmut");
        std::fs::write(&mpath, &rep.log).ok();
        timing.stage("redmut", store, id);
        match rep.verdict {
            Verdict::EnvBlocked { .. } => {
                eprintln!(
                    "{}FAIL: RED-MUT env-блокер — инструмент не распознан (лог: {}){}",
                    c.r,
                    mpath.display(),
                    c.z
                );
                mark_phase_named(
                    store,
                    id,
                    wt,
                    runstate::Phase::GateFailed,
                    "red-mutation",
                    2,
                );
                die(store, id, 2);
            }
            Verdict::Fail { .. } => {
                // Причина (kill-rate / ранний STOP) — в консоль.
                eprintln!("{}", tail_of(&rep.log, 40));
                eprintln!(
                    "{}FAIL: RED-MUT: red-тест убивает слишком мало мутантов — он не доказывает, что проверяет изменение{}",
                    c.r, c.z
                );
                eprintln!(
                    "      усили {} (или удали его — агент напишет заново); лог: {}{}",
                    red.file.display(),
                    mpath.display(),
                    c.z
                );
                mark_phase_named(
                    store,
                    id,
                    wt,
                    runstate::Phase::GateFailed,
                    "red-mutation",
                    1,
                );
                die(store, id, 1);
            }
            Verdict::Pass => {
                // P7-2.3: SAMPLE — досрочная остановка на выборке: причина
                // (rule-of-three) в консоль, а не только в лог.
                if rep.early {
                    eprintln!("{}", tail_of(&rep.log, 40));
                }
                // P7-2.2: пустой скоуп (--in-diff без мутантов на затронутых
                // строках / всё вычищено --iterate) — не «ловит мутантов»,
                // а честный «нечего убивать».
                if rep.stats.is_some_and(|s| s.total == 0) {
                    println!(
                        "{}OK red-mutation: 0 мутантов в изменённых файлах — нечего убивать{}",
                        c.g, c.z
                    );
                } else {
                    println!(
                        "{}OK red-mutation: red-тест ловит мутантов в изменённых файлах{}",
                        c.g, c.z
                    );
                }
            }
            Verdict::SpecBlock { .. } => unreachable!("red-mutate не отдаёт SpecBlock"),
        }
    }
    Ok(())
}

/// Финал прогона: summary обязан что-то сказать (или «blocked»), гигиена
/// сообщения → add/staged → commit → push → last-run. Стадия «commit+push».
#[allow(clippy::too_many_arguments)]
fn commit_push(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    wt: &Path,
    round: u32,
    timing: &mut Timing,
    root: &Path,
) -> anyhow::Result<()> {
    let c = colors();
    let summary = store.read_summary(id);
    if summary.trim().is_empty() {
        eprintln!(
            "{}FAIL: агент не написал summary ({}){}",
            c.r,
            store.summary_file(id).display(),
            c.z
        );
        die(store, id, 1);
    }
    if prompt::is_blocked(&summary) {
        println!("{}", summary.lines().next().unwrap_or("").trim());
        die(store, id, 1);
    }

    let title = store.read(item_rel(id, "title")).unwrap_or_default();
    let msg = prompt::commit_msg_from_summary(&summary, id, &title);

    if let Some(deny) = &cfg.msg_deny {
        if prompt::msg_hygiene_violation(&msg, deny) {
            eprintln!(
                "{}FAIL: сообщение коммита нарушает гигиену ({deny}): «{msg}»{}",
                c.r, c.z
            );
            eprintln!(
                "      поправь ПЕРВУЮ строку {} (без упоминаний workflow) и: devh commit {}",
                store.summary_file(id).display(),
                id.dir_name()
            );
            die(store, id, 1);
        }
    }

    git::add_all(wt)?;
    if !git::has_staged(wt) {
        eprintln!("{}FAIL: агент не оставил изменений{}", c.r, c.z);
        die(store, id, 1);
    }
    println!("— staged:");
    let stat = git::staged_stat(wt);
    let lines: Vec<&str> = stat.lines().collect();
    for l in lines.iter().skip(lines.len().saturating_sub(5)) {
        println!("{l}");
    }
    let dels = git::staged_deletions(wt);
    if dels > 5 {
        println!(
            "{}⚠  удалений в коммите: {dels} — проверь, что это задумано{}",
            c.y, c.z
        );
    }
    git::commit(wt, &msg)?;
    println!("{}OK commit: {msg}{}", c.g, c.z);
    // Коммит сделан → фаза «committed»: оракул предложит merge (S5).
    mark_phase(store, id, wt, runstate::Phase::Committed);

    let gates_list = match (cfg.mutate.is_some(), cfg.bench.is_some()) {
        (true, true) => "verify,mutation,bench",
        (true, false) => "verify,mutation",
        (false, true) => "verify,bench",
        (false, false) => "verify",
    };
    let last_run = format!(
        "fix-rounds={round} gates={gates_list} duration={}s\n{}\n",
        timing.elapsed(),
        timing.summary_line()
    );
    store.atomic_write(item_rel(id, "last-run"), &last_run)?;

    if git::has_remote(root) {
        if git::push(wt, &cfg.remote, &id.branch()).is_ok() {
            println!("{}OK push: {}/{}{}", c.g, cfg.remote, id.branch(), c.z);
        } else {
            println!(
                "{}⚠  push не прошёл — коммит локальный; продолжай: devh merge {}{}",
                c.y,
                id.dir_name(),
                c.z
            );
        }
    }
    timing.stage("commit+push", store, id);
    Ok(())
}

/// Хвост run(): сводка тайминга стадий (контракт `devh ps`), статус Active
/// (прогон завершён — item готов к ревью/merge) и инструкция «дальше».
fn finalize(cfg: &RunConfig, id: &ItemId, wt: &Path, timing: &Timing) -> anyhow::Result<()> {
    let c = colors();
    println!();
    println!("{}— тайминг стадий (сек):{}", c.c, c.z);
    for (name, sec) in timing.stages() {
        println!("{}  {name:<24} {sec}s{}", c.d, c.z);
    }
    // A3 (фикс дефекта 2): раньше тут стоял set_status(Active) — мёртвая
    // запись в обход transition (claim уже поставил Active; done Item мог
    // уползти обратно в active). Пишем статус только через apply_event.
    println!();
    println!(
        "{}ГОТОВО: {} на ветке {}.{} Ревью: git -C {} diff {}..{} — затем: {}devh merge {}{}",
        c.b,
        id.dir_name(),
        id.branch(),
        c.b,
        wt.display(),
        cfg.main_branch,
        id.branch(),
        c.b,
        id.dir_name(),
        c.z
    );
    Ok(())
}

/// Сдача после вердикта гейта: причина печатается по классу отказа, код
/// выхода приходит из decide_round (единственный источник кодов). Fix —
/// бюджет fix-раундов исчерпан (rc=1); Env — блокер СРЕДЫ, лог + недостающее
/// (rc=2); Spec — блокер СПЕКИ, чинит человек (rc=1).
fn give_up(
    store: &Store,
    id: &ItemId,
    stop: &GateStop,
    kind: gates::FailureKind,
    code: i32,
    max_fix: u32,
    wt: &Path,
) -> ! {
    let c = colors();
    // Сдача — фиксируем ФАКТ (S10 в оракуле `devh next`): бюджет/класс
    // отказа исчерпан, следующий шаг — `devh abort`, а не новый run.
    let head = git::short_head(wt);
    let digest = git::tree_digest(wt).unwrap_or_default();
    if let Err(e) = runstate::write_or_invalidate(
        store,
        id,
        &runstate::RunState::gate_failed(&verdict_gate_name(&stop.verdict), code, &head)
            .with_digest(&digest),
    ) {
        eprintln!("devh: маркер сдачи не записан ({e}) — оракул перепроверит гейты заново");
    }
    match kind {
        gates::FailureKind::Fix => {
            let Verdict::Fail { gate, .. } = &stop.verdict else {
                unreachable!("класс Fix несёт Fail-вердикт")
            };
            eprintln!(
                "{}FAIL: гейт {} красный после {} раундов фикса (логи: {}){}",
                c.r,
                gate.name(),
                max_fix,
                store.dir.join("logs").display(),
                c.z
            );
        }
        gates::FailureKind::Env => {
            let Verdict::EnvBlocked { missing } = &stop.verdict else {
                unreachable!("класс Env несёт EnvBlocked-вердикт")
            };
            eprint!("{}", stop.log);
            eprintln!("{missing}");
        }
        gates::FailureKind::Spec => {
            let Verdict::SpecBlock { gate, .. } = &stop.verdict else {
                unreachable!("класс Spec несёт SpecBlock-вердикт")
            };
            eprintln!(
                "{}FAIL: гейт {} — блокер СПЕКИ: чинит человек (правка спеки/red-expect), а не fix-раунд агента.{}",
                c.r,
                gate.name(),
                c.z
            );
        }
    }
    die(store, id, code)
}

/// Остановка гейта: вердикт (роутинг + gate/tail для fix-раунда) + полный лог.
/// Для env-ветки `Verdict::EnvBlocked{missing}` несёт ГОТОВОЕ человекочитаемое
/// сообщение гейта (как это уже делает `crap_gate`), `log` — вывод инструмента.
struct GateStop {
    verdict: Verdict,
    log: String,
}

fn run_red(red_file: &Path, wt: &Path, nice: bool) -> Raw {
    // P7: retry под nice — контеншен (load≈11 был) убивает red-таймаутом.
    let prefix = if nice { "nice -n 19 " } else { "" };
    let cmd = format!("{prefix}bash {}", shell_quote(&red_file.to_string_lossy()));
    match proc::run_logged(&cmd, wt, RED_TIMEOUT, &[]) {
        Ok(out) => Raw {
            exit: out.exit,
            log: proc::combined(&out),
            missing_tool: None,
        },
        Err(e) => Raw {
            exit: None,
            log: e.describe(&cmd),
            missing_tool: None,
        },
    }
}

/// P7 (red.expect): приоритет источников подписи причины падения:
/// 1. маркер `# red-expect:` в red.cmd (агент пишет его рядом с тестом);
/// 2. строка спеки `RED-EXPECT:` (файл items/W-NNN/red-expect от `devh new`);
/// 3. ключ конфига RED_EXPECT (глобальный дефолт).
fn resolve_red_expect(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    red_src: &str,
) -> Option<String> {
    gates::red_expect_marker(red_src)
        .or_else(|| {
            store
                .read(item_rel(id, "red-expect"))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| cfg.red_expect.clone())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[allow(clippy::too_many_arguments)]
fn run_agent(
    store: &Store,
    wt: &Path,
    id: &ItemId,
    prompt_text: &str,
    agent: &str,
    timeout: Duration,
    stage: impl Into<String>,
    timing: &mut Timing,
    cfg: &RunConfig,
) -> anyhow::Result<()> {
    let full = format!("{} {}", agent, prompt::shquote(prompt_text));
    let envs = agent_env(id, wt, store, cfg);
    match proc::run_logged(&full, wt, timeout, &envs) {
        Ok(out) => {
            let log = proc::combined(&out);
            std::fs::write(store.log_file(id, "agent"), &log).ok();
            anyhow::ensure!(
                out.exit == Some(0),
                "агент завершился с кодом {:?}",
                out.exit
            );
            timing.stage(stage, store, id);
            Ok(())
        }
        Err(e) => {
            std::fs::write(store.log_file(id, "agent"), e.describe(&full)).ok();
            Err(anyhow::anyhow!("{}", e.describe(&full)))
        }
    }
}

fn agent_env(id: &ItemId, wt: &Path, store: &Store, cfg: &RunConfig) -> Vec<(String, String)> {
    vec![
        ("HARNESS_WORKTREE".into(), wt.display().to_string()),
        (
            "HARNESS_SPEC".into(),
            store.item_dir(id).join("spec.md").display().to_string(),
        ),
        (
            "HARNESS_PLAN".into(),
            store.item_dir(id).join("plan.md").display().to_string(),
        ),
        (
            "HARNESS_SUMMARY".into(),
            store.summary_file(id).display().to_string(),
        ),
        ("HARNESS_VERIFY".into(), cfg.verify.trim().to_string()),
    ]
}

/// A6 FREEZE-гейт: дифф сессии против allowlist (per-item FILES: спеки +
/// глобальный FREEZE_ALLOW). Первый в лестнице — дешевле всех: git-дифф
/// вместо прогона тестов, fail-fast до VERIFY. Правка tests/** — только при
/// записанном обходе items/W-NNN/allow-test-edit (симметрия no-crap: обход
/// записывают с причиной, а не надеются на «и так пройдёт»). Пустой allowlist
/// (ни FILES: в спеке, ни FREEZE_ALLOW) — «скоп не заявлен»: пропуск С ЛОГОМ
/// (не молчаливый Pass, не tripwire на каждую правку). Считает чистый
/// gates::freeze_violations — IO только на чтении allowlist'а.
///
/// F-0.2 (N1): источник списка — fail-closed `git::changes_strict` (F-0.1).
/// Ошибка git (не репо, битый HEAD, сломанный индекс, нечитаемый конфиг) —
/// это НЕ «дифф пуст»: молчаливое «чисто» пропустило бы сессию мимо гейта
/// ровно тогда, когда гейт не смог посмотреть. Поэтому вместо fail-open
/// `changed_files()` берём Result и отдаём `EnvBlocked` (rc=2, блокер среды
/// чинит человек), а `gate_ladder` перестаёт гарантировать «Free не отдаёт
/// Env/Spec» — та ветка стала легальной, и `unreachable!` убран.
/// Чистая часть FREEZE-гейта (F-0.2): allowlist + список файлов → лог и
/// вердикт. Вынесена из `freeze_gate`, чтобы решение тестировалось без
/// Store/worktree: пустой allowlist, записанный обход и правка вне scope —
/// три разных судьбы, и мутант в любой из них обязан быть CAUGHT.
/// `git`-ошибка сюда НЕ входит: она решается раньше и даёт `EnvBlocked`.
fn freeze_verdict(changed: &[String], allow: &[String], test_edit: bool) -> (String, Verdict) {
    if allow.is_empty() {
        return (
            "freeze: scope не задан (нет FILES: в спеке и FREEZE_ALLOW) — гейт пропущен\n".into(),
            Verdict::Pass,
        );
    }
    let mut log = format!(
        "freeze: allowlist [{}]; обход allow-test-edit: {}; дифф: {}\n",
        allow.join(", "),
        if test_edit {
            "записан"
        } else {
            "нет"
        },
        if changed.is_empty() {
            "пусто".to_string()
        } else {
            changed.join(", ")
        }
    );
    let violations = gates::freeze_violations(changed, allow, test_edit);
    let verdict = if violations.is_empty() {
        Verdict::Pass
    } else {
        // причины — и в лог, и в tail: give_up печатает только имя гейта,
        // поэтому доказательство нарушения живёт в logs/W-NNN-freeze.log
        log.push_str(&violations.join("\n"));
        log.push('\n');
        Verdict::Fail {
            gate: Gate::Freeze,
            tail: violations.join("\n"),
        }
    };
    (log, verdict)
}

fn freeze_gate(store: &Store, cfg: &RunConfig, wt: &Path, id: &ItemId) -> (String, Verdict) {
    let changed: Vec<String> = match git::changes_strict(wt) {
        Ok(v) => v.into_iter().map(|c| c.path).collect(),
        Err(e) => {
            let missing = format!(
                "FREEZE: список изменений нечитаем ({e}) — гейт не может сравнить дифф со scope"
            );
            return (
                format!("freeze: {missing}\n"),
                Verdict::EnvBlocked { missing },
            );
        }
    };
    let mut allow = cfg.freeze_allow.clone();
    if let Some(files) = store.read(item_rel(id, "files")) {
        allow.extend(
            files
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }
    let test_edit = store
        .read(item_rel(id, "allow-test-edit"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    freeze_verdict(&changed, &allow, test_edit)
}

/// C1: контракт задачи — МЯГКАЯ проверка по диффу (НЕ гейт): goal/observe
/// заполнены, red.cmd имеет red.expect (P7), дифф внутри/вне files.touch
/// (FREEZE гейтит жёстко — A6; здесь только предупреждение и «заявлено,
/// не тронуто»). Никогда не двигает вердикт: пишет logs/W-NNN-contract.log
/// и печатает жёлтым. Читает items/W-NNN/contract.toml.
fn contract_note(store: &Store, wt: &Path, id: &ItemId) -> Option<String> {
    let text = store.read(item_rel(id, "contract.toml"))?;
    let c = crate::core::contract::Contract::parse(&text);
    if c.is_empty() {
        return None;
    }
    let has_red = store.read(item_rel(id, "red.cmd")).is_some();
    // F-0.2 (N1): проверка мягкая, но и она не делает вид, что смотрела:
    // нечитаемый дифф ≠ замечаний нет. FREEZE уже остановил бы такой прогон
    // (EnvBlocked), поэтому сюда доходит только FREEZE=0 — предупреждение
    // заменяет молчание, вердикт по-прежнему не двигается.
    let changed: Vec<String> = match git::changes_strict(wt) {
        Ok(v) => v.into_iter().map(|c| c.path).collect(),
        Err(e) => {
            return Some(format!(
                "контракт {} (МЯГКО, вердикт не двигает)\nконтракт: дифф нечитаем ({e}) — \
                 сверка touch/touched не выполнена\n",
                id.dir_name(),
            ));
        }
    };
    let findings = crate::core::contract::contract_check(&c, &changed, has_red);
    if findings.is_empty() {
        return None;
    }
    Some(format!(
        "контракт {} (МЯГКО, вердикт не двигает)\n{}\n",
        id.dir_name(),
        findings.join("\n")
    ))
}

fn gate_ladder(
    store: &Store,
    cfg: &RunConfig,
    wt: &Path,
    id: &ItemId,
    timing: &mut Timing,
    round: u32,
) -> Result<(), GateStop> {
    let c = colors();

    // A6: FREEZE — первым (дешевле всех): git-дифф до любого прогона.
    if cfg.freeze {
        let (flog, verdict) = freeze_gate(store, cfg, wt, id);
        std::fs::write(store.log_file(id, "freeze"), &flog).ok();
        timing.stage("freeze", store, id);
        match verdict {
            Verdict::Pass => {}
            // F-0.2: freeze_gate читает git (changes_strict) — значит env-ветка
            // легальна: нечитаемый дифф = блокер среды (rc=2), а не «чисто».
            // Fail-closed: любой не-Pass вердикт останавливает лестницу, и
            // новый вариант Verdict не превращается в панику harness'а.
            other => {
                return Err(GateStop {
                    verdict: other,
                    log: flog,
                })
            }
        }
    }

    // C1: контракт — мягкая проверка ПОСЛЕ жёсткой FREEZE (дифф уже известен).
    // Предупреждение, не вердикт: и заявленные-не тронутые, и незаявленные
    // правки видны в логе/консоли, пока гейты решают жёстко (advance2 §C1).
    if let Some(note) = contract_note(store, wt, id) {
        // в лог — одной строкой (grep-дружественно), в консоль — построчно:
        // замечания читаются списком, а не сбиваются в «:; …; …»
        std::fs::write(store.log_file(id, "contract"), note.replace('\n', "; ")).ok();
        for l in note.lines() {
            println!("{}.. {l}{}", c.y, c.z);
        }
    }

    let verify_raw = exec_logged(store, wt, id, &cfg.verify, "verify", VERIFY_TIMEOUT);
    // A8: flaky-suspect. Падение VERIFY в РАНД 1 при включённых повторах —
    // один повтор под nice + пауза 2с (контеншен — самый частый флейк).
    // Прошло → сессия живёт, НО строка в flaky.md + предупреждение в консоль:
    // молчаливого «прошло» не существует. Ретраи в прогодах fix-раундов
    // (>0) запрещены: там «зелёно» обязано быть честным с первого раза.
    let verify_raw = if verify_raw.exit != Some(0) && round == 0 && cfg.verify_retries > 0 {
        println!(
            "{}.. VERIFY упал в раунде 1 — повтор под nice (VERIFY_RETRIES={}){}",
            c.y, cfg.verify_retries, c.z
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
        let retry = verify_retry(store, wt, id, &cfg.verify);
        if retry.exit == Some(0) {
            println!(
                "{}⚠ flaky-suspect (retry ok): VERIFY раунда 1 прошёл со 2-го раза — .harness/flaky.md{}",
                c.y, c.z
            );
            append_flaky(store, id, 1);
            retry
        } else {
            verify_raw
        }
    } else {
        verify_raw
    };
    if verify_raw.exit != Some(0) {
        return Err(GateStop {
            verdict: Verdict::Fail {
                gate: Gate::Verify,
                tail: tail_of(&verify_raw.log, 40),
            },
            log: verify_raw.log,
        });
    }
    timing.stage(format!("verify-r{}", round.saturating_add(1)), store, id);

    if cfg.no_ignored {
        let violations = gates::unexpected_ignores(&verify_raw.log, &cfg.ignored_allowlist);
        if !violations.is_empty() {
            return Err(GateStop {
                verdict: Verdict::Fail {
                    gate: Gate::NoIgnored,
                    tail: violations.join("\n"),
                },
                log: String::new(),
            });
        }
    }

    let (lint_log, verdict) = lint_gate(cfg, wt);
    std::fs::write(store.log_file(id, "lint"), &lint_log).ok();
    timing.stage(format!("lint-r{}", round.saturating_add(1)), store, id);
    match verdict {
        Verdict::EnvBlocked { missing } => {
            return Err(GateStop {
                verdict: Verdict::EnvBlocked {
                    missing: format!(
                        "FAIL: lint env-блокер. Блокер СРЕДЫ, не работа агента (детали: {missing})."
                    ),
                },
                log: lint_log,
            })
        }
        Verdict::Fail { .. } => {
            return Err(GateStop {
                verdict,
                log: lint_log,
            })
        }
        Verdict::Pass => {}
        Verdict::SpecBlock { .. } => unreachable!("lint_gate не отдаёт SpecBlock"),
    }

    if let Some(cr) = &cfg.crap {
        let no_crap = store
            .read(item_rel(id, "no-crap"))
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if no_crap {
            let reason = store.read(item_rel(id, "no-crap")).unwrap_or_default();
            let bypass = format!("crap: записанный обход (no-crap): {}\n", reason.trim());
            std::fs::write(store.log_file(id, "crap"), &bypass).ok();
        } else {
            let (clog, verdict) = crap_gate(wt, cr);
            std::fs::write(store.log_file(id, "crap"), &clog).ok();
            timing.stage(format!("crap-r{}", round.saturating_add(1)), store, id);
            match verdict {
                Verdict::Pass => {}
                Verdict::EnvBlocked { missing } => {
                    return Err(GateStop {
                        verdict: Verdict::EnvBlocked {
                            missing: format!(
                                "FAIL: CRAP env-блокер ({missing}). Блокер СРЕДЫ, не работа агента (детали выше)."
                            ),
                        },
                        log: clog,
                    })
                }
                Verdict::Fail { tail, .. } => {
                    return Err(GateStop {
                        verdict: Verdict::Fail {
                            gate: Gate::Crap,
                            tail,
                        },
                        log: clog,
                    })
                }
                // Дефект 1 (был): SpecBlock уходил в Red — агент получал
                // fix-раунд на том, что чинит только человек (спека/red-expect).
                // Теперь это свой класс: failure_kind → Spec → die(1).
                Verdict::SpecBlock { tail, .. } => {
                    return Err(GateStop {
                        verdict: Verdict::SpecBlock {
                            gate: Gate::Crap,
                            tail,
                        },
                        log: clog,
                    })
                }
            }
        }
    }

    if cfg.review {
        let (review_log, verdict) = review_gate(store, cfg, wt, id);
        std::fs::write(store.log_file(id, "review"), &review_log).ok();
        timing.stage(format!("review-r{}", round.saturating_add(1)), store, id);
        match verdict {
            Verdict::EnvBlocked { missing } => {
                return Err(GateStop {
                    verdict: Verdict::EnvBlocked {
                        missing: format!(
                            "FAIL: review env-блокер ({missing}). Блокер СРЕДЫ, не работа агента."
                        ),
                    },
                    log: review_log,
                })
            }
            Verdict::Fail { .. } => {
                return Err(GateStop {
                    verdict,
                    log: review_log,
                })
            }
            Verdict::Pass => {}
            // F-0.3: дифф больше REVIEW_DIFF_MAX — блокер СПЕКИ (дроби
            // задачу), а не работа агента: fix-раунды его не размельчат.
            Verdict::SpecBlock { tail, .. } => {
                return Err(GateStop {
                    verdict: Verdict::SpecBlock {
                        gate: Gate::Review,
                        tail,
                    },
                    log: review_log,
                })
            }
        }
    }

    if let Some(m) = &cfg.mutate {
        let rep = mutate_gate(m, wt, &store.dir, None, "mutation", "MUTATE", Gate::Mutate)
            .unwrap_or_else(|e| {
                eprintln!("{}{e}{}", c.r, c.z);
                die(store, id, 1);
            });
        let mpath = store.log_file(id, "mutate");
        std::fs::write(&mpath, &rep.log).ok();
        timing.stage(format!("mutate-r{}", round.saturating_add(1)), store, id);
        perf_mutate(store, id, &rep, timing);
        // match по ссылке + clone: иначе байнд `verdict` тенейватся
        // чужой (lint/review) копией Pass, а Fail уносил бы Pass в
        // GateStop → failure_kind(Pass) паниковал (gates.rs unreachable).
        match &rep.verdict {
            Verdict::EnvBlocked { .. } => {
                return Err(GateStop {
                    verdict: Verdict::EnvBlocked {
                        missing: format!(
                            "{}FAIL: mutation env-блокер — инструмент не распознан. Блокер СРЕДЫ, не работа агента: установи (gobot: muex; rust: cargo-mutants) или исправь MUTATE в .harness/config. Детали: {}{}",
                            c.r,
                            mpath.display(),
                            c.z
                        ),
                    },
                    log: rep.log,
                })
            }
            Verdict::Fail { .. } => {
                // Хвост лога — в консоль: «FAIL: kill-rate …» и причина
                // раннего останова видны сразу, а не только в logs/mutate.
                eprintln!("{}", tail_of(&rep.log, 40));
                if rep.fatal {
                    // P7-2.3: ранний выход (DOOM / отравленное дерево) —
                    // вердикт сразу. Fix-раунды перепрогнали бы мутатор
                    // ради того же итога — ровно то, от чего мы уходим.
                    eprintln!(
                        "{}FAIL: гейт mutation остановлен досрочно (MUTATE_EARLY=1) — раунды фикса не нужны (детали: {}){}",
                        c.r,
                        mpath.display(),
                        c.z
                    );
                    die(store, id, 1);
                }
                return Err(GateStop {
                    verdict: rep.verdict.clone(),
                    log: rep.log,
                })
            }
            Verdict::Pass => {
                // P7-2.3: SAMPLE — решение «доверяем выборке» (rule-of-three).
                // Гейт зелёный, но часть мутантов не прогонялась: причина
                // обязана быть в консоли, иначе молчаливый пропуск хвоста.
                if rep.early {
                    eprintln!("{}", tail_of(&rep.log, 40));
                }
            }
            Verdict::SpecBlock { .. } => unreachable!("mutate_gate не отдаёт SpecBlock"),
        }
    }

    if let Some(bench) = &cfg.bench {
        let raw = exec_logged(store, wt, id, bench, "bench", BENCH_TIMEOUT);
        timing.stage(format!("bench-r{}", round.saturating_add(1)), store, id);
        if raw.exit != Some(0) {
            return Err(GateStop {
                verdict: Verdict::Fail {
                    gate: Gate::Bench,
                    tail: tail_of(&raw.log, 40),
                },
                log: raw.log,
            });
        }
    }

    Ok(())
}

fn exec_logged(
    store: &Store,
    wt: &Path,
    id: &ItemId,
    cmd: &str,
    name: &str,
    timeout: Duration,
) -> Raw {
    match proc::run_logged(cmd, wt, timeout, &[]) {
        Ok(out) => {
            let log = proc::combined(&out);
            std::fs::write(store.log_file(id, name), &log).ok();
            Raw {
                exit: out.exit,
                log,
                missing_tool: None,
            }
        }
        Err(e) => Raw {
            exit: None,
            log: e.describe(cmd),
            missing_tool: None,
        },
    }
}

/// A8: повтор VERIFY под nice (контеншен убивает таймаутом, урок P7) — лог
/// отдельный (`verify-retry`): первый провал остаётся доказательством в
/// `verify`, ретрай его не перезатирает.
fn verify_retry(store: &Store, wt: &Path, id: &ItemId, cmd: &str) -> Raw {
    let nice = format!("nice -n 19 {cmd}");
    match proc::run_logged(&nice, wt, VERIFY_TIMEOUT, &[]) {
        Ok(out) => {
            let log = proc::combined(&out);
            std::fs::write(store.log_file(id, "verify-retry"), &log).ok();
            Raw {
                exit: out.exit,
                log,
                missing_tool: None,
            }
        }
        Err(e) => Raw {
            exit: None,
            log: e.describe(&nice),
            missing_tool: None,
        },
    }
}

/// A8: запись строки flaky-журнала (`.harness/flaky.md`) — append через
/// атомарный хвост-append (create + push строки). Ledger НЕ удаляется
/// doctor'ом: это доказательство флейка, а не мусор.
fn append_flaky(store: &Store, id: &ItemId, round: u32) {
    let date = crate::core::survivors::today_ymd();
    let line = gates::flaky_line(&date, &id.dir_name(), "verify", round);
    let path = store.root().join(".harness").join("flaky.md");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    let _ = std::fs::write(path, text);
}

/// Линт-гейт: лог + вердикт. Env-причина (нет shellcheck при изменённых .sh)
/// строится там, где факт известен — stringly-детект `"lint env:"` не нужен.
fn lint_gate(cfg: &RunConfig, wt: &Path) -> (String, Verdict) {
    let mut log = String::new();
    let mut bad = false;
    // F-0.2 (N1): тот же fail-closed источник, что у FREEZE. Раньше ошибка git
    // давала пустой `files` → «изменённых .sh нет» → shellcheck пропускался
    // молча: гейт ослаблялся ровно тогда, когда дерево не читалось.
    let files: Vec<String> = match git::changes_strict(wt) {
        Ok(v) => v.into_iter().map(|c| c.path).collect(),
        Err(e) => {
            let missing = format!("lint: список изменений нечитаем ({e}) — гейт не может решить");
            return (
                format!("lint: {missing}\n"),
                Verdict::EnvBlocked { missing },
            );
        }
    };
    if cfg.lint {
        let has_sh = files.iter().any(|f| f.ends_with(".sh"));
        if has_sh && !which("shellcheck") {
            log += "lint env: нет shellcheck, а изменены .sh — блокер СРЕДЫ (установи)\n";
            return (
                log,
                Verdict::EnvBlocked {
                    missing: "нет shellcheck, а изменены .sh — блокер СРЕДЫ (установи)".into(),
                },
            );
        }
        for f in &files {
            if !f.ends_with(".sh") {
                continue;
            }
            let p = wt.join(f);
            if !p.is_file() {
                continue;
            }
            log += &format!("lint: shellcheck {f}\n");
            match std::process::Command::new("shellcheck")
                .arg(&p)
                .current_dir(wt)
                .output()
            {
                Ok(out) => {
                    log.push_str(&String::from_utf8_lossy(&out.stdout));
                    log.push_str(&String::from_utf8_lossy(&out.stderr));
                    if !out.status.success() {
                        bad = true;
                    }
                }
                Err(e) => {
                    log += &format!("shellcheck spawn failed: {e}\n");
                    bad = true;
                }
            }
        }
    }
    if !cfg.lint_cmd.is_empty() {
        log += &format!("lint: {}\n", cfg.lint_cmd);
        match std::process::Command::new("bash")
            .arg("-c")
            .arg(&cfg.lint_cmd)
            .current_dir(wt)
            .output()
        {
            Ok(out) => {
                log.push_str(&String::from_utf8_lossy(&out.stdout));
                log.push_str(&String::from_utf8_lossy(&out.stderr));
                if !out.status.success() {
                    bad = true;
                }
            }
            Err(e) => {
                log += &format!("lint_cmd spawn failed: {e}\n");
                bad = true;
            }
        }
    }
    if bad {
        log += "lint FAIL: замечания линтера выше\n";
        let tail = tail_of(&log, 30);
        return (
            log,
            Verdict::Fail {
                gate: Gate::Lint,
                tail,
            },
        );
    }
    (log, Verdict::Pass)
}

/// F-0.3: размер диффа для ревью. Чистая развилка (тестируется без git):
/// `max == 0` — без лимита (аварийный клапан), `lines <= max` — Pass,
/// иначе SpecBlock REVIEW: «дроби задачу» — это спека велика, чинит её
/// человек, а не агент fix-раундами (rc=1).
fn review_size_verdict(lines: usize, max: u32) -> Verdict {
    if max != 0 && lines > max as usize {
        return Verdict::SpecBlock {
            gate: Gate::Review,
            tail: format!("дифф {lines} строк > REVIEW_DIFF_MAX={max}"),
        };
    }
    Verdict::Pass
}

/// F-0.3: дерево после ревьюера. Расхождение дайджестов = `EnvBlocked`
/// «ревьюер изменил дерево»: ревьюер — второй запрос ТОМУ ЖЕ агенту, и
/// правка файлов «заодно с ревью» — это работа мимо гейтов (серия F:
/// гейты не обходятся молча). Нечитаемое дерево после ревью — тоже
/// env-блокер: без этого проверка вслепую превращалась бы в «чисто».
/// Дайджест дерева до/после ревью. `Some((причина, verdict))` — блокер:
/// ревьюер изменил дерево, либо дайджест не читается (обе — среда/обход,
/// rc=2, сильнее любого «VERDICT: OK»). `None` — дерево не тронуто.
fn post_review_guard(before: &str, wt: &Path) -> Option<(String, Verdict)> {
    let after = match git::tree_digest(wt) {
        Ok(a) => a,
        Err(e) => {
            return Some((
                format!("дайджест дерева после ревью не читается ({e})"),
                Verdict::EnvBlocked {
                    missing: "дайджест дерева после ревью не читается".into(),
                },
            ))
        }
    };
    (after != before).then(|| {
        (
            "ревьюер изменил дерево".to_string(),
            Verdict::EnvBlocked {
                missing: "ревьюер изменил дерево".into(),
            },
        )
    })
}

/// Ревью-гейт: лог + вердикт (Ok→Pass, Issues→Fix, всё остальное — среда).
///
/// F-0.3, три новых свойства:
/// 1) дифф читается единым `git::diff_text` — ошибка git роняет гейт в
///    `EnvBlocked`, а не в «диффа нет — пропуск» (слепота);
/// 2) дифф больше `REVIEW_DIFF_MAX` строк → `SpecBlock` (дроби задачу);
/// 3) дайджест дерева до и после запроса ревьюеру — правка файлов из
///    ревью ловится (`post_review_guard`).
fn review_gate(store: &Store, cfg: &RunConfig, wt: &Path, id: &ItemId) -> (String, Verdict) {
    let diff = match git::diff_text(wt, "") {
        Ok(d) => d,
        Err(e) => {
            return (
                format!("review: diff не читается ({e})\n"),
                Verdict::EnvBlocked {
                    missing: format!("git diff не читается ({e}) — не «чисто», а слепота"),
                },
            )
        }
    };
    if diff.trim().is_empty() {
        return ("review: диффа нет — пропуск".into(), Verdict::Pass);
    }
    let lines = diff.lines().count();
    match review_size_verdict(lines, cfg.review_diff_max) {
        Verdict::Pass => {}
        spec => {
            let log = format!(
                "review: дифф {lines} строк > REVIEW_DIFF_MAX={} — ревьюер прочитал бы его вслепую.\n\
                 Дроби задачу: этот дифф — признак спеки, которая велика, а не работы агента.\n",
                cfg.review_diff_max
            );
            return (log, spec);
        }
    }
    let before = match git::tree_digest(wt) {
        Ok(d) => d,
        Err(e) => {
            return (
                format!("review: дерево не читается ({e})\n"),
                Verdict::EnvBlocked {
                    missing: format!("дайджест дерева не читается ({e})"),
                },
            )
        }
    };
    let agent = store
        .read(item_rel(id, "agent"))
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| cfg.agent.clone());
    let full = format!(
        "{} {}",
        agent.trim(),
        prompt::shquote(&prompt::review_prompt(&diff))
    );
    let envs = agent_env(id, wt, store, cfg);
    let out = proc::run_logged(
        &full,
        wt,
        Duration::from_secs(cfg.agent_timeout as u64),
        &envs,
    );
    let (log, verdict) = match out {
        Ok(out) => {
            let log = proc::combined(&out);
            if out.exit != Some(0) {
                (
                    format!("{log}\nreview: агент-ревьюер упал (API/timeout)\n"),
                    Verdict::EnvBlocked {
                        missing: "агент-ревьюер упал (API/timeout)".into(),
                    },
                )
            } else {
                let outcome = match parse_review_verdict(&log) {
                    ReviewVerdict::Ok => Verdict::Pass,
                    ReviewVerdict::Issues(_) => Verdict::Fail {
                        gate: Gate::Review,
                        tail: tail_of(&log, 5),
                    },
                    ReviewVerdict::Unrecognized => Verdict::EnvBlocked {
                        missing: "вердикт не распознан (нужна строка VERDICT: …)".into(),
                    },
                };
                (log, outcome)
            }
        }
        Err(e) => (
            format!("review: агент-ревьюер упал: {}\n", e.describe(&full)),
            Verdict::EnvBlocked {
                missing: "агент-ревьюер упал (API/timeout)".into(),
            },
        ),
    };
    // Дайджест ПОСЛЕ — сильнее любого вердикта ревьюера: молчаливая правка
    // дерева перебивает даже «VERDICT: OK».
    // post_review_guard отдаёт (причина, EnvBlocked) — без внутреннего match с
    // `_`-arm: его «delete match arm» не компилировался (unviable = дыра).
    match post_review_guard(&before, wt) {
        Some((why, v)) => (format!("{log}review: {why}\n"), v),
        None => (log, verdict),
    }
}

fn crap_gate(wt: &Path, cr: &CrapCfg) -> (String, Verdict) {
    let files = git::changed_files_scoped(wt, &cr.scope);
    if files.is_empty() {
        return (
            format!(
                "crap: нет изменённых файлов в {} — гейт пропущен\n",
                cr.scope
            ),
            Verdict::Pass,
        );
    }
    let envs = vec![("DEVH_CRAP_FILES".to_string(), files.join("\n"))];
    let raw = match proc::run_logged(&cr.tool, wt, VERIFY_TIMEOUT, &envs) {
        Ok(out) => proc::combined(&out),
        Err(e) => e.describe(&cr.tool),
    };
    match gates::parse_crap_report(&raw, cr.max) {
        None => (
            format!(
                "crap env: инструмент не распознан (нет 'CRAP: functions=N'); CRAP='{}'\n",
                cr.tool
            ),
            Verdict::EnvBlocked {
                missing: "CRAP: вывод инструмента не распознан (нет 'CRAP: functions=N')".into(),
            },
        ),
        Some(report) => {
            let mut log = raw;
            if !log.ends_with('\n') {
                log.push('\n');
            }
            log += &report.derived;
            let verdict = if report.violations.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: Gate::Crap,
                    tail: report.derived,
                }
            };
            (log, verdict)
        }
    }
}

/// Статистика мутационного прогона (P4: из неё пишется perf.log).
///
/// `killed` = **только пойманные** (`CaughtMutant`): таймаут поимкой не
/// считается (F-3). `missed` и `timeout` — открытые вопросы (дыра в
/// покрытии / не уложился), `unviable` — провал (ноль терпимости).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MutStats {
    total: u32,
    /// Сколько мутантов ПЛАНИРОВАЛОСЬ (mutants.json) — для остановленного
    /// прогона: `total` = проверенные, `planned` = все. 0 = неизвестно.
    planned: u32,
    killed: u32,
    missed: u32,
    timeout: u32,
    unviable: u32,
    /// Мутанты без тестовой фазы (итог «N succeeded»): поимка не
    /// подтверждена; ни поимка, ни дыра покрытия.
    succeeded: u32,
    /// Мутанты с неопознанным исходом (слова нет в контракте
    /// cargo-mutants 27.1.0): «пойман или нет» неизвестно → поимка не
    /// подтверждена. Видно только в `outcomes.json` (в append-списки
    /// такие исходы не пишутся) — отсюда значение есть у остановленного
    /// прогона, а у разобранной строки итога — нет (там исход не
    /// распознан → `MutRun::Unknown`).
    unknown: u32,
}

/// Итог мутационного гейта: лог, вердикт и (если вывод распарсился) статы.
/// Default = пустой зелёный отчёт (мутантов не было) — нулевое значение.
#[derive(Default)]
struct MutReport {
    log: String,
    verdict: Verdict,
    stats: Option<MutStats>,
    /// P7-2.3: ранний выход (DOOM / отравленное дерево) — вердикт сразу,
    /// без fix-раундов: три перепрогона мутатора ради того же результата
    /// ровно то, от чего ранний выход и защищает.
    fatal: bool,
    /// P7-2.3: прогон остановлен досрочно (SAMPLE — «доверяем выборке»).
    /// Это нетривиальное решение — пользователь обязан увидеть причину,
    /// даже когда гейт зелёный.
    early: bool,
}

// P7-2.4: покрытийный отбор — подготовка nextest -E фильтра.
/// Строит/загружает coverage index, получает список мутируемых строк из
/// `cargo mutants --list`, генерирует объединённый фильтр.
fn prepare_coverage_filter(
    wt: &Path,
    _m: &MutateCfg,
    tool: &str,
    _files: &[String],
) -> anyhow::Result<Option<String>> {
    use crate::core::coverage::{build_coverage_index, compute_test_files_hash, CoverageIndex};

    // Используем store root для кэша (wt может быть worktree, store — в главном репо).
    // Находим store: поднимаемся от wt до корня репо (где .harness).
    let store_root = wt
        .ancestors()
        .find(|p| p.join(".harness").is_dir())
        .ok_or_else(|| anyhow::anyhow!("no .harness found from wt"))?;

    // Хэш тест-файлов для инвалидации
    let test_hash = compute_test_files_hash(wt)?;

    // Пытаемся загрузить существующий индекс
    let index = if let Some(idx) = CoverageIndex::load_if_valid(store_root, &test_hash) {
        println!(".. coverage: loaded cached index (hash match)");
        idx
    } else {
        println!(".. coverage: building new index (llvm-cov)...");
        let idx = build_coverage_index(wt, store_root)?;
        // Сохраняем с текущим хэшем
        let mut idx = idx;
        idx.test_files_hash = test_hash;
        idx.save(store_root)?;
        println!(".. coverage: index built and cached");
        idx
    };

    // Получаем список мутантов из cargo-mutants --list
    // Сначала запускаем с --list чтобы узнать какие строки будут мутироваться
    let list_cmd = format!("{} --list", tool);
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&list_cmd)
        .current_dir(wt)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "cargo mutants --list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Парсим вывод --list (формат: "mutant_name file:line:col")
    let mut all_lines: std::collections::HashMap<String, Vec<u32>> =
        std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let file = parts.get(1).and_then(|s| s.split(':').next()).unwrap_or("");
            let line_str = parts.get(1).and_then(|s| s.split(':').nth(1)).unwrap_or("");
            if let Ok(line_num) = line_str.parse::<u32>() {
                all_lines
                    .entry(file.to_string())
                    .or_default()
                    .push(line_num);
            }
        }
    }

    if all_lines.is_empty() {
        return Ok(None);
    }

    // Генерируем фильтр: для каждого файла берём тесты для всех его мутируемых строк
    let mut all_tests = std::collections::HashSet::new();
    for (file, lines) in &all_lines {
        let tests = index.tests_for_lines(file, lines);
        all_tests.extend(tests);
    }

    if all_tests.is_empty() {
        return Ok(None);
    }

    let filter =
        crate::core::coverage::make_nextest_filter(&all_tests.into_iter().collect::<Vec<_>>());
    Ok(Some(filter))
}

/// Применяет coverage фильтр к команде cargo-mutants.
/// Добавляет `-- <filter>` после `--test-tool nextest` или в конец.
pub(crate) fn apply_cov_filter(cmd: &str, filter: Option<&str>) -> String {
    let Some(filter) = filter else {
        return cmd.to_string();
    };
    // Ищем --test-tool nextest и добавляем фильтр после него
    if let Some(pos) = cmd.find("--test-tool nextest") {
        let end = pos.saturating_add("--test-tool nextest".len());
        let rest = cmd.get(end..).unwrap_or("");
        // Находим следующую позицию для вставки (после nextest, перед другими флагами)
        return format!(
            "{} -- {}",
            &cmd[..end],
            format!("{} {}", rest.trim(), filter).trim()
        );
    }
    // Fallback: добавляем в конец перед {files}/{file}/{module} если есть
    if cmd.contains("{files}") || cmd.contains("{file}") || cmd.contains("{module}") {
        return cmd.to_string(); // фильтр не применим к batch/per-file просто так
    }
    format!("{} -- {}", cmd, filter)
}

/// P8-1: хук выживших (MUTATE_SURVIVORS=1). Ставится строго ПОСЛЕ каждого
/// finish(): batch-режим пишет один outcomes.json, per-file — каждый прогон
/// перезаписывает его заново, поэтому сбор «по MutReport на call-site» потерял
/// бы все планы кроме последнего. Очередь/seen/killmap — под тем же
/// GlobalLock, что и `tickets --import` (§8.7): единственный append.
/// `tool0` — шаблон прогона (нужен флаг `--output`, early::mutants_out_dir).
fn survivors_hook(m: &MutateCfg, wt: &Path, tool: &str, obs: &Path, gate: &str) -> Option<String> {
    if !m.survivors {
        return None;
    }
    let _g = crate::shell::lock::GlobalLock::acquire(obs).ok()?;
    let line = crate::core::survivors::collect_after_run(wt, tool, obs, gate);
    if let Some(l) = &line {
        eprintln!("{l}");
    }
    line
}

/// Гейт мутаций. scope — список git-pathspec через пробел; git сам матчит
/// глобы (`crates/*/src/*`) и выкидывает удалённые файлы.
///
/// `test`/`label`/`key` — red-фаза (RED_MUTATE): подстановка `{test}` в
/// шаблон, метка в строках лога и ключ конфига в env-сообщении.
///
/// `obs` — каталог `.harness` store'а (main-репо): очередь наблюдений и
/// состояние третьей петли (P8-1). Из worktree брать нельзя — у worktree
/// свой .harness, а `devh tickets --import` читает очередь store'а.
///
/// Три режима по шаблону `m.tool`:
/// - `{files}` — ОДИН запуск на все живые изменённые файлы (cargo-mutants
///   принимает несколько `-f`): один baseline и один тёплый target вместо N
///   холодных;
/// - `{file}`/`{module}` — по файлу на запуск (обратная совместимость);
/// - без плейсхолдеров — запуск как задан, без scope-фильтра.
fn mutate_gate(
    m: &MutateCfg,
    wt: &Path,
    obs: &Path,
    test: Option<&str>,
    label: &str,
    key: &str,
    gate: Gate,
) -> anyhow::Result<MutReport> {
    // P7: in-place прогон = агент для этого дерева. Лок держится на весь
    // гейт (batch и per-file цикл) — §9.13 «отравленный коммит».
    let _mut = MutateLock::try_acquire(wt, label).map_err(|holder| {
        anyhow::anyhow!(
            "devh: в дереве {} идёт in-place мутационный прогон ({holder}) — этот запуск отклонлён: \
             два in-place прогона в одном дереве взаимно мутируют базу (дождись или убери holder)",
            wt.display()
        )
    })?;
    let tool0 = match test {
        Some(t) => m.tool.replace("{test}", t),
        None => m.tool.clone(),
    };
    // P7-2.2: `{diff}` → путь к scoped-diff'у для `--in-diff` («только мутанты
    // на строках, затронутых этим diff'ом»). Файл пишем здесь, а не в шаблоне:
    // devh знает MUTATE_SCOPE и видит «изменённое» ровно так же, как `-f {files}`.
    // Без {diff} в шаблоне git не трогаем вообще (ноль побочных эффектов).
    let tool0 = if tool0.contains("{diff}") {
        let path = wt.join(".harness").join("mutate.diff");
        let nonempty = crate::shell::git::write_mutate_diff(wt, &m.scope, &path).map_err(|e| {
            anyhow::anyhow!("{e} — шаблон {key} требует --in-diff, а diff не построить")
        })?;
        println!(
            ".. in-diff: {} ({})",
            path.display(),
            if nonempty {
                "diff непустой"
            } else {
                "diff пустой — cargo-mutants скажет «Diff file is empty»"
            }
        );
        tool0.replace("{diff}", &path.to_string_lossy())
    } else {
        tool0
    };
    let batch = tool0.contains("{files}");
    let per_file = tool0.contains("{file}") || tool0.contains("{module}");

    // P7-3: `--iterate` в шаблоне допустим ТОЛЬКО с нашим тест-ключеванным
    // гейтом: встроенный кэш cargo-mutants (outcomes.json) знает лишь
    // сценарий мутанта, не хэши тестов — ослабленный тест дал бы «пойман»
    // на бумаге. Совпал отпечаток дерева (.rs + Cargo.toml + тесты — в Rust
    // unit-тесты в тех же файлах) — кэш честен; изменилось — прошлый
    // skip-set уезжает в mutants.out.stale-*, A/B «слабили тест → выживший
    // обязан стать caught» выполняется механикой.
    if tool0.contains("--iterate") {
        if crate::shell::mutcache::gate_iterate_cache(wt) {
            println!(".. kill-cache: дерево не менялось — --iterate пропускает пойманных ранее");
        } else {
            println!(
                ".. kill-cache: дерево изменилось — прошлый skip-set сброшен (mutants.out → stale):\n                 каждый мутант проверен заново"
            );
        }
    }

    // Удалённые файлы НЕ отбрасываем: за них отвечает ветка «удалён агентом».
    let files = git::changed_files_scoped_all(wt, &m.scope);

    // P7-2.4: покрытийный отбор тестов (MUTATE_COV=1).
    // Строим/загружаем индекс line -> тесты, генерируем nextest -E фильтр
    // для всех строк, которые будут мутироваться (из --list).
    let cov_filter = if m.cov {
        println!(".. coverage: building/loading index (llvm-cov)...");
        match prepare_coverage_filter(wt, m, &tool0, &files) {
            Ok(Some(filter)) => {
                println!(
                    ".. coverage: filter generated ({} tests)",
                    filter.matches("test(").count()
                );
                Some(filter)
            }
            Ok(None) => {
                eprintln!(
                    ".. coverage: no filter generated (no mutated lines or no coverage data)"
                );
                None
            }
            Err(e) => {
                eprintln!(
                    ".. coverage: failed to prepare filter: {e}; proceeding without coverage"
                );
                None
            }
        }
    } else {
        None
    };

    if !batch && !per_file {
        let cmd = apply_cov_filter(&tool0, cov_filter.as_deref());
        let mut rep = finish(m, run_mut(wt, &cmd, m), label, key, gate.clone());
        if let Some(line) = survivors_hook(m, wt, &tool0, obs, label) {
            rep.log += &format!("{line}\n");
        }
        return Ok(rep);
    }

    if batch {
        // cargo-mutants мутирует то, что лежит на диске: удалённое ему не даём.
        let live: Vec<&String> = files.iter().filter(|f| wt.join(f).is_file()).collect();
        if live.is_empty() {
            return Ok(MutReport {
                log: mutate_skip_log(m, &files),
                verdict: Verdict::Pass,
                stats: None,
                fatal: false,
                early: false,
            });
        }
        let flags = live
            .iter()
            .map(|f| format!("-f \"{f}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd = tool0.replace("{files}", &flags);
        let cmd = apply_cov_filter(&cmd, cov_filter.as_deref());
        let head = format!(
            "mutation: один прогон на {} файл(ов): {}\n",
            live.len(),
            live.iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut rep = finish(m, run_mut(wt, &cmd, m), label, key, gate.clone());
        if let Some(line) = survivors_hook(m, wt, &tool0, obs, label) {
            rep.log += &format!("{line}\n");
        }
        let log = format!("{head}{}", rep.log);
        // Хвост для fix-раунда — от полного лога (head + мутатор), как раньше.
        let verdict = match &rep.verdict {
            Verdict::Fail { .. } => Verdict::Fail {
                gate: gate.clone(),
                tail: tail_of(&log, 40),
            },
            other => other.clone(),
        };
        return Ok(MutReport {
            log,
            verdict,
            stats: rep.stats,
            fatal: rep.fatal,
            early: rep.early,
        });
    }

    let mut acc = MutReport {
        log: String::new(),
        verdict: Verdict::Pass,
        stats: Some(MutStats {
            total: 0,
            planned: 0,
            killed: 0,
            missed: 0,
            timeout: 0,
            unviable: 0,
            succeeded: 0,
            unknown: 0,
        }),
        fatal: false,
        early: false,
    };
    for f in &files {
        if !wt.join(f).is_file() {
            acc.log += &format!("{label}: {f} удалён агентом — мутации не требуется\n");
            continue;
        }
        let module = mod_of(f, &m.scope);
        let cmd = tool0.replace("{module}", &module).replace("{file}", f);
        let cmd = apply_cov_filter(&cmd, cov_filter.as_deref());
        let mut rep = finish(
            m,
            run_mut(wt, &cmd, m),
            &format!("{label} {f}"),
            key,
            gate.clone(),
        );
        if let Some(line) = survivors_hook(m, wt, &tool0, obs, &format!("{label} {f}")) {
            rep.log += &format!("{line}\n");
        }
        acc.log += &rep.log;
        match &rep.verdict {
            // Инструмент не распознан — смысла продолжать цикл нет (как и раньше).
            Verdict::EnvBlocked { .. } => {
                return Ok(MutReport {
                    log: acc.log,
                    verdict: rep.verdict.clone(),
                    stats: None,
                    fatal: false,
                    early: false,
                });
            }
            Verdict::Fail { .. } => {
                acc.verdict = rep.verdict.clone();
                acc.fatal = acc.fatal || rep.fatal;
                acc.early = acc.early || rep.early;
            }
            _ => {}
        }
        // perf.log хочет ИТОГ по гейту, поэтому per-file статы суммируем.
        if let Some(s) = rep.stats {
            if let Some(a) = acc.stats.as_mut() {
                a.total = a.total.saturating_add(s.total);
                a.killed = a.killed.saturating_add(s.killed);
                a.missed = a.missed.saturating_add(s.missed);
                a.timeout = a.timeout.saturating_add(s.timeout);
                a.unviable = a.unviable.saturating_add(s.unviable);
            }
        }
    }
    // Хвост итога per-file — от слитого лога (его тащил gate_ladder ранее).
    if let Verdict::Fail { .. } = &acc.verdict {
        acc.verdict = Verdict::Fail {
            gate: gate.clone(),
            tail: tail_of(&acc.log, 40),
        };
    }
    Ok(acc)
}

/// Мутировать нечего: либо в scope нет изменений, либо агент всё удалил.
fn mutate_skip_log(m: &MutateCfg, files: &[String]) -> String {
    if files.is_empty() {
        return format!(
            "mutation: нет изменённых файлов в {} — гейт пропущен\n",
            m.scope
        );
    }
    files
        .iter()
        .map(|f| format!("mutation: {f} удалён агентом — мутации не требуется\n"))
        .collect()
}

/// Один прогон → лог + вердикт + статы. `label` = "mutation" /
/// "red-mutation" (или "{label} <файл>" для per-file), `key` — ключ конфига
fn finish(m: &MutateCfg, run: MutRun, label: &str, key: &str, gate: Gate) -> MutReport {
    if let MutRun::Empty { why } = &run {
        return MutReport {
            log: format!("{label}: 0 мутантов — {}, пропуск\n", why.0),
            verdict: Verdict::Pass,
            stats: Some(MutStats {
                total: 0,
                planned: 0,
                killed: 0,
                missed: 0,
                timeout: 0,
                unviable: 0,
                succeeded: 0,
                unknown: 0,
            }),
            fatal: false,
            early: false,
        };
    }
    // P7-2.3: ранний выход — вердикт, а не красивый отчёт.
    if let MutRun::Stopped {
        kind,
        reason,
        stats,
        actions,
        poisoned,
        repair,
    } = &run
    {
        let mut log = format!("{label} STOP [{}]: {reason}\n", kind.tag());
        if !repair.is_empty() {
            log += repair;
        }
        let planned = if stats.planned > 0 {
            stats.planned
        } else {
            stats.total
        };
        log += &format!(
            "{label}: остановлено досрочно ({tag}), проверено {total} из {planned}: killed {killed}, missed {missed}, timeout {timeout}, unviable {unviable}, succeeded {succeeded}, unknown {unknown}\n",
            tag = kind.tag(),
            total = stats.total,
            planned = planned,
            killed = stats.killed,
            missed = stats.missed,
            timeout = stats.timeout,
            unviable = stats.unviable,
            succeeded = stats.succeeded,
            unknown = stats.unknown
        );
        if !actions.is_empty() {
            log += &format!("{label}: проблемы этого прогона (чинить их):\n");
            for a in actions {
                log += &format!("  {a}\n");
            }
        }
        let fatal = matches!(
            kind,
            MutStop::Early(crate::core::early::EarlyStop::Doom { .. })
        ) || *poisoned;
        let verdict = if fatal {
            if *poisoned {
                log += &format!(
                    "{label} FAIL: мутант остался в дереве после ремонта — коммитить нельзя\n"
                );
            } else {
                log += &format!(
                    "{label} FAIL: kill-rate < {}% недостижим — хвост прогона не нужен\n",
                    m.min_kill
                );
            }
            Verdict::Fail {
                gate: gate.clone(),
                tail: tail_of(&log, 40),
            }
        } else {
            // Канон тот же: выживший/таймаут/некомпилящийся блокируют «чисто»
            // даже при ранней остановке (SAMPLE) — остановка не «прощает» дыру.
            match gates::judge_mutation_stats(
                &gates::MutationStats {
                    mutants: stats.total,
                    killed: stats.killed,
                    missed: stats.missed,
                    timeout: stats.timeout,
                    unviable: stats.unviable,
                    succeeded: stats.succeeded,
                    unknown: stats.unknown,
                },
                m.min_kill,
            ) {
                Verdict::Fail { gate: g, tail } => {
                    log += &format!("{label} FAIL: {tail}\n");
                    Verdict::Fail {
                        gate: g,
                        tail: tail_of(&log, 40),
                    }
                }
                other => other,
            }
        };
        return MutReport {
            log,
            verdict,
            stats: Some(*stats),
            fatal,
            early: true,
        };
    }
    let MutRun::Stats {
        total,
        killed,
        missed,
        timeout,
        unviable,
        succeeded,
        actions,
    } = run
    else {
        // Неизвестный исход мутанта — дыра, а не поломка инструмента:
        // вердикт Fail (чисто быть не может), хвост actionable.
        if let MutRun::Unknown(why) = run {
            let log = format!(
                "{label} FAIL: {why}\n{label}: исход мутанта не распознан — поимка не подтверждена, чистый вердикт невозможен (ожидается 'N mutants tested in T: C caught, M missed, U unviable, X timeouts')\n"
            );
            let tail = tail_of(&log, 40);
            return MutReport {
                log,
                verdict: Verdict::Fail {
                    gate: gate.clone(),
                    tail,
                },
                stats: None,
                fatal: false,
                early: false,
            };
        }
        if let MutRun::Blocked(why) = run {
            return MutReport {
                log: format!("{label} env-blocked: {why}\n"),
                verdict: Verdict::EnvBlocked {
                    missing: format!("{label} env-blocked: {why}"),
                },
                stats: None,
                fatal: false,
                early: false,
            };
        }
        return MutReport {
            log: format!(
                "{label} env: инструмент не распознан (ни 'N mutants tested in T: C caught, M missed', ни 'Mutants: N, Killed: M'); {key}='{}'\n",
                m.tool
            ),
            verdict: Verdict::EnvBlocked {
                missing: format!(
                    "{label} env: инструмент не распознан (ни 'N mutants tested in T: C caught, M missed', ни 'Mutants: N, Killed: M'); {key}='{}'",
                    m.tool
                ),
            },
            stats: None,
            fatal: false,
            early: false,        };
    };
    let stats = MutStats {
        total,
        planned: 0,
        killed,
        missed,
        timeout,
        unviable,
        succeeded,
        // Разобранная строка итога: неопознанный исход сюда не доходит
        // (он даёт `MutRun::Unknown` → Fail). Ноль = «неизвестных нет».
        unknown: 0,
    };
    // 0 мутантов — честный пропуск (нечего мутировать), не дыра.
    if total == 0 {
        return MutReport {
            log: format!("{label}: 0 мутантов — нечего убивать, пропуск\n"),
            verdict: Verdict::Pass,
            stats: Some(stats),
            fatal: false,
            early: false,
        };
    }
    let testable = total.saturating_sub(unviable);
    let mut log = match killed.checked_mul(100).and_then(|n| n.checked_div(testable)) {
        Some(pct) => format!(
            "{label}: Mutants {total}, killed {killed} ({pct}%) [testable {testable}], missed {missed}, timeout {timeout}, unviable {unviable}, succeeded {succeeded}\n"
        ),
        // testable == 0: все мутанты некомпилящиеся (см. вердикт ниже).
        None => format!(
            "{label}: Mutants {total}, killed {killed} [testable 0], missed {missed}, timeout {timeout}, unviable {unviable}, succeeded {succeeded}\n"
        ),
    };
    // Канон вердикта — ОДИН (core/gates.rs::judge_mutation_stats): тот же,
    // что у `devh mutate verdict`. Здесь он превращается в хвост fix-промпта.
    // ПЕРЕД вердиктом — строки проблем (файл:строка, замена, жанр): без них
    // fix-промпт несёт «хвост лога», и агент ищет дыру сам.
    if !actions.is_empty() {
        log += &format!("{label}: проблемы этого прогона (чинить их):\n");
        for a in actions {
            log += &format!("  {a}\n");
        }
    }
    let verdict = match gates::judge_mutation_stats(
        &gates::MutationStats {
            mutants: total,
            killed,
            missed,
            timeout,
            unviable,
            succeeded,
            unknown: 0,
        },
        m.min_kill,
    ) {
        Verdict::Fail { gate, tail } => {
            log += &format!("{label} FAIL: {tail}\n");
            Verdict::Fail {
                gate,
                tail: tail_of(&log, 40),
            }
        }
        other => other,
    };
    MutReport {
        log,
        verdict,
        stats: Some(stats),
        fatal: false,
        early: false,
    }
}

/// P4: строка perf.log на каждый мутационный прогон — регрессии ловим числами.
/// build_s/test_s поезда не разделяет: cargo-mutants печатает только общий
/// итог «N mutants tested in T», фазы не отдаёт — выдумывать их не будем.
fn perf_mutate(store: &Store, id: &ItemId, rep: &MutReport, timing: &Timing) {
    let Some(s) = rep.stats else {
        return;
    };
    let testable = s.total.saturating_sub(s.unviable);
    let kill_pct = s
        .killed
        .checked_mul(100)
        .and_then(|n| n.checked_div(testable))
        .map(|p| p.to_string())
        .unwrap_or_else(|| "-".into());
    let secs = timing.stages().last().map(|(_, s)| *s).unwrap_or(0);
    let header = "task,mutants,killed,missed,timeout,unviable,succeeded,kill_pct,mutate_s\n";
    if !store.dir.join("logs").join("perf.log").exists() {
        store.append_raw("logs/perf.log", header).ok();
    }
    store
        .append_raw(
            "logs/perf.log",
            &format!(
                "{},{},{},{},{},{},{},{},{}\n",
                id.dir_name(),
                s.total,
                s.killed,
                s.missed,
                s.timeout,
                s.unviable,
                s.succeeded,
                kill_pct,
                secs
            ),
        )
        .ok();
}

/// Пустой скоуп мутатора: cargo-mutants вышел 0 и написал почему
/// («Diff file is empty» и т.п.) — это не сломанный инструмент (Env),
/// а «нечего убивать».
#[derive(Debug)]
struct EmptyWhy(String);

#[derive(Debug)]
enum MutRun {
    Empty {
        why: EmptyWhy,
    },
    Stats {
        total: u32,
        killed: u32,
        missed: u32,
        timeout: u32,
        unviable: u32,
        /// Мутанты без тестовой фазы (итог «N succeeded»): поимка не
        /// подтверждена, но это и не дыра покрытия (тесты не запускались).
        succeeded: u32,
        /// Строки-руководства по плохим мутантам ТЕКУЩЕГО прогона
        /// (файл:строка, замена, жанр) — уходят в fix-промпт (problem-first).
        actions: Vec<String>,
    },
    /// P5.5: контенция — нагрузка выше порога, гейт перенесён (не молчаливый Env).
    Blocked(String),
    /// Итог прогона распознан, но исход мутанта НЕИЗВЕСТЕН — видимая дыра
    /// (вердикт Fail), а не «инструмент сломан» (Env).
    Unknown(String),
    /// P7-2.3: ранний выход по DOOM/SAMPLE — watcher остановил прогон
    /// (SIGINT группе). stats — live-счётчики на момент останова.
    Stopped {
        kind: MutStop,
        reason: String,
        stats: MutStats,
        /// Строки-руководства по плохим мутантам (то же, что в Stats).
        actions: Vec<String>,
        /// true: в дереве остались маркеры мутанта даже после ремонта —
        /// коммитить нельзя («отравленный коммит»), гейт честно FAIL.
        poisoned: bool,
        repair: String,
    },
    Env,
}

/// Причина останова синхронного мутационного прогона.
///
/// Два вида останова, и оба — «вердикт, а не отчёт»:
/// * `Early` — статистическая граница (DOOM/SAMPLE, MUTATE_EARLY=1):
///   kill-rate недостижим или выборка достаточна;
/// * `First` — problem-first: появилась ПЕРВАЯ проблема (выжил/таймаут/
///   не собрался/без тестовой фазы/неизвестный исход). Прогон ради
///   «дожимемся конца» тут только тратит время: проблема уже actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MutStop {
    /// Ранний выход по DOOM/SAMPLE.
    Early(crate::core::early::EarlyStop),
    /// Problem-first: первая проблема прогона.
    First(crate::core::early::FirstProblem),
}

impl MutStop {
    /// Человекочитаемая причина останова (уходит в лог гейта).
    fn describe(&self) -> String {
        match self {
            MutStop::Early(e) => e.describe(),
            MutStop::First(p) => p.describe(),
        }
    }

    /// Короткая метка для сводки гейта.
    fn tag(&self) -> &'static str {
        match self {
            MutStop::Early(_) => "early-stop",
            MutStop::First(_) => "problem-first",
        }
    }
}

/// fn-replace мутант (Default::default()) обязан быть VIABLE: cargo-mutants
/// подменяет тело run_mut/run_mut_watched этим значением — без Default
/// инструмент помечал бы их Unviable («не собирается» = дыра в отчёте, а не
/// пойманный мутант). Blocked — единственный вариант, который ломает любой
/// вызывающий гейт мутации (он ждёт Fail/Pass/Stats, а не Env-blocked), так
/// что подмена тела немедленно красная.
impl Default for MutRun {
    fn default() -> Self {
        MutRun::Blocked("mutant: function replaced by Default".into())
    }
}

fn run_mut(wt: &Path, cmd: &str, m: &MutateCfg) -> MutRun {
    // P5.5: под контенцией мутации стоят ×2–4 (load 24 в логах; §9.10).
    // Один замер перед запуском: порог max(4, 2×CPU) либо
    // HARNESS_MUTATE_MAX_LOAD ("off" — выключить). Неизвестная нагрузка
    // (нет /proc/loadavg) гейт не блокирует.
    if let Some(load) = crate::core::load::load_average() {
        let env = std::env::var("HARNESS_MUTATE_MAX_LOAD").ok();
        if let Some(why) = crate::core::load::block_reason(env.as_deref(), load) {
            return MutRun::Blocked(why);
        }
    }
    // Пробрасываем HARNESS_* переменные в мутатор (P7-2.3: ранний выход
    // читает их внутри cargo-mutants / скрипта).
    let envs: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("HARNESS_"))
        .collect();
    // ФИРМЕННЫЙ ВЫВОД (tui.rs): прогон всегда monitored — watcher рисует
    // процент-пилюлю + счётчики исходов строкой прогресса (легенда один раз
    // выше). Раньше так жил только MUTATE_EARLY=1, и оператор в обычном
    // аудите смотрел в сырой бар cargo-mutants — два разных «фирменных»
    // вывода, о чём приходило напоминание каждую сессию. Ранний выход
    // остался опциональным (MUTATE_EARLY) — печать отделена от остановки.
    run_mut_watched(wt, cmd, m, &envs)
}

/// Брендированный прогресс мутационного прогона: раз в ~1с (частота
/// `proc::run_monitored`) читает счётчики ` mutants.out` дельтой от базлайна
/// и печатает строку `tui::line`. В tty — каждую sekunda; в pipe/лог —
/// каждые STEP готовых исходов (иначе простыня в логах гейта).
struct TuiProgress {
    dir: std::path::PathBuf,
    /// Базлайн списков mutants.out ДО старта прогона: прошлый/`--iterate`
    /// оставил строки, текущий считаем дельтой (как в run_mut_watched).
    base: crate::core::early::LiveCounts,
    c: crate::shell::color::Colors,
    legend_done: bool,
    last_printed: u32,
    last_row: String,
    started: std::time::Instant,
}

impl TuiProgress {
    fn new(wt: &Path, cmd: &str) -> Self {
        let d = wt.join(crate::core::early::mutants_out_dir(cmd));
        let base = crate::core::early::read_live(&d);
        TuiProgress {
            dir: d,
            base,
            c: crate::shell::color::colors(),
            started: std::time::Instant::now(),
            legend_done: false,
            last_printed: 0,
            last_row: String::new(),
        }
    }

    /// Снимок + печать (если пора). `avg` — средняя длительность мутанта,
    /// сек (для ETA); `log` — вывод мутатора (fallback total).
    /// Снимок + РЕШЕНИЕ о печати: печать — у вызывающего (run_mut_watched),
    /// поэтому логику тика гоняют юнит-тесты без перенаправления stdout
    /// (гейт: мутанты throttle/dedup/легенды обязаны быть CAUGHT — иначе
    /// фирменная строка врёт про прогресс, а тесты молчат).
    /// `step` = HARNESS_MUTATE_STEP, `tty` = isatty(1) — читает вызывающий.
    /// None — молчок (троттлинг в pipe либо дубль строки).
    /// Some((row, legend)) — строка прогресса + флаг «сначала легенду».
    fn tick(&mut self, log: &str, step: u32, tty: bool) -> Option<(String, bool)> {
        let mut live = crate::core::early::read_live(&self.dir);
        live.sub_base(&self.base);
        let total = live
            .total
            .or_else(|| crate::core::early::parse_discovered(log));
        let counts = crate::core::tui::Counts {
            caught: live.caught,
            missed: live.missed,
            timeout: live.timeout,
            unviable: live.unviable,
        };
        let done = counts.done();
        // троттлинг: tty — каждый тик; pipe — каждые STEP исходов
        if !tty && done < self.last_printed.saturating_add(step) && self.last_printed > 0 {
            return None;
        }
        self.last_printed = done;
        let row = crate::core::tui::line(
            done,
            total,
            &counts,
            self.avg_secs(done),
            self.started.elapsed().as_secs_f64(),
            "",
            "",
            &self.c,
        );
        if row == self.last_row {
            return None; // ничего не изменилось — не зеркалим одну строку
        }
        let need_legend = !self.legend_done;
        self.legend_done = true;
        self.last_row = row.clone();
        Some((row, need_legend))
    }

    /// Средняя длительность мутанта, сек: прошедшее / готовые исходы.
    /// Честная оценка (включая сборку) — ETA строки не врёт.
    fn avg_secs(&self, done: u32) -> f64 {
        if done == 0 {
            0.0
        } else {
            self.started.elapsed().as_secs_f64() / f64::from(done)
        }
    }
}

/// Плохие мутанты ТЕКУЩЕГО прогона — строки руководства к действию.
///
/// Именно эти строки уходят в fix-промпт: файл, строка, что подменил
/// мутатор и жанр. Без них агент получает «хвост лога» и догадывается сам.
fn problem_actions(dir: &Path, base: &crate::core::mutaudit::BadBaseline) -> Vec<String> {
    crate::core::mutaudit::current_run_bad(dir, base)
        .iter()
        .map(crate::core::mutaudit::bad_line)
        .collect()
}

/// Вывод мутатора → MutRun (классификация общая для run_logged/run_monitored).
fn classify(log: &str) -> MutRun {
    match gates::parse_mutation_output(log) {
        Some(s) => MutRun::Stats {
            total: s.mutants,
            killed: s.killed,
            missed: s.missed,
            timeout: s.timeout,
            unviable: s.unviable,
            succeeded: s.succeeded,
            actions: Vec::new(),
        },
        // P7-2.2: пустой скоуп (--in-diff без мутантов на изменённых
        // строках, либо всё вычищен --iterate) — честный пропуск,
        // а не «инструмент не распознан».
        None if let Some(why) = gates::parse_no_mutants_reason(log) => {
            MutRun::Empty { why: EmptyWhy(why) }
        }
        // Ошибка фильтра --in-diff (rc 5/6): diff устарел/не читается.
        None if let Some(why) = gates::parse_diff_filter_error(log) => MutRun::Blocked(why),
        // Итоговая строка ЕСТЬ и по форме правильная, но один из сегментов
        // исхода не распознан («2 weird, 3 caught»): поимка не подтверждена →
        // чистый вердикт невозможен. Это ВИДИМАЯ ДЫРА (Fail), а не
        // «инструмент сломан» (EnvBlocked): инструмент отработал, но ответ
        // про исход мутанта мы не поняли.
        None if let Some(why) = gates::parse_unknown_outcome_reason(log) => MutRun::Unknown(why),
        None => MutRun::Env,
    }
}

/// Решение об останове по ЖИВОМУ состоянию прогона.
///
/// Приоритет:
/// 1. **problem-first** — реальная проблема (выжил / таймаут / не собрался /
///    без тестовой фазы / неизвестный исход). Она actionable: ждать конца
///    прогона только ради неё — трата времени. Detached-аудит при этом
///    идёт до конца (он не останавливается — см. `devh mutate`);
/// 2. **статистическая граница** (DOOM/SAMPLE) — только при MUTATE_EARLY=1.
fn stop_decision(
    live: &crate::core::early::LiveCounts,
    dir: &Path,
    first_problem: bool,
    m_early: bool,
    min_kill: u32,
    shuffled: bool,
    log: &str,
) -> Option<MutStop> {
    if first_problem {
        if let Some(p) = crate::core::mutaudit::first_problem(dir, live) {
            return Some(MutStop::First(p));
        }
    }
    if m_early {
        let total = live
            .total
            .or_else(|| crate::core::early::parse_discovered(log));
        return crate::core::early::evaluate(live, total, min_kill, shuffled).map(MutStop::Early);
    }
    None
}

/// P7-2.3: monitored-прогон. Watcher раз в секунду читает append-only
/// списки mutants.out (ДЕЛЬТОЙ от базлайна: прошлый прогон/--iterate
/// оставил строки) и mutants.json (total), останавливает по DOOM/SAMPLE
/// SIGINT'ом группы: cargo-mutants сам откатывает мутанта и выходит чисто.
fn run_mut_watched(wt: &Path, cmd: &str, m: &MutateCfg, envs: &[(String, String)]) -> MutRun {
    use crate::core::early;
    let dir = wt.join(early::mutants_out_dir(cmd));
    let base = early::read_live(&dir);
    // Базовая линия плохих мутантов: списки append-only — без неё прошлый
    // прогон попал бы в отчёт текущего.
    let bad_base = crate::core::mutaudit::BadBaseline::take(&dir);
    let shuffled = cmd.split_whitespace().any(|w| w == "--shuffle");
    let min_kill = m.min_kill;
    let m_early = m.early;
    // Problem-first: останов на ПЕРВОЙ проблеме (по умолчанию включён).
    let m_first = m.first_problem;
    let dir_clone = dir.clone();
    let base_clone = base;
    // Фирменная строка прогресса живёт здесь: тик watcher'а = печать + (при
    // MUTATE_EARLY) решение об остановке. Печать отделена от остановки.
    // tty/step — свойства вывода процесса: читаем один раз (не в тике),
    // иначе юнит-тесты тика зависели бы от окружения прогона.
    let step: u32 = std::env::var("HARNESS_MUTATE_STEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let tty = unsafe { libc::isatty(1) == 1 };
    let tui = std::cell::RefCell::new(TuiProgress::new(wt, cmd));
    match proc::run_monitored(cmd, wt, MUTATE_TIMEOUT, envs, move |log_text| {
        let mut live = early::read_live(&dir_clone);
        live.sub_base(&base_clone);
        // borrow_mut() не живёт в условии if let (clippy::borrow_interior_mutable_const + паника повторного borrow)
        let ticked = tui.borrow_mut().tick(log_text, step, tty);
        if let Some((row, need_legend)) = ticked {
            let t = tui.borrow();
            if need_legend {
                println!("{}", crate::core::tui::legend(&t.c));
            }
            drop(t);
            println!("{row}");
        }
        // Problem-first ВЫШЕ статистики: реальная проблема actionable —
        // ждать конца прогона ради неё незачем. Detached-аудит при этом
        // идёт до конца (он не останавливается — см. `devh mutate`).
        stop_decision(
            &live, &dir_clone, m_first, m_early, min_kill, shuffled, log_text,
        )
    }) {
        Ok(mn) => {
            let log = proc::combined(&mn.outcome);
            let Some(stop) = mn.stopped else {
                // не остановили — полный лог, обычная классификация
                let mut run = classify(&log);
                if let MutRun::Stats { actions, .. } = &mut run {
                    *actions = problem_actions(&dir, &bad_base);
                }
                return run;
            };
            let mut live = early::read_live(&dir);
            live.sub_base(&base);
            // Сколько мутантов ПЛАНИРОВАЛОСЬ (mutants.json) и сколько
            // РЕАЛЬНО получило исход. Знаменатель kill-rate — ПРОВЕРЕННЫЕ:
            // остановленный прогон не имеет права судить по непроверенным
            // (иначе SAMPLE-останов «выборка достаточна» всегда давал бы
            // kill-rate по всему плану и никогда не проходил).
            let planned = live
                .total
                .or_else(|| early::parse_discovered(&log))
                .unwrap_or(0);
            // «succeeded» (мутант без тестовой фазы) и неопознанные исходы
            // в append-списки НЕ пишутся — они видны только в outcomes.json.
            let det = crate::core::mutaudit::load(&dir, true, None);
            let done = live
                .caught
                .saturating_add(live.missed)
                .saturating_add(live.timeout)
                .saturating_add(live.unviable)
                .saturating_add(det.succeeded)
                .saturating_add(det.unknown);
            let (repair, poisoned) = repair_mutant_markers(wt, &dir);
            let reason = stop.describe();
            MutRun::Stopped {
                kind: stop,
                reason,
                stats: MutStats {
                    total: done,
                    planned,
                    killed: live.killed(),
                    missed: live.missed,
                    timeout: live.timeout,
                    unviable: live.unviable,
                    succeeded: det.succeeded,
                    // Неопознанные исходы видны ТОЛЬКО в outcomes.json — в
                    // списках их нет. Без этого числа остановленный прогон
                    // с неизвестным исходом прошёл бы общий kill-rate judge
                    // как «чисто» (поимка не подтверждена, а вердикт зелёный).
                    unknown: det.unknown,
                },
                actions: problem_actions(&dir, &bad_base),
                poisoned,
                repair,
            }
        }
        Err(_) => MutRun::Env,
    }
}

/// P7-2.3: после останова — маркеры cargo-mutants в дереве? Чистый SIGINT
/// откатывает мутанта сам (interrupt.rs → revert); жёсткий SIGKILL — нет.
/// Дерево в фазе гейта ГРЯЗНОЕ (коммит после лестницы) → `git checkout`
/// запрещён, ремонт — только reverse-apply свежих диффов mutants.out/diff.
fn repair_mutant_markers(wt: &Path, out_dir: &Path) -> (String, bool) {
    let marked = git::mutant_marked_files(wt);
    if marked.is_empty() {
        return (String::new(), false);
    }
    let (done, left) = git::revert_mutant_diffs(wt, out_dir);
    let mut note = format!(
        "STOP-repair: маркеры мутанта в дереве: {} — остановка раньше revert'а\n",
        marked.join(" ")
    );
    if !done.is_empty() {
        note += &format!(
            "STOP-repair: git apply -R свежих диффов: {}\n",
            done.join(" ")
        );
    }
    let poisoned = !left.is_empty();
    if poisoned {
        note += &format!(
            "STOP-repair FAIL: маркеры остались в {} — диффов не хватило; вручную: grep -rn '{}', коммитить это дерево НЕЛЬЗЯ\n",
            left.join(" "),
            crate::core::early::MARKER
        );
    }
    (note, poisoned)
}

fn mod_of(path: &str, scope: &str) -> String {
    let mut p = path.to_string();
    if !scope.is_empty() && scope != "." {
        let prefix = format!("{scope}/");
        if let Some(rest) = p.strip_prefix(&prefix) {
            p = rest.to_string();
        }
    }
    if let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    p = p
        .strip_suffix(".ex")
        .or_else(|| p.strip_suffix(".rs"))
        .unwrap_or(&p)
        .to_string();
    let mut out = String::new();
    for seg in p.split('/') {
        if seg.is_empty() {
            continue;
        }
        let mut cs = seg.chars();
        if let Some(first) = cs.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(cs.as_str());
        }
        out.push('.');
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

pub fn which(bin: &str) -> bool {
    if bin.is_empty() {
        return false;
    }
    if bin.contains('/') {
        return PathBuf::from(bin).is_file();
    }
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        freeze_verdict, post_review_guard, review_size_verdict, stop_decision, MutStop, TuiProgress,
    };
    use crate::core::gates::{Gate, Verdict};
    use crate::core::types::MutateCfg;
    use crate::shell::git;
    use std::path::Path;

    /// Парс-контракт `devh ps`: имена стадий тайминга печатаются дословно.
    /// Разрез run() по фазам (A2) переносит вызовы между функциями, но не
    /// имена — ассерт ловит любое переименование или потерю стадии.
    /// Неизвестный исход мутанта — ВИДИМАЯ ДЫРА (Fail), а не «инструмент
    /// сломан» (EnvBlocked).
    ///
    /// Инструмент отработал, строка итога есть, но один из сегментов исхода
    /// не распознан → «пойман или нет» неизвестно → чистый вердикт невозможен.
    /// Раньше такой прогон уезжал в Env («инструмент не распознан» = блокер
    /// среды, rc=2), то есть настоящая дыра маскировалась под поломку.
    #[test]
    fn unknown_mutant_outcome_is_fail_not_env() {
        let log = "5 mutants tested in 3s: 2 weird, 3 caught\n";
        let run = super::classify(log);
        assert!(
            matches!(run, super::MutRun::Unknown(_)),
            "ожидался Unknown, получено {run:?}"
        );
        let m = MutateCfg {
            tool: "cargo-mutants".into(),
            scope: "lib".into(),
            min_kill: 90,
            early: false,
            first_problem: false,
            cov: false,
            survivors: false,
        };
        let rep = super::finish(&m, run, "mutation", "MUTATE", Gate::Mutate);
        match &rep.verdict {
            Verdict::Fail { gate, tail } => {
                assert_eq!(*gate, Gate::Mutate);
                assert!(tail.contains("weird"), "{tail}");
            }
            other => panic!("неизвестный исход обязан быть Fail: {other:?}"),
        }
        assert!(
            !matches!(rep.verdict, Verdict::EnvBlocked { .. }),
            "Env маскировал бы дыру под поломку инструмента"
        );
    }

    /// Известный, но НЕ поимка исход «succeeded» (мутант без тестовой фазы):
    /// не Env, не «инструмент не распознан», а Fail с внятной причиной.
    #[test]
    fn succeeded_mutant_outcome_is_known_and_blocks_clean_verdict() {
        let log = "5 mutants tested in 3s: 2 succeeded, 3 caught\n";
        let run = super::classify(log);
        let super::MutRun::Stats {
            total,
            killed,
            missed,
            timeout,
            unviable,
            succeeded,
            ..
        } = run
        else {
            panic!("ожидались статы, получено {run:?}")
        };
        assert_eq!(
            (total, killed, missed, timeout, unviable, succeeded),
            (5, 3, 0, 0, 0, 2)
        );
        let m = MutateCfg {
            tool: "cargo-mutants".into(),
            scope: "lib".into(),
            min_kill: 90,
            early: false,
            first_problem: false,
            cov: false,
            survivors: false,
        };
        let rep = super::finish(&m, run, "mutation", "MUTATE", Gate::Mutate);
        match &rep.verdict {
            Verdict::Fail { tail, .. } => assert!(tail.contains("succeeded 2 > 0"), "{tail}"),
            other => panic!("непроверенный мутант обязан быть Fail: {other:?}"),
        }
    }

    #[test]
    fn stage_names_preserved_for_ps() {
        let src = include_str!("run.rs");
        for stage in [
            "worktree(+bootstrap)",
            "red-pre",
            "red-post",
            "redmut",
            "commit+push",
            "verify-r{}",
            "lint-r{}",
            "crap-r{}",
            "review-r{}",
            "mutate-r{}",
            "bench-r{}",
        ] {
            assert!(
                src.contains(stage),
                "стадия {stage} исчезла из run.rs — devh ps сломается"
            );
        }
    }

    /// Разрез A2: каждая фаза существует ровно один раз (не расползлась по
    /// копипасте и не исчезла при следующем рефакторе). Считаем только
    /// реализацию (до `#[cfg(test)]`) — иначе списки самих тестов удваивают счёт.
    #[test]
    fn run_is_split_into_phases() {
        let src = include_str!("run.rs");
        let (code, _) = src.split_once("#[cfg(test)]").unwrap_or((src, ""));
        let src = code;
        for phase in [
            "fn require_gates(",
            "fn lock_run<",
            "fn resolve_agent(",
            "fn claim_item(",
            "fn ensure_worktree(",
            "fn red_pre_phase(",
            "fn no_crap_notice(",
            "fn red_post(",
            "fn red_mutate_phase(",
            "fn commit_push(",
            "fn finalize(",
            "fn give_up(",
        ] {
            assert_eq!(
                src.matches(phase).count(),
                1,
                "фаза {phase} исчезла/раздвоилась в run.rs"
            );
        }
    }

    // ---- F-0.2: fail-closed git-слой FREEZE/lint (N1) ----
    // RED-цепочка плана F-0: `cargo test -p devh --lib failclosed_`.
    // Чистая часть гейта и требование «нечитаемый git ≠ чисто» — без Store и
    // worktree, поэтому решения проверяются точно, а не через полную сессию.

    #[test]
    fn failclosed_freeze_empty_scope_is_a_logged_pass_not_a_silent_one() {
        let (log, v) = freeze_verdict(&["src/calc.sh".into()], &[], false);
        assert_eq!(v, Verdict::Pass);
        assert!(log.contains("scope не задан"), "{log}");
    }

    #[test]
    fn failclosed_freeze_out_of_scope_is_fail_with_reason_in_tail() {
        let (log, v) = freeze_verdict(&["tests/all.sh".into()], &["src/calc.sh".into()], false);
        match v {
            Verdict::Fail { gate, tail } => {
                assert_eq!(gate, Gate::Freeze);
                assert!(tail.contains("правка вне scope: tests/all.sh"), "{tail}");
            }
            other => unreachable!("ожидался Fail, получено {other:?}"),
        }
        assert!(
            log.contains("обход: allow-test-edit"),
            "лечебный хинт в логе гейта: {log}"
        );
    }

    #[test]
    fn failclosed_freeze_recorded_bypass_legitimizes_tests() {
        let (_, v) = freeze_verdict(&["tests/all.sh".into()], &["src/calc.sh".into()], true);
        assert_eq!(
            v,
            Verdict::Pass,
            "записанный обход законен (симметрия no-crap)"
        );
    }

    /// ГЛАВНЫЙ тест F-0.2: `gate_ladder` обязан иметь легальную env-ветку.
    /// Прежний `unreachable!("freeze_gate не отдаёт Env/Spec")` ронял процесс
    /// паникой на первом же сбое git; теперь источник fail-closed, и Err
    /// превращается в `EnvBlocked { .. }`. Проверяем исходник: если
    /// `unreachable!` вернётся (или вердикт начнут игнорировать), тест краснеет.
    #[test]
    fn failclosed_gate_ladder_has_no_unreachable_on_git_verdict() {
        let src = include_str!("run.rs");
        let (code, _) = src.split_once("#[cfg(test)]").unwrap_or((src, ""));
        assert!(
            !code.contains("freeze_gate не отдаёт"),
            "в gate_ladder снова запрещена env-ветка — сбой git уронит харнесс паникой"
        );
        // fail-closed: любой не-Pass вердикт FREEZE останавливает лестницу,
        // а не проваливается сквозь неё.
        assert!(
            code.contains("verdict: other") && code.contains("log: flog"),
            "лестница обязана тормозить на ЛЮБОМ не-Pass вердикте FREEZE"
        );
        // и сам источник — fail-closed changes_strict, не changed_files.
        assert!(
            code.contains("git::changes_strict(wt)"),
            "FREEZE берёт файлы из changes_strict, а не из fail-open changed_files"
        );
    }

    // ---- TuiProgress::tick: фирменная строка прогресса (C1) ----
    // Мутанты тика throttle/dedup/легенда обязаны быть CAUGHT: строка
    // прогресса — единственный вывод аудита, и гейт её не проверяет.

    fn tick_fixture(dir: &Path, caught: u32, missed: u32, unviable: u32, timeout: u32) {
        // read_live читает mutants.out/<списки> — пишем туда же, куда смотрит
        // TuiProgress::new (mutants_out_dir по умолчанию).
        let out = dir.join(crate::core::early::OUT_DIR);
        std::fs::create_dir_all(&out).expect("mkdir mutants.out");
        let write = |name: &str, n: u32| {
            let body: String = (0..n).map(|i| format!("mutant-{i}\n")).collect();
            std::fs::write(out.join(name), body).expect("fixture list");
        };
        write("caught.txt", caught);
        write("missed.txt", missed);
        write("unviable.txt", unviable);
        write("timeout.txt", timeout);
    }

    fn tui_in(dir: &Path) -> TuiProgress {
        TuiProgress::new(dir, "cargo mutants")
    }

    #[test]
    fn tick_first_row_prints_with_legend() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut tui = tui_in(tmp.path());
        let (row, legend) = tui.tick("", 5, false).expect("первый тик печатает всегда");
        assert!(legend, "первая строка идёт ПОСЛЕ легенды");
        assert!(row.contains('%'), "строка несёт процент-пилюлю: {row}");
    }

    #[test]
    fn tick_throttles_in_pipe_until_step() {
        let tmp = tempfile::tempdir().expect("tmp");
        let out = tmp.path();
        let mut tui = tui_in(out); // base снят ДО фикстур, иначе done=0
        tick_fixture(out, 2, 0, 0, 0);
        assert!(tui.tick("", 2, false).is_some(), "первый тик: печать");
        tick_fixture(out, 3, 0, 0, 0);
        assert!(
            tui.tick("", 2, false).is_none(),
            "3 < 2+2 — молчок (троттлинг)"
        );
        tick_fixture(out, 4, 0, 0, 0);
        assert!(tui.tick("", 2, false).is_some(), "4 >= 2+2 — печать");
    }

    #[test]
    fn tick_tty_ignores_step() {
        let tmp = tempfile::tempdir().expect("tmp");
        let out = tmp.path();
        let mut tui = tui_in(out); // base пустой
        tick_fixture(out, 1, 0, 0, 0);
        assert!(tui.tick("", 100, true).is_some(), "tty: первый тик");
        tick_fixture(out, 2, 0, 0, 0);
        assert!(
            tui.tick("", 100, true).is_some(),
            "tty: печатаем каждый шаг"
        );
    }

    #[test]
    fn tick_dedups_identical_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let out = tmp.path();
        let mut tui = tui_in(out); // base пустой
        tick_fixture(out, 1, 0, 0, 0);
        assert!(tui.tick("", 1, false).is_some(), "первый тик печатает");
        assert!(tui.tick("", 1, false).is_none(), "та же строка — молчок");
    }

    #[test]
    fn tick_legend_printed_once() {
        let tmp = tempfile::tempdir().expect("tmp");
        let out = tmp.path();
        let mut tui = tui_in(out); // base пустой
        tick_fixture(out, 1, 0, 0, 0);
        let (_, l1) = tui.tick("", 1, false).expect("первый тик");
        tick_fixture(out, 2, 0, 0, 0);
        let (_, l2) = tui.tick("", 1, false).expect("второй тик");
        assert!(l1, "первая строка просит легенду");
        assert!(!l2, "легенда — ровно один раз");
    }

    #[test]
    fn avg_secs_zero_done_is_zero_not_nan() {
        let tmp = tempfile::tempdir().expect("tmp");
        let tui = tui_in(tmp.path());
        assert_eq!(tui.avg_secs(0), 0.0, "нет деления на ноль в ETA");
        assert!(tui.avg_secs(2).is_finite() && tui.avg_secs(2) >= 0.0);
    }

    // ─── F-0.3: размер диффа и неизменность дерева при ревью ───

    #[test]
    fn review_diff_over_max_is_specblock() {
        // Граница: ровно MAX — ещё ревью (Pass), MAX+1 — SpecBlock REVIEW.
        // Без границы мутант `<=` → `>=` выживает, и спека любого размера
        // молча проходит «ревью».
        assert_eq!(review_size_verdict(2000, 2000), Verdict::Pass);
        // MAX+1 — не Fail (не работа агента) и не Env (не среда): SpecBlock,
        // его чинит человек, дробя задачу.
        match review_size_verdict(2001, 2000) {
            Verdict::SpecBlock { gate, .. } => assert_eq!(gate, Gate::Review),
            other => panic!("превышение лимита не SpecBlock: {other:?}"),
        }
    }

    #[test]
    fn review_size_zero_max_is_no_limit() {
        // Аварийный клапан откатки: 0 = без лимита (не «ничего не ревьюить»).
        assert_eq!(review_size_verdict(50_000, 0), Verdict::Pass);
        assert_eq!(review_size_verdict(0, 2000), Verdict::Pass);
    }

    #[test]
    fn post_review_guard_flags_tree_change() {
        // Ревьюер — второй запрос ТОМУ ЖЕ агенту: правка файлов из него
        // (стаб-атака review-writes.sh) обязана дать EnvBlocked rc=2.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(d.path())
                .output();
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        std::fs::write(d.path().join("a.txt"), b"1").unwrap_or_else(|_| unreachable!());
        g(&["add", "-A"]);
        g(&["commit", "-qm", "base"]);
        let before = git::tree_digest(d.path()).unwrap_or_else(|_| unreachable!());
        assert!(
            post_review_guard(&before, d.path()).is_none(),
            "честное ревью (дерево не тронуто) — не блокер"
        );
        std::fs::write(d.path().join("a.txt"), b"2").unwrap_or_else(|_| unreachable!());
        let got = post_review_guard(&before, d.path());
        match got {
            Some((why, Verdict::EnvBlocked { missing })) => {
                assert!(
                    missing.contains("ревьюер изменил дерево"),
                    "причина названа: {missing}"
                );
                assert!(why.contains("ревьюер изменил дерево"), "лог назван: {why}");
            }
            other => panic!("правка дерева из ревью не поймана: {other:?}"),
        }
    }

    /// Остановленный problem-first прогон с неопознанным исходом — НЕ «чисто».
    ///
    /// Грабли, которые закрывает тест: неопознанный исход виден ТОЛЬКО в
    /// `outcomes.json` (в append-списки cargo-mutants его не пишет), а
    /// kill-rate считается по пойманным. Без `unknown` в статах 100 из 101
    /// пойманных давали 99% ≥ порога → вердикт ЗЕЛЁНЫЙ при неподтверждённой
    /// поимке. «Пойман или нет» неизвестно → поимка не подтверждена.
    #[test]
    fn stopped_run_with_unknown_outcome_is_fail() {
        let m = MutateCfg {
            tool: "cargo-mutants".into(),
            scope: "lib".into(),
            min_kill: 90,
            early: false,
            first_problem: false,
            cov: false,
            survivors: false,
        };
        let run = super::MutRun::Stopped {
            kind: MutStop::First(crate::core::early::FirstProblem::Unknown(1)),
            reason: "PROBLEM: неизвестный исход мутанта x1".into(),
            stats: super::MutStats {
                total: 101,
                planned: 101,
                killed: 100,
                missed: 0,
                timeout: 0,
                unviable: 0,
                succeeded: 0,
                unknown: 1,
            },
            actions: Vec::new(),
            poisoned: false,
            repair: String::new(),
        };
        let rep = super::finish(&m, run, "mutation", "MUTATE", Gate::Mutate);
        match &rep.verdict {
            Verdict::Fail { gate, tail } => {
                assert_eq!(*gate, Gate::Mutate);
                assert!(tail.contains("unknown 1 > 0"), "{tail}");
            }
            other => panic!("неопознанный исход обязан быть Fail: {other:?}"),
        }
    }

    #[test]
    fn stop_decision_stops_at_first_problem() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let dir = d.path().join("mutants.out");
        std::fs::create_dir_all(&dir).unwrap_or_else(|_| unreachable!());
        let live = crate::core::early::LiveCounts {
            total: Some(10),
            caught: 4,
            missed: 0,
            unviable: 0,
            timeout: 0,
        };
        // Проблем нет — идём до конца (даже при включённом problem-first).
        assert!(stop_decision(&live, &dir, true, false, 90, false, "").is_none());
        // Появился выживший — немедленный останов с actionable-причиной.
        std::fs::write(
            dir.join("missed.txt"),
            "devharness/src/core/plan.rs:19:5: replace x with None\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let mut live2 = live;
        live2.missed = 1;
        let stop = stop_decision(&live2, &dir, true, false, 90, false, "");
        assert_eq!(
            stop,
            Some(MutStop::First(crate::core::early::FirstProblem::Missed(1)))
        );
        // Выключенный problem-first — прогон идёт до конца.
        assert!(stop_decision(&live2, &dir, false, false, 90, false, "").is_none());
    }
}
