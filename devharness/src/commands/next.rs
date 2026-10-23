//! `devh next` — ОРАКУЛ следующего шага.
//!
//! Пока агент сам решает «что дальше» — он будет гадать. Оракул печатает
//! РОВНО ОДНУ исполняемую команду (без плейсхолдеров вида `<id>` — id
//! подставляется реальный) и одну строку «почему».
//!
//! Контракт:
//! * ровно одна команда (или `готово: …`, если следующего шага нет);
//! * код возврата ВСЕГДА 0 (это подсказка, а не проверка);
//! * состояние берётся из файлов репозитория (config/items/state/аудит),
//!   а не из «намерений» агента.
//!
//! Порядок решений — таблица состояний S0–S11 из канона
//! (`commands::workflow::STATES`): источник истины один, и оракул, и
//! документация печатают одно и то же.

use std::path::Path;

use crate::core::runstate::{self, Phase};
use crate::core::state::Status;
use crate::core::types::ItemId;
use crate::shell::git;
use crate::shell::store::{Item, Store};
use crate::shell::worktree;

/// Ответ оракула: команда + причина.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    /// Команда для исполнения (без плейсхолдеров).
    pub command: String,
    /// Почему именно она (одна строка).
    pub why: String,
}

impl Next {
    fn new(command: &str, why: &str) -> Self {
        Self {
            command: command.to_string(),
            why: why.to_string(),
        }
    }
}

/// Точка входа `devh next`.
pub fn next(root: &Path, cfg: &crate::core::types::RunConfig) -> anyhow::Result<()> {
    let store = Store::new(root);
    let answer = decide(root, cfg, &store);
    println!("{}: {}", answer.why, answer.command);
    Ok(())
}

/// Решение оракула (чистая функция состояния — её и тестируем).
pub fn decide(root: &Path, cfg: &crate::core::types::RunConfig, store: &Store) -> Next {
    // ── S0: репозиторий не инициализирован ───────────────────────────
    if !root.join(".harness/config").is_file() {
        return Next::new(
            "devh init",
            "репозиторий не инициализирован (нет .harness/config)",
        );
    }

    // ── S6/S7: мутационный аудит ─────────────────────────────────────
    // Аудит — отдельный контур: пока он идёт или в нём дыра, следующий
    // шаг — аудит, а не работа над item'ами.
    if let Some(n) = audit_next(root) {
        return n;
    }

    // ── item'ы: один живой, один готовый к работе ────────────────────
    let items = store.items();
    for it in &items {
        if it.status != Status::Active {
            continue;
        }
        // S2: прогон идёт — читать лог, НЕ вмешиваться.
        if item_alive(store, &it.id) {
            return Next::new(
                &format!("devh logs {}", it.id.dir_name()),
                &format!(
                    "{}: прогон идёт — читать лог, не вмешиваться",
                    it.id.dir_name()
                ),
            );
        }
        // S10/S5/S4 — по durable-маркеру прогона (гейты зелёные? сдача?).
        if let Some(n) = item_next(root, cfg, store, it) {
            return n;
        }
        // S1/S3: прогона не было или гейт красный с бюджетом → прогнать
        // (внутри run живёт fix-раунд с промптом, несущим список проблем).
        return Next::new(
            &format!("devh run {}", it.id.dir_name()),
            &format!(
                "{}: item активен, но не завершён (фикс-раунд или первый прогон)",
                it.id.dir_name()
            ),
        );
    }
    for it in &items {
        if it.status == Status::Open {
            return Next::new(
                &format!("devh run {}", it.id.dir_name()),
                &format!("{}: item создан, агент ещё не запускался", it.id.dir_name()),
            );
        }
    }

    // ── S9: ничего не начато, но есть спеки ──────────────────────────
    let specs = root.join("specs");
    if specs.is_dir() && has_entries(&specs) {
        return Next::new(
            "devh auto --from specs/ --max 5",
            "есть спеки, но ни одного item — запустить автопилот",
        );
    }

    // ── S11: мусор — старые worktree/логи ────────────────────────────
    if let Some(n) = cleanup_next(cfg, store) {
        return n;
    }

    // ── всё завершено ─────────────────────────────────────────────────
    Next::new(
        "готово: активных item'ов нет, спек нет — работа завершена",
        "проверять больше нечего",
    )
}

