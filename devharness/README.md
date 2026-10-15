# devharness — KISS dev harness

Один bash-файл (`devh`), дом — **project_uwa**, управляет **любым** git-репо
(ROOT детектится по cwd). Модель: харнесс владеет git и циклом; агент
(goose headless) работает в изолированном worktree и никогда не запускает git.

```bash
devh new → run → merge        # полный цикл; двухфазный merge (ревью до MAIN)
```

## Установка (один раз)

```bash
ln -sf /home/user/project_uwa/devharness/devh ~/.local/bin/devh   # уже сделано
devh help                                                          # проверка; PATH-строка в ~/.bashrc/.profile есть
```
Если в текущей сессии `command not found`: `export PATH="$HOME/.local/bin:$PATH"`
(новые терминалы подхватят автоматически).

## Где что живёт

| Что | Где |
|---|---|
| код харнесса | `~/project_uwa/devharness/` (в git, этот репозиторий) |
| состояние целевого репо | `<репо>/.harness/` (создаёт `devh init`; в git НЕ попадает) |
| логи прогонов | `<репо>/.harness/logs/W-NNN-*.log` |
| журнал закрытых сессий | `<репо>/.harness/log.md` |

## Быстрый старт

```bash
alias devh='/home/user/project_uwa/devharness/devh'
cd <твой-репо>
devh init                                    # один раз: конфиг, .harness/, локальный exclude
printf 'Сделай X.\n' | devh new "задача X"   # спека из stdin (tty → $EDITOR)
echo 'cd gobot && mix test test/x_test.exs' > .harness/items/W-001/red.cmd   # RED обязателен
devh run                                     # worktree → агент → гейты → commit → push
devh merge                                   # rebase → ff-push в MAIN → лог → next:
```

Сессии uwa-стиля: `devh new 40` — item W-040 из `todo/40.md` (title из
заголовка); `devh run 40 / merge 40` принимают номер без W-.

Что печатает `run`: `OK worktree` → `OK red: падает на HEAD` → `.. агент` →
`OK гейты: verify +mutation` → `OK commit` → `OK push` →
`ГОТОВО: W-001 на ветке devh/W-001 … затем: devh merge`.

## Как работать с devh — дневной цикл

Твоя работа — только два артефакта: **спека** (что делать) и **RED-пруф**
(чем доказать). Всё остальное — работа харнесса: он собирает промпт агенту,
гоняет гейты, коммитит, пушит и ведёт журнал.

### 1. Сформулируй спеку

Качество результата = качество спеки («спека несёт архитектуру» — агент
делает, а не думает): точные файлы/пути/имена, формат изменений, какой
тест считается done, что НЕ трогать. Источники:

```bash
printf '…' | devh new "задача X"   # спека из stdin
devh new "задача X"                # из $EDITOR (tty)
devh new 40                        # сессия из бэклога: копия todo/40.md
```

### 2. Дай RED-пруф

```bash
echo 'cargo test -p uwa-config human_timing' > .harness/items/W-001/red.cmd
```
Команда, которая **сейчас падает** (фичи нет), а после работы обязана
позеленеть — доказательство существования и решения задачи. Обход только
записанный: `devh new "…" --no-red "<почему RED невозможен до реализации>"` —
причина печатается при каждом run.

### 3. Запусти — и иди работать дальше

- `devh run` — в очках, вывод на экран;
- `devh bg` — параллельно: харнесс отвяжется (setsid), `devh ps` — стадия,
  `tail -f .harness/logs/W-001-agent.log` — живой вывод агента.

Пока агент работает — бери следующую задачу, разбирай `devh tickets`,
читай логи. `run.lock` и WIP-лимит не дадут двум run-ам столкнуться.

### 4. Что происходит без тебя

worktree → RED воспроизведён → агент → гейты по порядку
(VERIFY → NO-IGNORED → LINT → REVIEW → MUTATE → BENCH). Любой красный →
харнесс сам даёт агенту fix-раунд с хвостом лога и жёсткой инструкцией,
до `MAX_FIX` раз. Стадии и секунды — в timing.log и `devh ps`.

