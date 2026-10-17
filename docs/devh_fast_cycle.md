# План: максимально эффективный мутационный цикл внутри devharness

Создано 2026-10-17. Все числа — замеры на этой машине (2 ядра / 4 потока, ~7.5 ГБ RAM, часто с контенцией параллельной сессии).

## 0. Замеренная база (что реально происходит сегодня)

| Факт | Значение | Источник |
|------|----------|----------|
| devh делает **worktree на задачу**: `WT_ROOT/devh-<slug(root)>-<W-xxx>` | каждый новый id = новый абсолютный путь | `src/shell/worktree.rs:17` (`path_of`) |
| Новый путь = cold-build всего зависимого графа | `-p uwa-extract --tests` в новом пути: **19 мин**, sccache hit 85% → **34%** | A/B 2026-10-17 (§8.7 rust_build.md) |
| `mutate_gate` запускает MUTATE **отдельно на каждый изменённый файл** (`{file}`) | 3 файла = 3 полноценных прогона cargo-mutants, каждый со своим baseline+cold build | `src/commands/run.rs:831-880` |
| **Парсер вывода не понимает cargo-mutants** — devh ищет `Mutants: N, Killed: M`, а cargo-mutants 27.1.0 печатает `10 mutants tested in 10s: 1 missed, 9 caught` | мутационный гейт **всегда** даёт `MutateOutcome::Env` = «пропущен», kill-rate ≥90% не проверяется ни для кого | мини-репро `/tmp/mm`, замер; код `src/core/gates.rs:65-72` |
| devh-тесты: 68 тестов, 16.7s, из них sleep-поллингом ~10s | каждый мутант платит полные 14–17s | nextest log |
| target/ растёт: 1 ГБ на worktree-задачу | 29 ГБ после развития | `du -sh` |

**Итог по деньгам:** сегодня мутационный цикл = «3 × (30s холодный build + 17s baseline-прогон) + 3 × (1.8s × N мутантов + 14s тестов)» и гейт при этом **не работает** (Env).

---

## P0. Починить парсер вывода cargo-mutants (сначала — это баг, не оптимизация)

**Файл:** `devharness/src/core/gates.rs:65-72` (`parse_mutation_line`).

Добавить второй (основной) формат cargo-mutants 27.x:

```
N mutants tested in <time>: A caught, B missed[, C timeout][, D unviable]
```

Правила разбора:
- `total = N`, `killed = A` (caught), добавить `missed`/`timeout`/`unviable` в структуру, чтобы в лог писалось `Mutants N, killed A (P%), missed B, timeout C, unviable D`;
- старый формат `Mutants: N, Killed: M` (mix mutants, Elixir) сохранить как fallback;
- строка `Unmutated baseline` + `FAILED` = отдельный исход: **baseline красный → гейт обязан падать с понятной формулировкой**, а не молчаливым `Env`;
- выжившие мутанты тянуть из `mutants.out/missed.txt` и писать в `<id>-mutate.log` — это actionable-вывод для агента (`Gate::Mutate` текст в `prompt.rs:148` про «kill-rate < min_kill»).

**Цена/эффект:** гейт начинает реально работать. Следствие: задачи, чьи тесты были написаны «на проценталку», станут падать на гейте — это и есть цель. Плюс: сегодняшний факт «падает section25_auto» — baseline-гейт mutants'а refused to run, что devh показывал как `Env`.

**Окно в тестах:** `tests/integration.rs` — новый `section_NN_mutation_cargo_output` на юнит-уровне через `gates::parse_mutation_output` (как `fix_prompt_mutate_carries_min_kill` в `src/prompt.rs:258`).

---

## P1. Убить холостой cold-start (главный выигрыш по времени)

### 1a. Стабильный путь worktree вместо per-id

**Файл:** `src/shell/worktree.rs` — сейчас `path_of` = `WT_ROOT/devh-<slug>-<id>`.

Новый режим (env/config): при `HARNESS_WT_REUSE=1` (default для `devh run`, который последователен) путь **фиксированный**:

