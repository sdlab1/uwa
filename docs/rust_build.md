# Rust Build & Test Optimization Guide

Актуальное состояние на 2026-10-17. Рабочая машина: **2 физических ядра / 4 потока, ~7.5 ГБ RAM**, причём `target/` часто делится с параллельной сборкой другой сессии — из-за этого lock-wait и шумные замеры времени нормальны.

---

## Часть 1. Размер и скорость сборки

### История проблемы

`target/debug` раздувался до **58 ГБ**. Пожиратели:

- `target/debug/incremental/` — кеш инкрементальной компиляции растёт без сбора мусора (с `incremental = true` ограничитель — `cargo sweep --maxsize`);
- `target/debug/deps/` — полные DWARF-символы для **всех** зависимостей (tokio, axum, serde, regex, rustls, chromiumoxide, html5ever...);
- отсутствие оптимизации по размеру в debug-профиле.

### Профили в корневом `Cargo.toml`

**`[profile.dev]`** (пользовательские сборки `cargo build`):

| Параметр | Значение | Зачем |
|----------|----------|-------|
| `debug` | `1` (line-tables-only) | Бэктрейсы с file:line работают, символы в 5–10x меньше |
| `codegen-units` | `4` | Параллельность внутри крейта ≈ числу потоков; больше — только лишняя RAM |
| `incremental` | `true` | Пересборка крейта 16.4s → **0.34–1.8s** (§8.4). Плата: `incremental/` растёт (74 МБ на крейт devh) и не шарится через sccache; ограничитель — `cargo sweep --maxsize` |
| `opt-level` | `"s"` | Компактный бинарник; для отладочных прогонов CLI разницы нет |
| `dev.package."*".debug` | `0` | Зависимости не отлаживаем — главный выигрыш по размеру deps (60–80%) |

**`[profile.test]`** (сборка `cargo test` / `cargo nextest run`) — добавлено позже, отдельный блок:

| Параметр | Значение | Зачем |
|----------|----------|-------|
| `opt-level` | `1` | **Ключевое:** `[profile.dev.package."*"]` НЕ применяется к профилю `test` — до этого токio/serde_json/html5ever линковались с `-O0`, и все тесты шли неоптимизированным кодом. `-O1` даёт 2–5x ускорение рантайма парсинга HTML/JSON в тестах при почти той же скорости компиляции |
| `codegen-units` | `16` | На 2 ядрах мелкие unit'ы дают больше параллелизма внутри крейта → сборка быстрее; расплата — чуть более медленный машинный код, что для короткоживущих тестов неважно |
| `debug` | `"line-tables-only"` | file:line остаются в пане (незаменимо для диагностики), но артефакты в разы меньше, линковка ~62 бинарников заметно быстрее |
| `incremental` | `false` | Осознанно: suite-артефакты пересобирает каждым nextest-прогоном, инкремент только раздувал бы кеш. Быстрый цикл (cargo build --tests = dev-профиль) получает инкремент из `[profile.dev]` (§8.4) |

### Результат

| Метрика | До | После |
|---------|-----|-------|
| `target/debug` | 58 ГБ | ~1.1 ГБ |
| `target/debug/incremental` | 20–30 ГБ | ~100 МБ на активно собираемые крейты (74 МБ на devh) + ограничитель `cargo sweep --maxsize 20G` |
| Полная сборка (sccache-кэш тёплый) | ~5 мин | ~4.5 мин |
| Пересборка одного крейта | 30–60 сек | **0.34–1.8 сек** (incremental = true, §8.4); чистая cold-пересборка крейта всё ещё 10–60 с |

---

## Часть 2. Скорость тестов

### Было

875 тестов, **322 сек**, 17 падений. Причины: неоптимизированный рантайм, десятки лишних link-циклов, слепые `sleep` в тестах, иголка в стоге при поиске упавшего теста.

### Что сделано

**1. Мердж интеграционных тестов uwa-api (56 → 16 целей в пакете).**
Семь мелких файлов (`routing.rs`, `messages_count_tokens.rs`, `chat_pipeline.rs`, `profiles_stream.rs`, `multimodal.rs`, `ui.rs`, `selector_generate.rs`) слиты в один `crates/uwa-api/tests/endpoints_misc.rs` (703 строки). Каждый лишний интеграционный бинарник — это отдельный link на каждый из 62 бинарников workspace, а линковка на 2 ядрах — узкое место. Хелперы переименованы под столкновения (`server()`, `count_tokens_server()`, `server_with()`, `count()`), mid-file `//!` переделаны в `//`.

**2. Убраны слепые sleep'ы:**
- `crates/uwa-api/src/recorder.rs` — поллинг один раз **до** входа в интервальный ритм: первая партия событий на t≈0 вместо t=POLL_INTERVAL (снято ~900 мс с теста);
- `crates/uwa-lifecycle/src/shutdown.rs` — `#[tokio::test(start_paused = true)]` на таймер-тестах: виртуальные часы делают sleeps/timeouts мгновенными;
- `crates/uwa-api/tests/recorder.rs` — вместо 900-мс сна bounded poll-loop против `/admin/recorder/status` (deadline 2 сек, шаг 20 мс).

**3. cargo-nextest вместо libtest.** Process-per-test модель использует 2 ядра лучше in-binary threads (а на nextest каждый тест — отдельный процесс, падение одного не роняет бинарник). Конфиг `.config/nextest.toml`:
- `slow-timeout` 60s (default) / 120s (ci) с `terminate-after = 3` — повесившийся на порту тест убивается, а не залипает CI навсегда;
- в ci-профиле: `retries` (exponential, count=1 — flaky e2e с портами/браузером дешевле ретрайнуть, чем перегонять весь suite) и `fail-fast = { max-fail = 10 }`.

**4. Параллельные багфиксы, найденные на этом пути** (вне рамок скорости, но без них suite не зелёный):
- `crates/uwa-bin/src/commands/start.rs` — mismatch clap ID: arg объявлен `no_chromium`, читался `get_flag("no-chromium")` → panic → exit 101 без вывода;
- `crates/uwa-bin/src/main.rs` — кастомный panic-хук глушил все паники: теперь всегда печатает location + payload;
- `crates/uwa-bin/src/wiring.rs` — дефолт pid-файла `uwa.pid` в CWD → `paths::pid_file()` под `UWA_HOME` (нет коллизий инстансов и мусора в репо);
- `devharness/src/lib.rs` — `--max` был `ArgAction::Set` без `value_parser!(u32)`, паник на любом использовании;
- `crates/uwa-bin/tests/binary_smoke.rs` — переписан под текущий CLI (`-c` + `UWA_HOME`) + конфиг прибит к `kind = "cdp"` (дефолтный nodriver требует `--no-sandbox` в контейнерах); реальный Chromium-прогон: 4/4 за 12.1 сек под `UWA_CHROMIUM=1`, без флага — 0.00 сек.

### Стало

```
Starting 875 tests across 62 binaries (3 tests skipped)
     Summary [  69.554s] 875 tests run: 875 passed, 3 skipped
```

Спокойная машина: ~57 сек. 69.5 сек — под контенцией с параллельной сессией.

---

## Часть 3. Инструменты

| Тула | Для чего | Команда |
|------|----------|---------|
| **sccache** (`rustc-wrapper` в `~/.cargo/config.toml`) | Общий кеш компиляции между проектами | `sccache --show-stats`, `--stop-server` |
| **cargo-nextest** | Запуск тестов (process-per-test) | `cargo nextest run --workspace --no-fail-fast` |
| **cargo-sweep** | Чистка `target/` чужих/старых проектов | `cargo sweep --installed --time 30` |
| **cargo-bloat** | Что раздувает бинарник | `cargo bloat --release --all -n 20` |

sccache: кеш `~/.cache/sccache`, лимит 10 ГБ, гаснет через ~5 мин простоя (`SCCACHE_IDLE_TIMEOUT=300` в `[env]` конфига; после простоя первый `cargo` платит холодный старт сервера — это секунды).

---

## Часть 4. Глобальные rustflags — осторожно

В `~/.cargo/config.toml` глобально висят:

```toml
rustflags = ["-F", "clippy::unwrap_used", "-D", "clippy::expect_used", "-F", "clippy::panic", "-F", "clippy::todo", "-F", "clippy::unimplemented", "-F", "clippy::indexing_slicing", "-F", "clippy::arithmetic_side_effects"]
```

Идея правильная, механизм — нет:

1. `clippy::` линты в rustflags — **no-op под обычным `cargo build`** (rustc их игнорирует); они загораются только под `cargo clippy`. То есть 99% сборок «защищены» только самим фактом наличия флагов.
2. `-F` (forbid) запрещает переопределение: `#[allow]` не работает **даже в `#[cfg(test)]`** — именно из-за этого тестовый код вынужден обходить `unwrap`/`unreachable!()` костылями (см. коммит `1d44f0f`: `unwrap_or_else(unreachable)` → `match`).
3. Любое изменение флага = **полная пересборка всего + дивергенция hash'ей sccache** (rustflags входят в hash компиляции).

Правильное место — `[workspace.lints.clippy]` в корневом `Cargo.toml` с уровнем `deny`:

```toml
[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
indexing_slicing = "deny"
arithmetic_side_effects = "deny"
# + в крейтах: [lints] workspace = true
```

Цена перехода: одна полная пересборка (rustflags меняются один раз) + ручная простановка `[lints] workspace = true` по крейтам. Выигрыш: `#[allow(...)]` работает где надо (тесты, бенчи), линты больше не мусорятся в чужой кеш sccache.

---

## Часть 5. Команды для повседневной работы

```bash
# Тесты (так, как гоняет CI-профиль):
cargo nextest run --workspace --no-fail-fast
cargo nextest run --profile ci --workspace

# Один тест (синтаксис фильтра — ТОЛЬКО test(...), без '='):
cargo nextest run -p uwa-api -E 'test(streaming_reports_error_when_validation_never_passes)'

# Реальный Chromium-прогон (по умолчанию gated):
UWA_CHROMIUM=1 cargo nextest run -p uwa-bin --test binary_smoke

# Сборка CLI:
cargo build --bin uwa

# Статистика sccache / анализ размера:
sccache --show-stats
cargo bloat --release --all -n 20

# Чистка:
cargo sweep --installed --time 30 --dry-run
cargo clean -p <crate>     # точечно, без сноса всего target
cargo sweep --maxsize 20G    # ограничить рост target (срезка самых старых артефактов)
# Быстрый цикл мутаций devh (Часть 9):
cargo mutants --in-place --test-tool nextest -f devharness/src/ --list  # сколько мутантов
cargo mutants --in-place --test-tool nextest -f devharness/src/        # прогон в тёплом дереве
```

---

## Часть 6. Сценарии

**Активная разработка** (нужна пересборка крейта 0.3–1.8 сек):

```toml
[profile.dev]
debug = 1
incremental = true   # уже дефолт (§8.4): пересборка 16.4s → 0.34–1.8s
opt-level = 0        # быстрее компиляция
codegen-units = 4
```

> Цена: `target/debug` снова 5–10 ГБ, инкрементальный кеш не переживает `rustflags`/toolchain change и не шарится через sccache.

**CI / финальная сборка:**

```toml
[profile.release]
debug = 0
strip = true
lto = true
codegen-units = 1
```

---

## Часть 7. Что ещё можно ускорить (с ценой)

**Build:**

| Действие | Эффект | Цена |
|----------|--------|------|
| `[profile.dev] incremental = true` | rebuild 16.4s → **0.34–1.8s** (×9..×77) | ~74 МБ на крейт в target, кеш не шарится, сгорает при смене флагов — **сделано 2026-10-17** |
| `debug = 0` в dev | быстрее compile+link | теряем file:line в паниках (обратно проблемная диагностика) |
| `opt-level = 0` в dev | быстрее compile | больше бинарник |
| `codegen-units = 16..32` в dev | быстрее compile | чуть медленнее рантайм-бинарник, больше RAM на крейт |
| Линковщик `lld` — **проверено замерами, не рекомендовано на этом железе** (2026-10-17): lld 19.1.7 vs GNU ld, 66 одинаковых линковок: исполняемые тест-бинарники 45.98s → 79.43s (**в 1.73x медленнее**), ускорились только мелкие build-script-линковки (8.40s → 5.89s). Разброс машины (±70%) больше эффекта | пакет `lld`; смена rustflags = один full rebuild; при желании повторить: `sudo apt install -y lld` (ТОЛЬКО apt!), флаг линкера — в cargo, см. ниже |
| Песочница тестов на `codegen-units` dev | — | уже сделано под `[profile.test]` |

**Tests:**

| Действие | Эффект | Цена |
|----------|--------|------|
| Мерджить ещё интеграционные бинарники | −link-циклы на 2 ядрах | один файл на сотни строк; правка = перекомпиляция всего бинарника; растут риски столкновений имён |
| `start_paused = true` на таймер-тестах (circuit.rs, pinning.rs, stream.rs) | снять оставшиеся sleep-ы | виртуальное время: тесты с реальным I/O/Wall-clock повиснут, нужен ручной `advance` |
| Notify/channel вместо sleep-polling в `uwa-browser` transport/tabpool/page | −десятки мс на тест | правки в продакшен-путях, ревью поведения |
| Env-gate медленных e2e (как `UWA_CHROMIUM` у binary_smoke) | suite остаётся быстрым | тесты не идут по умолчанию → CI обязан выставить флаг |
| Feature-trim тяжёлых deps (chromiumoxide) | быстрее сборка всех | риск сломать CDP-путь |

---

## Часть 8. Лаборатория: журнал исследований (всё с замерами)

Все числа — на этой машине (2 ядра / 4 потока, ~7.5 ГБ RAM, часто с контенцией чужой сессии).

### 8.1 Тестовый профиль `[profile.test]`
Гипотеза: `[profile.dev.package."*"]` не действует на профиль `test`, зависимости токio/serde_json/html5ever шли с `-O0`. **Подтверждено.** Решение: `opt-level = 1`, `codegen-units = 16`, `debug = "line-tables-only"`, `incremental = false` в `[profile.test]`. Итог suite: 875 тестов, 17 падений/322с → **875 passed / 3 skipped, 57–70с**.

### 8.2 Мердж интеграционных тестов
Гипотеза: каждый интеграционный бинарник — отдельная линковка, на 2 ядрах это узкое место. 7 файлов uwa-api слиты в `tests/endpoints_misc.rs`. uwa-api: 22 → 16 целей.

### 8.3 Слепые sleep'ы в тестах
- recorder: поллинг до интервального ритма → первая партия на t≈0 (−900 мс);
- shutdown: `#[tokio::test(start_paused = true)]` — виртуальные часы;
- recorder status poll-loop с deadline 2с.

