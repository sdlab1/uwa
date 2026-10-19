use std::path::Path;
use std::time::Instant;

use crate::core::state::{self, Status};
use crate::core::types::{ItemId, RunConfig};
use crate::shell::color::{colors, Colors};
use crate::shell::git;
use crate::shell::store::{item_rel, Item, Store};
use crate::shell::worktree;

const SMOKE_TIMEOUT_SECS: u64 = 120;

pub fn doctor(store: &Store, cfg: &RunConfig, root: &Path) -> anyhow::Result<()> {
    let c = colors();
    let mut fail = 0u32;
    let mut warn = 0u32;
    let mut failed: Vec<String> = Vec::new();
    let mut say = |msg: &str| say_line(c, msg);

    let git_ver = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .nth(1)
                .unwrap_or("?")
                .to_string()
        })
        .unwrap_or_default();
    if git_ver.is_empty() {
        say("FAIL нет git");
        fail = fail.saturating_add(1);
        failed.push("нет git".into());
    } else {
        say(&format!("ok   git {git_ver}"));
    }

    if cfg.verify.is_empty() {
        say("FAIL VERIFY не задан");
        fail = fail.saturating_add(1);
        failed.push("VERIFY не задан".into());
    } else {
        say(&format!("ok   VERIFY: {}", cfg.verify));
        say(&format!("..   smoke VERIFY (≤{SMOKE_TIMEOUT_SECS}s)"));
        verify_smoke(root, &cfg.verify, &mut say, &mut warn);
    }

    let ag = cfg.agent.split_whitespace().next().unwrap_or("");
    if crate::commands::run::which(ag) {
        say(&format!("ok   агент: {ag}"));
    } else {
        say(&format!("FAIL агент '{ag}' не найден в PATH"));
        fail = fail.saturating_add(1);
        failed.push(format!("агент '{ag}' не найден в PATH"));
    }

    say("ok   flock (блокировки состояний)");

    if crate::commands::run::which("timeout") {
        say("ok   timeout (watchdog)");
    } else {
        say("WARN нет coreutils timeout");
        warn = warn.saturating_add(1);
    }

    if crate::commands::run::which("shellcheck") {
        say("ok   shellcheck (статический слой)");
    } else {
        say("WARN нет shellcheck — статика пропускается");
        warn = warn.saturating_add(1);
    }

    if git::has_remote(root) {
        if git::ls_remote_ok(root, &cfg.remote) {
            say(&format!("ok   remote {}", cfg.remote));
        } else {
            say(&format!("WARN remote {} недоступен", cfg.remote));
            warn = warn.saturating_add(1);
        }
    } else {
        say("ok   репо без remote (локальный режим)");
    }

    let excl = root.join(".git/info/exclude");
    let has_excl = std::fs::read_to_string(&excl)
        .map(|t| t.lines().any(|l| l.trim() == ".harness/"))
        .unwrap_or(false);
    if has_excl {
        say("ok   .harness/ в exclude");
    } else {
        say("WARN .harness/ не в .git/info/exclude");
        warn = warn.saturating_add(1);
    }

    if let Some(m) = &cfg.mutate {
        // Шаблон может начинаться с env-assignments
        // (CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants ...) — берём
        // первое слово после них, иначе «бинарь» = имя переменной.
        let mbin = m
            .tool
            .split_whitespace()
            .find(|w| !w.contains('='))
            .unwrap_or("");
        if crate::commands::run::which(mbin) {
            say(&format!("ok   mutation: {mbin} доступен ({})", m.scope));
        } else {
            say(&format!(
                "WARN mutation: '{mbin}' не найден — run будет заблокирован (установи инструмент)"
            ));
            warn = warn.saturating_add(1);
        }
        // P8-5: спека без точек наблюдения (OBSERVE) рискует породить слепые
        // тесты — мутанты там выживут. Мягкая проверка (не гейт): вратами
        // остаётся RED, а OBSERVE — про то, что автор до кода назвал, ЧЕМ
        // фича проверяется (план third_loop §7).
        let blind: Vec<ItemId> = store
            .items()
            .into_iter()
            .filter(|i| i.status != Status::Done)
            .filter(|i| {
                store
                    .read(item_rel(&i.id, "spec.md"))
                    .as_deref()
                    .and_then(crate::commands::new::observe_from_spec)
                    .is_none()
            })
            .map(|i| i.id)
            .collect();
        if blind.is_empty() {
            say("ok   OBSERVE: точки наблюдения у всех открытых спек");
        } else {
            let names: Vec<String> = blind.iter().map(|i| i.dir_name()).collect();
            say(&format!(
                "WARN спеки без точек наблюдения (OBSERVE): {} ({}) — тесты рискуют быть слепыми; перечисли return/side-effect/state/log до RED",
                blind.len(),
                names.join(", ")
            ));
            warn = warn.saturating_add(1);
        }
    } else {
        say("ok   mutation-гейт выключен");
    }

    // P3: сам мутатор — версия и общий конфиг параметров.
    if let Some(m) = &cfg.mutate {
        let is_cm = m.tool.contains("cargo mutants") || m.tool.contains("cargo-mutants");
        if is_cm {
            match cargo_mutants_version() {
                Some((major, ver)) if major >= 25 => {
                    say(&format!("ok   cargo-mutants {ver} (>=25, вывод парсится)"));
                }
                Some((_, ver)) => {
                    say(&format!(
                        "FAIL cargo-mutants {ver} < 25 — формат вывода другой, мутационный гейт молча станет Env"
                    ));
                    fail = fail.saturating_add(1);
                    failed.push(format!("cargo-mutants {ver} < 25 (нужен >=25)"));
                }
                None => {
                    say("WARN cargo-mutants не отвечает на --version");
                    warn = warn.saturating_add(1);
                }
            }
            let mt = root.join(".cargo").join("mutants.toml");
            if mt.is_file() {
                say("ok   .cargo/mutants.toml (общие параметры cargo-mutants)");
            } else {
                say("WARN нет .cargo/mutants.toml — параметры только в шаблоне MUTATE");
                warn = warn.saturating_add(1);
            }
            // P5.5: корневой рычаг slow-build мутантов — профиль test с
            // incremental (rust_build.md §9.7). Без env-оверрайда каждый
            // мутант платит полную сборку: молча терять нельзя.
            // P7-2.3: ранний выход — вердикт раньше хвоста. DOOM работает без
            // --shuffle; SAMPLE честна только с --shuffle в шаблоне.
            if m.early {
                if m.tool.contains("--shuffle") {
                    say("ok   MUTATE_EARLY=1 + --shuffle: DOOM и SAMPLE (rule-of-three)");
                } else {
                    say("ok   MUTATE_EARLY=1: DOOM (kill-rate недостижим); SAMPLE требует --shuffle в шаблоне");
                }
            }
            if m.tool.contains("CARGO_PROFILE_TEST_INCREMENTAL=true") {
                say("ok   MUTATE: CARGO_PROFILE_TEST_INCREMENTAL=true (быстрые мутанты)");
            } else {
                say("WARN MUTATE без CARGO_PROFILE_TEST_INCREMENTAL=true — мутанты платят полную сборку профиля test (§9.7)");
                warn = warn.saturating_add(1);
            }
        }
    }

    // P5.5: нагрузка — под контенцией мутанты ×2–4; doctor показывает факт,
    // решение принимает load-gate в mutate_gate (порог max(4, 2×CPU)).
    match crate::core::load::load_average() {
        Some(load) => match crate::core::load::max_load() {
            Some(max) if load > max => {
                say(&format!(
                    "WARN load {load:.1} > порог {max:.1} — мутанты сейчас ×2–4 времени (§9.10)"
                ));
                warn = warn.saturating_add(1);
            }
            _ => say(&format!("ok   load {load:.1}")),
        },
        None => say("ok   load неизвестен (нет /proc/loadavg)"),
    }

    // P5.5: кэш shellcheck-оракла — без него мут-прогон в копии платит
    // холодный оракул (~2с на файл; в логах 205-прогона — до 95s тест-фазы).
    if crate::commands::run::which("shellcheck") && shellcheck_cache(root) > 0 {
        say("ok   shellcheck-кэш тёплый");
    } else if crate::commands::run::which("shellcheck") {
        say("WARN shellcheck-кэш холодный — оракул будет перепроверять файлы (~2с на файл)");
        warn = warn.saturating_add(1);
    }

    // P3: тёплый target главного дерева — иначе первый прогон платит cold-build.
    if warm_target(root) {
        say("ok   target тёплый (инкремент devh)");
    } else {
        say("WARN target/devh не прогрет — первый прогон заплатит cold-build (или devh run один раз)");
        warn = warn.saturating_add(1);
    }

    // P3: регистрации worktree'ов, которых уже нет на диске.
    let registered = git::worktree_paths(root);
    let orphans = git::worktree_orphans(root);
    if orphans.is_empty() {
        say(&format!(
            "ok   git worktree: {} зарегистрированных, сирот нет",
            registered.len()
        ));
    } else {
        say(&format!(
            "WARN git worktree: {} сирот (путь пропал) — devh sweep снимет регистрации",
            orphans.len()
        ));
        warn = warn.saturating_add(1);
    }

    // P3: sccache hit-rate — низкий = инкремент/target не переиспользуется.
    match sccache_hit_rate() {
        Some(rate) if rate >= 50 => say(&format!("ok   sccache hit-rate {rate}%")),
        Some(rate) => {
            say(&format!(
                "WARN sccache hit-rate {rate}% < 50% — reuse-worktree/target холодные"
            ));
            warn = warn.saturating_add(1);
        }
        None => {
            say("WARN sccache недоступен — hit-rate неизвестен");
            warn = warn.saturating_add(1);
        }
    }

    if cfg.bench.is_some() {
        say(&format!(
            "ok   bench-гейт: {}",
            cfg.bench.clone().unwrap_or_default()
        ));
    } else {
        say("ok   bench-гейт выключен");
    }

    if let Some(cr) = &cfg.crap {
        say(&format!("..   smoke CRAP (≤{SMOKE_TIMEOUT_SECS}s)"));
        crap_smoke(root, cr, &mut say, &mut fail, &mut failed);
    } else {
        say("ok   CRAP-гейт не настроен");
    }

    for it in store.items() {
        orphan_checks(store, cfg, &it, &mut say, &mut warn);
        replay_checks(store, &it, &mut say, &mut warn);
    }

    let active = store.count_active();
    if active > 0 {
        say(&format!(
            "ok   active items: {active} (WIP-лимит {})",
            cfg.max_active
        ));
    } else {
        say("ok   очередей в работе нет");
    }

    println!("doctor: fail={fail} warn={warn}");
    if fail > 0 {
        anyhow::bail!("doctor: {} — fail={fail} warn={warn}", failed.join("; "));
    }
    Ok(())
}

