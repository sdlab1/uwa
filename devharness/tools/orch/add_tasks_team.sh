#!/bin/bash
cd /home/user/project_uwa
D=./target/debug/devh

# --- младшие (C2, с guardrails: read-only/детерминированно, узкая зона) ---
$D orch add --agent beam --p 1 --c 2 --prio 1 \
  --title "ДОД-ЧЕКЕР: read-only проверка полноты DoD/check задач на доске" \
  --zone devharness/src/core/dod_audit.rs \
  --dod "Модуль core/dod_audit.rs: read-only и детерминированный разбор реестра -> список задач с пустым DoD, без машинной проверки, без зоны или с C выше потолка модели." \
  --dod "Функция ничего не пишет и не меняет реестр; одинаковый вход => одинаковый выход (без времени/случайности в результате)." \
  --dod ">=6 юнит-тестов зелёные." \
  --check "cargo test -p devh --lib core::dod_audit" --notify

$D orch add --agent inkling2 --p 1 --c 2 --prio 0 \
  --title "ЗОМБИ-PID: зомби не считается живым супервизором (pid_alive/group_alive)" \
  --zone devharness/src/shell/proc.rs \
  --dod "В core/shell/proc.rs добавить pid_alive (kill(pid,0) + исключить состояния Z/X из /proc/<pid>/stat, парсить ПОСЛЕ последней скобки) и group_alive (скан /proc/*/stat по pgrp: kill(-pgid,0) считает группу из зомби живой)." \
  --dod "Юнит-тесты >=5: зомби, живой процесс, несуществующий pid, pid<=0, группа только из зомби." \
  --check "cargo test -p devh --lib proc" --notify

# --- nemotron-3-ultra (C3) ---
$D orch add --agent beam2 --p 2 --c 3 --prio 2 \
  --title "РЕЕСТР-СНАПШОТ: read-only снимок реестра (JSON) для диагностики и сравнения" \
  --zone devharness/src/core/registry_snapshot.rs \
  --dod "Модуль core/registry_snapshot.rs: детерминированный read-only снимок реестра (задачи, агенты, статусы, prio/deps) в JSON без доступа к main и без записи файлов." \
  --dod "Снимок пригоден для сравнения «было/стало» (без времени и случайных полей) и не падает на битом реестре (пустой снимок + признак ошибки)." \
  --dod ">=6 юнит-тестов зелёные." \
  --check "cargo test -p devh --lib core::registry_snapshot" --notify

$D orch add --agent beam3 --p 2 --c 3 --prio 2 \
  --title "МЕТРИКИ: время цикла задачи (start -> done) и разброс по агентам" \
  --zone devharness/src/core/task_metrics.rs \
  --dod "Модуль core/task_metrics.rs: по реестру считать длительность цикла задачи (started_at -> done) и агрегировать по агентам (медиана, максимум, число зависших)." \
  --dod "Результат детерминированный и read-only; незавершённые задачи считаются отдельной метрикой (не портят медиану завершённых)." \
  --dod ">=6 юнит-тестов зелёные." \
  --check "cargo test -p devh --lib core::task_metrics" --notify

# --- kilo (модель неизвестна => потолок fail-open, поэтому C3) ---
$D orch add --agent kilo-2ba65640 --p 1 --c 3 --prio 2 \
  --title "КОДЫ ВОЗВРАТА: единый контракт 0/1/2/3 во всех командах devh" \
  --zone devharness/src/commands/mod.rs \
  --dod "Проверить и зафиксировать единый контракт кодов возврата: 0 успех, 1 actionable проблема, 2 среда не готова, 3 внутренняя ошибка. Расхождения (например, паника/101 или 2 там, где нужен 1) перечислены и исправлены." \
  --dod "Не менять поведение команд успеха; покрыть тестом таблицу соответствия команд и кодов." \
  --dod ">=6 юнит-тестов зелёные." \
  --check "cargo test -p devh --lib commands" --notify

$D orch add --agent kilo-2ba65640 --p 2 --c 3 --prio 2 \
  --title "NEXT: devh next выдаёт ровно одну следующую команду и не падает на пустой очереди" \
  --zone devharness/src/commands/next.rs \
  --dod "devh next: всегда печатает РОВНО ОДНУ исполняемую команду (или понятный отказ), не падает при пустой очереди, битых deps и отсутствующем реестре." \
  --dod "Приоритет выбора: сначала задача с меньшим prio, затем P, затем возраст; блокированные по deps не выдаются." \
  --dod ">=6 юнит-тестов зелёные." \
  --check "cargo test -p devh --lib commands::next" --notify

echo "=== задачи командам созданы ==="
