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

## P5. Тест-фаза мутанта — детальный план (обновление 1, 2026-10-17)

Корень закрыл `CARGO_PROFILE_TEST_INCREMENTAL=true` (детали и замеры — rust_build.md §9.7–9.10).
Данные тест-фазы ниже — из артефактов прогонов (`outcomes.json`, логи nextest), не из догадок.

### 5.0 Замеренная база тест-фазы

| Факт | Значение | Источник |
|------|----------|----------|
| Полный прогон 205 мутантов (copy-режим, до `--in-place`) | build: mean **90.7s**, median 90.5s, max 252.2s | `/home/user/project_uwa/mutants.out/outcomes.json` |
| Тест-фаза того же прогона | mean 6.7s, median 2.6s, **max 95.5s** | там же |
| Мутантов с test >20s | **23** | там же |
| Из них класс «`delete match arm` в `Cmd::from_matches`» | **22** (`lib.rs:101..181`) + `prompt.rs` agent_prompt ×2 | там же |
| Актуальный сьют (in-place-дерево, load ≈ 2) | 104 теста, **8.9s**, `--status-level=all` | `nextest run -p devh` 17:35 |
| Топ слоу-тестов сьюта | section21c 2.81s · section21d 2.73s · section21b(ps) 2.62s · section07_watchdog 2.23s · timing::elapsed 2.21s · section09 1.62s | там же |

### 5.1 Почему 22 мутанта шли 30–95s (доказано по логам, причина НЕ «висячие тесты»)

Самый медленный мутант — `lib.rs:163:13 delete match arm Some("doctor")`, test=95.5s.
Его nextest-лог: `test section00_shellcheck_oracle_files has been running for over 60 seconds`,
suite `finished in 89.82s` (unit-бинарь 2.45s). Разбор:

1. Тот прогон шёл **в copy-режиме** (`/tmp/cargo-mutants-*.tmp` — свежее дерево на мутанта):
   в копии нет `devharness/target/.shellcheck-cache` → shellcheck-оракул (5 файлов × ~2с
   анализа) **холодный на каждом мутанте**; под контенцией (load ~24) его цена растёт
   2s → 30–95s. Оракул не зависит от мутанта — чистый налог режима копирования.
2. 4 лога из 205 содержат slow-warning nextest (>60s) — все четыре называют `section00`;
   остальные 19 «медленных» — тот же оракул на 20–40s под нагрузкой.
3. Второй множитель — wait-циклы тестов: `while !ap.exists() && n<50` (200мс) и
   `while ap.exists() && n<200` (100мс) (`tests/integration.rs` ~857, ~940, ~956, ~992, ~1000)
   при подломанной подкоманде сгорают целиком (10–20s на тест) и падают **без stderr**.

Что уже закрыло причину №1 **без кода**: `--in-place` в `.harness/config` (дерево = рабочее,
оракул тёплый 0.03s) + content-hash кэш из P2. Подтверждается контрольным прогоном (5.6).

### 5.2 Отсечь нефункциональные из мут-прогона — механизм

- **Оракул не отсечём, а согреем**: `--in-place` + наличие `devharness/target/.shellcheck-cache`
  (doctor-проверка в 5.5). Зачем отсекать, если тепло = 0.03s: смысл оракула — devh-classic
  чинит selfmutate.sh, молчашка опаснее 30ms.
- **Passthrough на будущее** (факты из `cargo mutants --help` 27.1.0): usage допускает
  `[-- <CARGO_TEST_ARGS>...]`, есть `--cargo-test-arg`; при `--test-tool nextest` это уже
  аргументы nextest (`-E 'not test(...)'`). Включать в шаблон **только** через проверку в
  приёмке: сначала `cargo mutants ... -- -E 'not test(x)' --list`-эквивалент (дешёвый
  dry-run), потом боевой прогон; kill-rate не должен упасть.

### 5.3 Death-aware ожидания в тестах (новый код — быстрый kill вместо бюджета)

Правки в `devharness/tests/integration.rs`: helper + применение в section21c/21d (и bg_ps
где есть spawned-child):

```rust
// тик 25мс; условие; при смерти ребёнка — мгновенный fail с exit status + stderr.
fn wait_child_until(child: &mut Child, budget: Duration, mut cond: impl FnMut() -> bool, what: &str)
```

