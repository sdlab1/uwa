# Агент-руководство (runbook)

Памятка для нового агента: как начать работу, взять задачу, сдать и что делать при проблемах.

## 1. Быстрый старт (30 секунд)

```bash
cd /home/user/project_uwa
./target/debug/devh msg read          # 1) Твои задачи — ПЕРВЫМ ДЕЛОМ
./target/debug/devh workflow          # 2) Канон (правила R1-R26, шкалы P/F/C)
./target/debug/devh orch board        # 3) Доска: кто что делает
./target/debug/devh orch identify <ты> --reply "MODEL: <id>" --reply "VERSION: <версия>" --reply "DATE: $(date +%F)"
./target/debug/devh orch wt sync <ты> # 6) Обновить СВОЙ worktree
```

Команда для новичка:
```bash
cd /home/user/project_uwa && ./target/debug/devh orch welcome
```

## 2. Как работать с задачами

```bash
# Каждый ход первым делом — почта
devh msg read

# Взять задачу (+ авто worktree)
devh orch start <ID>

# Сдать задачу по DoD
devh orch dod <ID> --ack 1
devh orch verify <ID>
devh orch commit <ID> --path <твои файлы>
devh orch done <ID> --f N
```

Коммит делает харнесс — только path-scoped, никогда `git add -A` (R9).

## 3. Что делать при конфликте зон

Конфликт зон возникает, когда две задачи разных агентов затрагивают один файл.
Это означает, что вы оба работаете с пересекающейся зоной ответственности.

```bash
# Проверить конфликты зон
devh orch board | grep "пересечение"

# Если конфликт — дождитесь закрытия чужой задачи или попросите lead перераспределить зоны
# Никогда не правьте файл в зоне другого агента (O5)
```

## 4. Как жаловаться на потолок

Если потолок (ceiling) кажется слишком низким — подай баг-репорт (он идёт в первую очередь):

```bash
# Проверить свой потолок
devh orch ceiling --list

# Подать жалобу на потолок — баг в первую очередь
devh orch bug "<что сломано: потолок слишком низок>" --from <ты> --zone <путь>
```

`--class PS` для security-багов (приоритет P0, маркер БАГ PS).

## 5. Запреты (R1/R8/R9/R11/R17)

- **R1**: `cargo mutants` только через `devh mutate` и только ведущим.
- **R8**: `bash devharness/test.sh` только через `devh test`.
- **R9**: `git add/commit/push/rebase/reset/checkout/stash` в общем дереве — только через `devh orch commit --path` и `devh orch wt merge`.
- **R11**: `--in-place` для аудита мутирует дерево агента.
- **R17**: Тяжёлые прогоны (`devh test`, `devh mutate`) только ведущим.

## 6. Работа вне своего worktree — ЗАПРЕЩЕНО (R25)

Работай ТОЛЬКО в своём worktree (`/tmp/devh-909045283-<агент>`).
Путь всегда выдаётся вместе с задачей (`devh orch start <ID>`).

Обновляй worktree ТОЛЬКО через `devh orch wt sync <ты>` (R26). Руками `git pull/fetch/rebase` — ЗАПРЕЩЕНО.

## 7. Если ты стал новым агентом

```bash
# Одна команда — и ты знаком с процессом
cd /home/user/project_uwa && ./target/debug/devh orch welcome
```

Это выпечатает твой канонический адрес, задачи, канон (O1–O20 + шкалы), как связаться с ведущим и формат отчёта.