fn orphan_checks(
    store: &Store,
    cfg: &RunConfig,
    it: &crate::shell::store::Item,
    say: &mut dyn FnMut(&str),
    warn: &mut u32,
) {
    let wt = worktree::path_for(cfg, store.root(), &it.id);
    if it.status == crate::core::state::Status::Active && !wt.is_dir() {
        say(&format!(
            "WARN {} active без worktree (прерванный run?) — devh run {} продолжит или devh abort {}",
            it.id,
            it.id,
            it.id
        ));
        *warn = warn.saturating_add(1);
    }
    if it.status == crate::core::state::Status::Open && wt.is_dir() {
        say(&format!(
            "WARN {}: worktree есть, статус open — devh run {} продолжит с места обрыва",
            it.id, it.id
        ));
        *warn = warn.saturating_add(1);
    }
}

/// A3: replay events.log → fold → сверка с файлом status. Рассинхрон —
/// строка в отчёт (WARN), не смерть doctor'а: status — источник истины для
/// гейтов, журнал — постмортем для человека.
fn replay_checks(store: &Store, it: &Item, say: &mut dyn FnMut(&str), warn: &mut u32) {
    let Some(text) = store.read(item_rel(&it.id, "events.log")) else {
        return;
    };
    let folded = state::fold(&state::events_from_log(&text));
    if folded != it.status {
        say(&format!(
            "WARN {}: events.log replay → {}, статус-файл {} — рассинхрон журнала (правь status вручную или переиграй событие)",
            it.id.dir_name(),
            folded.as_str(),
            it.status.as_str()
        ));
        *warn = warn.saturating_add(1);
    }
}