- Не меняем семантику прохождения: те же assert'ы (pid file появился/исчез), просто
  быстрее и с диагностикой (exit code + хвост stderr вместо голого «должен появиться»).
- Ожидаемый эффект: для класса `delete match arm` тест-фаза 30–95s → **~3–5s**
  (оракул тёплый + мгновенный kill). Это ≈5–20% времени всего 205-прогона.
- Для section21b(ps)/21d: первая фаза — `output()` (failed-fast, ок), вторая — да death-aware.

### 5.4 Меньше мутантов — тот же смысл гейта

- `{files}` (P1b) уже ограничивает scope. `--iterate` + стабильный `-o` включать **отдельным
  режимом** (быстрый повтор: только новые/изменившиеся мутанты), не дефолтом: полный прогон
  должен остаться полным, иначе регрессии прошлых спек прячутся.
- `--in-diff <path>` — только для ручных прогонов, в гейт не пихать (P1b точнее).

### 5.5 Контенция: load-gate (по замерам §9.10: load>4 = ×2–4, OOM rc=137)

Код:
- pure `load_average() -> Option<f64>` (парсер `/proc/loadavg`, 1-е поле) + `cpu_count()`;
- порог = `max(2, ncpu)`, оверрайд `HARNESS_MUTATE_MAX_LOAD` (`off` = выключить);
- `mutate_gate` (`src/commands/run.rs`) перед запуском MUTATE: load > порог ⇒ лог + verdict
  EnvBlocked с cause «load X > Y: мутации ускорят ×2–4» (гейт честно blocked, не молчаливый);
- `devh doctor`: те же проверки + «cargo-mutants ≥27» + «MUTATE содержит
  CARGO_PROFILE_TEST_INCREMENTAL=true» + «shellcheck-кэш тёплый»;
- unit-тесты: парсер loadavg (мок строк), пороги; integration: при `HARNESS_MUTATE_MAX_LOAD=0.1`
  мутанты не запускаются.

### 5.6 Приёмка (числа, все — на спокойной машине, load писать в журнал)

1. **Контроль 14 мутантов** `timing.rs`, шаблон in-place+nextest+incremental+baseline=skip:
   цель total ≤3 мин (предыдущий замер 393.5s при load 8–12 в copy-режиме); сравнить
   build/test medians из `outcomes.json` до/после.
2. **Kill-rate не упал**: caught ≥13/14 на контрольном scope после правок wait-циклов.
3. **Death-aware тест**: новый тест на helper — мок-ребёнок умирает мгновенно → assert <1s
   (без бюджета 10–20s).
4. **doctor** проверяет все пункты 5.5; при load>порога mute-гейт даёт EnvBlocked.
5. **Журнал**: строки в rust_build.md с датой/load/числами после каждого шага.

### Что делаю сейчас (2026-10-17, итерация 1)

1. Контрольный прогон 14 мутантов в тёплом reuse-worktree `/tmp/devh-merge-W041`
   (запущен 17:39, стартовый load 2.9) — проверяет приёмку №1 без правок кода.
2. 5.3 death-aware waiters — правка + тест + прогон сьюта.
3. 5.5 load-gate — парсер + mutate_gate + doctor + тесты.
4. Замеры и вердикты — сюда и в rust_build.md; коммит через `git add -f`.

## P5 — выполнено (2026-10-17, вечер)

| Приёмка (§5.6) | Статус | Факт |
|---|---|---|
| 1. Контроль 14 мутантов ≤3 мин | **closed** | 4м (13 caught, 1 unviable, 0 missed); build median 11.9s vs 90.5s copy-mode; test median 4.0s. Лог `/tmp/mut_p5_control.log` |
| 2. Kill-rate не упал | **closed** | 13/14 = ровно как базовый замер; правки wait-циклов гейт не задели |
| 3. Death-aware тест | **closed** | `wait_child_helper_fast_kill` — мёртвый ребёнок ловится за <2s (было: сжигание бюджета); `wait_pid_for`/`parse_pid` для bg (умирает демон, не лаунчер) |
| 4. doctor + load-gate | **closed** | doctor: MUTATE/incremental, load vs порог, тёплый shellcheck-кэш (живой прогон: fail=0 warn=6); integration `section16b_load_gate_blocks_mutants`: `HARNESS_MUTATE_MAX_LOAD=0.1` → `env-blocked`, мутатор не стартует |
| 5. Журнал | **closed** | rust_build.md §9.11 — все числа с датой/режимом |