### 8.4 Инкрементальность: полный A/B на devh (2026-10-17)

| Сценарий | Время |
|----------|-------|
| `cargo build --tests -p devh` из состояния после чужих правок (штorm пересборки) | 115.4s |
| Пересборка после `touch src/lib.rs`, `incremental = false` (текущий дефолт) | 16.4s |
| То же + `CARGO_INCREMENTAL=1` | **0.34s** |
| Повтор с тёплым инкрементальным кэшем | 0.21s |

**Вывод: инкрементальность даёт ×77 на пересборку.** Важно: cargo-mutants строит через `cargo build --tests` = **dev-профиль** → рычаг именно `[profile.dev] incremental = true` (профиль `test` для suite можно оставить `false` — это разные сценарии).

**Подтверждение после включения в `[profile.dev]` (2026-10-17):**

| Сценарий | Время |
|----------|-------|
| `touch devharness/src/lib.rs` + `cargo build --tests -p devh` | **1.84s** (было 16.4s) |
| Повторный `cargo build --tests -p devh` без правок | **0.35s** |
| Размер `target/debug/incremental` после сборки devh | 74 МБ |

Замер в чужой контенции (lock wait) — поэтому 1.84s, а не 0.34s; на чистой машине 0.2–0.35s. `incremental` в `[profile.test]` сознательно оставлен `false`: suite-хранилище артефактов живёт под nextest-прогонами, инкремент там только растил бы кеш.

### 8.5 Линковщик lld
Гипотеза: lld ускорит линковку тест-бинарников. **Опровергнуто на этом железе:** см. «Замер линковщика» ниже. Побочная находка: смена ключа линкера/flags обнуляет fingerprint'ы и стоит полной пересборки (~12 мин).

### 8.6 Тихие смерти сборки
Два прогона умерли mid-build без ошибки в логе (RAM ~700 МБ free под чужой нагрузкой, вероятно OOM). Детект — маркеры START/END, отсутствие END = смерть.

### 8.7 Общий `target/` через симлинк между repo и worktree — **не работает** (2026-10-17)

Запрос: «сделать чтобы папка target в оригинальном репо и worktrees была одна и та же — символьный линк?» Проверено: и симлинк, и общий `CARGO_TARGET_DIR` дают **худший из миров** — полную пересборку при КАЖДОМ переключении.

Механика: fingerprint юнита и `dep-info` содержат **абсолютные пути исходников**. `/home/user/project_uwa/devharness` и `/tmp/ab-wt/devharness` — два разных ключа. Cargo не знает, что это одно и то же дерево.

| Сценарий (цель `target/debug` один и тот же) | Полная пересборка |
|-------------------------------------------------|-------------------|
| main → warm, потом билд в worktree **той же целью** | **2m 22s**, `Compiling devh (/tmp/ab-wt/devharness)` |
| возврат в main с той же целью | **16.4s** снова full, инкремент сброшен |
| инкрементальный мусор в общей цели (1 крейт, 2 корня) | 206 МБ (вместо 74 МБ) |

Побочная находка про **новый путь**: свежий приватный target в новом worktree (`-p uwa-extract --tests`) = **19 мин** 4с, 268 `Compiling`, sccache hit rate упал до **34.35%** (281 miss). Причина: `--extern`/`-L` в аргументах rustc содержат абсолютные пути к локальным rlib → **каждый крейт, зависящий от воркспейс-крейта, промахивается мимо sccache**. Следствие: не плодить новые пути без нужды.

**Рабочая политика:**
1. `target/` остаётся **внутри основного дерева** (gitignored, путь стабилен, пересборка 0.35s / touch 1.84s). Симлинков нет.
2. Скретч-worktree'ы (чужие агенты, `/tmp/...`) живут со **своим** target'ом — изоляция, цена = один полный cold-build на каждый новый путь.
3. Для быстрого цикла worktree не нужен: `cargo mutants --in-place` собирает в тёплом дереве (см. Часть 9).
4. Рост target'а контролируется `cargo sweep --maxsize 20G` (срезает самые старые артефакты) или `cargo sweep -t 7` (старше 7 дней); сейчас `target/` = 29 ГБ, диск 100/219 ГБ.
5. Переключение веток в основном дереве больше не требует worktree: с incremental=true смена ветки = ~1–2s на крейт вместо 16.4s.

---

## Часть 9. Быстрый цикл мутационного тестирования devh