/// Следующий шаг по активному item'у: S10 (сдача) → S5 (мерж) → S4 (коммит).
///
/// Решение опирается на DURABLE-маркер прогона (`core::runstate`): статус
/// item'а не различает «гейты зелёные, нужен коммит» и «бюджет фиксов
/// исчерпан» (в обоих случаях item остаётся `active`), а маркер пишет
/// только харнесс — тот, кто знает вердикт гейта.
///
/// Маркеру верим только пока ДЕРЕВО не менялось: в нём записан SHA
/// проверенного дерева. Изменилось — вердикт протух, гейты надо
/// перепроверить (`devh run`).
fn item_next(
    root: &Path,
    cfg: &crate::core::types::RunConfig,
    store: &Store,
    it: &Item,
) -> Option<Next> {
    let wt = worktree::path_for(cfg, root, &it.id);
    let rs = runstate::read(store, &it.id)?;
    // S10: гейт красный — работа сдана.
    if rs.phase == Phase::GateFailed {
        let gate = if rs.gate.is_empty() {
            "гейт"
        } else {
            rs.gate.as_str()
        };
        // Блокер СРЕДЫ (код 2) — не приговор работе: проверку остановила
        // среда (нагрузка, инструмент, сборка), а не качество правок.
        // Выбрасывать работу из-за чужой нагрузки нельзя (`devh abort`
        // удаляет ветку и worktree) → честный ответ: снять блокер и
        // перепроверить гейты.
        if rs.code == 2 {
            return Some(Next::new(
                &format!("devh run {}", it.id.dir_name()),
                &format!(
                    "{}: {gate} заблокирован средой (код 2) — работа не признана плохой: \
                     сними блокер среды и перепрогони гейты",
                    it.id.dir_name()
                ),
            ));
        }
        return Some(Next::new(
            &format!("devh abort {}", it.id.dir_name()),
            &format!(
                "{}: {gate} красный — сдача: списать item (или переформулировать: devh hint)",
                it.id.dir_name()
            ),
        ));
    }
    if !matches!(rs.phase, Phase::GatesOk | Phase::Committed) {
        return None;
    }
    // Свежесть: HEAD дерева должен совпадать с проверенным.
    let head_now = git::short_head(&wt);
    if rs.head.is_empty() || rs.head != head_now {
        return None;
    }
    // Свежесть содержимого: вердикт относится к дереву, а не к коммиту.
    // Незакоммиченная правка после зелёных гейтов = другое дерево →
    // вердикт протух → оракул предлагает `devh run` (перепроверить гейты),
    // а не `commit` по чужому зелёному свету.
    //
    // Пустой дайджест = «свежесть НЕИЗВЕСТНА» (маркер записан, когда
    // дайджест дерева не удалось посчитать). Доверять такому вердикту
    // нельзя: фазы GatesOk/Committed разрешают коммит и мерж, значит
    // содержимое обязано быть проверено. Fail-closed → перепроверка.
    if rs.digest.is_empty() {
        return None;
    }
    let now = git::tree_digest(&wt).unwrap_or_default();
    if now != rs.digest {
        return None;
    }
    let ahead = git::rev_list_count(&wt, &cfg.main_branch).unwrap_or(0);
    match rs.phase {
        Phase::Committed if ahead > 0 => Some(Next::new(
            &format!("devh merge {}", it.id.dir_name()),
            &format!(
                "{}: гейты зелёные, коммит есть ({ahead} шт.) — смержить",
                it.id.dir_name()
            ),
        )),
        Phase::Committed => Some(Next::new(
            &format!("devh abort {}", it.id.dir_name()),
            &format!(
                "{}: гейты зелёные, но коммитов сверх {} нет — работа уже в базе, item списать",
                it.id.dir_name(),
                cfg.main_branch
            ),
        )),
        Phase::GatesOk if git::tree_dirty(&wt) => Some(Next::new(
            &format!("devh commit {}", it.id.dir_name()),
            &format!(
                "{}: гейты зелёные, коммита нет — закоммитить (коммит делает харнесс)",
                it.id.dir_name()
            ),
        )),
        Phase::GatesOk => Some(Next::new(
            &format!("devh abort {}", it.id.dir_name()),
            &format!(
                "{}: гейты зелёные, но изменений нет — нечего коммитить, item списать",
                it.id.dir_name()
            ),
        )),
        _ => None,
    }
}