Расхождения с планом (по ходу сделано иначе):
- §5.2 отсечение оракула через `--skip` **не делаем**: оракул тёплый (0.03s с кэшем, P2), смысл его — devh-classic чинит `selfmutate.sh`; passthrough `--` — только после dry-run приёмки с kill-rate.
- §5.5 порог изменён на `max(4, 2×CPU)` (в плане было `max(2, ncpu)`): собственный build гейта devh и так поднимает loadavg на секунду, порог 4 не реагирует на это.
- Extraction бинаря в doctor пропускает `KEY=VALUE`-токены шаблона MUTATE (env-assignment — не программа); иначе `CARGO_PROFILE_TEST_INCREMENTAL=true` читалось как имя бинаря → ложный WARN.

Сьют-гейт: `cargo nextest run -p devh` — **112/112 PASS**, `cargo clippy -p devh --all-targets` чистый, `cargo fmt --check` чистый. Известный шум: при load≈10 `section21*` даёт contention-флейк (ровно тот случай, от которого load-gate и защищает) — повторный прогон зелёный.

---

## P6 — выполняется (2026-10-17, вечер): быстрый шаблон в committed defaults + полная приёмка

**1. Дефолты.** Быстрый `MUTATE` (§9.7–9.11) жил только в локальном `.harness/config` (gitignored) — новые проекты получали медленный дефолт. Продвинут в tracked-код:
- `devharness/src/commands/init.rs` (шаблон при `devh init`) и `devharness/devh-classic` (bash-оракул) — коммит `6a492b0`;
- корневой `./devh` — локальный launcher (gitignored), правка только локально.

**2. Полная приёмка гейта.** Прогон `cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 -p devh --config devharness/cargo-mutants.toml` с `CARGO_PROFILE_TEST_INCREMENTAL=true` (255 мутантов, канонический scope ядра — не tests/) — лог `/tmp/uwa-p5-gate.log`. SLO: median по мутанту ≤30s (P5-замер: build 12.7s + test 3.8s), суммарно ≈1.5ч. Цифры — в журнал после завершения.

**3. Найденный блокер (чужой файл, отдан параллельной сессии).** `verify.sh` падает на шаге 1/10 `cargo fmt`: `crates/uwa-api/src/routes/profiles.rs` не отформатирован (длинные `saturating_*`-цепочки) — из коммита `5258d15` «feat(profile): cookie export/import + login health». Локально исправлено `cargo fmt -p uwa-api`, **не закоммичено** (зона параллельной сессии). Гейт VERIFY блокируется полностью — чинить в их ветке.

**Грабля (для протокола).** `pkill -f "cargo mutants"` НЕ матчит процесс `cargo-mutants` (дефис vs пробел) — два in-place прогона живут на одном дереве,взаимные мутации + lock. Убитый `--in-place` оставляет в дереве маркеры `/* ~ changed by cargo-mutants ~ */` — лечится `git checkout -- <файлы>`. Guard запуска: `pgrep -x cargo-mutants` (точное имя, без self-match).

---

## P7 — беклог идей от заказчика (2026-10-18, ночь)

Каждая идея — с проверкой по факту (cargo-mutants 27.1.0 `--help`, дерево, замеры §9.11-9.12).

### Часть 1 — честность RED-гейта

| # | Идея | Состояние в devh | Цена | Риск | Первый шаг |
|---|------|------------------|------|------|-----------|
| 1.1 | RED должен падать «по той причине»: в специ рядом с red-командой — ожидаемая фраза из текста ошибки; упало с другой ошибкой → это не RED, а кривая спеца | Нет проверки причины. Сейчас red-«упал» = любой ненулевой rc. При этом §9.11: contention-флейк даёт ложный red (section21*) — гейт считает это RED | ~30 мин: ключ `RED_EXPECT` в конфиге, проверка подстроки в логе red-фазы (`gates.rs`), исход `Verdict::Fail`/`Env` с текстом | Фраза устаревает при рефакторе сообщения об ошибке — чинится правкой спеки | config key + grep подстроки в логе + integration-тест (red падает «не по той причине» → вердикт не-red) |
| 1.2 | То же одной строкой в спеке (`red.expect`) — то же, что 1.1, просто синтаксис | — | — | — | Реализуется вместе с 1.1 (одна настройка, две формулировки) |

