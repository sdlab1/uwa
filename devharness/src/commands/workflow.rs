//! `devh workflow` — машинно-читаемая проекция канонического workflow.
//!
//! ## Иерархия источников истины
//!
//! 1. **код** (этот файл и `commands::mutate::CANON`);
//! 2. `devh workflow` — печать канона из кода;
//! 3. справка команды (`devh mutate help`, `devh test --help`);
//! 4. `docs/mutation_workflow.md`, `AGENTS.md`;
//! 5. память агента (goose).
//!
//! Правило: **если память агента противоречит `devh workflow` — правда
//! `devh workflow`.**
//!
//! Канон НЕ дублируется в документах вручную: `docs/mutation_workflow.md`
//! и `AGENTS.md` генерируются из этих констант (`devh workflow > файл`).

use crate::commands::mutate;

/// Порядок источников истины.
pub const SOURCE_ORDER: &str = r#"=== ИСТОЧНИКИ ИСТИНЫ (по убыванию авторитета) ===
  1. код харнесса (commands/workflow.rs, commands/mutate.rs)
  2. devh workflow   — печать канона из кода
  3. справка команды (devh mutate help)
  4. docs/mutation_workflow.md, AGENTS.md
  5. память агента
  Если память агента противоречит devh workflow — правда devh workflow."#;

/// Глобальные коды возврата.
pub const EXIT_CODES: &str = r#"=== КОДЫ ВОЗВРАТА (все команды) ===
  0 — успех (чисто / шаг выполнен)
  1 — actionable проблема проекта (дыра в тестах, красный гейт, FAIL в сьюте)
  2 — среда не готова / нечего проверять (нет аудита, аудит идёт, нет инструмента)
  3 — внутренняя ошибка харнесса (наблюдатель отвалился) — НЕ чинить код проекта"#;

/// Правила R1–R14.
pub const RULES: &str = r#"=== ПРАВИЛА ===
  R1  мутационный аудит запускается ТОЛЬКО через `devh mutate`; руками cargo-mutants — НЕЛЬЗЯ
  R2  внешнего watcher'а нет; наблюдение — родное (inotify), без sleep-циклов
  R3  дублирующих документов-«вторых источников истины» нет (MUTATION_WORKFLOW.md удалён)
  R4  пойман = ТОЛЬКО CaughtMutant; kill-rate = 100 * caught / (done - unviable)
  R5  выживший/таймаут/unviable/неопознанный исход/битые детали → финальный вердикт НЕ может быть «чисто»
  R6  Unviable — дыра с нулевой терпимостью (чинить код, пока мутант не собирается)
  R7  живой вывод — событийный (inotify), без внешнего скрипта и без sleep-цикла агента
  R8  сьюты запускаются ТОЛЬКО через `devh test` (полный лог + итог PASS/FAIL в stdout и в коде возврата)
  R9  git принадлежит харнессу: агент НЕ делает git add/commit/push/rebase/reset/checkout/stash
  R10 одна зона аудита за раз: mutants.out — один на прогон
  R11 фоновый аудит не трогает дерево агента (--in-place запрещён для `devh mutate start`)
  R12 артефакты аудита — read-only для агента (правка руками = читерство, результат обнуляется)
  R13 никаких «ручных» команд в bash: всё, что нужно от харнесса, делается его подкомандами
  R14 не гадать: непонятно, что делать дальше → `devh next` печатает ровно одну следующую команду"#;

/// Автомат состояний S0–S11.
pub const STATES: &str = r#"=== СОСТОЯНИЯ (маршрут) ===
  S0  нет .harness/config                     → devh init
  S1  item создан, агент не запускался         → devh run <ID>
  S2  идёт агент/прогон                       → devh logs <ID>
  S3  гейт упал, fix-раунд                     → внутри devh run (промпт несёт список проблем)
  S4  гейты зелёные, нужен коммит              → devh commit <ID>
  S5  коммит есть, не смержено                 → devh merge <ID>
  S6  мутационный аудит идёт                   → devh mutate problems (или watch, если проблем нет)
  S7  аудит завершён, есть дыра                → devh mutate problems
  S8  аудит завершён, чисто                    → devh commit <ID> / devh merge <ID>
  S9  ничего не начато, но есть спеки          → devh auto --from specs/ --max 5
  S10 лимит фиксов исчерпан                    → devh abort <ID>
  S11 мусор: старые worktree/логи              → devh clean / devh sweep --maxsize 20G"#;