/// S11: мусор — worktree закрытого item'а остался или логи протухли.
/// Уборку делает `devh clean` (он же чистит логи старше LOG_DAYS).
fn cleanup_next(cfg: &crate::core::types::RunConfig, store: &Store) -> Option<Next> {
    for it in store.items() {
        if it.status != Status::Done {
            continue;
        }
        let wt = worktree::path_for(cfg, store.root(), &it.id);
        if wt.is_dir() {
            return Some(Next::new(
                "devh clean",
                &format!(
                    "{}: item закрыт, но worktree остался — убрать мусор",
                    it.id.dir_name()
                ),
            ));
        }
    }
    if cfg.log_days > 0 {
        let logs = store.dir.join("logs");
        if let Ok(rd) = std::fs::read_dir(&logs) {
            for e in rd.flatten() {
                let Ok(meta) = e.metadata() else { continue };
                let Ok(mtime) = meta.modified() else { continue };
                let age_days = mtime.elapsed().map(|d| d.as_secs() / 86_400).unwrap_or(0);
                if age_days > cfg.log_days as u64 {
                    return Some(Next::new(
                        "devh clean",
                        &format!(
                            "логи старше {}д протухли — убрать (devh clean)",
                            cfg.log_days
                        ),
                    ));
                }
            }
        }
    }
    None
}

/// Следующий шаг по мутационному аудиту (S6/S7), если аудит есть.
fn audit_next(root: &Path) -> Option<Next> {
    use crate::commands::mutate::AuditState;
    match crate::commands::mutate::audit_state(root) {
        AuditState::None => None,
        AuditState::Running => {
            // Идёт: если уже есть проблемы — сразу к ним (problem-first),
            // иначе — наблюдать.
            let problems = crate::commands::mutate::live_problem_count(root);
            if problems > 0 {
                Some(Next::new(
                    "devh mutate problems",
                    &format!(
                        "мутационный аудит идёт, уже есть проблем: {problems} — чинить сразу, не ждать verdict (copy-режим: правки дерева безопасны)"
                    ),
                ))
            } else {
                Some(Next::new(
                    "devh mutate watch",
                    "мутационный аудит идёт — смотреть живой прогресс",
                ))
            }
        }
        AuditState::Stopped | AuditState::Aborted => Some(Next::new(
            &restart_command(root),
            "аудит не дал результата (остановлен/оборвался) — перезапустить тем же прогоном",
        )),
        AuditState::Finished => {
            // Аудит завершён: вердикт решает, чисто или нет.
            if let Some(false) = crate::commands::mutate::verdict_state(root) {
                return Some(Next::new(
                    "devh mutate problems",
                    "аудит завершён с дырой — чинить выживших/таймаутных, потом перепрогнать",
                ));
            }
            None
        }
    }
}

