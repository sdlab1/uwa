#!/bin/bash
cd /home/user/project_uwa
D=./target/debug/devh
B="--agent minimax --buffer --p 2 --c 3"

$D orch add $B --title "CLI: devh orch board --json — детерминированный JSON-снимок доски" \
  --zone devharness/src/commands/orch.rs \
  --dod "Команда печатает детерминированный JSON: агенты (роль, модель, очередь, active), задачи (id, agent, status, prio, deps, зоны, C), буфер lead, предупреждения." \
  --dod "Одинаковый реестр => побайтово одинаковый JSON (без времени и случайного порядка); вывод идёт в stdout, диагностика — в stderr." \
  --dod ">=4 юнит-теста зелёные." --check "cargo test -p devh --lib commands::orch"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "GUARD: самопроверка зон внутри одного агента (двойная работа своими руками)" \
  --zone devharness/src/core/guards/zone_guard.rs --zone devharness/src/core/guards/mod.rs \
  --dod "Guard проверяет, что у двух активных задач ОДНОГО агента нет пересекающихся зон; при нарушении — структурированная причина (файл+строка+правило)." \
  --dod "Guard ничего не меняет в реестре; зарегистрирован в core/guards/mod.rs и вызывается из selfcheck." \
  --dod ">=5 юнит-тестов зелёные." --check "cargo test -p devh --lib core::guards::zone_guard"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "ФОРМАТ ОТЧЁТА: валидатор 4-пунктного отчёта агента (+ строка P/F)" \
  --zone devharness/src/core/report_format.rs \
  --dod "Модуль core/report_format.rs: разбор и проверка отчёта агента (1) сделано 2) как проверено 3) доказательства 4) осталось/риски + строка вида P/F: P2/F1)." \
  --dod "Валидатор read-only, детерминированный, с понятной диагностикой по каждому отсутствующему пункту." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::report_format"

$D orch add --agent minimax --buffer --p 2 --c 3 \
  --title "ЖУРНАЛ РЕЕСТРА: ledger.jsonl с дедупликацией и ограничением размера" \
  --zone devharness/src/core/registry_ledger.rs \
  --dod "Модуль core/registry_ledger.rs: append-only журнал событий реестра (start/done/fail/revoke/reassign/merge) в .harness/orch/ledger.jsonl." \
  --dod "Дедупликация по (событие, id, ts), ограничение размера файла (ротация), устойчивость к битым строкам (пропускаются, не ломают чтение)." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::registry_ledger"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "ОТЧЁТ ПО МОДЕЛЯМ: рейтинг F-распределения и порог деградации (O13)" \
  --zone devharness/src/core/model_report.rs \
  --dod "Модуль core/model_report.rs: по истории задач считать рейтинг моделей (сдано, распределение F, потолок C) и признак срабатывания порога деградации (F>=F3, два подряд F>=F2, смена модели/рантайма)." \
  --dod "Результат read-only и детерминированный (без времени в выводе)." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::model_report"

$D orch add --agent minimax --buffer --p 2 --c 3 \
  --title "ПЕСОЧНИЦА: детектор правок вне worktree агента (по git status общего дерева)" \
  --zone devharness/src/core/worktree_escape.rs \
  --dod "Модуль core/worktree_escape.rs: read-only определение факта, что в ОБЩЕМ дереве main появились изменения, сделанные не через devh orch commit (сравнение с ожидаемым HEAD/состоянием)." \
  --dod "Результат: список подозрительных путей + уровень (warn/critical); без записи и без изменения чужих файлов." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::worktree_escape"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "ГИГИЕНА КОНФИГА: секреты не попадают в .harness/config и в логи" \
  --zone devharness/src/core/config_hygiene.rs \
  --dod "Модуль core/config_hygiene.rs: детект опасных значений в конфиге и логах (DEVH_LEAD_PASSWORD и подобное) — маскирование в выводе и предупреждение, если секрет лежит в .harness/config." \
  --dod "Маскирование детерминированное (в логе не остаётся значения секрета); тесты на маскирование и на детект в конфиге." \
  --dod ">=5 юнит-тестов зелёные." --check "cargo test -p devh --lib core::config_hygiene"

$D orch add --agent minimax --buffer --p 2 --c 3 \
  --title "ДОКУМЕНТАЦИЯ: docs/mutation_workflow.md и AGENTS.md генерируются из кода" \
  --zone devharness/src/commands/workflow.rs \
  --dod "Команда/функция генерирует docs/mutation_workflow.md и зеркало AGENTS.md из кода (канон devh workflow), детерминированно и без ручных правок." \
  --dod "Повторный запуск даёт побайтово тот же файл; расхождение с кодом обнаруживается проверкой." \
  --dod ">=4 юнит-теста зелёные." --check "cargo test -p devh --lib commands::workflow"

$D orch add --agent minimax --buffer --p 2 --c 3 \
  --title "ТЕСТЫ РОЛЕЙ: сценарные интеграционные тесты lead/worker" \
  --zone devharness/tests/orch_roles.rs \
  --dod "Сценарные интеграционные тесты: воркер не видит чужих задач и lead-only команд, буфер lead выдаётся self-service, DoD-гейт dod->verify->commit->done не обходится без ack, зона задачи не пересекается с чужой активной." \
  --dod "Тесты не зависят от реального /tmp/devh-msg и реестра: используют временное хранилище." \
  --dod ">=6 сценариев зелёные." --check "cargo test -p devh --test orch_roles"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "БУФЕР-ПРАВИЛА: backlog_rules — минимум 15, норма 15–20, детерминированный расчёт" \
  --zone devharness/src/core/backlog_rules.rs \
  --dod "Модуль core/backlog_rules.rs: чистый расчёт состояния буфера lead (сколько задач, минимум 15, норма 15–20, что предлагается докинуть) по реестру." \
  --dod "Расчёт детерминированный и не зависит от порядка задач в реестре (сортировка по prio, затем id)." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::backlog_rules"

$D orch add --agent minimax --buffer --p 1 --c 2 \
  --title "ФОРМАТ ЛОГА: единый формат строк оркестратора (файл+строка+причина+код)" \
  --zone devharness/src/core/log_format.rs \
  --dod "Модуль core/log_format.rs: единый формат диагностических строк оркестратора (путь, номер строки, правило, код возврата) — машинно-разбираемый, без неоднородных префиксов." \
  --dod "Формат стабильный (тест на «золотой» пример), свободный текст остаётся в отдельном поле." \
  --dod ">=6 юнит-тестов зелёные." --check "cargo test -p devh --lib core::log_format"

echo "=== буфер ведущего пополнен ==="