fn verify_smoke(root: &Path, verify: &str, say: &mut dyn FnMut(&str), warn: &mut u32) {
    let have_timeout = crate::commands::run::which("timeout");
    let mut args: Vec<String> = Vec::new();
    if have_timeout {
        args.push(SMOKE_TIMEOUT_SECS.to_string());
    }
    args.push("sh".into());
    args.push("-c".into());
    args.push(verify.to_string());
    let prog = if have_timeout { "timeout" } else { "sh" };
    let t0 = Instant::now();
    match std::process::Command::new(prog)
        .args(&args)
        .current_dir(root)
        .output()
    {
        Err(e) => {
            say(&format!("WARN smoke не запустился: {e}"));
            *warn = warn.saturating_add(1);
        }
        Ok(out) => {
            let elapsed = t0.elapsed().as_secs();
            let rc = out.status.code();
            if out.status.success() {
                say(&format!("ok   smoke VERIFY прошёл ({elapsed}s)"));
            } else if have_timeout && rc == Some(124) {
                say(&format!(
                    "WARN smoke не уложился в {SMOKE_TIMEOUT_SECS}s — прогони руками: {verify}"
                ));
                *warn = warn.saturating_add(1);
            } else {
                say(&format!(
                    "WARN smoke VERIFY упал rc={:?} ({elapsed}s): {}",
                    rc,
                    tail_line(&out)
                ));
                *warn = warn.saturating_add(1);
            }
        }
    }
}