```rust
pub fn path_of(cfg: &RunConfig, root: &Path, id: &ItemId) -> PathBuf {
    if cfg.wt_reuse {
        cfg.wt_root.join(format!("devh-{}-active", slug(root)))   // один путь на все задачи
    } else {
        cfg.wt_root.join(format!("devh-{}-{}", slug(root), id.dir_name()))
    }
}
```

Смысл: путь = константа ⇒ incremental и sccache греются один раз, каждая следующая задача платит только за реально изменённые крейты. Замер на devh: первый прогон 130s → touch-пересборка 1.8s → no-op 0.35s.

`ensure` дополнить защитой: если в reuse-пути лежит **чужая/грязная** ветка (аварийный пережиток предыдущей задачи) — сброс через `git reset --hard`/`git clean -fdx` (уже есть похожая логика в `misc::clean`, позаимствовать).

`bg`/параллельные задачи (`src/commands/bg.rs`) остаются на per-id path — там параллелизм важнее, и цена документирована.

### 1b. Один запуск mutants на все изменённые файлы

**Файл:** `src/commands/run.rs:831-912` (`mutate_gate`) — сейчас цикл `for f in &scoped` вызывает `run_mut(wt, &cmd)` на файл.

Заменить на сборку одной команды со всеми файлами (cargo-mutants принимает несколько `-f`):

```rust
let cmd = m.tool.replace("{files}", &scoped.iter()
        .map(|f| format!("-f \"{f}\""))
        .collect::<Vec<_>>()
        .join(" "));
```

(оставить `{file}`/`{module}` для обратной совместимости с Elixir-шаблонами).

Эффект: N файлов = **один** baseline + один warm-target вместо N холодных.

### 1c. Параметры запуска (шаблон в конфиге + mutants.toml)

ШАГ БЕЗ КОДА — правится в `.harness/config`:

```
MUTATE='cargo mutants --in-place --test-tool nextest --baseline=skip --jobs 2 --timeout-multiplier 2 -f "{files}" -- --filter-expr ...'
```

Разбор флагов по замеру:
- `--in-place` — без него cargo-mutants **копирует дерево в новый путь** и платит полный cold-build (замер: 30s build + 17s baseline); с ним cold-start = 0s и инкремент живёт между мутантами. В devh-worktree `--in-place` безопасен: дерево = чистый git-worktree, mutants откатывает по git;
- `--baseline=skip` — devh уже прогнал VERIFY-гейт (`cargo test`) в этом же дереве перед мутациями (`run.rs:348` порядок гейтов), второй baseline-прогон — 17s впустую;
- `--test-tool nextest` — process-per-test, точные timeout'ы, `--jobs 2` (больше на 7.5 ГБ с чужой сборкой = риск свопа);
- `--timeout-multiplier 2` — мутанты иногда медленнее baseline'а, не плодить ложные TIMEOUT.

Плюс `.cargo/mutants.toml` (формат ключей подтверждён `strings $(which cargo-mutants)`, версия 27.1.0):

```toml
exclude_globs = ["docs/**", "target/**", "dist/**", "tests/**"]
timeout_multiplier = 2.0
test_tool = "nextest"
minimum_test_timeout = 20
additional_cargo_args = ["--cap-lints"]     # опционально: убирает lint-fail у мутантов
```

Важно: файл конфига тут называется `.cargo/mutants.toml` (cargo-mutants читает его из корня workspace).

---

## P2. Быстрые тесты devh (Notify вместо sleep-поллинга)

Сейчас 16.7s из 68 тестов, топ: `section00_shellcheck_oracle_files` 9.7s (внешний процесс), `section21_bg_ps` 7.3s, `section21d_bg_auto` 6.9s, `section21c_auto_pid_ps` 6.9s, `section09_run_lock_blocks_merge` 3.9s, `section07_watchdog_timeout` 2.5s, `timing::tests::elapsed_counts_real_seconds` 2.2s.