/// Каталог команд.
pub const COMMANDS: &str = r#"=== КОМАНДЫ ===
  devh init                    инициализация репозитория
  devh new <спека>             создать item
  devh run <ID>                прогон агента + гейты + фиксы
  devh bg <ID>                 фоновый прогон (наблюдение — devh ps)
  devh logs <ID>               хвост лога item'а
  devh commit <ID>             коммит (делает харнесс)
  devh merge <ID>              rebase → ff-push → лог
  devh abort <ID>              списать item
  devh list                    список item'ов
  devh ps                      живые прогоны
  devh doctor                  диагностика окружения
  devh clean / devh sweep      уборка мусора
  devh test                    полный сьют (test.sh): полный лог + PASS/FAIL + код возврата
  devh test --list             список проверок сьюта
  devh test --case <N|имя>     точечная проверка (фильтр отчёта)
  devh mutate start|status|watch|problems|verdict|stop   мутационный аудит
  devh next                    оракул: ровно одна следующая команда (всегда rc=0)
  devh workflow                печать этого канона из кода"#;

/// Запрещённые действия.
pub const FORBIDDEN: &str = r#"=== ЗАПРЕЩЕНО (R1/R2/R9/R13) ===
  ✗ cargo mutants / cargo-mutants руками        → только devh mutate
  ✗ bash devharness/test.sh руками              → только devh test
  ✗ внешний watcher/sleep-цикл/фоновый скрипт   → только devh mutate watch
  ✗ ad-hoc разбор логов (grep/awk по outcomes)  → devh mutate problems / status --json
  ✗ git add/commit/push/rebase/reset/checkout   → devh commit / devh merge
  ✗ правка артефактов аудита руками             → результат обнуляется
  ✗ тяжёлые прогоны параллельно и при перегруженном CPU → сначала свежая проверка нагрузки
  ✗ убивать процессы соседнего агента            → ждать или останавливать ТОЛЬКО свой прогон"#;

/// Нагрузка и соседи.
pub const SAFETY: &str = r#"=== БЕЗОПАСНОСТЬ (соседние агенты) ===
  Тяжёлые проверки (мутации, полный сьют) — последовательно, не параллельно.
  Перед тяжёлым прогоном — свежая проверка нагрузки (не устаревший снимок).
  Процессы соседнего агента НЕ убивать: ждать или останавливать только свой прогон.
  Порог нагрузки: HARNESS_MUTATE_MAX_LOAD (по умолчанию max(4, 2*CPU)), off — отключить."#;

/// Печать канона.
pub fn workflow() {
    println!("{SOURCE_ORDER}");
    println!();
    println!("{EXIT_CODES}");
    println!();
    println!("{RULES}");
    println!();
    println!("{STATES}");
    println!();
    println!("{COMMANDS}");
    println!();
    println!("{FORBIDDEN}");
    println!();
    println!("{SAFETY}");
    println!();
    println!("{}", mutate::CANON);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_mentions_all_rules() {
        for r in [
            "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "R13", "R14",
        ] {
            assert!(RULES.contains(r), "канон не упоминает {r}");
        }
    }

    #[test]
    fn canon_mentions_all_states() {
        for s in [
            "S0", "S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10", "S11",
        ] {
            assert!(STATES.contains(s), "канон не упоминает {s}");
        }
    }

    #[test]
    fn canon_forbids_manual_cargo_mutants() {
        assert!(FORBIDDEN.contains("cargo mutants"), "{FORBIDDEN}");
        assert!(COMMANDS.contains("devh mutate"), "{COMMANDS}");
        assert!(COMMANDS.contains("devh test"), "{COMMANDS}");
        assert!(COMMANDS.contains("devh next"), "{COMMANDS}");
    }

    #[test]
    fn exit_codes_are_canonical() {
        assert!(EXIT_CODES.contains("0 — успех"));
        assert!(EXIT_CODES.contains("1 — actionable"));
        assert!(EXIT_CODES.contains("2 — среда"));
    }
}
