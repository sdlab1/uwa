# devharness (Rust) — KISS dev harness

Один бинарник `devh`. Модель прежняя: **харнесс владеет git и циклом;
агент работает в изолированном worktree и никогда не запускает git.**
State — `.harness/` у целевого репо (flock + atomic writes, без БД).

## Установка

    cargo install --path devharness        # devh в PATH; в чекауте — ./devharness/devh
                                           # (обёртка: cargo run того же пакета)

`devh` в каталоге — обёртка над Rust-бинарём; bash-эталон сохранён как
`devh-classic`. `test.sh` — 95 ассертов, по умолчанию гоняется против
Rust-`devh` (CI `devharness-rs.yml`), отдельно — против bash:

    bash devharness/test.sh                                    # Rust (по умолчанию)
    DEVH=$PWD/devharness/devh-classic bash devharness/test.sh  # bash-эталон

## Дневной цикл

    cd <твой-репо>
    devh init                              # один раз: автодетект VERIFY/RULES
    printf 'Сделай X.\nRED: mix test …\n' | devh new "задача X"
    devh bg 1 && devh ps                   # detached + наблюдение
    tail -f .harness/logs/W-001-agent.log
    devh merge                             # rebase → ff-push → лог → next:

Автопилот: `devh auto --from specs/ --max 5` — спеки → items (RED из
строки `RED:`) → run+merge+tickets; item без RED честно остаётся в
очереди.

## Гейты (обязательны, кроме bench)

verify → no-ignored → lint → review → mutation(kill≥MIN_KILL) → [bench];
RED-пруф первым, MSG_DENY-гигиена последней. Падение → fix-раунд с
хвостом лога (≤ MAX_FIX); нет инструмента = блокер СРЕДЫ, не fix-раунд.

| Гейт | Конфиг | Смысл |
|---|---|---|
| VERIFY | `VERIFY` | тесты/сборка до коммита |
| NO-IGNORED | `NO_IGNORED=1` | `N ignored > 0` = скрытый красный |
| MUTATE | `MUTATE`, `MIN_KILL=90` | kill-rate ≥ MIN_KILL% на изменённых `MUTATE_SCOPE`-файлах (`{module}`/`{file}`); формат вывода `Mutants: N, Killed: M` (muex / cargo-mutants); инструмент не установлен = блокер среды (rc=2), не fix-раунд |
| RED | `items/W-NNN/red.cmd` | в первую очередь: падает на чистом HEAD до работы, зеленеет после; обход только записанный (`--no-red`) |
| LINT | `LINT=1`, `LINT_CMD` | авто-shellcheck изменённых *.sh + проектный линтер |
| REVIEW | `REVIEW=1` | self-review диффа вторым запросом; `VERDICT: OK` / `VERDICT: ISSUES: …` → fix-раунд |
| BENCH | `BENCH` | единственный опциональный: перф против baseline |

## Команды

| Команда | Синтаксис | Что делает |
|---|---|---|
| init | `devh init` | конфиг с автодетектом (uwa→scripts/verify.sh; gobot→mix test+muex; rust→cargo test), todo/report.md→RULES/RULES_LOG/MSG_DENY; `.harness/` в локальный exclude |
| new | `devh new "title" [--no-red "причина"]` / `devh new <N>` | item + spec.md; `<N>` — копия `todo/N.md` → `W-0NN` |
| list | `devh list` | таблица: ID / STATUS(open→active→done) / WT / TITLE |
| hint | `devh hint [id]` | спека + план + готовый промпт следующей модели |
| run | `devh run [id]` | claim (WIP-лимит, flock) → worktree → RED pre → агент → гейт-цикл (падение → fix-раунд с хвостом лога) → RED post → гигиена → commit → push ветки |
| commit | `devh commit [id]` | повторить commit/push по готовой работе; идемпотентен |
| merge | `devh merge [id]` | guard (дерево чисто, есть коммиты) → rebase → ff-push в MAIN → `## Session N` в RULES_LOG → лог → cleanup → `next:` |
| abort | `devh abort [id]` | worktree/ветку вон, item → open (работа выброшена) |
| clean | `devh clean` | worktree/ветки done-items + retention логов (LOG_DAYS) |
| doctor | `devh doctor` | окружение: git/VERIFY/агент/flock/timeout/remote/exclude/mutation/shellcheck/bench + crash-recovery сироты; exit≠0 при fail |
| logs | `devh logs [id]` | пути логов item + хвосты agent/verify; без id — свежие логи всех items |
| bg | `devh bg [id]` | detached-прогон: setsid+nohup (свой session, immune to HUP), лог в bg.log; параллельная работа безопасна (run.lock + WIP) |
| ps | `devh ps` | PID, elapsed, текущая стадия из timing.log; мёртвые pid-файлы помечаются |
| tickets | `devh tickets [--import]` | OnFlyAnalysis-наблюдения: показать / превратить в items (без red-идеи — отклоняется) |