1. `src/shell/proc.rs`, `src/shell/lock.rs`, `src/core/state.rs` — заменить циклы `sleep` + poll на `tokio::sync::Notify` (watchdog/lock/bg-состояния). Прод-эффект: мгновенная реакция watcher'ов, а не тик 100–300 мс.
2. `tests/integration.rs` `section21*`, `section09`, `section07`, `section25` — ждать события (Notify/hook), а не опрашивать файлы; где нельзя (shellcheck-оракул) — вынести в отдельный процесс и оставить медленным.
3. Шелл-оракул: `section00_shellcheck_oracle_files` гоняет внешний `shellcheck` на каждый файл фикстур — кешировать результат по hash файла (`.harness/cache/shellcheck-<hash>`), экономия ~9.7s.

Ожидаемо: baseline-прогон 16.7s → **5–7s**, т.е. ≈2.5x на КАЖДОМ мутанте.

---

## P3. Контроль роста target (обратная сторона incremental)

- `devh clean`/`devh sweep` (`src/commands/misc.rs`): добавить подкоманду `sweep` = `cargo sweep --maxsize 20G` в главном дереве + удаление завершённых worktree'ов вместе с их `target/` (по 1 ГБ каждый);
- `devh doctor` (`src/commands/doctor.rs`): новые проверки — версия cargo-mutants ≥25, наличие `.cargo/mutants.toml`, «target тёплый» (наличие `target/debug/incremental/devh-*`), `git worktree list` без осиротевших путей, sccache hit-rate под порогом;
- При `wt_reuse=1` (P1a) архивация worktree-target'ов перестаёт расти линейно по числу задач.

---

## P4. Измеримость (чтобы ловить регрессии)

- `timing.stage` уже пишет стадии — добавить в `mutate_gate` стадии `mutate:build`, `mutate:test`, и запись `perf.log` строки: `task, mutants, killed, missed, build_s, test_s, total_s`;
- `devh bench` (или флаг `devh run --bench`): один холодный прогон цикла + baseline, чтобы сравнивать «до/после» на числах, а не на ощущениях.

---

## Порядок выполнения и оценка

| # | Шаг | Эффект (замер) | Цена/риск |
|---|-----|----------------|-----------|
| 1 | P0 парсер cargo-mutants | гейт вообще начинает работать (сейчас пропускается) | часть задач начнёт падать на kill-rate — это правильно; правка 1 файл + тест |
| 2 | P1c `.harness/config` + `.cargo/mutants.toml` | холостой baseline-прогон и холодный copy-build уходят (17s + 30s на запуск) | без кода, правка конфигов; требует зелёного baseline → ждать параллельную сессию (devharness редактируется сейчас) |
| 3 | P1a стабильный путь worktree | 19-мин класс cold-build → 1.8s на крейт (второй и далее задачи) | риск: reuse-путь должен корректно сбрасываться; `bg`-параллель остаётся на per-id |
| 4 | P1b один запуск на все файлы | N файлов: 3×17s → 1×17s | правка `run.rs`, тест на боевом логе |
| 5 | P2 Notify-тесты | 16.7s → ~6s на мутант (×2.7) | переписывание прод-watcher'ов, меняется семантика «ожидания» — тесты надо аккуратно |
| 6 | P3 sweep/doctor | контроль диска (29 ГБ → управляемо) | подкоманды devh |
| 7 | P4 perf.log | регрессии ловятся числами | мелочь |

## Что делаю следующим шагом (по запросу)
P0 (парсер) — самый дешёвый и самый полезный: без него прочие оптимизации ускоряют **выключенный** гейт.

---

## Статус выполнения (2026-10-17, ветка `devh/W-041`)

Все шаги P0–P4 выполнены. Работа — в worktree `/tmp/devh-909045283-W-041`, коммитами на `devh/W-041` (7 коммитов после `f1ebc66`).

