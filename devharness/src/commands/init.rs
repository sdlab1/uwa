use std::path::Path;

use crate::shell::color::colors;
use crate::shell::git;

fn env_or(k: &str, d: &str) -> String {
    match std::env::var(format!("HARNESS_{k}")) {
        Ok(v) if !v.is_empty() => v,
        _ => d.to_string(),
    }
}

fn env_num_or(k: &str, d: u32) -> u32 {
    env_or(k, &d.to_string()).trim().parse().unwrap_or(d)
}

pub fn init(root: &Path) -> anyhow::Result<()> {
    let col = colors();
    let conf = root.join(".harness/config");
    if conf.exists() {
        anyhow::bail!(
            "devh: {} уже есть (перезапись: rm {})",
            conf.display(),
            conf.display()
        );
    }

    let mut verify = env_or("VERIFY", "");
    let mut bootstrap = env_or("BOOTSTRAP", "");
    if verify.is_empty() {
        if is_exec(root.join("scripts/verify.sh")) {
            verify = "./scripts/verify.sh".into();
        } else if root.join("gobot/mix.exs").exists() {
            verify = "cd gobot && mix test".into();
            bootstrap = "cd gobot && mix deps.get && mix compile".into();
        } else if root.join("mix.exs").exists() {
            verify = "mix test".into();
            bootstrap = "mix deps.get && mix compile".into();
        } else if root.join("Cargo.toml").exists() {
            verify = "cargo test".into();
            bootstrap = "cargo fetch".into();
        } else if makefile_has_verify(root.join("Makefile")) {
            verify = "make verify".into();
        }
    }

    let mut mutate = env_or("MUTATE", "");
    let mut mutate_scope = env_or("MUTATE_SCOPE", "");
    if mutate.is_empty() && root.join("gobot/mix.exs").exists() {
        mutate = "cd gobot && mix mutants --module {module}".into();
        mutate_scope = "gobot/lib".into();
    }
    if mutate.is_empty() && root.join("Cargo.toml").exists() {
        // {diff} (P7-2.2) → ОДИН запуск на все изменённые строки: devh сам
        // строит scoped-diff (git diff HEAD + intent-to-add) и отдаёт путь в
        // --in-diff cargo-mutants. Один baseline и один тёплый target вместо
        // N холодных прогонов (смысл P1b, тем же механизмом). --in-place:
        // без него mutants копирует дерево в новый путь и платит
        // cold-build; в devh-worktree дерево и так чистое. --baseline=skip: devh уже прогнал VERIFY в этом дереве.
        // CARGO_PROFILE_TEST_INCREMENTAL=true — корневой рычаг муть-сборки
        // (замер docs/rust_build.md §9.7: профиль test по умолчанию
        // incremental=false → каждый мутант платил полную пересборку 90s;
        // с true — 4-12s). --test-tool nextest — process-per-test: точные
        // таймауты, дешевле cargo test на каждый мутант.
        // --timeout-multiplier 2 — мутант иногда медленнее baseline'а, не
        // плодить TIMEOUT'ы (docs/rust_build.md §9.7-9.11).
        // --iterate (P7-3) — пропуск пойманных ранее: допустим ТОЛЬКО с нашим
        // kill-cache (отпечаток дерева .rs+тестов гейтит outcomes.json);
        // пустой остаток («No mutants to filter») гейт называет «0 мутантов».
        mutate = "CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 --iterate --in-diff \"{diff}\"".into();
        mutate_scope = rust_mutate_scope(root);
    }
    if mutate_scope.is_empty() {
        mutate_scope = "lib".into();
    }

    let mut crap = env_or("CRAP", "");
    let mut crap_scope = env_or("CRAP_SCOPE", "");
    if crap.is_empty() && root.join("gobot/mix.exs").exists() {
        crap = "cd gobot && mix crap.report".into();
        crap_scope = "gobot/lib dev".into();
    }
    if crap.is_empty() && root.join("scripts/crap-rs.sh").exists() {
        crap = "bash scripts/crap-rs.sh".into();
        crap_scope = "uwa-core/src crates".into();
    }
    let crap_max = env_num_or("CRAP_MAX", 12);

    let mut rules = env_or("RULES", "");
    let mut rules_log = env_or("RULES_LOG", "");
    if root.join("todo/report.md").exists() && rules.is_empty() {
        rules = "todo/report.md".into();
        rules_log = "todo/report.md".into();
    }
    let msg_deny = env_or("MSG_DENY", r"todo/|report\.md|Session [0-9]+");

    let main = git::branch_current(root).unwrap_or_else(|| "main".into());
    let agent = env_or("AGENT", "goose run --no-session -t");
    let agent_timeout = env_num_or("AGENT_TIMEOUT", 1800);
    let max_fix = env_num_or("MAX_FIX", 3);
    let max_active = env_num_or("MAX_ACTIVE", 3);
    let remote = env_or("REMOTE", "origin");
    let wt_root = env_or("WT_ROOT", "/tmp");
    let log_days = env_num_or("LOG_DAYS", 30);
    let min_kill = env_num_or("MIN_KILL", 90);
    let bench = env_or("BENCH", "");
    let no_ignored = env_num_or("NO_IGNORED", 1);
    let ignored_allowlist = env_or("IGNORED_ALLOWLIST", "scripts/ignored-tests.txt");
    let lint = env_num_or("LINT", 1);
    let lint_cmd = env_or("LINT_CMD", "");
    let review = env_num_or("REVIEW", 1);
    let freeze = env_num_or("FREEZE", 1);
    let freeze_allow = env_or("FREEZE_ALLOW", "");

    let mut c = String::new();
    c += "# devh config (source-ится bash'ем). Env-переменные HARNESS_* перекрывают.\n";
    c += &format!(
        "VERIFY='{verify}'            # команда проверки; обязана проходить после работы агента\n"
    );
    c += &format!(
        "BOOTSTRAP='{bootstrap}'       # однократная подготовка свежего worktree (deps и т.п.)\n"
    );
    c += &format!("AGENT='{agent}'               # агент; промпт передаётся одним аргументом\n");
    c += &format!(
        "AGENT_TIMEOUT={agent_timeout} # сек на один запуск агента (watchdog: «агент сдох»)\n"
    );
    c += &format!("MAX_FIX={max_fix}             # раундов «гейт упал → агент чинит»\n");
    c += &format!("MAX_ACTIVE={max_active}       # WIP-лимит: сколько items могут быть active одновременно (0 = выкл)\n");
    c += &format!(
        "REMOTE='{remote}'             # git remote; репо без remote — пуш пропускается\n"
    );
    c += &format!("MAIN='{main}'                 # ветка, куда мержим (текущая на момент init)\n");
    c += &format!("WT_ROOT='{wt_root}'           # где живут worktree'ы\n");
    c += &format!(
        "LOG_DAYS={log_days}            # retention логов в .harness/logs (0 = не чистить)\n"
    );
    c += "# ── гейты качества тестов/кода (пусто = выкл) ──\n";
    c += &format!(
        "MUTATE='{mutate}'  # мутационный гейт; {{diff}} = diff всех правок агента (devh строит сам, --in-diff cargo-mutants), {{file}}/{{module}} = запуск на файл, {{files}} = один запуск на все изменённые файлы\n#   {{diff}}: путь к .harness/mutate.diff пишет devh — в шаблоне его не вычислять; пустой скоуп («Diff file is empty») = Ok, «0 мутантов — нечего убивать»\n#   --iterate (пропуск пойманных ранее) допустим: devh гейтит кэш отпечатком ДЕРЕВА (.rs+тесты), дерево изменилось — прошлый outcomes.json уезжает в mutants.out.stale-* (слабый тест не «скипнет» мутанта)\n"
    );
    c += &format!("MUTATE_SCOPE='{mutate_scope}'  # какие пути считать «прод-кодом» для мутаций\n");
    c += &format!("MIN_KILL={min_kill}           # минимальный kill-rate, %\n");
    c += "MUTATE_EARLY=0            # P7-2.3 ранний выход: DOOM (kill-rate недостижим) всегда, SAMPLE (rule-of-three ≥95%) — только с --shuffle в шаблоне\n";
    c += "MUTATE_COV=0              # P7-2.4 покрытийный отбор: llvm-cov -> nextest -E (требует nightly rustc)\n";
    c += "MUTATE_SURVIVORS=1        # P8-1 третья петля: MissedMutant'ы каждого прогона → .harness/observations.md (seen-дедуп + killmap.json + survivors.log); 0 = выкл\n";
    c += "RED_MUTATE=''  # контроль red-теста: шаблон мутантов с {test} и {diff} (пусто = выкл)\n";
    c += &format!("RED_MIN_KILL={min_kill}      # минимальный kill-rate именно red-теста, %\n");
    c += "RED_EXPECT=''      # P7 red.expect: regex ОЖИДАЕМОЙ ошибки red-фазы (глобальный дефолт; пер-таск — строка спеки RED-EXPECT: или маркер '# red-expect:' в red.cmd). Пусто = выкл: любой exit≠0 на pre считается RED\n";
    c += &format!(
        "CRAP='{crap}'  # гейт CRAP (guardrail №2): score = C²·(1−cov)³ + C; пусто = выкл\n"
    );
    c += &format!(
        "CRAP_SCOPE='{crap_scope}'  # какие пути анализировать (пусто = все изменённые)\n"
    );
    c += &format!("CRAP_MAX={crap_max}            # порог нарушения: score > MAX = красный\n");
    c += &format!("BENCH='{bench}'                     # перф-гейт, пример: bash devharness/examples/check_bench.sh\n");
    c += "# ── session-workflow (uwa: todo/report.md) ──\n";
    c += &format!("RULES='{rules}'               # файл правил проекта; промпт: «прочитай полностью, исполняй беспрекословно»\n");
    c += &format!("RULES_LOG='{rules_log}'       # куда дописывать «## Session N …» после merge (пусто = выкл)\n");
    c += &format!("MSG_DENY='{msg_deny}'         # гигиена сообщения коммита (grep -E): матч → reject, лечение: devh commit\n");
    c += &format!("NO_IGNORED={no_ignored}          # 1 = незаявленный ignored-тест в verify = fail (0 = выкл для чужих форматов)\n");
    c += &format!("IGNORED_ALLOWLIST='{ignored_allowlist}'  # файл: regex имён ignored-тестов, которым разрешён игнор (нет файла = пустой allowlist)\n");
    c += &format!("LINT={lint}                      # 1 = авто-shellcheck изменённых *.sh\n");
    c += &format!("LINT_CMD='{lint_cmd}'         # проектный линтер (напр. cargo clippy -- -D warnings); пусто = выкл\n");
    c += &format!("REVIEW={review}                  # 1 = self-review диффа вторым запросом агенту (VERDICT: OK|ISSUES)\n");
    c += &format!("FREEZE={freeze}                    # 1 = A6: дифф сессии не выходит за заявленные файлы (первый гейт, дешевле VERIFY)\n");
    c += &format!("FREEZE_ALLOW='{freeze_allow}'          # глобальный allowlist FREEZE: глобы через пробел ('src/* *.md'; per-item — секция FILES: в спеке)\n");
    c += "#   FREEZE: allowlist = FILES: спеки (items/W-NNN/files, глобы по строке) + FREEZE_ALLOW; пусто у обоих = гейт пропущен с логом\n";
    c += "#   правка tests/** — только через записанный обход items/W-NNN/allow-test-edit (симметрия no-crap): devh run остановится и попросит причину\n";

    std::fs::create_dir_all(root.join(".harness/items"))?;
    std::fs::create_dir_all(root.join(".harness/summaries"))?;
    std::fs::create_dir_all(root.join(".harness/logs"))?;
    std::fs::write(&conf, &c)?;

    let excl = root.join(".git/info/exclude");
    if let Some(parent) = excl.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut text = std::fs::read_to_string(&excl).unwrap_or_default();
    // mutants.out/ — live-протокол cargo-mutants лежит в рабочем дереве
    // (--in-place); без exclude `devh commit` (git add -A) утащит его в
    // фичу-ветку. info/exclude общий для всех worktrees репозитория.
    for entry in [".harness/", "mutants.out/"] {
        if !text.lines().any(|l| l.trim() == entry) {
            text += &format!("{entry}\n");
        }
    }
    std::fs::write(&excl, text)?;

    println!("{}OK init: {}{}", col.g, conf.display(), col.z);
    if verify.is_empty() {
        println!(
            "{}⚠  VERIFY не задетектирован — пропиши в {}{}",
            col.y,
            conf.display(),
            col.z
        );
    }
    Ok(())
}

/// Какие пути Rust-проекта считать прод-кодом для мутаций.
///
/// Ловушка git-pathspec'ов: матч идёт ДО КОНЦА строки, поэтому `crates/*/src`
/// (каталог с глобом) не ловит НИЧЕГО внутри src — нужен `crates/*/src/*`.
/// Голый каталог без глобов (`src`) — prefix, он матчит рекурсивно.
fn rust_mutate_scope(root: &Path) -> String {
    if root.join("crates").is_dir() {
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
        if manifest.contains("[workspace]") || manifest.contains("members") {
            return "crates/*/src/*".into();
        }
    }
    "src".into()
}

fn is_exec(p: std::path::PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn makefile_has_verify(p: std::path::PathBuf) -> bool {
    std::fs::read_to_string(p)
        .map(|t| t.lines().any(|l| l.starts_with("verify:")))
        .unwrap_or(false)
}
