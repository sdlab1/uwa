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

use crate::core::state::Status;
use crate::core::types::ItemId;
use crate::shell::git;
use crate::shell::store::Store;
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
        // S5: коммит есть, не смержено → мерж.
        let wt = worktree::path_for(cfg, root, &it.id);
        if wt.is_dir() {
            if let Ok(n) = git::rev_list_count(&wt, &cfg.main_branch) {
                if n > 0 {
                    return Next::new(
                        &format!("devh merge {}", it.id.dir_name()),
                        &format!(
                            "{}: гейты зелёные, коммит есть ({n} шт.) — смержить",
                            it.id.dir_name()
                        ),
                    );
                }
            }
        }
        // S1/S3: прогона не было или гейт красный → прогнать (внутри
        // run живёт fix-раунд с промптом, несущим список проблем).
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

    // ── всё завершено ─────────────────────────────────────────────────
    Next::new(
        "готово: активных item'ов нет, спек нет — работа завершена",
        "проверять больше нечего",
    )
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
                return Some(Next::new(
                    "devh mutate problems",
                    &format!("мутационный аудит идёт, уже есть проблем: {problems} — чинить"),
                ));
            }
            return Some(Next::new(
                "devh mutate watch",
                "мутационный аудит идёт — смотреть живой прогресс",
            ));
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
    pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