### 5. Ревью и merge

`git -C <worktree> diff main..devh/W-001` (путь печатает run) →
`devh merge`: rebase → ff-push в MAIN → `## Session N` в report.md →
лог → `next:`. Коммитит только харнесс: сообщение — из summary агента,
гигиена MSG_DENY проверена механически.

### 6. Если что-то пошло не так

| Симптом | Действие |
|---|---|
| run упал (API/timeout агента) | работа не потеряна — тот же `devh run <id>` продолжит в том же worktree |
| агент сказал BLOCKED | 1-я строка summary + `plan.md`; уточни спеку, повтори run |
| гейт красный N раундов | `devh logs <id>` — хвосты логов каждого гейта |
| env-блокер («установи …») | среда, не работа: поставь инструмент, повтори |
| выбросить | `devh abort <id>` — item → open, правь спеку, заново |

### Что на самом деле получает агент

Харнесс собирает промпт за тебя: спека + план прошлых попыток + справочник
граблей (`todo/report.md`) + 3 последних коммита HEAD + правила гейтов
(verify/mutation/RED) + OnFlyAnalysis-дисциплина + «git не трогать».
Твоя спека — единственное, что определяет качество результата.

## Команды

| Команда | Синтаксис | Что делает |
|---|---|---|
| init | `devh init` | конфиг с автодетектом (uwa→scripts/verify.sh; gobot→mix test+muex; rust→cargo test), todo/report.md→RULES/RULES_LOG/MSG_DENY; `.harness/` в локальный exclude |
| new | `devh new "title" [--no-red "причина"]` / `devh new <N>` | item + spec.md; `--no-red` — записанный обход RED (выводится при run); `<N>` — копия `todo/N.md` → `W-0NN` |
| list | `devh list` | таблица: ID / STATUS(open→active→done) / WT / TITLE |
| hint | `devh hint [id]` | спека + план + ГОТОВЫЙ промпт следующей модели (repo-контекст, правила, гейты) |
| run | `devh run [id]` | claim (WIP-лимит, flock) → worktree (+bootstrap) → RED pre → агент → гейт-цикл (verify → NO-IGNORED → mutation → bench; падение → fix-раунд с хвостом лога) → RED post → гигиена → commit → push ветки |
| commit | `devh commit [id]` | повторить commit/push по готовой работе (напр., поправил summary после гейт-фейла); идемпотентен |
| merge | `devh merge [id]` | guard (дерево чисто, есть коммиты) → rebase на upstream → ff-push в MAIN → дописать `## Session N` в RULES_LOG → лог → cleanup → `next:` |
| abort | `devh abort [id]` | worktree/ветку вон, item → open (работа выброшена) |
| clean | `devh clean` | worktree/ветки done-items + retention логов (LOG_DAYS) |
| doctor | `devh doctor` | окружение: git/VERIFY/агент/flock/timeout/remote/exclude/mutation-инструмент/shellcheck/bench + crash-recovery сироты; exit≠0 при fail |
| logs | `devh logs [id]` | все пути логов item + хвосты agent/verify; без id — свежие логи всех items |
| bg | `devh bg [id]` | detached-прогон: setsid+nohup (свой session, immune to HUP), лог в bg.log; параллельная работа — безопасно (run.lock + WIP) |
| ps | `devh ps` | кто в полёте: item, PID, elapsed, текущая стадия из timing.log; мёртвые pid-файлы помечаются |
| tickets | `devh tickets [--import]` | OnFlyAnalysis-наблюдения агентов: показать / превратить в items (без red-идеи — отклоняется; идея уходит в спеку тикета) |

## Гейты — ОБЯЗАТЕЛЬНЫ (проза report.md переведена в код); опционален только bench

