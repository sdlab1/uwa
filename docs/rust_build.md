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