fn crap_smoke(
    root: &Path,
    cr: &crate::core::types::CrapCfg,
    say: &mut dyn FnMut(&str),
    fail: &mut u32,
    failed: &mut Vec<String>,
) {
    let have_timeout = crate::commands::run::which("timeout");
    let mut args: Vec<String> = Vec::new();
    if have_timeout {
        args.push(SMOKE_TIMEOUT_SECS.to_string());
    }
    args.push("sh".into());
    args.push("-c".into());
    args.push(cr.tool.clone());
    let prog = if have_timeout { "timeout" } else { "sh" };
    let contract = match std::process::Command::new(prog)
        .args(&args)
        .current_dir(root)
        .env("DEVH_CRAP_FILES", "")
        .output()
    {
        Ok(out) => {
            let mut all = String::from_utf8_lossy(&out.stdout).into_owned();
            all.push_str(&String::from_utf8_lossy(&out.stderr));
            all.lines()
                .any(|l| l.trim_start().starts_with("CRAP: functions="))
        }
        Err(_) => false,
    };
    if contract {
        say("ok   CRAP: инструмент отвечает (контракт распознан)");
    } else {
        say("FAIL CRAP: инструмент не отвечает по контракту (нет 'CRAP: functions=N') — блокер СРЕДЫ");
        *fail = fail.saturating_add(1);
        failed.push("CRAP-инструмент не распознан (нет 'CRAP: functions=N')".into());
    }
}

/// Последняя непустая строка вывода процесса — для диагностических WARN.
/// (major, полная-строка-версии) из `cargo-mutants --version`.
fn cargo_mutants_version() -> Option<(u64, String)> {
    let out = std::process::Command::new("cargo")
        .args(["mutants", "--version"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let txt = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let ver = txt.split_whitespace().last()?.to_string();
    let major = ver.split('.').next()?.parse::<u64>().ok()?;
    Some((major, ver))
}

/// Число записей в кэше shellcheck-оракла (devharness/target/.shellcheck-cache).
fn shellcheck_cache(root: &Path) -> usize {
    let dir = root
        .join("devharness")
        .join("target")
        .join(".shellcheck-cache");
    std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().count())
        .unwrap_or(0)
}

/// Инкрементный кеш devh в главном дереве: признак прогретого target.
fn warm_target(root: &Path) -> bool {
    let dir = root.join("target").join("debug").join("incremental");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("devh"))
}