Контекст замеров: `devh` = **ноль внешних зависимостей**, 68 тестов (7.3s — интеграционные, spawn'ящие процессы), suite 14.1s. Один мутант сегодня = **0.35–1.8s сборка** (incremental) + ~14s прогон + анализ.

### Рецепт (по приоритету)

**Важные уточнения (замеры 2026-10-17, вторая половина дня — см. §9.7):**

- **cargo-mutants строит НЕ через dev-профиль.** Фактический argv из `mutants.out/outcomes.json`: `cargo test --no-run --verbose --package=devh@0.1.0` — это профиль **`test`**, где у нас стоит `incremental = false`. Рычаг `[profile.dev] incremental = true` до мут-сборки **не доходит** — это и было корнем проблемы.
- **«devh = ноль внешних зависимостей» — устарело.** В `devharness/Cargo.toml`: clap, thiserror, anyhow, regex, fs2, libc (+dev tempfile). Пересборка мутанта по-прежнему = один крейт (deps закешированы как rlib), но «ноль deps» больше не аргумент.

**1. `[profile.dev] incremental = true` — ВКЛЮЧЕНО (2026-10-17).** Для обычного цикла `cargo build`. Для мутаций работает рычаг §9.7 (`CARGO_PROFILE_TEST_INCREMENTAL=1`). Цена мутанта 16.4s → **0.3–0.5s** (замер: 0.34s standalone, 1.84s в контенции). Цена: `target/debug/incremental` растёт (74 МБ на один крейт devh), сгорает при смене флагов/тулчейна, не шарится через sccache. Ограничитель — `cargo sweep --maxsize`.

**2. `cargo-mutants --in-place --test-tool nextest -j 3`** (версия 27.1.0 установлена: `cargo mutants --version`). Без `--in-place` cargo-mutants **копирует дерево в скретч-путь с холодным target** — срабатывает ровно тот же баг, что и симлинк (§8.7): новый абсолютный путь → все артефакты мимо кеша, 34% hit rate, полная пересборка на каждый мутант. С `--in-place` — сборка в тёплом дереве, дерево обязательно чистое (оно же и откат по git). ВАЖНО: каждый **новый** путь (copy-режим) обнуляет и инкремент, и sccache — держите мутационный цикл в одном постоянном пути.

**3. Скоуп:** `cargo mutants -f "devharness/src/**/*.rs"` — мутировать только логику harness'а, не CLI-обвязку (glob с `*` обязателен, см. замер ниже).

**4. Отсечь медленные нефункциональные тесты из прогона мутантов:** топ slow-тестов devh (nextest):
- `section00_shellcheck_oracle_files` — 9.7s (внешний shellcheck, не про мутации);
- `section21_bg_ps` 7.3s / `section21d_bg_auto` 6.9s / `section21c_auto_pid_ps` 6.9s — bg/ps, sleep-поллинг;
- `section09_run_lock_blocks_merge` 3.9s — lock-поллинг;
- `section07_watchdog_timeout` 2.5s.

Опции: (а) переписать поллинг на Notify-уведомления в devh shell-watchers (dev-loop тоже ускорится); (б) для прогонов мутантов — `cargo mutants -- --skip <regex>` отключить shellcheck-тест (он про фикстуры, не про логику). Виртуальные часы в `timing::tests::elapsed_counts_real_seconds` (2.2s) возможны, но меняют семантику «real seconds» — только если принять tolerance-переписывание.

**5. Параллелизм:** `-j 3..4` (мутанты независимы; 4 потока), но на 7.5 ГБ RAM с чужими сборками — аккуратно. В режиме `--in-place` мутанты переписывают одно дерево, поэтому большую параллельность даёт copy-режим (ценой холодного кеша — см. §8.7); для узкого scope (1–3 файла) in-place с `-j 1..2` полностью достаточно.

**6. `mutants.toml`:** `timeout_multiplier = 2`, `exclude_globs = ["docs/**", "target/**"]`, `test_tool = "nextest"` — nextest даёт process-per-test и точные slow-timeout'ы на повесившемся мутанте.

### Замер живого прогона (cargo-mutants 27.1.0, 2026-10-17)

| Что | Результат |
|-----|----------|
| `-f "devharness/src/timing.rs"` | **14 мутантов** |
| `-f "devharness/src/**/*.rs"` (весь src) | **727 мутантов** (glob обязателен: `devharness/src/` без `*` не матчит ничего) |
| Baseline в copy-режиме (`--test-tool nextest -j 2`) | **30s build + 17s test** на пустом target; build собрал ВСЕ dev-зависимости clap/syn/regex заново — в новом пути sccache тоже промахивается |
| Baseline в дереве с чужими правками | `FAILED Unmutated baseline in 30s build + 17s test` → `ERROR cargo test failed in an unmutated tree, so no mutants were tested` |

Два вывода из замера: (а) cargo-mutants **всегда** сначала гоняет базовый прогон — красный baseline блокирует мутационный анализ целиком (сейчас так и есть: падает `section25_auto` из-за правок параллельной сессии в `devharness/src/commands/run.rs` + `prompt.rs`); (б) copy-режим платит полный cold-build (30s) на ПЕРВЫЙ прогон, и каждый мутант внутри одного copy-dir уже получает инкремент — с `--in-place` этот cold-start исчезает полностью.

Скель по мутантам: 727 × (1.8s сборка + 14s прогон) ≈ 3.2 часа; с --in-place сужение прогона (--skip медленных test'ов из §9.4) → ~727 × 6s ≈ 73 мин. Поэтому scope имеет смысл сужать: `-f "devharness/src/core/**/*.rs"` / `-f "devharness/src/shell/**/*.rs"`.

### Ожидаемый эффект
Сейчас мутант = 1.8s build (incremental) + ~14s прогон, плюс разовый cold-start 30s в copy-режиме или 0s с --in-place. После п.1+2 (in-place, тёплый кеш): ~14с на мутант, cold-start = 0. После п.1+2+4а (убрать медленные sleep/shellcheck-тесты из прогона): ~4–6с на мутант. Мутационный прогон на десятки мутантов переходит из «часов» в «минуты».

### 9.7 Корень медленной мут-сборки: профиль `test` + `incremental=false` (замер 2026-10-17)

Симптом: «мутации съедают часы». Разбор лога `mutants.out/` (205 мутантов, прогон 13:57–15:21):

| Фаза мутанта | mean | median | max | доля цикла |
|---|---|---|---|---|
| Build (`cargo test --no-run`) | **90.80s** | 90.57s | 252.18s | **93%** (18613s суммарно) |
| Test | 6.58s | 2.61s | 95.46s | 7% (1348s) |
| Исходы | 177 caught, 28 unviable, 0 missed, 0 timeout | | | всего ~5.5 ч |

Три причины, все подтверждены артефактами:
1. **Профиль.** Фазы собирались как `cargo test --no-run --package=devh@0.1.0` → профиль `test`, а в нём `incremental = false` (осознанно, §8.1: инкремент раздувал бы кеш под nextest-прогонами). Каждый мутант = полная пересборка lib + тест-бинарников.
2. **Copy-режим.** Прогон шёл в скретч-пути `/tmp/cargo-mutants-project_uwa-*.tmp` — холодный target в НОВОМ абсолютном пути, ровно баг §8.7 (deps пересобираются на baseline, sccache hit-rate падает).
3. **Контенция.** В логах мутантов — `Blocking waiting for file lock on package cache`: параллельная сессия билдит в это же время, lock общий (`~/.cargo`). В контенции пересборка 16.4s → **463s**.

**A/B в тёплом дереве** (`/tmp/devh-merge-W041`, target гретый, команда = ровно та, что зовёт cargo-mutants):

| Сценарий (`touch gates.rs` → `cargo test --no-run -p devh`) | Время |
|---|---|
| Профиль `test`, как сейчас (`incremental=false`), при контенции | **462.9s** |
| То же на спокойной машине (замер соседней сессии) | **13.5–25s** |
| `CARGO_PROFILE_TEST_INCREMENTAL=true`, первый touch (прогрев кеша) | **47.2s** |
| Повторный touch, тёплый инкремент-кеш | **4.1s** |
| No-op (touch без правок): `0 Compiling` — только freshness-check sccache под load | 4.1s |

Механика: инкрементальные fingerprints **контентные** — `touch`/git-revert с тем же содержимым кеш не сбрасывают (проверено: C = 0.33s сразу после touch). Именно это даёт эффект «немутирующая часть уже скомпилирована»: пересобирается только мутированный крейт.

**Вывод:** сборка мутанта — 93% цикла, и она убирается одним env-переопределением без правки `Cargo.toml` (обычные прогоны не затронуты). Цена: `target/debug/incremental/` растёт (~74 МБ на крейт, как в dev-профиле) → ограничитель `cargo sweep --maxsize 20G`.

**Ловушка замера (поймана контрольным прогоном):** `CARGO_PROFILE_TEST_INCREMENTAL=1` не парсится (profile-override требует `true`/`false`) — cargo падает мгновенно, cargo-mutants помечает всех мутантов `unviable`, а «0.33s» в сыром A/B выглядели как успех. Всегда: сначала один мутант на `caught`, потом цифры.

**Контрольный прогон** (14 мутантов `timing.rs`, полный конфиг §9.9, дерево `/tmp/devh-merge-W041`, load 8-12):

| Фаза | median | max | сумма |
|---|---|---|---|
| build | **12.9s** | 151.9s (первый мутант: прогрев инкремент-кеша) | 337s |
| test (nextest) | **4.0s** | 5.3s | 55s |

Итог: **14 мутантов за 393.5s (6.6 мин): 13 caught, 1 unviable** — против 90.8s+6.6s ≈ 97s/мутант в логе `mutants.out` это **×3.4 по wall-time** (×7 по build). Остаток build (+8s к A/B's 4.1s) — патч/реверт cargo-mutants ломают mtime, load, sccache-пробуждение; для спокойной машины ожидается ~5s. Unviable=1 (5.5%) — норма для `Vec::leak`-мутантов, gate не гейт.

**Тот же A/B на простаивающей машине (17:07, load ~1) — уточнение рычага:**

| Сценарий (`cargo test --no-run -p devh`) | Время |
|---|---|
| touch, `incremental=false` (как было) | **11.8s** |
| Переключение профиля (false→true, первый билд — пересобирает юнит) | 26.1s (разово) |
| touch, `incremental=true`, steady-state | **2.2s** |
| Чистый suite (`nextest run -p devh`, без мутантов) | 13.4s |

Итог steady-state на спокойной машине: **2.2s build + ~13.4s suite (мутант гибнет быстрее — пойманный ≈4s) ≈ 6–16s/мутант против ~97s** — рычаг перевернул узкое место: build был 93%, стал ~15%; дальше растёт тест-фаза (её лечит `-- --skip` нефункциональных тестов §9.4 и Notify-переписывание П2 devh_fast_cycle.md, а не язык тестов).

### 9.8 Почему НЕ менять язык (C / bash / python / luajit / Rust-DSL) — запись для будущих сессий

Вопрос поднимался дважды: «скомпилировать Rust в C», «писать тесты на C», «взять bash/python/luajit — нам же не бинарник нужен, а результат». Разбор, чтобы не повторять:

**Различать два слоя:**
1. **Драйвер** (devh спавнит cargo). Его собственный ход — миллисекунды. 90 секунд — это rustc, а не драйвер. Перевод драйвера на bash/python/luajit двигает ~0% (bash-драйвер в репе уже есть как `devh-classic`; перевод на Rust был осознанным).
2. **Код под мутантой.** Чтобы узнать, проходит ли тест после мутации, мутированный код должен **исполниться**; для стека tokio/async исполнитель — только машкод. Интерпретатора Rust не существует (Miri — на порядки медленнее компиляции), LuaJIT Rust не интерпретирует, зов Rust из Lua требует extern-"C"-шайма = снова компилируемый артефакт.

**rustc → C:** существующие пути — mrustc (заброшен, ~Rust 1.19, нет async/tokio — весь наш стек), `-Z codegen-backend=c` (nightly-эксперимент, неполный, медленный кодоген), `rustc_codegen_cranelift` (nightly, быстрее кодоген но МЕДЛЕННЕЕ рантайм — а рантайм тестов важнее). Все ломают инкремент/sccache и добавляют шаг; главное — сегодня сборка дешева ИМЕННО благодаря кешу rlib-зависимостей, любой C-маршрут гоняет весь мир через gcc на каждый мутант. Промежуточный C не убирает работу, а добавляет.

**Тесты на C:** рантайм наших тестов — process-spawn'ы, порты, sleep-поллинг (§9.4) — в C те же секунды. FFI к Rust API (cbindgen + `cc` в build.rs + unsafe + UB на панике через границу + потеря tokio/axum-харнесса) — огромный налог. И главное: cargo-mutants мутирует Rust-src и гоняет cargo/nextest; C-бинарники вне этого цикла — kill-rate гейт их не видит.

**«Разбить тест на файлы, чтобы мутации были инкрементальными»:** модули одного крейта — один линк-юнит; тест-бинарник обязан перелинковаться с мутированной lib. Больше файлов в `tests/` = больше линковок, а линковка — узкое место на 2 ядрах (§8.5: 66 линковок = 46s). Отсюда же политика «мержить интеграционные файлы, а не дробить». Нужный эффект («немутирующая часть уже скомпилирована») даёт инкремент крейта (§9.7), а не нарезка файлов.

**Где скрипт-языки выигрывают по-настоящему:** когда мутируется сам скрипт. Репо уже эксплуатирует это: `devh-classic/selfmutate.sh` патчит bash-гейты awk'ом и гоняет suite — **0s сборки**, 21/21 kill-rate. Для Rust-крейтов аналог — инкремент, и потолок тут ~0.3–1.8s на мутант (ирредуцибельно для мутируемого юнита).

### 9.9 Рецепт конфига (применено 2026-10-17 в `.harness/config`)

```
MUTATE='CARGO_PROFILE_TEST_INCREMENTAL=1 cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 {files}'
```

Разбор:
- `CARGO_PROFILE_TEST_INCREMENTAL=true` — корневой рычаг §9.7 (env, `Cargo.toml` не трогаем: обычные прогоны остаются с `incremental=false`). Ловушка: значения `1`/`0` НЕ парсятся (profile-override требует exactly `true`/`false`), cargo падает `provided string was not true or false` — и cargo-mutants помечает всех мутантов `unviable`. Урок: сначала валидировать синтаксис env и факт сборки на одном мутанте, потом верить замерам.
- `--in-place` — тёплое дерево, без cold-target; дерево должно быть чистым (оно же и откат по git).
- `--test-tool nextest` — process-per-test, точные slow-timeout'ы на повесившем мутанте.
- `--baseline=skip` — VERIFY-гейт уже прогнал suite в этом дереве до мут-гейта (второй baseline = 30–100s впустую).
- `--timeout-multiplier 2` — мутант иногда медленнее baseline'а.
- `{files}` — один запуск на все изменённые файлы (§devh_fast_cycle.md 1b).

Осталось вне конфига (следующие шаги): `-- --skip <regex>` медленных нефункциональных тестов (§9.4) — ОПЦИОНАЛЬНО и с потерей силы гейта: skip убивает мутантов, которых ловят только эти тесты; гейтить по умолчанию — честный suite. `-j` в in-place невозможен (одно дерево); для параллелизма — copy-режим + `--copy-target` + `-j 2..3`, требует отдельного замера. `--in-diff` — мутировать только затронутое диффом (меньше мутантов, тот же смысл).

### 9.10 Конкурентная среда и CI-часть (замеры соседней сессии, 2026-10-17)

**Главный множитель — контенция.** Локальная спокойная пересборка `devh` после touch = 13.5–25s, но медианы build по 25-минутным окнам прогона = 41…111s (среднее 98s). Причины: параллельные сессии грузят rustc (load 4→18), zram полон, swap 6–11 ГБ из 11; замер соседей получил даже OOM-kill (rc=137). Т.е. «85 минут» прогона — это 2–4× раздутые ~25–30 мин. **Не гонять мутанты, пока другие сессии билдит** — дешевле любого флага.

**Что работает (по убыванию эффекта):**

| Мера | Эффект |
|---|---|
| CI: матрица `--shard i/4` (16 ядер вместо 4) | 85 → ~20–25 мин |
| `taiki-e/install-action` для cargo-mutants/llvm-cov вместо `cargo install` | −3–6 мин/запуск CI (сейчас каждый раз компилирует тулы: `devharness-rs.yml:37,41`) |
| `--test-tool nextest` | тест-фаза 61→12.8s (4.8×), baseline 29.7→11s, auto-timeout 147→60s; smoke 5 мутантов rc=0 за 182s |
| Локально `--iterate` + постоянный `-o` | повтор = только новые мутанты (177 caught → секунды) |
| Локально `-D --in-diff` | только мутанты в изменённом диффе |
| Починка висячих интеграций (`lib.rs delete match arm` → test 36–95s) | −8–10 CPU-мин + ниже auto-timeout |
| Запуск на простаивающей машине | 2–4× |

**Бесполезно:** `-j >4` (jobserver режет по NCPUS), `--in-place` **в CI** (разрешение `--jobs` теряется → сериализация; локально в тёплом дереве in-place наоборот оправдан), sccache для `devh` (исходник меняется каждый мутант → мисс; депы и так `Fresh`).

**Шум:** `CARGO_PROFILE_TEST_DEBUG=0` / `OPT_LEVEL=0` / `INCREMENTAL=true` — замеры 7.1–42.6s при load 10, рисунок не читается; инкремент всё равно нужен как база (0 Compiling при no-op, §9.7). Bool-оверрайды профиля: только `true`/`false`.

Итог CI: sharding×4 + install-action + nextest = ~20–25 мин против одной 90-минутной джобы (`--in-place` в CI не годится из-за блокировки `-j`; локально — наоборот).

---

### 9.11 P5 — тест-фаза мутанта в devh: замеры и код (2026-10-17)

**Что сделано (devharness, коммит-кандидат после `edd9541`):**

| Артефакт | Смысл |
|---|---|
| `core/load.rs` (новый, 6 unit-тестов) | load-gate мутаций: `parse_load` (1-е поле `/proc/loadavg`), порог по умолчанию **max(4, 2×CPU)**, override `HARNESS_MUTATE_MAX_LOAD` (`off` — выключить; мусор/отрицательное → дефолт), `block_reason` — текст причины в лог гейта |
| `commands/run.rs` | `run_mut()` читает нагрузку ДО запуска мутатора (один замер, свой build гейта себя не блокирует); `MutRun::Blocked(why)` → `MutReport{outcome: Env, log: "{label} env-blocked: {why}"}` — гейт **перенесён с честной причиной**, не молчаливый Env |
| `commands/doctor.rs` | `MUTATE` содержит `CARGO_PROFILE_TEST_INCREMENTAL=true` (иначе WARN §9.7); `load X > порог Y` (WARN, §9.10); тёплый `.shellcheck-cache`; extraction бинаря мутатора пропускает env-assignments (`CARGO_PROFILE_TEST_INCREMENTAL=true` — не программа) |
| `tests/integration.rs` | §5.3 death-aware: `wait_child_for` (тик 25мс, бюджет 10s, `try_wait` ловит смерть ребёнка мгновенно) — в `section21c`; pid-вариант `wait_pid_for`/`parse_pid`/`assert_pid_file_gone` — в `section21_bg_ps`, `section21d_bg_auto` (у `bg` умирает демон, не лаунчер); §5.5 тест `section16b_load_gate_blocks_mutants`: `HARNESS_MUTATE_MAX_LOAD=0.1` → `env-blocked`, заглушка мутатора **не запускается** |

**Контрольный замер (2026-10-17, тёплое in-place-дерево `/tmp/devh-merge-W041`, reuse-worktree, `MUTATE='CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2'`, 14 мутантов timing.rs):**

| Метрика | Copy-mode (205-прогон, §9.7) | In-place warm (этот замер) |
|---|---|---|
| build mean / median | 90.7s / 90.5s | **12.7s / 11.9s** (max 24.9s) |
| test mean / median | 6.7s / 2.6s | **3.8s / 4.0s** (max 6.1s) |
| Итог | 22 мутанта с test 30–95s | 14 мутантов за **4м**: 13 caught, 1 unviable, 0 missed |

Приёмка §5.6 №1 закрыта: тест-фаза перестала быть узким местом — теперь ею снова является build (median ~12s), а он уже инкрементален (§9.7). Сьют: **112/112 PASS**, fmt/clippy чистые (под контенцией load≈10 дал флейк в `section21*` — повторный прогон зелёный; это ровно тот contention, от которого load-gate и защищает).

### Детали замера линковщика (детализация к §8.5)

Правильная команда установки (флаг `-C link-arg=...` НЕ идёт в apt — отсюда типичная ошибка `apt install lld + -C link-arg=...`; `-C` — опция rustc/cargo, не apt):

```bash
sudo apt install -y lld          # только установка
which ld.lld && ld.lld --version # проверка
```

Как мерили: линковщик обёрнут таймером (`CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=<wrapper>`), wrapper логирует длительность каждого вызова `cc`, затем `cargo clean -p uwa-api` + `cargo nextest run -p uwa-api --no-run` — один и тот же скоуп двумя драйверами: `cc` и `cc -fuse-ld=lld`.

| Метрика | GNU ld | lld 19.1.7 |
|--------|--------|------------|
| 66 линковок, исполняемые (без build scripts) | 45.98s | 79.43s |
| 56 линковок build-скриптов | 8.40s | 5.89s |
| Топ-бинарник `e2e_pipeline` | 1.73s | 9.44s |
| Повтор GNU ld (контроль шума) | 78.06s | — |

**Вывод:** lld быстрее только на мелких build-script-линковках; на тяжёлых тест-бинарниках медленнее в 1.2–5x, а повторный прогон plain показал ±70% разброс — эффект тонет в шуме разделяемой машины. Смена ключа сбросила fingerprint и вызвала полную пересборку (~12 мин) на КАЖДУЮ пробу линковщика — это и есть настоящая цена эксперимента.

---

## Troubleshooting

**"Blocking waiting for file lock on build directory"** — параллельная сборка в том же `target/`. Либо ждать, либо `pkill -f "cargo (build|test)"` (сначала убедиться, что чужая сессия не в середине!).

**Не хватает RAM** — снизить `codegen-units` до 1 или добавить swap; на 7.5 ГБ разделяемой машины это частая история.

**Тест повис** — `slow-timeout` в nextest.toml убьёт его с диагностикой; вне nextest повисший тест виден по `RUST_BACKTRACE` в отдельном прогоне.

**Хочу инкрементальность выключить** — `[profile.dev] incremental = false` (обратно) + `cargo sweep -a` для срезки `incremental/`; цена — возврат к 16.4s на пересборку крейта.

### Сборка молча умирает mid-build (без error, без END)
На разделяемой машине дважды наблюдалось: `cargo nextest/build` обрывался без единой строки ошибки в логе (процессы просто исчезали, RAM ~700 МБ free). Похоже на OOM-kill под чужой нагрузкой. Детект: в фоновых сборках всегда пише маркеры `START/END`; отсутствие `END` + замерший хвост лога = смерть. Лечение: перезапуск с кэша (deps уже собраны), не с `clean`; при массовом OOM — `CARGO_BUILD_JOBS=3`.

---

## Итог

| Слой | Что дало |
|------|----------|
| `profile.dev` + `package."*".debug = 0` | 58 ГБ → 1.1 ГБ |
| `profile.test` (`-O1`, line-tables) | рантайм тестов 2–5x |
| Мердж интеграционных тестов | −десятки link-циклов на 2 ядрах |
| first-tick / `start_paused` / poll-loop | −секунды слепых sleep |
| nextest + конфиг | стабильность, slow-timeout, retries, удобные фильтры |
| Багфиксы (clap ID, panic-хук, pid-файл, smoke-config) | suite зелёный: **875 passed / 3 skipped** |

## 9.12. Полная приёмка P5/P6: канонический гейт крейта (2026-10-18, ночь)

Прогон (лог `/tmp/uwa-p5-gate.log`): `CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 -p devh --config devharness/cargo-mutants.toml` — 255 мутантов, scope ядра (config excludes tests/**, shell/**, commands/**).

**Итог: 255 мутантов за 51 мин — 226 caught, 2 missed, 27 unviable, 0 timeout. Kill-rate 226/228 testable = 99.1% (гейт MIN_KILL=90 — pass).**

| Фаза | До (copy-mode, §9.7) | Сейчас (in-place+incremental, приёмка) |
|------|--------------------|----------------------------------------|
| build/мутант | mean 90.7s, median 90.5s | ~7.5s (первый cold-ребuild графа ~200s под контенцией — единоразово) |
| test/мутант | 6.7s | ~11-16s при load≈11 ( contention с параллельной сессией uwa-api; на спокойной машине ~9s) |
| на мутанта суммарно | ~97s | **~12s** (×8), весь гейт 51 мин вместо ~4.3ч |

**Два MISSED — в `core/load.rs` P5 (мой код): `load_average -> Some(1.0)` и `max_load -> Some(-1.0)` — чистые обёртки без прямых тестов. Гейт поймал дырку в P5-коде; лечение — unit-тесты (load_average сверять с parse_load(/proc/loadavg), max_load с фиксированным env). После фикса — точечный перепрогон load.rs.**

Замечания по эксплуатации:
- `mutants.out/` — живые артефакты (caught/missed/unviable.txt пополняются по ходу; debug.log с elapsed по фазам) — годятся для раннего выхода (идея 2.3, беклог P7 в devh_fast_cycle.md).
- Параллельный прогон на том же дереве = порча (pkill -f "cargo mutants" не матчит `cargo-mutants`; guard — `pgrep -x cargo-mutants`); убитый in-place оставляет `/* ~ changed by cargo-mutants ~ */` → `git checkout --`.
- `--iterate` (пропуск пойманных ранее) и `--in-diff REF` (только строки диффа) есть в 27.1.0, но НЕ подключены к шаблону — кандидаты очередного рычага (беклог P7).
- **Восстановление после in-place — инвариант протокола.** 27.1.0 НЕ гарантирует restore на выходе: и убитый (`timeout`, SIGTERM), и нормально завершившийся прогон могут оставить последнего мутанта (`/* ~ changed by cargo-mutants ~ */`). Хуже: новый прогон, стартовавший на отравленном файле, «восстановит» его К МУТАНТУ (restore идёт от базовой копии запуска) — вердикт такого прогона нельзя считать чистым. Рецепт после ЛЮБОГО in-place: (1) `git status --porcelain` БЕЗ `| head` (обрезка списка стоила отравленного max_load в коммит-кандидате), (2) `grep -rn '~ changed by cargo-mutants ~' <src>` (маркер мутанта в теле), (3) `git checkout -- <файл>`, (4) быстрый `cargo test --lib` как санити.
- **Подвид той же грабли:** `git add <файл>` запущенный ВО ВРЕМЯ in-place пргона застейджил мутанта (первый мутант применяется за секунды — коммит `ae9f619` содержал `parse_load -> None`). Правило: при открытом прогоне (`pgrep -x cargo-mutants`) не коммитить файлы его scope'а; фикс — surgical-правка тела + быстрый `cargo test --lib`.

## 9.13. P7-2.3 ранний выход: вердикт вместо отчёта (2026-10-18)

- Механизм: cargo-mutants пишет live-протокол (append-only mutants.out/*.txt + mutants.json), раннего выхода в нём нет; devh (MUTATE_EARLY=1) опрашивает раз в секунду ДЕЛЬТОЙ от базлайна (прошлый прогон/--iterate оставляет строки — sub_base обязателен).
- DOOM bound: (total-unviable)*(100-min_kill)/100+1; unviable монотонен ⇒ bound конца ≤ текущего, преждевременной тревоги нет. SAMPLE: k=⌈3/(1-min_kill/100)⌉ (90→30, 95→60, 99→300), честна только при --shuffle (cargo-mutants мешает до запуска).
- Стоп: SIGINT группе (cargo-mutants сам откатывает мутанта), грейс 30s, затем SIGKILL. Жёсткий kill оставляет маркеры `~ changed by cargo-mutants ~` — ремонт только git apply -R свежих диффов (дерево в фазе гейта грязное, checkout запрещён); не ушли → poisoned FAIL.
- Семантика: DOOM/poisoned = fatal (вердикт сразу, без 3 fix-раундов — иначе смысл раннего выхода обнуляется); SAMPLE = Ok, но причина rule-of-three печатается в консоль.
- Грабля: `devh commit` = git add -A ⇒ mutants.out/ живущего прогона утаскивался в фичу-ветку — init кладёт mutants.out/ в .git/info/exclude (общий для worktrees).
- Наблюдение из E2E (section37): списки append-only, фикс-раунды видели leftovers прошлых раундов — с fatal-семантикой проблема закрыта структурно.
- Приёмка: section37 (DOOM→immediate FAIL + частичные live-счётчики; SAMPLE→Ok, commit/merge зелёные; MUTATE_EARLY=0→обычный kill-rate вердикт; poisoned→repair+FAIL), сьют 154/154.

## 9.14. P7-2.4 покрытийный отбор тестов (2026-10-18)

- Механика: один инструментированный прогон (`cargo test --no-run --lib --tests` с `RUSTFLAGS="-Zinstrument-coverage" LLVM_PROFILE_FILE=...`) + прогон тестов для профиля -> `llvm-profdata merge` -> `llvm-cov show --format=json` -> парсим JSON, маппим покрытые строки на nextest test ids (через `cargo nextest list --message-format=json`).
- Кэш: `.harness/coverage-index.json` с version + test_files_hash (SHA256 от tests/*.rs + #[cfg(test)] модулей). При несовпадении хэша — перестройка.
- На мутационном прогоне: `cargo mutants --list` выдаёт все мутанты с файлом:строкой; собираем объединение тестов для всех мутируемых строк, генерируем `nextest -E 'test(A) | test(B) | ...'`, подставляем в команду через `--test-tool nextest -- <filter>`.
- Инвалидация работает: hash тест-файлов меняется при правке тестов -> перестройка индекса.
- Требования: nightly rustc (для `-Zinstrument-coverage`), `llvm-cov` и `llvm-profdata` в PATH.
- Ограничение MVP: маппинг покрытия на тесты эвристический (по имени бинаря); точное per-function покрытие требует `llvm-cov export` — отложено.
- Приёмка: сьют 155/155, fmt/clippy чистые; отдельный прогон coverage-теста не добавлен (требует nightly в CI).

## 9.15. P8 третья петля: выжившие → observations/RULES/промпт (2026-10-19)

Ветка `devh/p8-third-loop` (вне темы сборки, но материал журнала — аудит новых модулей). Коммиты поверх `main`: P8-1 `3765653`, P8-5 `c8fa042`, P8-3 `dbac1e9`, P8-4 `419ea7a`, P8-6 `fe6a564`.

- P8-1 «данные потекли немедленно»: хук `collect_after_run` под GlobalLock очереди после КАЖДОГО `finish()` (single/batch/per-file) — per-file перезаписывает outcomes.json каждым запуском, чтение идёт из каталога текущего прогона (`early::mutants_out_dir`). Выживший = MissedMutant ТОЛЬКО: Timeout = убит, Unviable = вне тестов (иначе kill-rate лгал бы). seen-дедуп SHA256(file:line:col:mutation) — «вечный» выживший одной строкой в очереди; `killmap.json` накопительно (missed/runs/last, без decay); файл, не задетый scope прогона, ≠ «0 killed» — строка карты не трогается.
- P8-5 (OBSERVE) и P8-3 (hot-spot пул в промпт + таблица в hint) — инъекции в `prompt.rs`, обе с явной подписью «на вердикт гейта не влияет».
- P8-4 (дистилляция): `devh survivors` по умолчанию печатает только basename'ы (load.rs ×2) — координаты инстансов остаются в observations.md и тикетах, в RULES и RULES_LOG не текут (§8); `--rules --sessions K` считает тишину по датам «## Session N» из RULES_LOG, но пенсионер ставится ТОЛЬКО если тег класса называет свои жанры словами (пустой маппинг — не догадка тулинга, а «допиши в тег»).
- P8-6 (§8.6, `fe6a564`): red-mutation — ОТДЕЛЬНЫЙ поток. `is_red(gate)` = метка гейта `red-mutation`; `group_by_genre` ключуется (поток, жанр) — код первым, красный своей группой без суммирования с кодовым одноимённым жанром, digest с префиксом `red:`, строки режима с меткой `[red-mutation]`. Ключевое: `collect_after_run` НЕ пишет дельты red-гейта в killmap — killmap меряет слепые места КОДА, spec-проблема в кодовую карту не идёт. `rule_classes` тоже по потоку: класс, назвавший в тексте тега «red-mutation» (blind:red-asserts), считает только красные выстрелы и свою пенсию. RULES_LOG: `session_survivors_lines(code_logs, red_logs)` даёт ДВЕ строки при необходимости — `- survivors [red-mutation]: … — отдельный поток (§8.6)`; `streams_line` печатает итог обоих потоков в `--rules` (материал виден всегда, даже если классы названы на один поток). - Гейт ветки: 137 unit + 54 integration (P8-1 был 119+50), fmt/clippy чистые; Cargo.lock не менялся.
- Мутационный аудит («ирония приёмки» §9 плана): worktree `/tmp/devh-p8-audit`, detached на `3765653`, команда `cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 -p devh --config devharness/cargo-mutants.toml -f devharness/src/core/survivors.rs -f devharness/src/core/killmap.rs` — 97 мутантов. На 05:45 (1ч56м прогона): 58 исходов, 56 caught / 2 unviable / 0 missed, ~2 мин/мутант; на 06:45 (3ч в прогоне) — тот же срез (58/56/0/2), аудит продолжается.
- ВАЖНО про этот аудит: он стартовал на P8-1-era коде (survivors.rs 613 строк + killmap.rs 166), а финальное дерево после P8-5/P8-3/P8-4 — 1404 + 342. Вердикт (если останется 0 missed) НЕ покрывает дельту. Догон после сдачи: `cargo mutants --in-diff` сначала на дельту P8-6 (`git diff 419ea7a fe6a564 -- devharness/src/core/survivors.rs ...`), затем P8-4 — новые/изменённые строки обоих коммитов обязаны быть покрыты вердиктом, иначе вердикт не покрывает финальное дерево.
- Грабля параллельного воркета на общем `.git`: `git add` в своём воркте безопасен (отдельные index'ы), но инвариант «`pgrep -x cargo-mutants` пусто перед git add scope'а» держим и дальше — он защищал от стейджа живого мутанта (§9.12); проверка «`git status --porcelain` без `| head` + grep маркера в теле» прошла чисто.
- СДАЧА (2026-10-19, «всё объединить в текущий head»): main-чекаут был грязным только от чужой parallel-сессии — ` D .cargo/mutants.toml` (не наш файл, не трогал) и ` M Cargo.lock` (содержимое идентично дельте ветки — сверил `git diff`, откатил `git checkout --`; merge вернул ровно это же). `git merge --ff-only devh/p8-third-loop` → main = `4fcedf1` (FF от `9d1daf5`, +6 коммитов P8-1..P8-6). Гейты на merged-дереве: 137 unit + 54 integration, `cargo fmt --all -- --check` чист (в т.ч. uwa-api — чужой блокер §9.12 ушёл), `cargo clippy -p devh --all-targets -- -D warnings` чист. Push: origin/main `8d7aa32..4fcedf1`, плюс новая remote-ветка `devh/p8-third-loop` = `4fcedf1` с upstream (всё в одном HEAD).
- АУДИТ-1 (базовый, `/tmp/devh-p8-audit`, 97 мутантов P8-1-era): процесс **завис в состоянии T (SIGSTOP) на 04:12** — фоновый запуск получил стоп от тулкита и три часа стоял без прогресса (58 исходов: 56 caught / 2 unviable / 0 missed). Убит, живой мутант в `survivors.rs` откачен (`git checkout --`), `mutants.out/lock.json` удалён, воркет чист. Вывод на будущее: длинные фоновые прогоны — `setsid nohup`, иначе SIGSTOP съедает часы молча.
- АУДИТ-2 (догон по финальному дереву, `setsid nohup`): `CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 --config devharness/cargo-mutants.toml --in-diff <git diff 3765653 4fcedf1>` на воркте `/tmp/devh-p8-loop` (4fcedf1) — точечно дельта всех шести коммитов. `cargo-mutants 27.1.0` берёт `--in-diff` **файлом диффа** (голый ref — «Failed to open diff file»), поэтому `git diff 3765653 4fcedf1 > /tmp/p8-delta.diff`. Скоуп: 155 мутантов — `survivors.rs` 126, `killmap.rs` 18, `prompt.rs` 7, `lib.rs` 4 (unviable ожидаемо = arithmetic/тип-мутации вроде `+=`→`-=` и `from_matches`→`Ok(Default...)` в prompt/lib — не компилируются; в тестовый знаменатель kill-rate не идут).
- АУДИТ-2 ИТОГ (08:01, 155/155): **147 caught / 8 unviable / 0 missed → kill-rate 100.0% по тестируемым** (147/147), ~21 с/мутант (155 мутантов за ~54 мин на спокойной машине), воркет чист после прогона (cargo-mutants восстановил все файлы). Вердикт аудита теперь ПОКРЫВАЕТ финальное дерево `4fcedf1` целиком (дельты P8-1..P8-6): ни одного выжившего мутанта в `survivors.rs`/`killmap.rs`/`prompt.rs`/`lib.rs`. Это и есть «ирония приёмки» §9 — модуль сбора выживших сам прошёл собственный гейт.

## 9.16. A1: единый роутинг вердиктов гейтов (FailureKind + GateStop) (2026-10-19)

Сессия A1 детального плана `docs/devh_advance.md` (ветка `devh/a1-verdict`, база `eaf4dac`, коммит `b873a18`, FF-merge в main + push origin/main и ветки).

- Суть: выпилены carrier-переменные `GateFail`/`LintKind`/`ReviewOutcome`/`MutateOutcome` — гейты отдают `(String, Verdict)`, `gate_ladder` — `Result<(), GateStop{verdict, log}>`, `run()` роутит по `gates::failure_kind` (новая единица в `core/gates.rs` + 4 unit-теста): `Fail→fix-раунд`, `EnvBlocked→die(2)`, `SpecBlock→die(1)`.
- Дефект 1 плана закрыт: `crap_gate` SpecBlock больше не приходит как Red (агент получал fix-раунды на спека-блокере) — свой класс, rc=1, текст «блокер СПЕКИ: чинит человек».
- Два сознательных дельта поведения: env-блокеры `rc=1 → rc=2` (red-pre-фаза уже была 2); SpecBlock — мёртвая ветвь → живой rc=1. Оракул (test.sh-ассерты + integration) цел: 127 PASS / 0 FAIL, section16 сверяет ТЕКСТ «Блокер СРЕДЫ», не код возврата.
- Грабля (код): в `gate_ladder` mutation-ветка делала `Err(GateStop{verdict, ..})` где голый `verdict` — это stale-байнд из `let (lint_log, verdict) = lint_gate(...)` (всегда Pass) → `failure_kind(Pass)` = `unreachable!` в gates.rs:71, паника (симптом: section02/section20). Лечение: `match &rep.verdict` + `verdict: rep.verdict.clone()` — вердикт берётся из MutReport, не из висячего байнда. Rust shadowing; мьютекс-урок = «матч по ссылке, когда тело ветки тащит значение в конструктор».
- Грабля (окружение): 30 integration-фейлов «mutation env-блокер: load 9.1 > порог 8.0» — load-gate (P5 §5.1) срабатывал от САМОГО сьюта: 54 параллельных теста на 4 ядра гонят loadavg выше порога. Это не регрессия; лечится спокойной машиной (1-мин avg < 8) или `HARNESS_MUTATE_MAX_LOAD=off` для верификации. Урок: прогон сьюта сам создаёт нагрузку — паника «сначала сломалось 2 теста, потом 30» была именно накатом load.
- Мутантный аудит дельты — конфиг: `devharness/cargo-mutants.toml` исключает `commands/**`, и `-f <файл>` в cargo-mutants 27.1.0 НЕ снимает `exclude_globs` (проверено `--list`: 0 мутантов run.rs при `-f`). Нужен отдельный конфиг `/tmp/a1-mutants.toml` без commands-исключения. Это правка протокола аудитов сессий A2/A3 (`commands/run.rs` — главная цель!): аудит-конфиг ≠ канонический гейт-конфиг.
- Мутантный аудит дельты: `CARGO_PROFILE_TEST_INCREMENTAL=true cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2 --config /tmp/a1-mutants.toml --in-diff <git diff eaf4dac..рабочее дерево run.rs+gates.rs> -f run.rs -f gates.rs` — 25 мутантов (diff-фильтр честен: весь скоуп файлов 169+159, но в диффу реально мутируемых строк мало — рефактор-перенос). ИТОГ: **21 caught / 4 unviable / 0 missed → 100% kill-rate на viable**; unviable = арифметика на String (`+=`→`-=`/`*=`, жанр-шум).
- `Default` для `Verdict`(Pass)/`MutReport`(пустой зелёный)/`FailureKind`(Fix): без них мутанты FnValue-жанра на моих возвратах кортежей `(String, Verdict)` были unviable (первый аудит: 10/25 unviable). Дефолты семантичны (нулевое значение = «нечего не упало»), в продах не используются, и вердикт честнее: мутант «тело → Default::default()» теперь КОМПИЛИРУЕТСЯ и обязан ловиться тестами (ловится: 21/21).
- Приёмка сессии: 141 unit + 54 integration; `cargo fmt -p devh -- --check` и `cargo clippy -p devh --all-targets -- -D warnings` чисты; `bash devharness/test.sh` 127 PASS / 0 FAIL; аудит 25 (21/4/0). Пост-аудит протокол: `git status --porcelain` без `| head` + grep маркера в теле (детект невосстановленного мутанта) — чисто; `git add` только при пустом `pgrep -x cargo-mutants`.
## 9.17. A2: разрез run() на фазы + чистый decide_round (2026-10-19)

Сессия A2 детального плана `docs/devh_advance.md` (ветка `devh/a2-run-phases`, база `79d09cc` = main, коммит `971a245`, FF-merge в main + push origin/main и ветки).

- Суть: `run()` 551 → **88 строк**: водитель (лок→ claim→ worktree→ red-pre→ раундовый цикл→ OK-гейты→ red-post→ red-mutate→ commit+push→ finalize) + **11 фазовых fn** (`require_gates`, `lock_run`, `resolve_agent`, `claim_item`, `ensure_worktree`, `red_pre_phase`, `no_crap_notice`, `red_post`, `red_mutate_phase`, `commit_push`, `finalize`) + единый хвост `give_up(kind, code)` — три inline-ветки `die` свернуты в одну диспетчерную (die-вызовы 22→20 by design). `gates::decide_round(round, max_fix, kind) -> RoundDecision{Continue, GiveUp{code}}` — чистый классификатор: Env→2, Spec→1, Fix: `round < max_fix` → Continue, иначе 1.
- Метод (главный урок сессии): тела фаз подняты **байт-идентично** питон-экстрактором `/tmp/transform_a2.py` (читает исходный run.rs, режет по анкерам, dedent тела claim, применяет только подстановки путей параметров: `&wt`→`wt`, `&red_file`→`&red.file`, `red_file.display()`→`red.file.display()`, `!no_red && has_red`→`!red.no_red && red.has_red`). Ручная транскрипция 550 строк = класс опечаток; приёмка «diff по println!/die нулевой» = **74/74 литерала совпадают**, smoke-выхлоп после правки нормализован байт-идентичен снимку ДО правок (md5 `46a0245b7d38cdf79d7f2a729c975239`; norm-фильтр: `/tmp/devh-test-*`→TMP, `\d+s`→Ns, `W-\d+`→W-NNN). Оракул-снимок ДО — обязательный шаг любого переноса.
- RED честный: стаб `decide_round` (всё → `GiveUp{code:1}`) уронил 2 из 4 тестов в РАНТАЙМЕ (`continues_below_max`, `env_gives_two`), два прошли тривиально; реализация → 4/4. `#[derive(Default)]` с `#[default] Continue` на enum'е — умышленно: FnValue-мутант «тело → Default::default()» компилится и обязан ловиться тестами (ловится).
- Грабля (код, компиляция): (1) `lock_run` вернул E0106 — нужен `<'a>` на fn (`PidGuard<'a>` в типе возврата). (2) `red_pre_phase` — не находились `root`/`agent`/`red_file`: тело поднято из тела `run()`, где это были локальные `PathBuf`/`String` → добавлены параметры `root:&Path, wt:&Path, agent:&str` + `#[allow(clippy::too_many_arguments)]`. (3) `&mut timing` в шаблоне нового fn + параметр `timing: &mut Timing` = двойной borrow → в поднятом теле замена `&mut timing,`→`timing,`, а в `commit_push`/`finalize` тип параметра `&Timing`→`&mut Timing` (звали `.stage()`). (4) `commit_push`/`finalize` неявно возвращали `()` при `?`-вызове → добавлены `Ok(())`. (5) шаблон фазы содержал свои `let no_red/red_file/has_red` до поднятого блока — дублирующие объявления, убраны. (6) экстрактор: первая попытка взяла ПЕРВЫЙ `run_agent` (авторский, в red-pre-цикле) → драйвер 230 строк; анкер = хит `run_agent` ПОСЛЕ `let mut round`, не до.
- Грабля (self-reference в тесте): `run_is_split_into_phases` через `include_str!("run.rs")` матчил **свой собственный** список фаз → «left: 2 right: 1»; лечится `src.split("#[cfg(test)]").next()` — берём только код ДО тестов (ошибка «left: 0» следующего вида — сигнатура реальна `fn lock_run<'a>`, паттерн `fn lock_run<`).
- Грабля (clippy, вскрылось при `-D warnings`): поднятые байт-идентично тела тянут за собой контекст оригинала — в `red_pre_phase` `prompt::red_author_prompt(store, cfg, &root, &wt, id, &agent)` + `run_agent(..., &wt, ..., &agent)` + `tree_dirty(&wt)` + `run_red(&red_file, &wt, …)` дали **9 needless_borrow** (параметры фазы — уже ссылки; `&wt` = `&&Path`). Правка механическая, литералы не тронуты. Отдельно: `.expect()` в НОВОМ тесте — deny через sandbox-rustflags (`clippy::expect_used`) → `split_once(...).unwrap_or((src, ""))` (unwrap_or ≠ unwrap, не ловится).
- Мутантный аудит дельты (тот же протокол/конфиг, что A1 §9.16): `--in-diff /tmp/a2-delta.diff` (git diff рабочего дерева, 755 строк) — **29 мутантов: 27 caught / 2 unviable / 0 missed / 0 timeout**. 2 unviable — целые функции (`lock_run`, `red_pre_phase`, span = весь fn), не арифметика-на-String как в A1. НОВЫХ тестовых мутантов 0 (cargo-mutants не генрит внутри `#[test]` — проверяли ДО добавления тестов). Окружающий loadavg в момент ауйда доходил до 12.9 (сам cargo-mutants build+nextest на 4 ядрах), но 0 Timeout — фейковых «caught» нет; load-gate порога 8.0 применяется к devh-гейту, не к процессу cargo-mutants.
- Приёмка сессии: unit **147** (141 + 4 `decide_round_*` + 2 структурных run.rs) + integration **54**; fmt/clippy чисты; `bash devharness/test.sh` **127 PASS / 0 FAIL**, нормализованный выход = снимку ДО (md5 совпал). Пост-аудит протокол чист: `git status --porcelain` без `| head` = только 2 файла зоны; grep маркера в теле — только защитные константы (`early.rs::MARKER`, фикстура `git.rs`), не загрязнение.
- Stage-контракт `devh ps` (11 имён `timing.stage`) закрыт тестом `stage_names_preserved_for_ps`; структурный инвариант — `run_is_split_into_phases` (ровно 1 сигнатура каждой фазы). Механический критерий `run() ≤ 100` = 88.

## 9.18. A3: единый писатель статуса (claim_action + apply_event), фикс дефекта 2 (2026-10-19)

Сессия A3 детального плана `docs/devh_advance.md` (ветка `devh/a3-state-events`, база `5822179` = main после A2, коммит `f5427ef`, FF-merge в main + push origin/main и ветки). **План A1–A3 исчерпан.**

- Суть (дефект 2 плана): статус item'а писался `set_status` инлайн в обход `state::transition` — `claim_item` ставил Active безусловно (done-item мог уползти обратно в active — «переоткрытие смёржено»), `finalize` повторял мёртвую запись Active. Теперь единственный продакшен-писатель — `Store::apply_event(id, event)` (`shell/store.rs`): `status_of` → `state::transition` → при Ok `atomic_write(status, next)` + строка в `events.log` («{дата} | {W-NNN} | {event}»); недопустимый переход — Err с текстом перехода (run/merge/abort падают, не молчат). `set_status` → `#[cfg(test)]` + `pub(crate)` — только подготовка состояния в тесте worktree.rs (мьюты тестового кода cargo-mutants не генерит, аудит чист).
- `claim_action(prev: Status) -> ClaimAction{Claim, Resume, RejectDone}` — чистая (core/state.rs), `#[derive(Default)]` + `#[default] Claim` (как RoundDecision в A2: FnValue-мутант компилится и обязан ловиться). RED выполнялся буквально: стаб `Claim` → 3 из 4 новых тестов упали в рантайме (`rejects_done`, `resumes_active`, `fold_replay_matches_transition`), `claims_open` прошёл тривиально. Итог claim: Done → `bail «уже done — создай новый: devh new …»` (rc≠0), Active → resume-printf, Open → WIP-чек (как today) + `apply_event(Claimed)`.
- `fold(events: &[Event]) -> Status` + `event_parse`/`events_from_log` (взятие последнего `|`-поля строки журнала; мусорные/обрезанные строки пропускаются — журнал не убивает реплей). Doctor: `replay_checks` (commands/doctor.rs) — fold(events.log) vs файл status; рассинхрон = `WARN … replay → X, статус-файл Y` в отчёт (warn+1), НЕ смерть doctor'а — status остаётся источником истины для гейтов, журнал — постмортем.
- Семантические дельты (осознанны, все вытекают «обход transition удалён»): (1) abort Open-item'а (создан, но не взятый) — без события, статус уже open (transition Open+Aborted недопустим; поведение для пользователя то же: «OK → open, работа выброшена»). (2) abort Done-item'а — теперь Err «недопустимый переход Done + Aborted» вместо старого воскрешения Open (смёрженное не откатывают — ровно дефект-2 в квадрате). (3) run без id → first_live пропускает done (как и раньше), поэтому тест зовёт `devh run W-001` явно — resolve_id проверяет только существование dir.
- Грабля (регрессия интеграции, поймана сьют-ом): `observability_list_logs_clean` — abort Open-item'а падал «недопустимый переход Open + Aborted» (п.1 выше). Старый код писал Open безусловно, так что поведение часть тестов было невидимо. Урок: любой вызов transition-guarded писателя на НОВОМ месте = потенциально новый класс; диагностика через `cargo test --test integration <name>` в одиночку.
- Грабля (окружение, повтор A1 §9.16): полный прогон 55 integration'ов сам гонит loadavg > 8 (mutation load-gate P5 §5.1) → 31 фейк-фейл «mutation env-блокер: load 8.3 > 8.0». Верификация: `HARNESS_MUTATE_MAX_LOAD=off` → 55/55 и естественный прогон при load<4 → 55/55. Шум окружения, не регрессия; фиксировать в журнал повторно, т.к. сьют растёт.
- Грабля (clippy): `items_after_test_module` — вставил `#[cfg(test)] mod tests` перед verify_smoke по анкеру; тест-модуль обязан быть в КОНЦЕ файла. `indexing_slicing` — `lines[0]` в тесте → `lines.first().unwrap_or_else(|| unreachable!())`.
- Мутантный аудит дельты (протокол/конфиг §9.16-9.17): `--in-diff /tmp/a3-delta.diff` (8 файлов, 473 строки диффа) — **22 мутанта: 18 caught / 4 unviable / 0 missed / 0 timeout**. 4 unviable — целые функции (`event_parse`, `events_from_log`, `fold`, `Store::apply_event`), та же форма, что A1/A2 (целые тела в одном мутанте). Пост-аудит протокол чист (без `| head`, маркеров в теле нет).
- Приёмка сессии: unit **155** (147 + 8) + integration **55** (новый `section43_run_refuses_done_item`: rc≠0 + «уже done», статус не тронут, events.log = claim→merged без повторов, doctor без WARN реплея); test.sh **127 PASS / 0 FAIL**, нормализованный выход = снимку до сессий (md5 `46a0245b…`); fmt/clippy `-D warnings` чисты.

## 9.19 — A4: smoke-скелет (стаб-агенты + микрорепо, devh_advance2)

**Задача (план advance2 §A4):** детерминированный happy-path цикла харнесса без
LLM и без cargo-mutants: фейковые инструменты говорят на диалектах настоящих
судей (`parse_mutation_output` / `parse_crap_output` / `parse_review_verdict`),
поэтому гейты прогоняются по-настоящему.

**Артефакты:**
- `devharness/tests/mk_smoke_repo.sh <dir> <stub> [wt-root]` — генератор
  микрорепо: bash-проект (src/calc.sh с HEAD-дефектом: нечисловой аргумент
  молча уходит в `$(( )`), runner tests/all.sh на диалекте cargo test,
  стабы-инструменты tools/mutate.sh + tools/crap.sh (режимы через
  `.smoke/<tool>-mode` — задел A5), полный .harness/config, bare-remote.
- `devharness/stubs/good.sh` — эталонный стаб-агент: чинит дефект, пишет
  summary, на REVIEW-промпт отвечает `VERDICT: OK`. Guard инжекции OBSERVE:
  нет точки в промпте → rc=3 (регрессия agent_prompt→observe.md зелёной
  smoke-сессию быть не может).
- `test.sh` секция 31 (smoke happy path): new→run→merge, 17 ассертов, полный
  цикл ~1s (план: <60s); doctor на smoke-репо rc=0. Секция 0 (shellcheck)
  расширена двумя новыми файлами — автоматический статический слой.

**Калибровки (все — поправки к черновику плана):**
1. `gates::failing_tests` требует якорь `::` в test-id — runner обязан печатать
   `test calc::add_non_numeric ... FAILED`, плоский id (`test add_two`) НЕ
   выделяется как упавший тест (RED-EXPECT тогда даёт SpecBlock «не дошёл»).
2. Журнал событий A3 — per-item: `.harness/items/W-001/events.log`, не
   `.harness/events.log` (apply_event пишет через item_rel).
3. `agent.log` содержит ВЫВОД агента, не промпт — proof инжекций только через
   маркер stdout стаба (guard rc=3).
4. Микрорепо требует скелет `.harness/{items,summaries,logs}` (его создаёт
   `devh init`, а мы init не зовём — конфиг пишем свой; summary агент пишет
   САМ в готовый файл).
5. test.sh: секции экспортят `HARNESS_*` в env, а `load_config` отдаёт env
   приоритет над файлом — S31 сбрасывает все `HARNESS_*` перед стартом
   (иначе VERIFY предыдущей секции перебивает конфиг smoke-репо).
6. `mk_smoke_repo.sh` печатает путь (как `fresh()`): вызывающий ловит в `S=$()`.
7. `.harness/` в .gitignore микрорепо (как в бою), worktree — вне репо
   (`WT_ROOT` соседом): иначе untracked-файлы портят `changed_files`/`tree_dirty`.

**Приёмка:** test.sh 143 PASS / 0 FAIL (smoke-цикл ~1s); unit 155 + integration
55 зелёные (естественный прогон при load<4; каскад env-блокера при load≈8 —
   известный шум P5, проверено через HARNESS_MUTATE_MAX_LOAD=off); fmt/clippy
чисты; doctor smoke-репо rc=0. Rust-кода A4 не касается — аудита мутантов нет.

## 9.20 — A5a: adversarial-корпус v1 (читеры выхода агента, devh_advance2 §A5)

**Задача (план advance2 §A5a):** 5 стабов-читеров против контракта харнесса
(кейсы 1–4, 8): каждый ломает РОВНО одно допущение; судьи настоящие (стабы
инструментов говорят на реальных диалектах — наследие A4). Приёмка: 3 подряд
одинаковых rc на кейс (детерминированность), корпус ≤3 мин, таблица
«чит → гейт → rc» тут, kill-rate одной строкой в выводе test.sh (метрика для B1).

**Артефакты:**
- `devharness/stubs/red-green.sh` — в фазе RED пишет red.cmd
  `bash tests/all.sh; exit 0` (раннер совпадает с VERIFY, LINK проходит) → RED pre.
- `devharness/stubs/red-wrong.sh` — red.cmd честный, но в worktree сажает
  «фикс», ломающий числовой путь: падает add_two вместо add_non_numeric.
- `devharness/stubs/no-summary.sh` — делает работу, не пишет summary.
- `devharness/stubs/bad-summary.sh` — summary с первой строкой `Session 9: …`.
- `devharness/stubs/hang.sh` — `sleep 9999` (watchdog через `HARNESS_AGENT_TIMEOUT=5`).
- `test.sh` секции 32–36: helpers `adv_run`/`adv_expect` (микрорепо через
  `mk_smoke_repo.sh` + прогон `devh run W-001`, 3 итерации на кейс), счётчики
  `ADV_OK`/`ADV_N`, строка «adversarial corpus: поймано N/N»; `smoke_reset_env`
  вынесен из S31 в общий хелпер (строка 44).

**Таблица kill-rate (чит → гейт → rc):**

| # | стаб | гейт | rc | маркер |
|---|---|---|---|---|
| 1 | red-green | RED pre (Fail verdict) | 1 | `red.cmd уже зелёный … это не баг` |
| 2 | red-wrong | RED pre → SpecBlock | 1 | `RED упал НЕ ПО ТОЙ ПРИЧИНЕ` |
| 3 | no-summary | commit_push | 1 | `FAIL: агент не написал summary` |
| 4 | bad-summary | MSG_DENY | 1 | `сообщение коммита нарушает гигиену` |
| 8 | hang | watchdog (run_agent) | 1 | `FAIL агент … watchdog убил группу после 5s` |

Приёмка: 15/15 прогонов поймано (kill-rate 100%), корпус ~35s в suite, каждая
секция детерминированна (3 разных свежих микрорепо, одинаковый rc и маркер).

## 9.20.1 — ДЕФЕКТ ГЕЙТА, найденный корпусом: RED-EXPECT матчил ЗЕЛЁНУЮ строку

`judge_red_pre` честный RED проверял `regex.is_match(&raw.log)` по ПОЛНОМУ логу
red-фазы. Корпусная атака 2 вывела это в SpecBlock-дырку: при
`RED-EXPECT: add_non_numeric` красный прогон падал по СВОЕЙ причине
(`test calc::add_two ... FAILED`), но в том же логе проходила зелёная строка
`test calc::add_non_numeric ... ok` — подписи хватало её, пре-гейт рапортовал
«честный RED: [calc::add_two] матчит /add_non_numeric/». Диагноз: ложное
срабатывание regex на не-провал — ровно тот класс, из-за которого P7
(`red.expect`) вводился.

**Фикс (devharness/src/core/gates.rs):**
- `is_green_line(line)` — строки результата, не являющиеся доказательством
  провала: `… ok`, `… ok,`, `… ignored`, `✓`, `PASS [`, `test result: ok`.
- `failure_stream(log)` — лог без зелёных строк; `judge_red_pre` матчит подпись
  ТОЛЬКО по нему.
- SpecBlock получил хинт `(подпись матчит ЗЕЛЁНЫЙ тест — это не падение)`,
  когда подпись матчит полный лог, но не поток провала — оператору сразу виден
  класс ошибки спеки.
- 3 новых unit-теста: `red_pre_spec_block_when_signature_matches_only_green_line`
  (регресс дефекта), `failure_stream_drops_green_lines`, 
  `red_pre_passes_when_failure_carries_reason` (честный RED с причиной в потоке
  провала и зелёными строками рядом — не ломаем ложным SpecBlock).

**Калибровки A5a (поправки к плану):**
1. Атака 2 требует `RED-EXPECT:` в спеке ПРИ отсутствии строки `RED:` — иначе
   red.cmd пишет `new` и фаза автора пропускается; подпись берётся из
   `store.read("red-expect")` через `resolve_red_expect` (маркер `# red-expect:`
   в red.cmd приоритетнее спеки).
2. Без RED-EXPECT `judge_red_pre` остаётся лояльным (любой exit≠0 = Pass) —
   атака 2 в этом режиме ловит УЖЕ пост-гейт (`FAIL: гейт VERIFY красный после
   2 раундов`), но маркера `НЕ ПО ТОЙ ПРИЧИНЕ` не даёт: подпись причины — ключ
   pre-детекта (философия P7 «упал ≠ упал по той причине»).
3. Атаки 3/4 держаются на инварианте «обман не дошёл до main»: `devh run`
   умирает до `commit_push`'а (3 — до записи summary, 4 — на MSG_DENY),
   `git rev-parse HEAD == origin/main`; контроль `OK гейты` в логе доказывает,
   что поймана не лень, а именно финализация.
4. Висячий агент печатает промпт в TIMEOUT-строку watchdog'а — в ассертах
   маркер берём как `FAIL агент` + `watchdog`, не по телу промпта.
5. `adv_*` хелперы повторяют `mk_smoke_repo.sh` на КАЖДУЮ итерацию (свежее
   микрорепо) — цена ~0.3s, зато исключено contaminated-state (worktree/branch
   прошлой итерации).

**Приёмка:** test.sh 234 PASS / 0 FAIL (37 секций, корпус ~35s); unit 158 +
integration 55 зелёные (естественный прогон, load≈1.8); fmt/clippy чистые;
shellcheck 11 файлов (0 хелперов shellcheck-disabled). Rust-правка gates.rs —
аудит мутантами: `-f devharness/src/core/gates.rs`, 175 мутантов, in-place +
nextest + incremental (протокол §9.7–9.11): итог — **175 мутантов, 170 caught / 0 missed / 5 unviable (kill-rate 100% testable)**.

## 9.21 — A5b: корпус обманов ГЕЙТОВ (cases 5–7 + 9–12 плана advance2)

Идея та же, что в §9.20, но цель — гейты качества (не red/commit): стаб-читёр
честно делает работу и ломает РОВНО одно допущение гейта. Каждый случай — 3
свежих микрорепо (детерминированность, §9.20.1 калибровка 5), маркер в stdout
прогона + лог-контроль (файл в .harness/logs: доказательство ПРИЧИНЫ, а не
самого rc).

**Итог: 12 атак × 3 = 36/36 поймано (kill-rate 100%), test.sh 357 PASS / 0 FAIL
(было 234 в §9.20). Ни одного дефекта гейта не найдено — в отличие от A5a
(§9.20.1) все семь обязательств устояли без правки .rs.**

Таблица kill-rate (чит → гейт → rc → маркер):

| # | атака (стаб) | ломаемое допущение | гейт | rc | маркер в stdout |
|---|---|---|---|---|---|
| 37 | ignored-test.sh | «нет скрытых ignored» | NO-IGNORED | 1 | `FAIL: гейт NO-IGNORED красный после 2 раундов фикса` |
| 38 | lint-dirty.sh | «shellcheck чист на изменённых .sh» | LINT | 1 | `FAIL: гейт LINT красный после 2 раундов фикса` |
| 39 | mutate-killer.sh | «kill-rate ≥ 90%» | MUTATE | 1 | `kill-rate 80%` + `FAIL: гейт MUTATE красный` |
| 40 | tool-gone.sh | «инструмент гейта жив» | MUTATE (env) | 2 | `FAIL: mutation env-блокер` + `инструмент не распознан` |
| 41 | crap-violation.sh | «score ≤ CRAP_MAX=12» | CRAP | 1 | `FAIL: гейт CRAP красный` + лог `crap VIOLATION` |
| 42 | review-issues.sh | «ревьюер доволен / замечания чинятся» | REVIEW | 1 | `FAIL: гейт REVIEW красный` + лог `VERDICT: ISSUES` |
| 43 | review-mute.sh | «ревьюер отдал вердикт» | REVIEW (env) | 2 | `FAIL: review env-блокер` + `вердикт не распознан` |

Ключевые факты механики, подтверждённые прогонами:

1. **Worktree = чистый checkout**: пустого `.smoke/` в smoke-репо git не хранит
   (как и `.harness/` — он в .gitignore). Стаб, включающий режим инструмента
   (.smoke/mutate-mode, .smoke/crap-mode, .smoke/ignored), обязан делать
   `mkdir -p "$wt/.smoke"` — иначе `touch` молча падает (`set -uo pipefail`, без
   `-e`) и атака не происходит. Первая итерация стаба 37 именно так «ушла»:
   прогон был ЗЕЛЁНЫЙ (`OK гейты`) — ложный PASS корпуса, пойманный только
   ручным прогоном. (Урок для будущих стабов: атака должна сама проверять, что
   её артефакт реально создан.)
2. **Таксономия вердиктов на двух атаках проверена вживую**: сломанный
   инструмент (снос мутатора) и молчание ревьюера — Env (rc=2), гейт БЕЗ
   fix-раундов (`decide_round`: Env → GiveUp{code:2} сразу); красные гейты —
   Fix (rc=1) после ровно max_fix=2 раундов. stdout для env-пути несёт и лог
   (`stop.log`), и `missing`.
3. **GiveUp(Fix) печатает в stdout только ИМЯ гейта** («FAIL: гейт LINT
   красный...»), хвост вердикта живёт в .harness/logs — поэтому лог-контроль
   сделан 10-м/11-м аргументами adv_expect (`ctrl_file:ctrl_pat` внутри
   микрорепо). Иначе маркер доказывал бы факт падения, но не ПРИЧИНУ.
4. REVIEW-атаки различают промпты по `Ниже дифф` (единственная строка
   prompt::review_prompt, не встречается в fix-промптах): стабы 42/43 отвечают
   ISSUES/молчат ТОЛЬКО на ревью-промпт, а fix-раунды отрабатывают как работу.
5. МУТАТЕ-хвост (kill-rate строка) уходит в консоль из gate_ladder
   (`eprintln tail_of(&rep.log, 40)`) — маркер 39 виден в stdout прогона без
   лог-контроля. Для lint/verify/crap/review такого нет — там лог-файлы.

**Приёмка:** test.sh 357 PASS / 0 FAIL (44 секции; корпус ~55s на 36 запусков);
unit 158 + integration 55 зелёные; fmt/clippy чистые; shellcheck 18 файлов
(+7 стабов, все чистые). Rust-код в A5b НЕ тронут — аудит мутантами не
требуется. Один раз integration-сьют дал 31 флейк (первый прогон сразу после
bash-сьюта — contention/остаточные процессы в общем /tmp WT_ROOT); два
подряд ретрая — 55/55 чисто. Плана на K1/B1 это не меняет: метрика kill-rate
корпуса (36/36) теперь доступна для B1 «autonomy metric».

## 9.22 — A6: FREEZE-гейт — смерть дыры №1 (2026-10-20, ветка devh/a6-freeze, коммит 597d448)

Документированная дыра №1 плана advance2 (§A6): агент честно чинит HEAD-дефект
и попутно ослабляет `tests/all.sh` — **проходит всё** (verify зелёный, red.post
зелёный: red.cmd = тот же tests/all.sh). FREEZE-гейт закрывает класс: дифф
сессии не выходит за заявленные файлы. Позиция — **первый в gate_ladder**,
дешевле всех (git-дифф вместо прогона тестов), fail-fast до VERIFY.

**Allowlist** = секция спеки `FILES:` (new.rs `files_from_spec` →
items/W-NNN/files, инлайн или буллеты `- …` до следующей секции) + глобальный
`FREEZE_ALLOW` (пробел-разделённые глобы). Матчер (`freeze_glob`, gates.rs):
`*`-суффикс = каталог целиком (`src/*`), `*`-префикс = расширение (`*.md`),
иначе точное совпадение; серединный `*` спека не обещает — тесты пинят
near-miss'ы (`src2/`, `src`, `README.mdx`). Правка `tests/**` — только через
записанный обход items/W-NNN/allow-test-edit с причиной (симметрия no-crap:
обход записывают, а не надеются «и так пройдёт»). Пустой allowlist (нет
`FILES:`, нет FREEZE_ALLOW) — **пропуск С ЛОГОМ** («freeze: scope не задан…»),
не молчаливый Pass и не tripwire на каждую правку; doctor soft-warn, если
FREEZE=1 при незаявленном scope у всех открытых спек. `judge(Gate::Freeze)`
= **Fail**, намеренно: гейт считается по диффу (чистая функция, команд нет),
через judge() потеря гейта должна быть красной, а не тихим проходом (урок A1).

Ключевые факты механики (грабли сессии):

1. **`--in-diff` в cargo-mutants 27.1.0 — это ФАЙЛ диффа, а не ref**: без
   подготовки `ERROR Failed to open diff file`. Рецепт: `git diff HEAD~1 >
   /tmp/a6mut.diff && cargo mutants --in-diff /tmp/a6mut.diff -p devh`. Скоуп
   = только тронутые строки (43 мутанта), при этом commands/** в скоупе —
   в отличие от `devharness/cargo-mutants.toml`, который их исключает (а для
   A6 нельзя: `files_from_spec` живёт в commands/new.rs).
2. **Worktree без `.harness/config` = 31/55 integration красных**: микрорепо
   тестов получает дефолтный шаблон MUTATE (gobot muex) → «инструмент не
   распознан» → env-блокер. Лечение: `cp` конфига из main-репо (gitignored,
   ручной копир, как в W-041). Не дефект A6 — ловушка любой новой worktree-сессии.
3. **next_id = max(items)+1**: обход allow-test-edit, положенный в mk-скрипт
   ДО `devh new`, сдвигает item на W-002. adv_run пишет обход ПОСЛЕ new.
4. **Мутационный аудит (--in-diff, 43 мутанта, 17 мин, incremental=true):**
   34 caught / 9 unviable / **0 missed** / 0 timeout. Kill-rate 34/34 testable
   = 100%. Unviable — граница мутатора, не дыра тестов: 8× замена `+=` на
   `-=`/`*=` в init.rs (String не импл SubAssign/MulAssign) + 1× fn-replace
   create_item (ItemId не Default). Вся freeze-поверхность поймана:
   freeze_violations ×4 (vec![]/vec![new]/xyzzy/delete !), freeze_allowed ×3
   (true/false/&&→||), freeze_glob ×3 (true/false/==→!=).
5. **GiveUp(Fix) печатает только имя гейта** (факт 3 из §9.21) — поэтому
   причины freeze идут и в logs/W-NNN-freeze.log, и в tail; лог-контроль
   (`ctrl_file:ctrl_pat`) доказывает ПРИЧИНУ: «правка вне scope: tests/all.sh
   (обход: allow-test-edit)».
6. **Стаб 44 переживает fix-раунды FREEZE** (маркер `*FREEZE*` в case, как
   стабы 37–43 в §9.21): иначе корпус видел бы смерть агента (rc=3), а не
   отработку гейтом ровно max_fix=2. Расширение stub-промпта — как стаб
   отличил fix-промпт от рабочего: в fix-промпте нет OBSERVE-точки, зато есть
   FREEZE-хвост.

**Приёмка A6:** ручная 3-сторонняя проверка — дыра (freeze-off → rc=0,
`OK гейты`), поимка (FREEZE=1 + FILES: src/calc.sh → rc=1, маркер `FREEZE красный после 2 раундов фикса` + лог-контроль), обход (allow-test-edit → rc=0).
Корпус A5 — секции 44/44b/44c: 13-й кейс freeze-cheat **пойман** (kill-rate
42/42, 100%; 13/13 по плану). test.sh 391 PASS / 0 FAIL (44 секции + 44b/44c),
unit 164 + integration 55 зелёные, fmt/clippy чистые, shellcheck чист.

## 9.23 — A7: chaos-e2e, kill -9 в случайной точке (2026-10-20, ветка devh/a7-chaos)

**A7 — не Rust-код, а e2e-жгут:** `tests/chaos.sh <iterations> [seed]` +
стаб `chaos-slow.sh` + ручка SMOKE_SLOW в mk_smoke_repo.sh + секция 45 в
test.sh + CI-шаги. `.rs` не тронуты → мутационный аудит не требуется;
fmt/clippy чисты, bash-сьют зелёный.

1. **`kill -9` должен бить в САМ devh, не в subshell-обёртку.** Наивная
   запись `( cd $S && $DEVH run W-001 ) & RUNPID=$!` даёт в RUNPID PID
   обёртки: kill -9 убивает обёртку, devh продолжает жить сиротой и держит
   run.lock → recovery «W-001 сейчас в работе, run отклонён» (rc=1).
   Лечение: `exec` перед командой в под-оболочке — тогда RUNPID есть devh.
   Тот же класс, что orphan-settle (§9.12): убитый родитель оставляет
   живого ребёнка.
2. **Орфана агента надо дождать.** chaos-slow.sh (good.sh + sleep 2 перед
   фиксом) — kill -9 в окне агента оставляет sleeping-процесс, который
   ДОПИСЫВАЕТ файл уже без родителя. Без settle abort/resume лезет в
   недописанный write → гонка. Лечение: settle-петля
   `for _ in $(seq 1 16); do pgrep -f $STUB || break; sleep 0.5; done`
   (8 с бюджет) ПЕРЕД doctor/abort/resume.
3. **SMOKE_SLOW должен попасть в КОММИТ микрорепо, не после него.** Блок
   `printf 'slow' > .smoke/mutate-mode` идёт ДО `git add -A`: worktree —
   чистый checkout, файла вне коммита в нём нет (первая попытка писала блок
   после commit — worktree видел быстрый режим, kill пришёлся бы в red-pre,
   и растяжку фаз никто не проверил бы).
4. **Стадия убийства читается ДО recovery.** recovery-прогон перезаписывает
   W-001-timing.log — иначе в логе итерации всегда «commit+push» (финальная
   стадия любого успешного прогона), а не та, на которой прилетел kill -9.
5. **run.pid: stale = находка doctor, НЕ падение ассерта.** check_invariants
   печатает CHAOS-PID и не роняет тест: отсутствие процесса при живом
   pid-файле лечится doctor/recovery, а не ассертом восстановления.
6. **SC2002 на чтении pid:** `cat f | tr` → useless cat; починено
   `pid=$(awk '{gsub(/[ \t]/,""); print}' f)`.

**check_invariants = 4 ассерта + 1 находка** (каждый ест механизм харнесса):
1) status парсится open/active/done (atomic_write, no torn file);
2) fold(events.log) == status — прямой A3-replay под kill -9 (awk-реплика
state::fold: claimed→active, merged→done, aborted→open);
3) нет .harness/**/*.tmp осиротевших атомарных записей;
4) grep маркера «~ changed by cargo-mutants ~» в $S.wt — урок ae9f619;
5) stale run.pid — только лог. Размаз: SLP=((SEED+i*7)%8) сек детерминированно
по стадиям red → агент(sleep 2) → verify → mutate(slow 10).

**Приёмка A7:** chaos.sh 5 итераций × 2 сида (1234: все 5 kill-нуты, 0
нарушений; 77: те же 5, 0) — зелёные; test.sh секция 45 (3 итерации,
seed 4242) PASS; вся bash-сьюта PASS=393 FAIL=0; CI devharness-rs.yml ×5
seed 2024; ночной devharness-chaos.yml (cron 03:00, dispatch до 50 итераций,
tee в STEP_SUMMARY). `cargo test -p devh` standalone = 164 unit + 55
integration зелёные; тот же прогон сразу после bash-сьюты даёт 31/55 —
контеншн-флейк общего /tmp WT_ROOT (§9.21), ретрай чисто.

## 9.24 — A8: flaky-дисциплина v1 — VERIFY_RETRIES + ledger (2026-10-20, ветка devh/a8-flaky)

**Один повтор VERIFY в раунде 1 — под `nice -n 19` + sleep 2, но НЕ
молчаливо:** прошёл со 2-го раза → сессия живёт, `.harness/flaky.md`
получает строку `дата | W-NNN | verify | round=1 | retried-ok`, консоль —
`⚠ flaky-suspect (retry ok)`. Ключ `VERIFY_RETRIES` (дефолт 0 = поведение
как до A8).

1. **Ретрай ТОЛЬКО в круг 0 (`round == 0` && `verify_retries > 0`).** В
   fix-раундах повторов нет принципиально: там «зелёное» — это СЛЕДСТВИЕ
   лечения, и повтор его бы маскировал (агент «долечил» на 2-й попытке, но
   расхождение должно было отправить его в ещё один круг, а не списаться
   во флейк). Настоящая поломка не маскируется тоже: ALWAYS-режим
   (SMOKE_FLAKY_ALWAYS=1) красный каждый раз → retry красный → наружу
   идёт ПЕРВЫЙ verify_raw (лог первого провала сохранён в
   W-001-verify.log, ретрай — отдельный W-001-verify-retry.log), rc=1,
   строка `VERIFY красный`, ledger пуст. Проверено секцией 46c.
2. **Почему флейк модельнут ВНУТРИ tests/all.sh, а не отдельным
   flaky-verify.stub:** RED-LINK-гейт (gates::red_links_verify) требует,
   чтобы red.cmd и VERIFY бежали одним раннером — отдельный
   verify-инструмент ломает red-фазу (проверено: `RED-LINK: раннер
   red.cmd не совпадает`). Ручка SMOKE_FLAKY=<путь> в mk_smoke_repo.sh
   препендит tests/all.sh пролог: счётный файл ЖИВЁТ ВНЕ репо (в $T),
   первые ДВА вызова красные (red-pre + первый verify), дальше зелёный.
   Коммит пролога — обычный `git add -A` (урок A7: SMOKE_SLOW-in-commit).
   Побочно это честнее стаба: флейк проходит настоящий run_logged-путь,
   настоящие логи, настоящий leder.
3. **Стаб flaky-agent.sh = good.sh БЕЗ OBSERVE-guard.** good.sh отказывает
   fix-round промпту без OBSERVE-точки (rc=3, `стub: в промпте нет
   OBSERVE-точки`) — а 46b (честное лечение флейка fix-раундом) и 46c
   (промпт лечения тоже приходит) требуют выживший стаб: VERDICT: OK на
   ревью + идемпотентный фикс calc.sh.
4. **Doctor:** строка состояния `VERIFY_RETRIES` (`ok` если >0, иначе `..`
   — ключевой параметр выключен), далее WARN `flaky-suspect` с числом
   записей, группировкой gates::flaky_groups (count desc, затем name asc)
   и хвостом 3 строк. doctor не удалитель ledger'а: это доказательство
   среды, не мусор. append_flaky — read-modify-write, дата из
   survivors::today_ymd (общая с observations-очередью машина даты).
5. **Баш-грабли:** SC1087 — `$case[$sub]` → `${case}[${sub}]` (array
   expansion без скобок = ошибка shellcheck-оракула); SC2034 —
   `local_rc=$?` после flaky_expect (функция всегда `return 0` — rc мёртв,
   проверки едят глобалы LED/LOGS); smoke_reset_env чистит только
   HARNESS_*, поэтому SMOKE_FLAKY_ALWAYS unset'ится вручную в
   flaky_expect (иначе 46c отравляет последующие секции).

**Приёмка A8:** `cargo test -p devh` = 167 unit + 55 integration зелёные;
bash-сьюта PASS=411 FAIL=0 (393 + 18 секции 46: 46a×2 — retries=1 на
флейке: rc=0, OK гейты, warning, ledger, разнесённые verify/verify-retry
логи; 46b — retries=0: тот же флейк лечится честным fix-раундом, ledger
пуст, warning нет; 46c — ALWAYS+retries=1: rc=1, `VERIFY красный`, ledger
пуст — retry не маскирует); fmt/clippy чистые; мутационный аудит diff'а
.rs (40 мутантов по --in-diff): 31 caught / 0 missed / 9 unviable,
kill-rate 100% testable; ночной canary devharness-canary.yml (cron 04:00):
oracle ×3 подряд с variance-детектом (любое различие rc или `ИТОГО` между
прогонами = флаг) + chaos ×50 (A7, seed 2024), логи-артефакты при
покраснении. Живого LLM-агента в canary нет — с реальным агентом canary
остаётся ручным ритуалом (см. advance2 §A8).

\n
## 9.25 — B1: ledger побегов + уровень автономии (2026-10-20, ветка devh/b1-autonomy)
1. **Ledger правит человек, тулинг только читает.** `.harness/escapes.md`
   рядом с observations.md — оба вне git (`.harness/`), оба руками.
   Формат строки: `дата | W-NNN | найдено: … | класс: blind:* | реакция: …`
   (пять ` | `-полей, префиксы `найдено:`/`класс:`/`реакция:` снимаются
   л tossedо). Парсер (core/escapes.rs::parse_escape_line) — как
   flaky_groups: просеивающий. Мусор без структуры пропускается, дата
   обязана выглядеть датой (10 символов, дефисы на 5/8), сессия —
   начинаться с W, класс — быть непустым. Реакция по умолчанию пуста
   (4 поля — валидная строка) = debt.
2. **Уровень — чистая функция, не настроение.** `autonomy_level(escapes,
   streak, corpus_kill)`: любой долг (реакция пусто/`-`/`—`/`нет`) → L0
   ВСЕГДА (непогашенный урок не поднимает autonomию); далее L1 = streak≥10
   && kill≥1.0, L2 = streak≥30 && kill≥1.0, иначе L0. Границы закреплены
   юнит-тестами (streak 9/10 → L0, streak 29 → L1, kill 0.99 → L0,
   долг+streak 40 → L0) и мутантами аудита.
3. **Streak — сессии log.md ПОСЛЕ последнего побега.** `streak_of`
   парсит заголовки `## W-… — title (YYYY-MM-DD)` (общая с survivors
   механика `date_in_parens`), берёт `max(даты побегов)` и считает сессии
   с датой СТРОГО больше (строковое сравнение ISO-дат). Сессия без даты
   не считается (ленивая арифметика хуже честной неизвестности). Важно:
   это счётчик ЗАГОЛОВКОВ, не календарных дней — две сессии в один день
   дают +2, а пустая неделя между сессиями — ничего.
4. **Корпус пишет сам suitescript.** После adversarial-раздела test.sh
   пишет `.harness/corpus.json` = `{"caught":ADV_OK,"total":ADV_N,
   "date":...}` — доля ПОЙМАННЫХ атак, не самооценка агента. Пустой
   прогон (ADV_N=0) → total 0 → `kill_rate` = 0.0, уровень невозможен:
   «нет данных» не даёт 100%. Ключ `AUTONOMY` — только голосование
   отображением (дефолт 1): `devh merge` допечатывает `level_line`
   (`автономия L1 (streak 13, корпус 13/13): ревью выборочное — 2 из 3
   можно пропустить`), механических запретов уровень не создаёт — для
   solo это был бы театр. `AUTONOMY=0` — полная тишина.
5. **CONFIG_KEYS 35→36** — fixed-size массив, ключ `AUTONOMY` (как
   RED_MUTATE/VERIFY_RETRIES: код-дефолт + строка в init-шаблоне).
   Команда `devh escapes` — чистый ридер: агрегирует ledger + streak +
   корпус, показывает группы классов (count desc, name asc — зеркало
   flaky_groups), долг списком, итоговую строку уровня.
6. **Баш-грабли:** 47d — `devh run` НЕ мержит (в happy-path это две
   команды: run → merge): строка уровня печатает merge, и ретраить надо
   её вывод; seed-ножи SMOKE_ESCAPE/SMOKE_SESSIONS/SMOKE_CORPUS в
   mk_smoke_repo.sh кладут файлы в .harness/ и делают `git add -f`
   (`.harness/` в .gitignore — обычный `git add -A` их не возьмёт; урок
   A7: сессия бежит в worktree = чистый checkout origin/main, вне коммита
   файлов там нет); пустой ledger (47a) — нож с reaction=none вообще не
   отдаёт SMOKE_ESCAPE.

**Приёмка B1:** `cargo test -p devh` = 175 unit + 55 integration зелёные
(core/escapes.rs: 8 юнитов — parse полный/debt/мусор, границы уровня,
streak, группы классов, corpus json + rate, формат level_line);
bash-сьюта секция 47 = 15 проверок (47a пустой ledger → L0 без данных;
47b погашенный побег + streak 12 + корпус 13/13 → L1; 47c долг `-` → L0
при любом streak/корпусе; 47d AUTONOMY=1 merge печатает уровень (streak
+1 от сессии), AUTONOMY=0 — тишина, merge дописал `## W-001` в log.md;
47e мусор отсеян); fmt/clippy чистые; полный сьют — PASS=425 FAIL=0
(411 A8 + 14 B1); мутационный аудит diff'а — 0 missed на границах
(autonomy_level 9/10 и 0.99/1.0, streak_of строгое сравнение,
class_groups сортировка, parse_escape_line восстановление префиксов).

### 9.25.1 — аудит зоны B1 + правило про unviable (2026-10-20)

Скоп: только новые файлы B1 (`core/escapes.rs` + `commands/escapes.rs`),
`cargo mutants --in-place --test-tool nextest --baseline=skip --timeout-multiplier 2`
(профиль incremental=true, `~50с/мутант`):

- **Прогон 1: 130 мутантов → 129 caught / 0 missed / 1 unviable** (kill-rate
  99.2% testable). Единственный unviable — `escapes.rs:98 replace
  autonomy_level -> Default::default()` (genre FnValue): у `Level` не было
  `Default` → билд падал (process_status Failure 101).
- **Лечение (правило заказчика): unviable = дыра в покрытии, лечим КОД** —
  `#[derive(Default)]` + `#[default] L0` на `Level`. Повторный прогон с
  `--iterate` (85 мутантов — пере-бежались все, кроме ранее пойманных):
  **85/85 caught, 0 missed, 0 unviable**. Итог по зоне — чисто.
- Instrument-факт по cargo-mutants 27.1.0 (`--help`): лечения/замены
  мутантов НЕТ; есть только скрытие — `-e/--exclude <GLOB>`,
  `-E/--exclude-re <RE>` (регексп по именам из `--list`), `--skip-calls`,
  `--list`/`--list-files`; в конфиге — `exclude_globs`/`exclude_re`/
  `skip_calls`. Рабочий «ремоунт» — правка кода + `--iterate`.
- Инструмент наблюдения: `devharness/tools/mutants-watch.sh` (в коммите B1) —
  event-based (inotify на outcomes.json, таймаут-тик 5с как страховка),
  одна строка прогресса: бар с `done/total` внутри (заливка фоном, цифры
  ярким), `✓ ✗ ⏱ ⊘` (caught / missed / timeout / unviable), среднее на
  мутант, ETA в «мин», текущий мутант из свежего `.diff`. `NO_COLOR`, `--once`,
  троттлинг в не-tty. Заменяет слепые sleep-петли (каждый poll = turn).

---

## 9.26 — E-1: E2 (грамматика конвертации) + E4 (здоровье журнала) (2026-10-21, main)

Зона: `core/escapes.rs`, `commands/escapes.rs`, `commands/merge.rs`,
`commands/doctor.rs`, `tests/integration.rs` (s47b-фикстура + s47f/s47g),
`devharness/test.sh` (47f + проверка подсказки в 47a).

**E2.** `const CONVERTS: [&str; 3] = ["гейт:", "стаб:", "принял риск:"]`,
`is_debt = !CONVERTS.iter().any(|p| r.starts_with(p))` после `trim() +
to_lowercase()`. RED: `debt_is_default_free_text_is_debt` — падал на «ок»
(старый контракт: debt только у пустой/прочерковой/«нет»). Грабля плана
№1 сработала буквально: фикстуры `"стаб"`, `"гейт NO-IGNORED"`,
`"стаб-кандидат №14"` молча стали долгом → 2 теста ушли в L0; миграция —
замена в фикстурах на грамматику (зафиксировано как смена контракта).

**E4.** `LogHealth{headers, dated}` + `log_health()` + `undated()` +
`distrust()` (undated*2 > headers, строго; пустой лог → false).
`streak_of -> (u32, bool)`, `aggregate -> 4-элементный кортеж`.
Предупреждение в трёх местах одной строкой: `devh escapes` (⚠ строка после
streak), `devh merge` (перед `level_line`), `devh doctor` (soft-warn в блоке
flaky — чистые функции общие).

**Две ошибки моих ожиданий (не кода):** заголовок `## W-002 — b` без даты —
заголовок (headers++), undated=1; и 1/2 без даты — ровно половина = доверие
целое. Правило: ожидания писать ПО границе, а не по наитию.

**Числа приёмки:** unit 208 + integration 56; test.sh 437 PASS / 0 FAIL
(+6 проверок в 47); fmt `-p devh` чистый. Clippy: 11 ошибок expect() в
`run.rs`-тестах — предсуществующие (песочница с forbid-линтами, не гейт).

**Мутационный аудит зоны E-1 — СДАН (2026-10-21):** дифф
`git diff 4c55bfa beef7f4` (8 файлов, лог `/tmp/e1_audit.log`), прогон
`cargo mutants --in-place --test-tool nextest --baseline=skip
--timeout-multiplier 2 --in-diff /tmp/e1.diff -p devh
--config devharness/cargo-mutants.toml` → **96 мутантов** (`Found 96
mutants to test`). Счёт по `outcomes.json` (`o['summary']`): **96
CaughtMutant / 0 Missed / 0 Timeout / 0 Unviable, kill-rate 100.0%** —
вся зона = `core/escapes.rs`, мутанты `is_debt → true/false`, `>` ↔ `>=`
и граница 50% distrust убиты тестами. Средняя скорость 0.4 мин/мутант.
Грабля сессии: `outcomes.json` пишется инкрементально (`total_mutants`
растёт по мере тестов) — «96 mutants» берём из строки `Found N mutants to
test` в логе, не из джейсона.

## §9.23 suite-логирование: ре-экзек вместо exec-подстановки (урок 30-минутного виса)

- test.sh теперь пишет ПОЛНЫЙ поток прогона в `.harness/logs/test-<UTC>.log` (каталог исключён локально через .git/info/exclude — в git не течёт), fail-имена дополнительно синхронно в `test-failures.log` (без pipe-гонок), симлинк `test-latest.log`, финальный блок печатает ПАДЕНИЯ поимённо. Хвост `tail -15` больше не может потерять имена.
- ГРАБЛИ (стоило 30 прогон-минут): первая версия логирования была `exec > >(tee -a "$LOG") 2>&1`. В bash 5.2 процесс подстановки регистрируется как job, и ПЕРВЫЙ же голый `wait` в скрипте (секция 8 «параллельные new») ждёт tee вечно — suite молча висел доTool-timeout. Проверка гипотезы за 8 секунд: `bash -c 'exec > >(tee /tmp/x); sleep 1 & wait; echo DONE'` → 124.
- ПРАВИЛЬНЫЙ паттерн: guard + ре-экзек себя через pipe в подоболочке: `[ guard ] || { "$0" | tee -a "$LOG"; exit "${PIPESTATUS[0]}"; }` — свои потоки не трогаем, `wait` честный, rc дочернего прогона через PIPESTATUS. Микротест: sleep 1 & wait → WAITED-OK мгновенно, rc=3 сохраняется.
- ВТОРАЯ грабля: `smoke_reset_env()` (секция 47*) делает unset ВСЕХ `HARNESS_*` и `SMOKE_*` — переменные suite'а с таким префиксом умирали до финального echo (`unbound variable`, rc=1 при FAIL=0). Переменные suite'а названы вне этих namespace: `SUITE_REEXEC`, `SUITE_LOG`, `SUITE_LOG_DIR`, ручка каталога — `TEST_LOG_DIR`.
- Isolation-репро секции 8 (параллельные new) отдельно проходил за секунды — не список в suite, а о-инструментария. Урок: воспроизводить секцию изолированно ДО полного прогона.

## §9.24 F-0.3 review-hardening: единый diff, размер спеки, дайджест дерева

Что сделано (план `docs/F_series.md` F-0.3, дыры N2 «ревьюер слепо читает урезанный дифф» и N3 «ревьюер = второй запрос тому же агенту → может править дерево мимо гейтов»):

- **Один источник диффа.** `shell/git.rs::diff_text(wt, scope) -> Result<String>` (`git diff HEAD` + `git add -N .`, pathspec-scope) заменил `review_diff` (та молча делала `.lines().take(400)` и `.unwrap_or_default()`). Ошибка git → `Err` (fail-closed), вызывающий гейт даёт `EnvBlocked`, а не «диффа нет — пропуск». Тот же источник кормит и будущий детектор ослабления F-2 — их взгляды на «что менял агент» не могут разойтись.
- **Размер = блокер СПЕКИ.** `run.rs::review_size_verdict(lines, max)`: `max==0` — без лимита (аварийный клапан), `lines<=max` — Pass, иначе `SpecBlock Gate::Review` rc=1 «дроби задачу». Это НЕ `Fail` (не работа агента) и не `Env`: чинит человек, дробя спеку; fix-раунды её не размельчат. Конфиг `REVIEW_DIFF_MAX` (код-дефолт 2000, env `HARNESS_REVIEW_DIFF_MAX`, шаблон init), CONFIG_KEYS 37→38.
- **Дайджест дерева до/после ревью.** `run.rs::post_review_guard(before, wt)`: sha256-дайджест до ревью и после — расхождение = `EnvBlocked «ревьюер изменил дерево»` rc=2, сильнее любого `VERDICT: OK`. Чистое ядро — `core/digest.rs`: `tree_digest(entries)` (сортировка по байтам пути, **length-prefix-кадрирование** sha256 — иначе («a»,«bc») и («ab»,«c») сольются в один хэш, и подмена содержимым проходит; дубль пути → `Err` fail-closed, молча не схлопываем), `parse_ls_files_z` (NUL-парс + дедуп: `ls-files -c -o` после `git add -N .` печатает путь дважды, молча это ломало бы дайджест), `mode_from_permissions` (exec-бит → 755). IO-обвязка `git::tree_entries/tree_digest`: `git ls-files -z -c -o --exclude-standard`, симлинк через `symlink_metadata` (свой режим 0o120000, содержимое = цель).
- **Границы модели (записано намеренно).** Дайджест не видит игнорируемое (`--exclude-standard`): агент, положивший логику в `.gitignore`-путь, невидим и для дайджеста, и для FREEZE — это честная граница покрытия, не дыра ревью (ревьюер смотрит дифф, дифф тоже не показывает игнорируемое). intent-to-add-индекс не шумит: `f03_tree_digest_ignores_intent_to_add_index_noise` — `git add -N .` не должен ломать дайджест (иначе честное ревью стало бы env-блокером).
- **Класс вердикта.** Превышение → Spec (человек), правка дерева из ревью → Env (среда/процесс), не Fail: это отличает «спека велика» и «работа мимо гейтов» от «агент плохо написал код» (ранняя маршрутизация `decide_round` уже различает Fix/Env/Spec — rc 1/2/1).
- **Приёмка.** test.sh **477 PASS / 0 FAIL**; секция 44f стаб-атака `review-writes.sh` (агент честно чинит calc.sh, а на ревью-промпте «заодно» пишет `src/review-sneak.sh` + `VERDICT: OK`) → rc=2, маркер «ревьюер изменил дерево», `review-sneak.sh` в main не уходит. Юнит-границы: `==MAX` Pass / MAX+1 Spec / `0` без лимита, `post_review_guard_flags_tree_change`, `f03_tree_digest_changes_on_edit_and_on_untracked` (правка/откат/untracked/chmod), `f03_diff_text_scoped_sees_only_scope`.
- **Грабли сессии.** (1) Первый полный прогон suite дал 24 «падения» — все один корень: параллельная сессино cargo-mutants подняла load 9.7→8.2, и **load-гейт** (`core/load.rs`) честно резал мутационный гейт. Повтор с `HARNESS_MUTATE_MAX_LOAD=off` — 477/0. Урок: suite детерминирован при спокойной машине; contention-флейк ловить по одинаковой подписи «load N > порог», не по именам тестов (имя — `test.sh` логирует, §9.23). (2) sha2 0.10 API — `finalize()`, не `finish()`. (3) `clippy::indexing_slicing` в песочнице: в `windows(2)` использовать паттерн `if let [l, r] = w`, не `w[0]/w[1]`.
- **Мутаудит дельты** (cargo-mutants 27.1.0, `--in-diff /tmp/f03.diff`, in-place, nextest): 43 мутанта на F-0.3-строки (digest 16, run.rs 14 включая `review_size_verdict`/`post_review_guard`, git.rs 9, mod/init 4). Вердикт — по outcomes.json; канонический конфиг `devharness/cargo-mutants.toml` исключает `commands/** shell/**` (их покрывает integration-сьют), поэтому дельта-ядро отдельно гонялось БЕЗ конфига — проверить, что новые юнит-тесты (`review_diff_over_max_is_specblock`, `post_review_guard_flags_tree_change`, `f03_tree_digest_*`) убивают своих мутантов, а не греются.