## Наблюдаемость — где что смотреть

Всё состояние и логи — у целевого репо в `.harness/` (в git не попадает):

| Путь | Что внутри |
|---|---|
| `.harness/logs/W-NNN-agent.log` | полный вывод агента за последний раунд |
| `.harness/logs/W-NNN-verify.log` / `-mutate.log` / `-bench.log` | выводы гейтов последнего раунда |
| `.harness/logs/W-NNN-bootstrap.log` | подготовка worktree (deps) |
| `.harness/logs/W-NNN-timing.log` | тайминг стадий (сек): worktree / red / agent-rN / verify-rN / mutate-rN / bench-rN / commit — печатается в конце run и уходит в log.md; bottleneck ищется одним взглядом |
| `.harness/log.md` | append-only журнал закрытых сессий: commit, fix-rounds/gates/duration, summary |
| `.harness/summaries/W-NNN.md` | summary агента (1-я строка = сообщение коммита) |
| `.harness/items/W-NNN/` | spec.md, plan.md, red.cmd, status, agent (override) |
| `.harness/observations.md` | очередь OnFlyAnalysis: `<файл>:<строка> | <подозрение> | red-тест: <идея>`; `devh tickets --import` превращает в items |

- **Живой прогон**: `tail -f .harness/logs/W-001-agent.log` (или `devh logs W-001`).
- **Что сейчас в работе**: `devh list`, `devh doctor`, `devh ps`.
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
| MUTATE / MUTATE_SCOPE / MIN_KILL | muex / lib / 90 | мутационный гейт (автодетект при init) |
| BENCH | '' | перф-гейт (опционален) |
| RULES / RULES_LOG | todo/report.md | справочник граблей в промпте / куда дописывать «## Session N» |
| MSG_DENY | `todo/\|report\.md\|Session [0-9]+` | гигиена сообщения коммита |
| NO_IGNORED | 1 | гейт ignored-тестов |
| LINT / LINT_CMD | 1 / '' | авто-shellcheck *.sh / проектный линтер |
| REVIEW | 1 | self-review диффа вторым запросом агенту |
| HARNESS_COLOR / NO_COLOR | auto / — | always/never/auto — цветной вывод в стиле cargo; NO_COLOR уважается |

## Self-test — тот же стек, что гейтит чужие проекты

clippy (shellcheck-слой) → сьют 25 секций (порт `test.sh`, 95 ассертов;
оракул гоняется напрямую: `DEVH=<rust-bin> bash devharness/test.sh`) →
cargo-mutants на core/ (kill 100%) → llvm-cov ≥80%.

CI: `.github/workflows/devharness-rs.yml` (Rust: fmt → clippy →
тесты → oracle через обёртку `devh` → llvm-cov → cargo-mutants) и
`.github/workflows/devharness.yml` (bash-эталон `devh-classic`:
shellcheck → test.sh → selfmutate.sh).

## Миграция

- Выполнено: bash-`devh` → `devh-classic` (по-прежнему гейтится CI
  `devharness.yml`: shellcheck + test.sh + selfmutate 16/16); `devh` —
  обёртка над Rust, CI `devharness-rs.yml` гоняет оракул через неё.
- Спеки старых сессий → по одной через `devh new <N>` / `devh new "..."`.