/// Процент попаданий sccache из `--show-stats` (None — не отвечает).
fn sccache_hit_rate() -> Option<u64> {
    let out = std::process::Command::new("sccache")
        .arg("--show-stats")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let line = text
        .lines()
        .find(|l| l.contains("Cache hits") && l.contains('%'))?;
    // «52.86 %»: хвост пробелов срезаем, иначе обратный скан цифр встаёт на пробел.
    let before_pct = line.split('%').next()?.trim_end();
    let digits: String = before_pct
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let normal: String = digits.chars().rev().collect();
    normal.parse::<f64>().ok().map(|v| v.round() as u64)
}

pub(crate) fn tail_line(out: &std::process::Output) -> String {
    let mut all = String::from_utf8_lossy(&out.stdout).into_owned();
    all.push_str(&String::from_utf8_lossy(&out.stderr));
    let last = all
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(нет вывода)")
        .trim()
        .to_string();
    let mut chars: Vec<char> = last.chars().collect();
    if chars.len() > 120 {
        let skip = chars.len().saturating_sub(120);
        chars = chars.get(skip..).unwrap_or_else(|| unreachable!()).to_vec();
    }
    chars.into_iter().collect()
}

fn say_line(c: Colors, msg: &str) {
    if msg.starts_with("ok") {
        println!("{}{msg}{}", c.g, c.z);
    } else if msg.starts_with("FAIL") {
        println!("{}{msg}{}", c.r, c.z);
    } else if msg.starts_with("WARN") {
        println!("{}{msg}{}", c.y, c.z);
    } else {
        println!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::Event;
    use crate::core::types::ItemId;

    fn seed(store: &Store, id: &ItemId, status: Status, events: &[Event]) {
        std::fs::create_dir_all(store.item_dir(id)).unwrap_or_else(|_| unreachable!());
        store
            .set_status(id, status)
            .unwrap_or_else(|_| unreachable!());
        let mut log = String::new();
        for e in events {
            log.push_str(&format!(
                "2026-10-19 | {} | {}\n",
                id.dir_name(),
                e.as_str()
            ));
        }
        store
            .append_raw(item_rel(id, "events.log"), &log)
            .unwrap_or_else(|_| unreachable!());
    }

    fn run_replay(store: &Store, id: &ItemId) -> (Vec<String>, u32) {
        let out = std::cell::RefCell::new(Vec::new());
        let mut warn = 0u32;
        let it = store
            .items()
            .into_iter()
            .find(|i| i.id == *id)
            .unwrap_or_else(|| unreachable!());
        {
            let mut say = |m: &str| out.borrow_mut().push(m.to_string());
            replay_checks(store, &it, &mut say, &mut warn);
        }
        (out.into_inner(), warn)
    }

    #[test]
    fn replay_quiet_on_consistent_journal() {
        let d = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        seed(&store, &ItemId(1), Status::Active, &[Event::Claimed]);
        let (lines, warn) = run_replay(&store, &ItemId(1));
        assert!(lines.is_empty(), "здоровый журнал молчит: {lines:?}");
        assert_eq!(warn, 0);
    }

    #[test]
    fn replay_warns_on_status_drift() {
        let d = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        // журнал говорит Active, файл status Done — рассинхрон (дефект 2 плана).
        seed(&store, &ItemId(1), Status::Done, &[Event::Claimed]);
        let (lines, warn) = run_replay(&store, &ItemId(1));
        assert_eq!(warn, 1);
        assert_eq!(lines.len(), 1);
        let line = lines.first().unwrap_or_else(|| unreachable!());
        assert!(line.contains("WARN"), "{lines:?}");
        assert!(line.contains("replay → active"), "{lines:?}");
    }
}
