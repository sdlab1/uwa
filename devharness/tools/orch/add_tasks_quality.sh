#!/usr/bin/env bash
# Гейт качества: заглушки, guard, слияние, детектор обхода оркестратора.
# Задачи 1-3 -> opencode-0a0e0cfc (deepseek-4.1), 4 -> kilo (F0-серия).
set -u
D=/home/user/project_uwa/target/debug/devh
cd /home/user/project_uwa || exit 1

O=opencode-0a0e0cfc

$D orch add --agent $O --p 2 --c 3 --prio 0 \
  --title "ГЕЙТ КАЧЕСТВА (1/4): детектор заглушек — код-заглушка не должен проходить как выполненная задача" \
  --zone devharness/src/core/stub_check.rs \
  --dod "Модуль core/stub_check.rs (read-only, без записи в репозиторий): детектор заглушек и тавтологий в diff. Правила: тело функции вида Ok(())/Ok(true)/true; path.exists(); !s.is_empty(); identity (возвращается сам аргумент, x -> x); format! без логики (эхо входа); публичная функция без единого вызова в коде (dead-код); публичная функция без #[test] в своей зоне. Каждая находка: путь:строка + правило + доказательство (фрагмент кода), не bool." \
  --dod "Эталон-регрессия: 10 реальных заглушек из коммитов агента inkling (ветка devh/agent-inkling) обязаны быть найдены: e98fce2 core/git_failclosed.rs atomic_commit -> Ok(()); b41f4f1 core/tripwire_policy.rs arm_tripwire -> !zone.is_empty(); 34c0b93 core/weaken_scope.rs detect_weakening -> old.len() > new.len(); e910678 core/mutation_blindspot.rs check_blindspot -> path.exists(); b2a4ffd core/parser_contract.rs verify_summary_format -> path.exists(); d829ce5 core/autonomy_consequence.rs trace_action -> format!(\"след: {action}\"); e879693 core/structure_map.rs map_zone -> module; 4a28290 core/kani_specs.rs spec_invariant -> true; 6549a7f core/session_cwd.rs cwd_for_agent -> format!(\"/tmp/devh-work-{agent}\"); dbe0404 core/session_cwd.rs is_inside_worktree -> contains(agent)." \
  --dod "Ноль ложных срабатываний на 10 реальных завершённых модулях репозитория (core/ledger.rs, core/queue_policy.rs, core/handoff.rs, core/agent_state.rs, core/check_runner.rs, core/lock_guard.rs, core/registry_invariants.rs, core/task_transition.rs, core/rot_checker.rs, core/presence.rs) — отдельный тест-корпус." \
  --dod "Не менее 10 юнит-тестов зелёные. API: pub fn scan_diff(diff: &str) -> Vec<StubFinding>. Регистрация pub mod stub_check; в core/mod.rs — только если такой строки ещё нет (core/mod.rs в зону не входит)." \
  --check "cargo test -p devh --lib core::stub_check" --notify || exit 1

$D orch add --agent $O --p 2 --c 3 --prio 0 --dep T-190 \
  --title "ГЕЙТ КАЧЕСТВА (2/4): guard — задача с заглушками / без тестов / без вызовов отклоняется" \
  --zone devharness/src/core/guards/quality_guard.rs \
  --dod "Guard core/guards/quality_guard.rs собирает по задаче данные (diff зон против merge-base, результат core::stub_check, наличие #[test] в зоне, наличие вызовов новых pub-функций) и возвращает список нарушений: файл:строка + правило + причина. Любое нарушение = отказ, не тихий pass." \
  --dod "Регистрация в core/guards/mod.rs (строка pub mod quality_guard; добавляется только если её ещё нет) и вызов из общего selfcheck: devh orch guard --all обязан падать на задаче с заглушкой." \
  --dod "Не менее 8 юнит-тестов зелёные, включая: чистый код -> пустой список; заглушка -> нарушение; зона без тестов -> нарушение; вызов неиспользуемой функции -> нарушение." \
  --check "cargo test -p devh --lib core::guards::quality_guard" --notify || exit 1

