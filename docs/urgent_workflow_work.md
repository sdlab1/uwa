# СРОЧНАЯ РАБОТА: единый детерминированный workflow (агент ↔ харнесс)

Статус: **в работе** (2026-10-22). Это вне очереди F-серии: сначала довести
до ума единый вход (CLI + документация + память), потом — F-1…F-8/K1,
потом — E-серия.

Причина «вне очереди»: пока в репозитории жили два конкурирующих входа
(`devharness/tools/mutants-watch.sh` вручную + `cargo-mutants` руками) и
наблюдение, которое нельзя вывести из кода — оператору приходилось
напоминать правило каждый раз. Правило обязано быть **в коде, в справке CLI,
в документации и в памяти агента** — тогда напоминать не нужно.

Документ состоит из двух частей:

* **ЧАСТЬ A — КАНОН.** Что делать, в каком состоянии, какой ровно командой.
  Не обсуждается. Это спецификация, а не заметки.
* **ЧАСТЬ B/C/D — РЕАЛИЗАЦИЯ И ПРИЁМКА.** Что и как чинить в коде, чтобы
  канон стал правдой.

## Текущий статус (2026-10-23)

**Сделано и проверено:**

* Части B/C реализованы: нативный `devh mutate` (start/status/watch/problems/
  verdict/stop), событийный `core/watch.rs`, оракул `devh next`,
  `devh test`, `devh workflow`; удалены второй watcher и `MUTATION_WORKFLOW.md`.
* Найдена причина «аудита без результата»: супервизор наблюдал
  `.harness/mutate` и сам писал туда `live.log` → inotify-событие → новый
  кадр → новое событие. `live.log` вырос до 1.47 MB за секунды, супервизор
  умер, `state.json` остался `running` (aborted). Исправлено: каталог
  артефактов больше не под наблюдением, `stop.requested` проверяется на
  каждой итерации (включая `Timeout`), а не только внутри `Changed`.
* `status`/`watch` теперь fail-closed при оборванном аудите: не печатают
  кадр с kill-rate 100% и «вердикт: чисто», возвращают rc=2 и отправляют
  в `devh next` (перезапуск тем же прогоном). В `status --json`
  `blocks_clean=true` при aborted.
* `docs/mutation_workflow.md` регенерирован из `devh workflow` (убраны
  случайные строки сборки в начале).
* Проверки: `cargo test -p devh --lib` — **390 passed / 0 failed**;
  `cargo test -p devh --test integration` — **69 passed / 0 failed**;
  `rustfmt --check` по своим изменённым файлам — чисто.
* `cargo fmt --check` по всему пакету по-прежнему красный из-за чужих
  файлов (`core/coverage.rs`, `shell/msg.rs`) — их не трогаем (чужая зона).

* Исправления закоммичены: `494d0b3` (14 файлов). Аудит перезапущен в
  copy-режиме с быстрым профилем (`CARGO_PROFILE_TEST_INCREMENTAL=true`,
  `CARGO_PROFILE_TEST_OPT_LEVEL=0`): ETA упала с ~5 ч до ~1 ч.
* **Поток проблем включён и используется:** пока аудит идёт, супервизор
  пишет `problems.json` на каждом событии; `devh next` уже отдаёт
  `devh mutate problems` при `live_problem_count > 0`. Ждать `verdict`
  не нужно — первая actionable-проблема сразу уходит в работу. Правило
  и реализация зафиксированы в `docs/F_series.md` §F-1.8. Текст `next`
  усилен: «чинить сразу, не ждать verdict (copy-режим: правки дерева
  безопасны)»; добавлен тест на этот контракт.
* **F-1 Tripwire в дереве:** `core/tripwire.rs` (13 тестов) +
  `shell/tripwire.rs` (3 теста: tamper → check → rollback → чисто,
  warn/refs, resume/corrupt). Подключены `Gate::Tripwire`, `TRIPWIRE`/
  `PROTECTED`, arm после RED и проверки после агента/гейтов/red-post/
  RED-MUT/перед коммитом. `cargo check` зелёный; закоммичено `a1679ea`.
* **Живые проблемы аудита закрыты тестами:** `Outcome::is_succeeded`
  (+`is_survivor/timeout/action`), `parse_outcomes` с пустым `file`,
  `genre_tag` (все ветки) и `civil_from_days` (эпоха/високосные/вековые
  правила) получили точные кейсы; targeted `core::survivors` — 31/31.
* **Протухание снимка обнаружено на практике:** после этих правок старый
  аудит продолжал показывать 12 «проблем» по уже исправленным строкам.
  Это не новые дыры, а copy-снимок ДО фикса. Аудит остановлен и
  перезапущен (pid 302941) по новому дереву; старые блоки не чиним
  повторно.
* **F-1.8.3 сделана:** `devh mutate recheck <name>` — один мутант по имени,
  отдельный `--output`, overlay `rechecked.json`; `problems/status` снимают
  только `caught` на текущем дереве; `verdict` overlay не доверяет.
* **F-1.8.2 сделана:** `devh mutate watch --until-problem` — блокирующий
  канал: первая проблема → rc=1 сразу, `--batch-ms N` копит похожие
  событиями, без sleep; тест `section00i_watch_until_problem_exits_on_first_problem`.