| Гейт | Конфиг | Смысл |
|---|---|---|
| VERIFY | `VERIFY` | тесты/сборка до коммита (uwa: scripts/verify.sh, 10 шагов) |
| NO-IGNORED | `NO_IGNORED=1` | ни одного ignored-теста в выводе verify; `N ignored > 0` = скрытый красный |
| MUTATE | `MUTATE`, `MIN_KILL=90` | kill-rate ≥ MIN_KILL% на изменённых `MUTATE_SCOPE`-файлах (`{module}`/`{file}`); формат вывода `Mutants: N, Killed: M` (muex / cargo-mutants); инструмент не установлен = **блокер среды** (rc=2), не fix-раунд |
| RED | `items/W-NNN/red.cmd` | В ПЕРВУЮ ОЧЕРЕДЬ: падает на чистом HEAD до работы, зеленеет после; обход только записанный (`--no-red`) |
| LINT | `LINT=1`, `LINT_CMD` | авто-shellcheck изменённых *.sh + проектный линтер; нет shellcheck при .sh-диффе = блокер среды |
| REVIEW | `REVIEW=1` | self-review диффа ВТОРЫМ запросом той же модели («review code for inconsistencies and bugs»); вердикт `VERDICT: OK` / `VERDICT: ISSUES: …` → fix-раунд; упал/не распознан = блокер среды |
| BENCH | `BENCH` (единственный опциональный) | перф против baseline: `examples/check_bench.sh` (criterion, gobot) |

Маппинг «железные правила → гейты»: №1 CI-зелёный→VERIFY, №2 ignored=0→NO-IGNORED,
№3 гигиена сообщений→MSG_DENY (+`devh commit` для лечения), №4 выводы→логи,
№5 staged-диф→commit-гейты. Падение гейта → fix-раунд агенту с хвостом лога
и жёсткой инструкцией (для mutation: «усиль ассерты, ослаблять запрещено»),
до `MAX_FIX` раундов.

## Наблюдаемость — где что смотреть

Всё состояние и логи — у целевого репо в `.harness/` (в git не попадает):

| Путь | Что внутри |
|---|---|
| `.harness/logs/W-NNN-agent.log` | полный вывод агента за последний раунд |
| `.harness/logs/W-NNN-verify.log` / `-mutate.log` / `-bench.log` | выводы гейтов последнего раунда |
| `.harness/logs/W-NNN-bootstrap.log` | подготовка worktree (deps) |
| `.harness/logs/W-NNN-timing.log` | тайминг стадий (сек): worktree / red / agent-rN / verify-rN / mutate-rN / bench-rN / commit; печатается в конце run и уходит в log.md (`- run: … timing: …`) — bottleneck ищется одним взглядом |
| `.harness/log.md` | append-only журнал закрытых сессий: commit, `fix-rounds/gates/duration`, summary |
| `.harness/summaries/W-NNN.md` | summary агента (1-я строка = сообщение коммита) |
| `.harness/items/W-NNN/` | spec.md, plan.md (заметки между попытками), red.cmd, status, agent (override) |
| `.harness/observations.md` | очередь OnFlyAnalysis: `<файл>:<строка> | <подозрение> | red-тест: <идея>` — каждый прочитанный агентом файл должен дать наблюдение или чистоту; `devh tickets --import` превращает в items |

- **Живой прогон**: `tail -f .harness/logs/W-001-agent.log` (или `devh logs W-001`).
- **Что сейчас в работе**: `devh list` (active+WT=yes), `devh doctor`.
- **Метрики за прогон**: строка `run: fix-rounds=N gates=… duration=Ns` в log.md.
- `devh hint` — готовый промпт следующей модели: спека + план + правила + repo-контекст.

## Конфиг (`.harness/config`, env `HARNESS_*` перекрывает)