$D orch add --agent $O --p 2 --c 3 --prio 0 --dep T-190 --dep T-191 --dep T-133 --dep T-152 --dep T-153 \
  --title "ГЕЙТ КАЧЕСТВА (3/4): СЛИЯНИЕ И КОММИТ — только свой task, только зарегистрированный worktree, заглушки не проходят" \
  --zone devharness/src/commands/orch.rs \
  --dod "devh orch commit <ID> --path ... обязан отказать (rc=1, понятная причина), если: (a) diff содержит заглушки (core::stub_check); (b) в зоне задачи нет ни одного #[test]; (c) вызывающий не исполнитель задачи и не lead (authz по реестру, не по строке адреса); (d) worktree задачи не зарегистрирован в реестре (чужой/самодельный путь или ветка)." \
  --dod "devh orch wt merge <agent> обязан ДО изменения main прогнать проверку по diff сливаемой ветки против merge-base и отказать (rc=1), если среди коммитов есть заглушки или задачи без подтверждённого DoD; main не трогается при отказе." \
  --dod "Обход через 'nothing to commit' закрыт: привязка к HEAD разрешена только если этот HEAD уже записан как commit_sha ЭТОЙ задачи; иначе rc=1 'изменений нет — задача не выполнена'." \
  --dod "Не менее 6 юнит-тестов зелёные, в том числе: коммит из чужого worktree отклонён; коммит с заглушкой отклонён; HEAD без изменений отклонён; merge с заглушкой отклонён и main не изменён." \
  --check "cargo test -p devh --lib commands::orch" --notify || exit 1

$D orch add --agent kilo-2ba65640 --p 1 --c 2 --prio 1 \
  --title "ГЕЙТ КАЧЕСТВА (4/4): детектор работы в обход оркестратора (прямой git, самодельный worktree, правки в main)" \
  --zone devharness/src/core/untracked_work.rs \
  --dod "Модуль core/untracked_work.rs (read-only, репозиторий не меняет) находит: (1) коммиты в ветках devh/agent-*, сделанные мимо devh orch commit (sha не совпадает ни с commit_sha задачи, ни с активной задачей агента); (2) worktree'ы, заведённые вручную и не зарегистрированные в реестре (пример-эталон: /tmp/devh-inkling-worktree с веткой devh/agent-inkling); (3) незакоммиченные правки в общем дереве main, сделанные не через фасад devh git." \
  --dod "Вывод: список находок (путь/ветка/sha/время + причина), структурированно, без изменения файлов репозитория." \
  --dod "Не менее 6 юнит-тестов зелёные; эталон-регрессия на структуре 'ветка + 3 коммита + task.commit_sha=null + незарегистрированный worktree'." \
  --check "cargo test -p devh --lib core::untracked_work" --notify || exit 1

$D orch add --agent kilo-2ba65640 --p 1 --c 2 --prio 1 \
  --title "НЕПРЕРЫВНОСТЬ РАБОТЫ: после done — следующая задача сразу; отчёт без результата отчётом не считается" \
  --zone devharness/src/core/continuity.rs \
  --dod "Модуль core/continuity.rs: (1) расчёт следующей задачи для агента по prio/deps/возрасту (FIFO + приоритет) — чтобы агент после сдачи не простаивал; (2) валидатор отчёта агента: письмо-отчёт без 4 пунктов и без ссылки на конкретный коммит/тест -> 'не отчёт'; текст вида 'не жду. работаю. не останавливаюсь.' без результата -> отчёт не засчитан, агенту возвращается требование с ровно одной следующей командой (devh orch start <ID>)." \
  --dod "Не менее 6 юнит-тестов зелёные, включая: очередь пуста -> агент не получает фиктивную задачу; отчёт без результата -> отклонён; отчёт с коммитом и тестом -> принят." \
  --check "cargo test -p devh --lib core::continuity" --notify || exit 1

echo "ADDED_OK"
