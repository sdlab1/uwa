# devharness — KISS dev harness

Дом: **project_uwa**. Один bash-файл (`devh`), работает с **любым** git-репо
(ROOT детектится по cwd) — uwa, gobot (projectC), что угодно. Агент (goose
headless) делает только работу в изолированном worktree и никогда не трогает
git — харнесс владеет циклом.

```
devh new "title"   # спека из stdin/$EDITOR → .harness/items/W-NNN/ (у целевого репо)
devh run           # worktree → агент → гейты → commit → push ветки devh/W-NNN
devh merge         # ревью сделано → rebase → ff-push в MAIN → лог → next:
devh hint          # промпт следующей модели (спека + план + контекст)
devh list / abort / clean / doctor
bash devharness/test.sh   # regression suite харнесса (25 сценариев, ~30 сек)
```

Алиас: `alias devh='/home/user/project_uwa/devharness/devh'`, затем из целевого
репо: `devh init` (сам детектит VERIFY: uwa → `scripts/verify.sh`, gobot →
`mix test` + muex-мутации, rust → `cargo test`).

## Гейты (все опциональны, пустой конфиг = пропуск)

| Гейт | Конфиг | Смысл |
|---|---|---|
| verify | `VERIFY` | тесты/сборка обязаны проходить |
| mutation | `MUTATE` + `MIN_KILL` | тесты обязаны **убивать мутантов** на изменённых файлах (`{module}`/`{file}` подставляются; формат вывода `Mutants: N, Killed: M`, muex). Нет инструмента → честная деградация |
| bench | `BENCH` | перф против baseline; готовый пример: `examples/check_bench.sh` (criterion, gobot) |
| RED | `items/W-NNN/red.cmd` | до работы обязан падать, после — проходить |

Per-item маршрутизация модели: `items/W-NNN/agent` — команда агента для конкретной
задачи (лёгкое — дешёвой модели, сложное — сильной; бывший AgentScore).

Падение гейта → fix-раунд агенту (хвост лога + жёсткая инструкция; для mutation:
«усиль ассерты, ослаблять запрещено»), до `MAX_FIX` раундов.

## Надёжность состояния — почему без БД

Старая система: SQLite → миграции (v7, v8…), бэкапы (health-варнинг
«нет бэкапа dev_db >24ч» блокировал коммиты!), слой аудита синка БД↔md.
Здесь: состояние — плоские файлы, но не «script kiddy»:

- **flock** на claim-секциях (`next_id`, WIP-лимит, статусы) — параллельные
  вызовы не склеиваются;
- **per-item run.lock** (`flock -n`): два run/merge/abort одного item
  невозможны; лок — открытый fd, **ядро отпускает его при смерти процесса**
  → stale-локов не бывает в принципе;
- **атомарные записи** (tmp + mv в той же ФС) — torn write невозможен;
- **retention** логов (`LOG_DAYS=30`), **doctor** ловит сирот (active без
  worktree, worktree без active — crash-recovery);
- worktree-пути слагированы по репо — несколько репо не конфликтуют.

Ровно та же транзакционность, за которой в старой системе шли в SQLite,
но без schema/backup/audit-слоя. Их же собственная jobs-координация жила в
JSON+flock, а не в БД — «писатели не должны блокироваться».

## Что взято из старой системы / спек (Documents/new_functionality_*.md)

Взято: `dev work`-фасад, worktree-per-agent (ADR-0050), RED-пруф, verify-loop,
hint, WIP-лимит, doctor, mutation (из `dev/mutation.ex`, Guardrails-5),
bench-gates (Session 29), watchdog→`AGENT_TIMEOUT` (5b), chain done→next
(workflow/6), retention (workflow/8), crash-recovery (Session 21), regression
suite (5b/REGRESSION-1 → `test.sh`), транзакционность (stabilize/db-tx → flock),
run-метрики в log.md: fix-rounds/gates/duration (бывший coverage_history),
repo-контекст в промпте (бывший context_pack), per-item маршрутизация агента
(бывший AgentScore/workflow/4-score).

Отброшено: daemon, bugnet, embeddings, fleet, inbox, suggest, MCP,
anti-cheat/simhash, coverage history, TUI, stabilize-серия. Если понадобится —
это строки в devh, а не модули.

## Миграция

- projectC (gobot): `mv dev dev-classic` (когда параллельная сессия закоммитит
  `dev/work.ex`); harness управляет gobot отсюда — location-independent.
- Старые спеки (Documents/new_functionality_*.md) → по одной через `devh new`,
  если идеи ещё ценные.