| Ключ | Дефолт | Смысл |
|---|---|---|
| VERIFY | автодетект | команда проверки; обязана проходить |
| BOOTSTRAP | автодетект | однократная подготовка worktree (deps) |
| AGENT | `goose run --no-session -t` | агент; промпт одним аргументом |
| AGENT_TIMEOUT | 1800 | сек на раунд агента (watchdog «агент сдох») |
| MAX_FIX | 3 | fix-раундов на гейт-цикл |
| MAX_ACTIVE | 3 | WIP-лимит |
| REMOTE / MAIN | origin / текущая | куда пушим |
| WT_ROOT | /tmp | где worktree'и (слаг по репо — коллизий нет) |
| LOG_DAYS | 30 | retention логов |
| MUTATE / MUTATE_SCOPE / MIN_KILL | muex / lib / 90 | мутационный гейт |
| BENCH | '' | перф-гейт (опционален) |
| RULES / RULES_LOG | todo/report.md | справочник граблей в промпте / куда дописывать «## Session N» |
| MSG_DENY | `todo/\|report\.md\|Session [0-9]+` | гигиена сообщения коммита |
| NO_IGNORED | 1 | гейт ignored-тестов |
| LINT / LINT_CMD | 1 / '' | авто-shellcheck *.sh / проектный линтер |
| REVIEW | 1 | self-review диффа вторым запросом агенту |
| HARNESS_COLOR / NO_COLOR | auto / — | always/never/auto — цветной вывод в стиле cargo; NO_COLOR уважается |

## Самопроверка харнесса — четыре слоя (+CI)

1. **Строгий режим** — `set -euo pipefail` везде.
2. **Статика** — `shellcheck devh test.sh selfmutate.sh examples/check_bench.sh` = 0 findings (гейт: префет в test.sh + doctor + CI).
3. **Чёрный ящик** — `test.sh`: 25 секций / 87 ассертов; реальные git-репо в песочнице, стабы только на границах (агент, мутатор). Полностью герметично, ~40 сек.
4. **Самомутация** — `selfmutate.sh`: 15 известных багов вносятся в изолированную копию devh, suite обязан убить каждый (kill-rate **15/15**); kill-rate валиден только на зелёном базовом — вшито предусловием.

CI: `.github/workflows/devharness.yml` — shellcheck + test.sh + selfmutate.sh
на каждый пуш в `devharness/**`.

## Аудит против старой системы dev/ (~107K строк)

**Реализовано**: work-фасад, worktree-per-agent, RED-пруф, verify-loop,
mutation, bench-gates, WIP, doctor, watchdog→timeout, hint, chain next,
retention, crash-recovery, транзакционность (flock), per-item маршрутизация
агента, run-метрики, адаптация todo/NN.md + report.md, гигиена сообщений.

**Subsumed более сильными гейтами**: anti-cheat AST-эвристики (checks.ex,
~3К строк) — мутационный гейт строго сильнее (`assert true` мутантов не убивает);
causality «фикс — причина зелёного» — RED-пруф; тест-качество — kill-rate.

**Осознанно отброшено** (проблемы не существуют в модели «одна задача →
worktree → гейты → merge»): daemon+FIFO, bugnet/Jaccard, embeddings, fleet,
inbox, suggest-телеметрия, TUI/dashboard, coverage history, MCP, jobs/queue,
flaky-bisect (урок CI#5: не лечить ретраями), `--json` (таблицы читаемы),
shell-completion. Если понадобится — это строки в devh, а не модули.

## Надёжность состояния — почему без БД

flock на claim-секциях; per-item run.lock (лок = открытый fd → **ядро отпускает
при смерти процесса**, stale-локов не бывает); атомарные записи (tmp+mv);
worktree-слаги по репо. Та же транзакционность, за которой шли в SQLite, без
schema/migrations/backup/audit-слоя (их собственная jobs-координация жила в
JSON+flock — «писатели не должны блокироваться»).

## Осознанные ограничения (не баги)

- `NO-IGNORED` парсит cargo-формат `N ignored`; чужие форматы — `NO_IGNORED=0`.
- muex/cargo-mutants ставится **до первого run** — env-блокер честно об этом говорит.
- Уроки/грабли остаются **справочником** в промпте (`RULES`) — знания в гейт не компилируются; enforcement — только код.

## Миграция

- projectC (gobot): `mv dev dev-classic` (когда параллельные сессии закончатся);
  harness управляет gobot отсюда — location-independent.
- Спеки старых сессий → по одной через `devh new <N>` / `devh new "..."`.