* **F-1.8.1 сделана:** `State.tree_stamp` (sha256 дерева без docs/*.md),
  `audit_stale`; `status`/`problems` помечают `stale`, `next` при stale
  ведёт к `stop` (Running) или перепрогону (Finished), `verdict` — rc=2.
  Тесты `audit_stale_detects_tree_change_and_tolerates_old_state`,
  `stale_audit_stops_or_restarts_instead_of_chasing_problems`; lib 413/413.
  Закоммичено `baf8b11`. Свежий аудит на новом дереве: **228/228, 0 проблем,
  kr 100%, `devh mutate verdict` rc=0** (07:11).

**Осталось до закрытия части D:**

* F-1/F-1.8.1/F-1.8.2/F-1.8.3 и N4 закоммичены; `devh test` — **PASS 477 /
  FAIL 0**, `devh mutate verdict` — **rc=0** (228/228).
* Добавлен `devh test --bg` / `--status`: полный сьют больше не блокирует
  агента; лог сьюта — `.harness/logs/test-<ts>.log`, фон — `test-bg.log`.
* Осталась run-level приёмка tamper-сценариев — полировка F-1; затем F-2.
* 12 старых «проблем» `survivors.rs` закрыты тестами; свежий аудит их не
  подтверждает, отдельно чинить нечего.

---

# ЧАСТЬ A. КАНОН

## A0. Источник истины

| Ранг | Источник | Что это |
| --- | --- | --- |
| 1 | **код** (`devharness/src/**`) | единственный источник истины; всё остальное — его projections |
| 2 | `devh workflow` (печать канона из кода) | то, что агент читает, чтобы узнать правила |
| 3 | `devh mutate help`, `devh test --help` | локальные справки команд |
| 4 | `docs/mutation_workflow.md`, `AGENTS.md` | человекочитаемые копии канона (генерируются из кода, тест сверяет) |
| 5 | память goose (`devh-mutation-rule`) | **производная**: страховка от потери контекста, не источник |

Правило: **если память агента противоречит `devh workflow` — правда `devh workflow`.**
Память не обновляется вручную «по памяти правила»: обновляется при смене канона.

## A1. Глобальный контракт кодов возврата

Единый для **всех** команд `devh`. Агент не парсит stdout — он читает код.

| Код | Смысл | Что делает агент |
| --- | --- | --- |
| **0** | успех / чисто | идти дальше по автомату (A4) |
| **1** | **есть проблема, требуется действие агента** | чинить код/тесты по содержимому хвоста |
| **2** | **состояние среды / не завершено** (нет инструмента, аудит ещё идёт, нечего проверять) | **код НЕ чинить**: доустановить / дождаться / проверить `devh next` |
| **3** | **внутренняя ошибка харнесса** (баг) | не чинить код проекта; записать факт в отчёт |

Разница 1 и 2 — принципиальна: `1` = «твоя работа не сделана», `2` = «пока нельзя
сделать» (ждём/ставим/смотрим). Смешивать их нельзя: иначе агент «чинит код»
из-за отсутствующего `cargo-mutants`.

## A2. ЖЁСТКИЕ ПРАВИЛА (нарушение = брак)

| # | Правило | Где закреплено |
| --- | --- | --- |
| R1 | Мутационный аудит запускается **только** через `devh mutate`. Руками `cargo mutants` — НЕЛЬЗЯ | `devh mutate help`, `devh workflow`, `AGENTS.md`, память goose |
| R2 | `devharness/tools/mutants-watch.sh` — **удалён**, без обёртки и без второго входа | `git rm`, тест-контракт `section00b` |
| R3 | `/home/user/project_uwa/MUTATION_WORKFLOW.md` — **удалён** (второй источник истины) | `git rm` |
| R4 | Пойман (`killed`) — **только** `CaughtMutant`. `MissedMutant` и `Timeout` — открытые вопросы (руководство к действию), `Unviable` — **провал** (ноль терпимости) | `core/mutaudit.rs` — единственный источник истины |
| R5 | Пока есть выживший/таймаутный/unviable/неопознанный — финальный вердикт **не может** быть «чисто» | `Audit::has_hole`, `gates::judge_mutation`, `devh mutate verdict` (rc=1) |
| R6 | `Unviable` не «граница мутатора», а дыра: чинить исходник/типы/тесты, пока мутант не собирается | гейт падает, fix-раунд |
| R7 | Живой вывод — **нативный**, событийный (inotify), без `sleep`-цикла в агенте и без внешнего скрипта | `commands/mutate.rs`, `core/watch.rs` |
| R8 | Сьюты запускаются **только** через `devh test` (полный лог + итог PASS/FAIL в stdout и в коде возврата) | `commands/test.rs`, `AGENTS.md` |
| R9 | **Git принадлежит харнессу.** Агент не выполняет `git add/commit/push/rebase/reset/checkout/stash` в репозитории харнесса | `devh commit`, `devh merge`; тест-сканер запретов |
| R10 | **Одна зона аудита за раз.** `mutants.out` — один на прогон; два одновременных аудита в одном репо = мусор | `devh mutate start` отказывает, если аудит уже идёт |
| R11 | **Фоновый аудит не трогает дерево агента.** `--in-place` — только синхронно (внутри гейта) | канон фоновых прогонов: без `--in-place` либо в отдельном worktree |
| R12 | **Артефакты аудита — read-only для агента.** Правка `caught.txt`/`missed.txt`/`outcomes.json` руками = читерство, результат обнуляется | `devh mutate start` чистит артефакты перед прогоном |
| R13 | **Никаких «ручных» команд в bash.** Всё, что нужно от харнесса, делается его подкомандами; в bash — только `cd`, `cat`/`ls` результата и вызов `devh` | `devh workflow` (список запретов A8) |
| R14 | **Не гадать.** Если непонятно, что делать дальше → `devh next` печатает ровно одну следующую команду | `commands/next.rs` |

## A3. Оракул: `devh next` (главный недостающий элемент)

Пока агент сам решает «что дальше» — он будет гадать. **Оракул — одна
команда, которая печатает ровно одну следующую команду и одну строку «почему».**

```
$ devh next
W-001: гейт mutation упал (3 выживших мутанта) → чинить по списку проблем
devh mutate problems
```

Контракт `devh next`:

* печатает **ровно одну** исполняемую команду (без плейсхолдеров вида `<id>` —
  id подставляется реальный);
* код возврата **всегда 0** (это подсказка, а не проверка);
* если следующего шага нет (всё зелёное) — печатает `готово: <что завершено>`.

## A4. Конечный автомат состояний

Один цикл работы агента = проход по этой таблице. Возврат назад = новый item
или fix-раунд (внутри `devh run`).

| ID | Состояние (как обнаружить) | СЛЕДУЮЩАЯ КОМАНДА (ровно одна) | Что делает агент | Код |
| --- | --- | --- | --- | --- |
| **S0** | нет `.harness/config` (`devh doctor` → «нет config») | `devh init` | инициализация репо | 0 |
| **S1** | спека есть, item создан, агент не запускался (`devh list`) | `devh run <ID>` | работа агента + гейты + фиксы | 0/1/2 |
| **S1b** | то же, но нужен фон (долгий item) | `devh bg <ID>` | detached-прогон; наблюдение — `devh ps` | 0 |
| **S2** | идёт агент/прогон (`devh ps` → живой pid) | `devh logs <ID>` | читать хвост, **не** вмешиваться | 0 |
| **S3** | гейт упал, fix-раунд пошёл | (внутри `devh run`) | агент чинит по хвосту лога — промпт уже несёт список | — |
| **S4** | гейты зелёные, нужен коммит | `devh commit <ID>` | коммит делает **харнесс** | 0/1 |
| **S5** | коммит есть, не смержено | `devh merge <ID>` | rebase → ff-push → лог | 0/1/2 |
| **S6** | мутационный аудит идёт (`devh mutate status` → alive) | `devh mutate problems` | **работать параллельно**, чинить выживших | 1 (если есть) |
| **S7** | аудит завершён, есть дыра (`devh mutate verdict` → 1) | `devh mutate problems` | чинить по списку, потом **перепрогнать** | 1 |
| **S8** | аудит завершён, чисто (`devh mutate verdict` → 0) | `devh commit <ID>` / `devh merge <ID>` | продолжать цикл | 0 |
| **S9** | ничего не начато, но есть спеки в `specs/` | `devh auto --from specs/ --max 5` | автопилот | 0/1 |
| **S10** | лимит фиксов исчерпан (`decide_round` → GiveUp) | `devh abort <ID>` (или `devh hint <ID>`) | списать/переформулировать item | 1/2 |
| **S11** | мусор: старые worktree/логи | `devh clean` / `devh sweep --maxsize 20G` | уборка | 0 |

Переходы между состояниями **не обсуждаются**: таблица A4 — это и есть
маршрут. Если текущее состояние не определяется — `devh next` определит.

## A5. Каталог команд (точные формы)

### A5.1 Мутации (единственный вход — `devh mutate`)

| Команда | Что делает | Что печатает | Код |
| --- | --- | --- | --- |
| `devh mutate help` | канон мутаций | текст правил | 0 |
| `devh mutate start <args>` | запустить аудит; **setsid**-супервизор, cargo-mutants его ребёнок; чистит старые артефакты | pid, путь лога, «дальше: status/watch/verdict» | 0 / 2 (идёт) / 1 (нет флагов) |
| `devh mutate status [--json]` | мгновенный снимок, ничего не ждёт | легенда + пилюля % + 4 счётчика + kill-rate + ETA + текущий мутант + список проблем | 0 / **1** (есть проблемы) / 2 (аудита нет) |
| `devh mutate watch [--timeout N]` | живой CLI: одна строка, ширина терминала, обновление по событиям | живая строка + финальный отчёт | 0 |
| `devh mutate problems [--json]` | **руководство к действию**: что чинить | по одному блоку на проблему: файл:строка, замена, жанр, исход | 0 / **1** (есть проблемы) / 2 (аудита нет) |
| `devh mutate verdict` | финальный вердикт (гейт) | отчёт + вердикт | 0 чисто / **1** дыра / 2 не закончен |
| `devh mutate stop` | остановить идущий аудит (супервизор → группа cargo-mutants) | «остановлен» | 0 / 2 (нечего останавливать) |

Типовые вызовы (аргументы `start` — **флаги cargo-mutants, как есть**):

```
devh mutate start --config devharness/cargo-mutants.toml
devh mutate start --in-diff /tmp/delta.diff --test-tool nextest --baseline=skip --timeout-multiplier 2
```

### A5.2 Сьюты (единственный вход — `devh test`)

| Команда | Что делает | Что печатает | Код |
| --- | --- | --- | --- |
| `devh test` | полный `devharness/test.sh`, **полный лог** в `<store>/logs/test-<ts>.log` | живой прогресс секций + итог `PASS: N` / `FAIL: M` + путь лога | 0 (0 FAIL) / 1 (есть FAIL) / 2 (среда) |
| `devh test --case N` | один кейс/секция (точечная проверка) | лог секции + итог | 0/1/2 |
| `devh test --list` | список кейсов | список | 0 |

Канон: полный прогон сьютов = `devh test`. `bash devharness/test.sh` руками —
**не нужно** (лог всё равно пишется, тот же exit code доступен).

### A5.3 Остальной цикл (уже существует, канон зафиксирован)

| Команда | Когда | Код |
| --- | --- | --- |
| `devh init` | один раз на репо | 0/1 |
| `devh new "<title>"` (спека на stdin, строка `RED: …`) | новая задача | 0/1 |
| `devh auto --from specs/ --max N` | автопилот по спекам | 0/1/2 |
| `devh bg <ID>` / `devh bg auto --from specs/ --max N` | фоновый прогон | 0 |
| `devh ps` | что живо сейчас | 0 |
| `devh logs [ID]` | логи прогона (хвосты) | 0 |
| `devh list` | список items | 0 |
| `devh hint [ID]` | подсказка по item (следующий шаг по спеке) | 0 |
| `devh commit [ID]` | коммит (делает харнесс) | 0/1 |
| `devh merge [ID]` | rebase → ff-push в main | 0/1/2 |
| `devh abort [ID]` | списать item | 0/1 |
| `devh clean` / `devh sweep --maxsize 20G` | уборка worktree/логов/диска | 0 |
| `devh doctor` | диагностика окружения | 0/1/2 |
| `devh survivors [--rules]` | классы слепоты (после мутаций) | 0 |
| `devh escapes` | ledger побегов | 0 |
| `devh tickets --import` | очередь наблюдений → тикеты | 0 |
| `devh next` | **оракул**: одна следующая команда | всегда 0 |
| `devh workflow` | печать полного канона (A2+A8) | 0 |

## A6. Протоколы (сценарии)

### P1. Закрыл зону кода → мутации

```
1. devh mutate start --config devharness/cargo-mutants.toml     # ядро
   (или: devh mutate start --in-diff <file.diff> --test-tool nextest --baseline=skip --timeout-multiplier 2)
2. devh mutate status          # снимок; rc=1 → уже есть проблемы
3. devh mutate watch           # живой CLI (по желанию; можно не запускать)
4. devh mutate verdict         # rc=0 — только теперь «чисто»
```

Правила: **не ждать конца**, если уже есть проблемы (шаг 2 даёт rc=1) —
иди чинить (P2). Аудит при этом продолжает идти.

### P2. Появился выживший / таймаут / unviable → чинить СЕЙЧАС

```
devh mutate problems           # rc=1, если есть; блок на каждую проблему
```

Блок проблемы (идёт в fix-промпт агента как есть):

```
✗ выжил (дыра в тестах): core/escapes.rs:249 — replace x with None [genre: FnValue]
⏱ таймаут тест-фазы (пойман не был): core/plan.rs:12 — replace y with 0 [genre: BinaryOperator]
⊘ не компилится (дыра в покрытии): commands/run.rs:88 — replace z with false [genre: MatchArmGuard]
? неопознанный исход: core/x.rs:5 — summary=WeirdThing (инструмент/парсер требует внимания)
```

Маршрут по типу проблемы (не выбирается на глаз — из типа):

| Тип | Что делать | Чем закрывается |
| --- | --- | --- |
| `✗ выжил` | написать тест, который ловит мутацию | новый тест → мутант становится `CaughtMutant` |
| `⏱ таймаут` | тест-фаза не уложилась: найти зависание/долгий тест, ускорить, выставить `--timeout-multiplier` | мутант доходит до `CaughtMutant` |
| `⊘ unviable` | мутант не компилируется: чинить типы/исходник, чтобы мутация собиралась | мутант становится viable и ловится тестом |
| `? неопознанный` | баг контракта/инструмента: проверить версию cargo-mutants и парсер | исход станет известным |

После фикса: **перепрогон** (`devh mutate start ...`) — счётчики нельзя
«дописать» руками (R12).

### P3. Аудит идёт, а агенту надо работать

Фоновый аудит **не должен** быть `--in-place` в дереве агента (R11):
либо без `--in-place` (copy-режим), либо в отдельном worktree.

Пока аудит идёт — агент продолжает свою задачу и периодически (по событию,
а не по `sleep`) проверяет `devh mutate status` / `devh mutate problems`.

### P4. Аудит не идёт / упал

| Симптом | Команда | Что дальше |
| --- | --- | --- |
| `status` → «аудит не запущен» | `devh mutate start ...` | запустить |
| `status` → прогон идёт, но кадр не меняется | `devh mutate watch --timeout 60` | посмотреть; если завис — `devh mutate stop` |
| `verdict` → rc=2 «ещё идёт» | `devh mutate status` | ждать (или работать параллельно) |
| `verdict` → rc=2 «аудита нет» | `devh mutate start ...` | запустить |
| супервизор мёртв, state=`running` | `devh mutate stop` → `devh mutate start ...` | перезапуск |
| `cargo mutants` не установлен | `devh doctor` | установить (rc=2, **код не чинить**) |

### P5. Сьюты

```
devh test                       # полный прогон, полный лог, PASS/FAIL в stdout
devh test --case 44             # точечно
```

rc=1 → читать **лог из вывода `devh test`** (путь печатается), не искать
файлы руками.

### P6. Застрял

| Симптом | Команда | Код |
| --- | --- | --- |
| лимит фиксов исчерпан | `devh abort <ID>` (или переформулировать спеку → новый item) | 1 |
| гейт-блокер среды (нет инструмента) | `devh doctor` | 2 |
| непонятно, что делать | `devh next` | 0 |
| мусор в репо | `devh clean`, `devh sweep --maxsize 20G` | 0 |

### P7. Мутационный гейт внутри `devh run`

Гейт `MUTATE` (в `commands/run.rs`) — это **тот же** аудит, но
синхронный и in-place (в worktree агента):

* упал → `Verdict::Fail` с хвостом из **списка проблем** (файл:строка,
  замена, жанр) → `prompt::fix_prompt` → агент чинит **в том же раунде**;
* `unviable`/`timeout` → `Fail` **всегда** (R5/R6), даже при высоком kill-rate;
* rate считается по `caught` **только** (R4).

## A7. Матрица «симптом → команда» (анти-гадание)

| Что видит агент | Команда | Код |
| --- | --- | --- |
| «с чего начать?» | `devh next` | 0 |
| «хочу проверить мутации» | `devh mutate start <args>` | 0 |
| «идёт ли аудит?» | `devh mutate status` | 0/1/2 |
| «что чинить?» | `devh mutate problems` | 0/1 |
| «всё ли чисто?» | `devh mutate verdict` | 0/1/2 |
| «прогон тестов» | `devh test` | 0/1 |
| «где лог прогона агента?» | `devh logs <ID>` | 0 |
| «кто сейчас работает?» | `devh ps` | 0 |
| «застрял» | `devh next` → `devh doctor` | 0/2 |
| «мусор / старые worktree» | `devh clean` / `devh sweep` | 0 |
| «надо закончить задачу» | `devh commit <ID>` → `devh merge <ID>` | 0/1 |
| «выжившие мутанты → правила» | `devh survivors` | 0 |

## A8. ЗАПРЕЩЁННЫЕ ДЕЙСТВИЯ (жёсткий список)

| НЕЛЬЗЯ | Почему | ПРАВИЛЬНО |
| --- | --- | --- |
| `cargo mutants ...` руками | второй вход, семантика исходов расходится, обходит гейт и логи | `devh mutate start <args>` |
| `bash devharness/test.sh` | нет единого exit code/итога, агент импровизирует разбор лога | `devh test` |
| `devharness/tools/mutants-watch.sh` | удалён (R2); скрипт = второй вход с собственной семантикой | `devh mutate watch` |
| `sleep 60; <команда>` | цикл опроса вместо события; агент «спит» и не реагирует на проблемы | `devh mutate watch` (событийный) / `devh mutate status` |
| `while pgrep -x cargo-mutants; do sleep 5; done` | то же, только в bash | `devh mutate watch --timeout N` / `devh mutate stop` |
| `nohup cargo mutants … &` / `setsid … &` | агент управляет процессом вручную, теряется лог/вердикт | `devh mutate start` (сам делает setsid) |
| `pkill -f 'cargo mutants'` | **не матчит** `cargo-mutants` (дефис!) — ложная уверенность | `devh mutate stop` |
| `git add/commit/push/rebase/reset/checkout/stash` | git принадлежит харнессу (R9) | `devh commit`, `devh merge` |
| `cat .harness/logs/*.log` руками | путаница в логах, агент теряет «источник правды» | `devh logs <ID>` / `devh test` (печатает путь) |
| `cat mutants.out/outcomes.json` + свой парсер | второй парсер = второй источник истины | `devh mutate problems [--json]` |
| правка `caught.txt` / `missed.txt` / `outcomes.json` | читерство (R12) | `devh mutate start` заново |
| `python3 -c '...'` для разбора JSON | самодеятельность (R13) | `devh ... --json` |
| `kill -9` по pid из `ps` | харнесс не знает, что убито; остаются «мёртвые» pid-файлы | `devh mutate stop`, `devh ps` |
| запуск мутаций в чужом репо/чужой worktree без `--in-diff`/`--config` | аудит всей репы = 900+ мутантов и 2 часа | зона аудита = дельта (§A10) |

**Разрешено** в bash: `cd`, `ls`, `cat`/`head`/`tail` файла, который напечатала
команда `devh` (лог), запуск `devh …`, `cargo build`/`cargo test` **своей
правки** в своей worktree (это то же, что делает VERIFY), `git status`
(read-only, для самопроверки). Всё остальное — через `devh`.

## A9. Границы зоны аудита (что именно мутируем)

| Зона | Команда |
| --- | --- |
| ядро репо (канон, без команд/шеллла/тестов) | `devh mutate start --config devharness/cargo-mutants.toml` |
| дельта (только что изменённые файлы) | `devh mutate start --in-diff <file.diff> --test-tool nextest --baseline=skip --timeout-multiplier 2` |
| «всё подряд» | **не делается** (это 900+ мутантов; если нужно — по зонам) |

`--in-diff` берёт **файл** диффа, а не ref: `git diff <base> > /tmp/delta.diff`,
затем `--in-diff /tmp/delta.diff`.

## A10. Что ещё нельзя забыть (грабли, проверено)

* `cargo-mutants 27.1.0`: `--in-place` **несовместим** с `--jobs` → последовательно.
* Имена исходов 27.1.0: `CaughtMutant | MissedMutant | Unviable | Timeout`
  (НЕ `TimeoutMutant`/`UnviableMutant`; легаси парсится, но не порождается).
* После `--in-place` прогона: `git status --porcelain` (БЕЗ `| head`!) и
  `grep -rn '~ changed by cargo-mutants ~'` — мутант мог остаться в дереве.
* `git add` — только когда `pgrep -x cargo-mutants` пуст (иначе закоммитится мутант).
* `mutants.out/`, `target/` — в `.gitignore`, **не коммитить**.
* Свежая worktree без `.harness/config` → integration-тесты красные (шаблон
  MUTATE не распознан) → скопировать конфиг.
* Контренция: load ≥ 8–10 → мутации ×2–4 по времени и флейки; гонять на
  спокойной машине. `HARNESS_MUTATE_MAX_LOAD=off` — для сьюта на загруженной.
* `outcomes.json` переписывается целиком → читать в середине записи нельзя;
  живые счётчики берутся из append-списков.

---

# ЧАСТЬ B. СЕМАНТИКА ИСХОДОВ (ядро всего)

```
CaughtMutant  → РЕЗУЛЬТАТ. Единственный, закрывающий вопрос.
MissedMutant  → РУКОВОДСТВО. Дыра в тестах. Блокирует «чисто».
Timeout       → РУКОВОДСТВО. Пойман не был (тест-фаза не уложилась). Блокирует «чисто».
Unviable      → ПРОВАЛ. Не компилируется. Ноль терпимости.
Success       → baseline-прогон (не мутант) — не проблема, но и не «пойман».
Прочее        → НЕОПОЗНАННЫЙ исход → видимая проблема (не молчаливый «успех»).
```

Производные величины (везде одинаково):

```
killed    = caught
testable  = done - unviable        (таймаут в знаменателе остаётся)
kill_rate = 100 * caught / testable
unresolved= missed + timeout        → блокирует финальный вердикт
hole      = unresolved > 0 || unviable > 0 || unknown > 0
fatal     = unviable > 0
```

## B1. Карта правок

| Файл | Что сделать |
| --- | --- |
| `core/mutaudit.rs` | ✅ сделано: `killed()`, `testable()`, `unresolved()`, `kill_rate()` (только caught), `has_hole()` (missed+timeout+unviable+unknown), `fatal()`, `Bad{replacement,genre}`, `parse_problems()`, `action_items()` |
| `core/tui.rs` | kill-rate только по `caught`; легенда с семантикой («✓ пойман · ✗ выжил · ⏱ таймаут · ⊘ unviable») |
| `core/early.rs` | `LiveCounts::killed()/clean()` = **только caught**; DOOM-граница считает `missed+timeout` («непойманные»), а не только missed |
| `core/gates.rs` | `MutationStats` += поле `timeout`; парсер: `timeouts` → `timeout` (НЕ в killed); `judge_mutation`: unviable>0 → Fail; timeout>0 → Fail; rate = caught/testable |
| `core/survivors.rs` | принимать реальные имена cargo-mutants (`Timeout`, `Unviable`) + легаси; `Outcome::delta`: killed = только `CaughtMutant` |
| `core/killmap.rs` | killed = только `CaughtMutant`; комментарии про «таймаут = поимка» убрать |
| `commands/run.rs` | `MutStats`/`MutRun::Stats` += `timeout`; `finish()`: Fail при timeout/unviable/kill-rate; **problem-first** останов аудита; в хвост вердикта — список проблем (файл:строка + замена) |
| `commands/mutate.rs` | нативный CLI: `start/status/watch/verdict/problems/stop/help`; rc=1 при проблемах; событийный watch; supervisor пишет `problems.json` **вживую** |
| `core/watch.rs` (новый) | inotify-обёртка: `Watcher::new/add/wait(ms)` — события ФС вместо sleep-цикла |
| `commands/next.rs` (новый) | оракул A3: одна следующая команда по состоянию репо |
| `commands/test.rs` (новый) | `devh test`: полный сьют, полный лог, PASS/FAIL, exit code |

## B2. Приёмка семантики (тесты, обязательны)

1. `kill_rate` падает при timeout (9 caught + 1 timeout → 90%, не 100%).
2. `has_hole()` истинно при missed **и** при timeout **и** при unviable **и** при неизвестном исходе.
3. `fatal()` истинно только при unviable.
4. `judge_mutation`: unviable>0 → `Verdict::Fail`; timeout>0 → `Verdict::Fail`.
5. `parse_mutants_summary_line`: «42 mutants tested in 1m 30s: 10 missed, 5 caught, 3 unviable, 2 timeouts» → caught=5, timeout=2, missed=10, unviable=3 (не 7 killed!).
6. `devh mutate verdict` → rc=1 при выжившем/таймауте/unviable; rc=0 только когда **все** `CaughtMutant`.
7. `devh mutate problems` → rc=1 при проблемах, блок содержит файл:строку и замену.

---

# ЧАСТЬ C. РЕАЛИЗАЦИЯ

## C1. Нативный `devh mutate` — единственный вход

### C1.1 Подкоманды

| Команда | Что делает | Когда звать |
| --- | --- | --- |
| `devh mutate help` | канон: что запускать и когда | «что дальше?» |
| `devh mutate start <args>` | запустить аудит; **setsid**-супервизор, cargo-mutants его ребёнок | начать проверку зоны |
| `devh mutate status` | мгновенный снимок (легенда + пилюля + счётчики + проблемы); **rc=1**, если есть проблемы | «что сейчас?» / проверка агентом |
| `devh mutate watch` | живой CLI, событийный (inotify), одна строка обновляется на месте | смотреть прогресс в терминале |
| `devh mutate problems` | **руководство к действию**: список проблем (файл:строка, замена, жанр) | чинить мутанты |
| `devh mutate verdict` | финальный вердикт: 0 чисто / 1 проблемы / 2 не закончен | гейт перед мержем |
| `devh mutate stop` | остановить идущий аудит | «аудит завис / не нужен» |

### C1.2 Артефакты (`.harness/mutate/`)

`state.json` (state/pid/exit/started/args), `live.log` (кадры+отчёт),
`cargo.log` (полный stdout/stderr cargo-mutants), `verdict.json`
(машиночитаемый итог), `supervisor.log`, **`problems.json`** (список проблем
вживую — пишется супервизором по мере появления, не только в конце).

### C1.3 Событийность (без sleep)

- `core/watch.rs`: `inotify_init1(IN_NONBLOCK)` + `inotify_add_watch` на
  `mutants.out` (и на корень — поймать создание каталога) + `poll()`.
- Рендер происходит **по событию** (файл изменился), а не раз в секунду.
- Таймаут `poll()` (≈1 c) — только чтобы заметить завершение супервизора;
  это не «цикл со sleep», а ожидание события.
- Внешнего скрипта и `sleep` в командах агента — нет.

## C2. Проблема → промпт агента (fail-fast, параллельная работа)

Требование: как только появился выживший/таймаут/unviable — **ошибка сразу в
промпт**, агент начинает чинить **не дожидаясь конца аудита**.

### C2.1 Канал доставки

1. Супервизор пишет `problems.json` сразу, как увидел проблему (append-списки
   `missed.txt`/`timeout.txt`/`unviable.txt` + `outcomes.json` для деталей).
2. `devh mutate status`/`problems`/`watch` → rc=1 и список проблем.
3. Гейт `MUTATE` в `commands/run.rs`:
   - **свой** прогон (in-place, синхронно) → **problem-first**: при первой
     проблеме остановить аудит (SIGINT группе → cargo-mutants откатывает
     мутанта) и вернуть `Verdict::Fail` с хвостом из списка проблем →
     fix-промпт несёт «что чинить» → агент работает сразу;
   - **фоновый** аудит (`devh mutate start`, detached) → гейт читает его
     живое состояние: появились проблемы → `Verdict::Fail` с проблемами,
     **аудит продолжает идти** (он detached), агент чинит параллельно.
4. Для параллельной работы аудит **не должен быть `--in-place` в дереве
   агента** → канон: фоновый аудит запускается без `--in-place` (copy-режим
   cargo-mutants) либо в отдельном дереве; in-place — только синхронно.

### C2.2 Что должно быть в хвосте вердикта (идёт в промпт)

```
✗ выжил (дыра в тестах): core/escapes.rs:249 — replace x with None (замена → None)
⏱ таймаут тест-фазы (пойман не был): core/plan.rs:12 — replace y with 0 (замена → 0)
⊘ не компилится (дыра в покрытии): commands/run.rs:88 — replace z with false (замена → false)
```

## C3. `devh test` — канонический вход для сьютов

- `devh test` → запускает `devharness/test.sh`, **полный лог** в файл
  (`<store>/logs/test-<ts>.log`) + живой прогресс в stdout + **итог в stdout**
  (`PASS: N`, `FAIL: M`) + код возврата = 1 если есть FAIL, иначе 0.
- `devh test --case <N>` — один кейс (для точечной проверки), если нужно.
- Канон в `AGENTS.md`: «полный прогон сьютов = `devh test`; руками
  `bash devharness/test.sh` — не нужно (лог всё равно пишется)».

## C4. Оракул `devh next`

Реализация: определить состояние по `.harness/` (config, items, pid-файлы,
`state.json` аудита, exit-коды последних прогонов) → напечатать **одну**
команду из A4. Снапшот-режим (только чтение) — ничего не меняет.

## C5. Удаления и документация

| Действие | Путь |
| --- | --- |
| удалить | `devharness/tools/mutants-watch.sh` (R2) |
| удалить | `devharness/tools/mutants-run.sh` (уже удалён) |
| удалить | `MUTATION_WORKFLOW.md` в корне репо (R3) |
| создать | `docs/mutation_workflow.md` — канонический workflow мутаций (что запускать и когда) |
| создать | `AGENTS.md` — агентские инструкции: `devh mutate` / `devh test` / `devh next`, запрет ручного cargo-mutants, запрет sleep-циклов |
| создать | `docs/workflow.md` (или раздел в `docs/mutation_workflow.md`) — полный автомат A4/A5/A8 для человека |
| обновить | `docs/F_series.md` (F-3: мутационная жёсткость), `docs/rust_build.md` (журнал сессии) |
| обновить | память goose: категория `devh-mutation-rule` — «только `devh mutate`, руками cargo-mutants/watcher НЕЛЬЗЯ» + «`devh test` для сьютов» + «`devh next` — оракул» |

## C6. Тесты

1. **unit** (`cargo test -p devh`): семантика (§B2), `core/watch.rs`,
   `commands/mutate.rs` (парсинг имён, вердикт, канон справки),
   `commands/next.rs` (оракул), `commands/test.rs` (exit code).
2. **integration** (`tests/integration.rs`): заменить
   `section00b_branded_audit_cli_present` (она требует удаляемый watcher) на
   контракт нативного CLI:
   - `tools/mutants-watch.sh` **не существует** (R2);
   - `MUTATION_WORKFLOW.md` в корне **не существует** (R3);
   - есть `src/commands/mutate.rs`, `src/core/mutaudit.rs`, `src/core/tui.rs`,
     `src/core/watch.rs`, `src/commands/next.rs`, `src/commands/test.rs`;
   - `devh mutate help` печатает все подкоманды и правило «unviable = провал»;
   - `devh mutate` без подкоманды → справка, rc=0;
   - `devh mutate verdict` без аудита → rc=2;
   - `devh mutate problems` без аудита → rc=2;
   - `devh next` печатает ровно одну команду;
   - в `core/tui.rs` есть `pub fn pill` и `pub fn legend`;
   - в репо нет скриптов с `sleep`-циклом опроса мутаций (сканер запретов).
3. **`devharness/test.sh`**: базовая линия **477 PASS / 0 FAIL**, корпус
   **48/48** (если новые тесты не меняют счёт — зафиксировать новый счёт).
   Перед прогоном: `HARNESS_MUTATE_MAX_LOAD=off` (иначе контенция даёт
   «mutation env-blocked: load … > порог …»).

---

# ЧАСТЬ D. ПРИЁМКА

## D1. Мутационный аудит (после всех правок)

- Аудит **через `devh mutate`** (не руками!):
  `devh mutate start --config devharness/cargo-mutants.toml` либо
  дельта-аудит по диффу (`--in-diff <file>`).
- Требование: **0 missed, 0 timeout, 0 unviable, 0 неопознанных**, kill-rate 100%.
- Таймаут = 0: если cargo-mutants не успел — это не «поимка», а открытый
  вопрос: чинить (тест/код) и перепрогонять.
- Unviable = 0: чинить исходник, пока мутант не компилируется.

## D2. Чек-лист работ (порядок фиксирован)

- [x] 1. `core/mutaudit.rs` — семантика
- [x] 2. `core/tui.rs` — kill-rate + легенда
- [x] 3. `core/early.rs` — killed/clean + DOOM по «непойманным»
- [x] 4. `core/gates.rs` — `timeout` в статах + `judge_mutation` (unviable/timeout → Fail)
- [x] 5. `core/survivors.rs`, `core/killmap.rs` — реальные имена, timeout ≠ killed
- [x] 6. `commands/run.rs` — `timeout` в статах, `finish()`, **problem-first** останов, список проблем в хвост
- [x] 7. `core/watch.rs` — inotify (новый файл)
- [x] 8. `commands/mutate.rs` — `problems`, `stop`, rc=1, событийный watch, живой `problems.json`;
      плюс фикс feedback-loop супервизора и fail-closed `status`/`watch` при aborted (2026-10-23)
- [x] 9. `commands/next.rs` (новый) + регистрация → `devh next` (оракул)
- [x] 10. `commands/test.rs` (новый) + регистрация → `devh test`
- [x] 11. `commands/workflow.rs` (новый) или печать канона из `mutate.rs` → `devh workflow`
- [x] 12. Удалить `tools/mutants-watch.sh`, `MUTATION_WORKFLOW.md`
- [x] 13. `docs/mutation_workflow.md` + `AGENTS.md` (+ правки `docs/F_series.md`, `docs/rust_build.md`)
- [x] 14. Память goose: `devh-mutation-rule` → новая формулировка (+ `devh-test-rule`, `devh-next-rule`)
- [x] 15. integration: заменить `section00b` на контракт нативного CLI
- [x] 16. `cargo build` + `cargo test -p devh` (unit 390 + integration 69) → зелено;
      `rustfmt --check` по своим файлам — чисто (чужие файлы не форматируем)
- [ ] 17. `devh test` (весь сьют) → **477 PASS / 0 FAIL**, корпус 48/48
- [ ] 18. Мутационный аудит **через `devh mutate`** по изменённой зоне → 0 missed / 0 timeout / 0 unviable
- [ ] 19. Коммит через harness-овый git-workflow (не руками), без `git add -A`
      (доки — `git add -f`: `*.md` в `.gitignore`)

## D3. Дальше: F-серия (после п.19)

Очередь (без изменений): **F-1** Tripwire → **F-2** weakening detector →
**F-3** mutation hardening (частично закрывается этой работой) →
**[E-3, F-5]** → **F-4** → **E-4** → **F-6** → **F-7** → **F-8** → **K1**.

E-серия — после F: E-2/E-4 (сделаны), E-3 (свежесть корпуса), E-1
(фальсификация AUTONOMY_SAMPLE), E-6 (chaos-2).

## D4. ЗАПРЕТ НА «ПОТОМ СДЕЛАЮ»

Каждый пункт чек-листа §D2 закрывается **проверкой**, а не обещанием:
`cargo fmt --check`, `cargo test -p devh`, `devh test` (счётчики PASS/FAIL в
выводе), `devh mutate verdict` (rc), `devh mutate problems` (список),
`devh next` (одна команда). Прогресс-бары, «примерно работает», ручной
`cargo mutants` и внешние скрипты-обёртки — **не считаются результатом**.

---

## Приложение: почему «всё точно определено» (сводка)

* **Нет выбора**: `devh next` печатает одну команду; A4 — единственный маршрут.
* **Нет ручного браузинга**: логи читаются через `devh logs` / `devh test`;
  артефакты мутаций — через `devh mutate problems`.
* **Нет фоновых процессов в руках агента**: `devh mutate start` сам делает
  setsid и сам владеет cargo-mutants; `devh mutate stop` — единственный
  способ остановить.
* **Нет sleep-циклов**: события ФС (inotify) + `poll()`; ожидание — это
  ожидание события, а не такт.
* **Нет разночтений в семантике**: один контракт исходов в `core/mutaudit.rs`;
  легаси-имена только для чтения старых логов.
* **Нет «забытых» правил**: правила — в коде, в `devh workflow`, в `AGENTS.md`,
  в памяти; тесты сверяют, что watcher удалён, канон на месте, а запреты
  (A8) не нарушены в самом репо.