| Шаг | Состояние | Коммит | Факт/замер |
|-----|-----------|--------|------------|
| P0 парсер cargo-mutants | готово | `bb877b9` | гейт понимает `10 mutants tested in 10s: 1 missed, 9 caught` + старый muex-формат как fallback |
| P1a reuse-worktree | готово | `b437dae` | стабильный путь `WT_ROOT/devh-<slug>-active`; `ensure` сбрасывает чужую/грязную ветку. **Инвариант теплоты P1a**: reuse-путь НЕ удаляется — `merge` возвращает его на main через `release_active` + `git clean -fd` (без `-x`, т.п. `target/` сохраняется). Баг «merge удалял reuse-путь ⇒ каждая задача платила cold-build» найден и исправлен при написании `section29` (`814ee70`) |
| P1b один запуск mutants на все файлы | готово | `8e47f1a` | `{files}` → `-f "a" -f "b"` одним прогоном; `{file}`/`{module}` сохранены для Elixir-шаблонов |
| P1c `.cargo/mutants.toml` + шаблон | готово | `b26867b` | batch-шаблон `{files}` в конфиге; doctor теперь проверяет версию cargo-mutants ≥25 и наличие `.cargo/mutants.toml` |
| P2 быстрые тесты | готово | `237ea7e` | см. ниже — предпосылка плана («Notify вместо sleep-поллинга») оказалась устаревшей, реальный байт — sleep-оракулы в тестах |
| P3 sweep/doctor | готово | `814ee70` | `devh sweep` (проверен боевом: снял 3 реальных сироты `/tmp/devh-1428311232-W-039`, `/tmp/s10`, `/tmp/s10v2`), `devh doctor` +5 проверок (проверен: cargo-mutants 27.1.0 ok, sccache 53% ok, 3 сироты WARN) |
| P4 perf.log | готово | — | `perf_mutate` пишет `logs/perf.log` (`run.rs:1044`); стадии `mutate:build`/`mutate:test` уже пишет `timing.stage` |

### P2 — что реально ускорило (замеры этой машины, контенция load≈24)

План предполагал `Notify` в прод-watcher'ах. Проверка показала, что единственный src-side sleep — watchdog в `proc.rs:85` (50 мс), он не на критическом пути — предпосылка устарела. Что было сделано:

- **shellcheck-оракул: content-hash verdict cache.** Замер cost-профиля развеял две гипотезы плана: батчинг (single_ms=18482) и параллель (parallel5_ms=15146 против sequential_ms=16877) не помогают — цена в ~2 с **анализа** на файл, не в старте процесса. Помогает кэш: FNV-1a по (байты файла ++ `shellcheck --version`) → маркер `.ok`/`.fail` в `target/.shellcheck-cache` (gitignored, умирает с `cargo clean`). Холодно ~16.7 с → тепло **0.03 с**. Оракул сохранён: на подсунутой SC2034/SC2154/SC2086 в `test.sh` кэш честно падает (файл восстановлен).
- **Event-wait вместо секундных тиков в тестах:** poll `run.pid`/`auto.pid` тиками 50–100 мс вместо фиксированных 1 с; `VERIFY 'sleep 6; test -f done.txt'` → `sleep 2` (3 места); flock-холдер в `section09` 3 с → 1 с.

| Тест | До | После |
|------|-----|-------|
| `section00_shellcheck_oracle_files` | 9.7 с (холодно) | **0.03 с** (тепло) |
| `section21_bg_ps` | 7.70 с | 3.57 с |
| `section21c_auto_pid_ps` | 7.43 с | 3.61 с |
| `section21d_bg_auto` | 7.53 с | 3.69 с |
| `section09_run_lock_blocks_merge` | 3.61 с | 2.55 с |
| весь integration-сьют | ~16.7 с | **16.1 с** тепло (34 теста), + 59 юнит за 2.9 с |

Итог: сьют 34 + 59 зелёный, `cargo fmt -p devh` чисто, `cargo clippy -p devh --all-targets` — 0 предупреждений.

### Флаги плана, закрытые иначе
- Пункт 7 порядка («P1c требует зелёного baseline → ждать параллельную сессию»): конфиги правились в worktree, ждать не пришлось.
- Пункт порядка 5: переписывание прод-watcher'ов не потребовалось — семантика ожидания в прод не менялась.
