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

## Гейты: ОБЯЗАТЕЛЬНЫ (проза report.md переведена в код); опционален только bench

| Гейт | Конфиг | Смысл |
|---|---|---|
| verify | `VERIFY` | ОБЯЗАТЕЛЕН: тесты/сборка до коммита |
| NO-IGNORED | `NO_IGNORED=1` | ОБЯЗАТЕЛЕН: ни одного ignored-теста в выводе verify — «ignored = скрытый красный» |
| mutation | `MUTATE` + `MIN_KILL` | ОБЯЗАТЕЛЕН: тесты обязаны **убивать мутантов** на изменённых файлах (`{module}`/`{file}`, формат `Mutants: N, Killed: M`). Инструмент не установлен = **блокер среды** с сообщением «установи», а не тихий пропуск |
| RED | `items/W-NNN/red.cmd` | ОБЯЗАТЕЛЕН, В ПЕРВУЮ ОЧЕРЕДЬ: до работы обязан падать, после — проходить. Обход только записанный: `devh new --no-red "<причина>"` |
| bench | `BENCH` | опционален: перф против baseline (`examples/check_bench.sh`, criterion/gobot) |

Маппинг «проза report.md → код»:

| Железное правило (проза) | Гейт (код) |
|---|---|
| №1 «CI зелёный ДО коммита» | VERIFY (verify.sh с точным env) |
| №2 «ненулевой ignored = красный прогон» | NO-IGNORED (grep по выводу verify) |
| №3 «коммиты никогда не упоминают todo/» | MSG_DENY (гейт сообщения) |
| №4 «длинные выводы — в /tmp» | логи в `.harness/logs/`, retention |
| №5 «verify.sh + staged-диф на удаления» | VERIFY + staged-stat + warn>5 удалений |

Per-item маршрутизация модели: `items/W-NNN/agent` — команда агента для конкретной
задачи (лёгкое — дешёвой модели, сложное — сильной; бывший AgentScore).

Падение гейта → fix-раунд агенту (хвост лога + жёсткая инструкция; для mutation:
«усиль ассерты, ослаблять запрещено»), до `MAX_FIX` раундов.

## Адаптация под session-workflow (uwa: todo/report.md)

`devh init` в репо с `todo/report.md` автоматически включает:

- `RULES=todo/report.md` — в промпте как СПРАВОЧНИК известных граблей (уроки —
  знания, их не перевести в гейты); процессные же правила переведены в КОД:
  №1→VERIFY, №2→NO-IGNORED, №3→MSG_DENY, №5→commit-гейты, RED-пруф —
  обязательный гейт (см. таблицу маппинга выше);
- `RULES_LOG=todo/report.md` — после merge в report.md дописывается
  `## Session N — <title>` с коммитом и summary (твой ритуал ведения отчёта);
- `MSG_DENY='todo/|report\.md|Session [0-9]+'` — железное правило №3
  («коммиты никогда не упоминают todo/») стало механическим гейтом;
- `devh new 40` — item `W-040` из `todo/40.md` (спека копируется, title из
  заголовка); `devh run 40` / `merge 40` принимают номер без W-.

Флоу сессии: `devh new 40 → run 40 → merge 40` → «next: …» → в report.md
дописана секция. Если агент нарушил гигиену сообщения — поправь первую строку
`.harness/summaries/W-040.md` и вызови `devh commit 40` (работа не пропадает).

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