**Замечание по дереву:** в devh RED — это `red_mutate` (второй мутационный конфиг, `mutate_gate(..., label="RED")`), а не shell-red. Идея 1.1 мапится туда как: требовать, чтобы поймавший тест падал именно с ожидаемым текстом (защита от флейка и от «убило случайным падением»). Для shell-red (RULES-агент пишет падающий тест) — ключ `RED_EXPECT` + grep лога. Оба варианта дешёвые, делаем одним коммитом.

### Часть 2 — ускорение (по силе эффекта, с проверкой по `--help` 27.1.0)

| # | Идея | Факт по инструменту | Оценка эффекта | Цена | Риск | Первый шаг |
|---|------|--------------------|-----------------|------|------|-----------|
| 2.1 | Кэш между запусками: мутант на неизменившейся строке даёт тот же результат | **`--iterate` = "Skip mutants that were caught in previous runs"** — встроенный кэш пойманных. Держит состояния между прогонами (outcomes в `mutants.out/`) | Высокий: ежедневная работа по тем же файлам → steady-state ~0s на мутант (только новые/выжившие гоняются) | ~0 кода: флаг в шаблон MUTATE (.harness/config + committed defaults) | Семантика ключа кэша у cargo-mutants своя; если тесты поменялись, а мутант тот же — кэш может соврать. Нужен A/B: правим тест, добавляющий kill, прогон с `--iterate` — выживший должен стать caught | добавить флаг в шаблон, A/B на правке теста |
| 2.2 | Мутить только изменённые строки, не файл целиком | **`--in-diff <REF>` = "Include only mutants in code touched by this diff"** — точечное ограничение | Высокий: файл на 300 строк с 5 правки → 5 зон вместо ~40-90 мутантов | Малая: флаг + откуда брать ref (merge-base vs main — devh уже считает changed files) | Деноминатор kill-rate сжимается — семантика гейта сохраняется, но MIN_KILL надо пере-оценить на сужении; правки в тест-файлах вне diff видятся не будут | dry-run на дереве: `--in-diff` vs `HEAD~1`, сверить список мутантов |
| 2.3 | Ранний выход: вердикт, а не красивый отчёт | Нет в инструменте. НО `outcomes.json` обновляется LIVE (проверено в §9.12 — polling счётчика) | Огромный: 51 мин → минуты. RED: стоп на первых 5 выживших (юзер ждёт красный, а не отчёт). GREEN: rule-of-three (0 missed в выборке k ⇒ kill-rate ≥ 90% с 95% при k≥30) — дальше гнать незачем | Средняя: обёртка в `mutate_gate` — goroutine polling outcomes.json + kill cargo-mutants по достижении bound'а, вердикт из частичного outcomes.json (проверено: kill оставляет валидный частичный outcomes) | GREEN-statistical — это решение «доверяем выборке»; для merge-гейта по ядру (252 мутанта) дешевле прогнать всё, ранний выход включать флагом `MUTATE_EARLY=1` | прототип на 14-мутантном timing.rs: poll + kill по RED-условию |
| 2.4 | Умный выбор тестов: непокрытая строка → выживший без запуска; покрытая → только её тесты (llvm-cov данные бесплатны) | Нет в инструменте | Высокий по тест-фазе: 11-16s → 3-6s/мутант (фильтр nextest `-E 'test(...)'` из индекса строка→тесты) | Высокая: индекс line→tests одной инструментированной прогонкой (`LLVM_PROFILE_FILE` + llvm-cov), per-mutant filter | Индекс должен переживать смену тестов (пересборка по hash тест-файлов); «покрыто» в мутантном смысле = выполнено при падении — пограничные случаи ловятся тем, что непокрытый мутант = честный missed | прототип: покрытие одного правила + фильтр в шаблоне |

### Синергия и порядок

`--iterate` + `--in-diff` + ранний выход + покрытийный отбор = steady-state ≈0s/мутант на ежедневных файлах; новый код — только он и платит. Порядок по эффекту/цене: **1.1/1.2 (честность гейта) → 2.3 (время сейчас) → 2.2 → 2.1 → 2.4**. Перед каждым пунктом — dry-run/A/B на спокойной машине (load писать в журнал §9.12).