/// Исполняемая команда перезапуска аудита: ТЕ ЖЕ флаги, что и в прошлом
/// прогоне. «devh mutate start» без флагов — неисполняемая команда
/// (rc=1), значит оракул не имеет права её предлагать.
fn restart_command(root: &Path) -> String {
    let args = crate::commands::mutate::state_args(root);
    if args.is_empty() {
        // Флагов не сохранилось — каноническая зона ядра (см. mutate::CANON).
        return "devh mutate start --config devharness/cargo-mutants.toml".to_string();
    }
    let quoted: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains(char::is_whitespace) {
                format!("'{a}'")
            } else {
                a.clone()
            }
        })
        .collect();
    format!("devh mutate start {}", quoted.join(" "))
}

/// Есть ли в каталоге хоть что-то (файлы спек).
fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut rd| rd.next().is_some())
        .unwrap_or(false)
}

/// Живой ли прогон item'а (pid-файл + процесс на месте).
fn item_alive(store: &Store, id: &ItemId) -> bool {
    let Some(text) = store.read(crate::shell::store::item_rel(id, "run.pid")) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        return false;
    };
    crate::shell::proc::pid_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Мини-репозиторий worktree'а: git init + коммит(ы).
    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} не прошёл в {}", dir.display());
    }

    /// Worktree задачи: базовый коммит на `main` (+ ветка задачи с коммитом).
    fn make_wt(dir: &Path, branch: &str, work_commit: bool) {
        std::fs::create_dir_all(dir).unwrap_or_else(|_| unreachable!());
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap_or_else(|_| unreachable!());
        git(dir, &["add", "f.txt"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        if work_commit {
            git(dir, &["checkout", "-q", "-b", branch]);
            std::fs::write(dir.join("g.txt"), "y").unwrap_or_else(|_| unreachable!());
            git(dir, &["add", "g.txt"]);
            git(dir, &["commit", "-q", "-m", "work"]);
        }
    }

    /// Песочница: item W-001 в active + worktree + конфиг.
    fn sandbox(d: &Path) -> (crate::core::types::RunConfig, Store, Item) {
        let root = d.join("repo");
        std::fs::create_dir_all(root.join(".harness/items/W-001"))
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(root.join(".harness/config"), "MIN_KILL=90\n")
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(root.join(".harness/items/W-001/status"), "active")
            .unwrap_or_else(|_| unreachable!());
        let store = Store::new(&root);
        let mut cfg = crate::core::types::RunConfig::default();
        cfg.wt_root = d.join("wt");
        let id = ItemId(1);
        let wt = worktree::per_id_path(&cfg, &root, &id);
        make_wt(&wt, &id.branch(), false);
        let it = Item {
            id,
            status: Status::Active,
            title: "t".to_string(),
            red_cmd: None,
            no_red: None,
            agent: None,
        };
        (cfg, store, it)
    }

    #[test]
    fn next_is_a_single_executable_command() {
        let n = Next::new("devh run W-001", "причина");
        assert_eq!(n.command, "devh run W-001");
        assert!(!n.command.contains('<'), "без плейсхолдеров: {}", n.command);
    }

    #[test]
    fn audit_next_is_none_without_state() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert!(audit_next(d.path()).is_none());
    }

    #[test]
    fn running_audit_points_to_watch() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness/mutate"))
            .unwrap_or_else(|_| unreachable!());
        // pid = наш собственный: супервизор «жив» → состояние Running.
        std::fs::write(
            d.path().join(".harness/mutate/state.json"),
            format!(r#"{{"state":"running","pid":{}}}"#, std::process::id()),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = audit_next(d.path()).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh mutate watch");
    }

    #[test]
    fn running_audit_with_problem_points_to_problems_immediately() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness/mutate"))
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join(".harness/mutate/state.json"),
            format!(
                r#"{{"state":"running","pid":{},"args":[]}}"#,
                std::process::id()
            ),
        )
        .unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join("mutants.out")).unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("mutants.out/missed.txt"),
            "devharness/src/core/survivors.rs:85:22: replace == with !=\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let n = audit_next(d.path()).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh mutate problems");
        assert!(n.why.contains("не ждать verdict"), "{}", n.why);
        assert!(n.why.contains("copy"), "{}", n.why);
    }

    #[test]
    fn stopped_audit_restarts_with_the_same_flags() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness/mutate"))
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join(".harness/mutate/state.json"),
            r#"{"state":"stopped","pid":1,"args":["--config","devharness/cargo-mutants.toml"]}"#,
        )
        .unwrap_or_else(|_| unreachable!());
        let n = audit_next(d.path()).unwrap_or_else(|| unreachable!());
        assert_eq!(
            n.command, "devh mutate start --config devharness/cargo-mutants.toml",
            "перезапуск — теми же флагами: без них команда неисполняема"
        );
    }

    #[test]
    fn oracle_command_has_no_placeholders() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let n = decide(
            d.path(),
            &crate::core::types::RunConfig::default(),
            &Store::new(d.path()),
        );
        assert_eq!(n.command, "devh init", "S0: нет конфига → devh init");
        assert!(!n.command.contains('<'), "без плейсхолдеров: {}", n.command);
    }

    #[test]
    fn failed_run_marker_points_to_abort() {
        // S10: бюджет фиксов исчерпан → сдача, а не новый прогон.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gate_failed("verify", 1, &head),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh abort W-001", "S10: сдача → abort");
    }

    #[test]
    fn gates_ok_with_dirty_tree_points_to_commit() {
        // S4: гейты зелёные, коммита нет → коммит (делает харнесс).
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        std::fs::write(wt.join("new.txt"), "z").unwrap_or_else(|_| unreachable!());
        let digest = git::tree_digest(&wt).unwrap_or_default();
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gates_ok(&head).with_digest(&digest),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh commit W-001", "S4: гейты зелёные → commit");
    }

    #[test]
    fn committed_marker_points_to_merge() {
        // S5: гейты зелёные + коммит → мерж.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        git(&wt, &["checkout", "-q", "-b", &it.id.branch()]);
        std::fs::write(wt.join("g.txt"), "y").unwrap_or_else(|_| unreachable!());
        git(&wt, &["add", "g.txt"]);
        git(&wt, &["commit", "-q", "-m", "work"]);
        let head = git::short_head(&wt);
        let digest = git::tree_digest(&wt).unwrap_or_default();
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::committed(&head).with_digest(&digest),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh merge W-001", "S5: коммит есть → merge");
    }

    #[test]
    fn stale_marker_falls_back_to_run() {
        // Дерево изменилось после вердикта → маркер протух: гейты надо
        // перепроверить, а не коммитить/мержить вслепую.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        runstate::write(&store, &it.id, &runstate::RunState::gates_ok("deadbeef"))
            .unwrap_or_else(|_| unreachable!());
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh run W-001", "протухший маркер → run");
    }

    #[test]
    fn fresh_digest_allows_commit() {
        // Тот же маркер, но с дайджестом СОДЕРЖИМОГО дерева: вердикт
        // относится к этому дереву → оракул предлагает коммит.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        std::fs::write(wt.join("new.txt"), "z").unwrap_or_else(|_| unreachable!());
        let digest = git::tree_digest(&wt).unwrap_or_default();
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gates_ok(&head).with_digest(&digest),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh commit W-001", "свежий дайджест → commit");
    }

    #[test]
    fn stale_digest_falls_back_to_run() {
        // Незакоммиченная правка ПОСЛЕ зелёных гейтов = другое дерево:
        // вердикт протух, даже если HEAD тот же → оракул предлагает run.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        let digest = git::tree_digest(&wt).unwrap_or_default();
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gates_ok(&head).with_digest(&digest),
        )
        .unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("new.txt"), "z").unwrap_or_else(|_| unreachable!());
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh run W-001", "протухший дайджест → run");
    }

    #[test]
    fn marker_without_digest_is_not_trusted() {
        // Дайджест дерева не посчитан (маркер записан при ошибке) → свежесть
        // содержимого НЕИЗВЕСТНА. Фаза GatesOk разрешает коммит, а значит
        // содержимое обязано быть проверено: fail-closed → перепроверка.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        // Дерево грязное — иначе оракул предложил бы `abort`, а не `commit`.
        std::fs::write(wt.join("new.txt"), "z").unwrap_or_else(|_| unreachable!());
        runstate::write(&store, &it.id, &runstate::RunState::gates_ok(&head))
            .unwrap_or_else(|_| unreachable!());
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh run W-001", "без дайджеста → перепроверка");
    }

    #[test]
    fn committed_without_digest_does_not_offer_merge() {
        // То же для фазы Committed: без дайджеста мерж не предлагается —
        // после коммита дерево могли изменить, а это другой вердикт.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        runstate::write(&store, &it.id, &runstate::RunState::committed(&head))
            .unwrap_or_else(|_| unreachable!());
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh run W-001", "без дайджеста → перепроверка");
    }

    #[test]
    fn env_blocked_gate_does_not_throw_the_work_away() {
        // Блокер СРЕДЫ (код 2) — гейт не прошёл из-за нагрузки/инструмента,
        // а не из-за правок. `devh abort` удаляет ветку и worktree, поэтому
        // предлагать его здесь = выбросить хорошую работу из-за чужой
        // нагрузки. Честный ответ — снять блокер и перепроверить гейты.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gate_failed("mutation", 2, &head),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(
            n.command, "devh run W-001",
            "блокер среды — работа не плоха, выбрасывать её нельзя"
        );
        assert!(
            n.why.contains("сред"),
            "причина должна называть среду: {}",
            n.why
        );
    }

    #[test]
    fn failed_gate_marker_points_to_abort_with_honest_reason() {
        // Гейт красный по существу (код 1) — работа сдана. Формулировка не
        // должна врать «бюджет исчерпан», если исчерпание — не причина.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        runstate::write(
            &store,
            &it.id,
            &runstate::RunState::gate_failed("red-post", 1, &head),
        )
        .unwrap_or_else(|_| unreachable!());
        let n = item_next(store.root(), &cfg, &store, &it).unwrap_or_else(|| unreachable!());
        assert_eq!(n.command, "devh abort W-001");
        assert!(
            n.why.contains("red-post"),
            "причина должна называть гейт: {}",
            n.why
        );
        assert!(
            !n.why.contains("бюджет исчерпан"),
            "причина не обязана быть «бюджет исчерпан»: {}",
            n.why
        );
    }

    #[test]
    fn closed_item_with_leftover_worktree_points_to_clean() {
        // S11: item закрыт, а его worktree остался — это мусор, а не работа.
        // Оракул обязан предложить уборку (`devh clean`), а не новый прогон.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        std::fs::write(store.item_dir(&it.id).join("status"), "done")
            .unwrap_or_else(|_| unreachable!());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        assert!(wt.is_dir(), "worktree закрытого item'а остался");
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh clean", "S11: мусор → уборка");
    }

    #[test]
    fn in_progress_marker_falls_back_to_run() {
        // Fail-closed: прогон начат, но финального вердикта нет (упал или
        // запись маркера не удалась) → оракул предлагает ПЕРЕПРОВЕРИТЬ гейты,
        // а не коммитить по маркеру прошлого прогона.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let (cfg, store, it) = sandbox(d.path());
        let wt = worktree::path_for(&cfg, store.root(), &it.id);
        let head = git::short_head(&wt);
        runstate::write(&store, &it.id, &runstate::RunState::gates_ok(&head))
            .unwrap_or_else(|_| unreachable!());
        // Прогон стартовал — старый «зелёный» маркер перекрыт.
        runstate::write(&store, &it.id, &runstate::RunState::in_progress(&head))
            .unwrap_or_else(|_| unreachable!());
        assert!(
            item_next(store.root(), &cfg, &store, &it).is_none(),
            "незавершённый прогон не должен давать commit/merge"
        );
        let n = decide(store.root(), &cfg, &store);
        assert_eq!(n.command, "devh run W-001", "незавершённый прогон → run");
    }
}
