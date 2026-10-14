#!/usr/bin/env bash
# devharness/test.sh — regression suite харнесса.
# Идея из старой системы (5b/REGRESSION-1): тесты харнесса — обязательны.
# Полностью самодостаточен: temp-репо + стаб-агенты; ничего не трогает вне /tmp.
# shellcheck shell=bash
# shellcheck disable=SC2015  # ассерт-DSL: cond && pass || fail — pass/fail не падают

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
DEVH="$HERE/devh"
T="$(mktemp -d /tmp/devh-test-XXXXXX)"
trap 'rm -rf "$T"' EXIT
PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); echo "  PASS: $1"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }

fresh() { # $1=имя → путь к новому репо с bare-remote и одним коммитом
  local R="$T/$1"
  mkdir -p "$R"
  git init -q --bare "$T/$1.git"
  git init -q "$R"
  git -C "$R" remote add origin "$T/$1.git"
  echo base > "$R/b.txt"
  git -C "$R" add . >/dev/null && git -C "$R" commit -qm base
  git -C "$R" branch -M main
  git -C "$R" push -q -u origin main
  echo "$R"
}

base_env() {
  export HARNESS_AGENT="$T/stub.sh" HARNESS_WT_ROOT="$T/wt" HARNESS_VERIFY='test -f done.txt'
  export HARNESS_MUTATE="$T/mut-ok.sh {module}" HARNESS_MUTATE_SCOPE='.'
  export HARNESS_BENCH='' HARNESS_MAX_ACTIVE=3 HARNESS_MAX_FIX=3 HARNESS_AGENT_TIMEOUT=60
  export HARNESS_LOG_DAYS=30 HARNESS_NO_IGNORED=1 T_MUT_GOOD="$T/mut-good"
}

cat > "$T/stub.sh" <<'S'
#!/usr/bin/env bash
[ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$T_MUT_GOOD"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: маркер-файл создан\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
S
cat > "$T/stub-blocked.sh" <<'S'
#!/usr/bin/env bash
printf 'BLOCKED: не хватает данных\n' > "$HARNESS_SUMMARY"
S
cat > "$T/stub-noop.sh" <<'S'
#!/usr/bin/env bash
printf 'noop: ничего не делал\n' > "$HARNESS_SUMMARY"
S
cat > "$T/sleeper.sh" <<'S'
#!/usr/bin/env bash
sleep 5
S
cat > "$T/stub2.sh" <<'S'
#!/usr/bin/env bash
echo second > "$HARNESS_WORKTREE/done2.txt"
printf 'feat: второй агент отработал\n\nдетали: стаб2\n' > "$HARNESS_SUMMARY"
S
cat > "$T/stub-deny.sh" <<'S'
#!/usr/bin/env bash
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: реализация todo/40.md спеки\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
S
cat > "$T/stub-ign.sh" <<'S'
#!/usr/bin/env bash
[ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$HARNESS_WORKTREE/ign-fix"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: ignored-тесты разобраны\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
S
cat > "$T/mut-ok.sh" <<'S'
#!/usr/bin/env bash
echo "Mutants: 10, Killed: 10 (100%)"
S
cat > "$T/mut-bad.sh" <<'S'
#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi
S
cat > "$T/mut-garbage.sh" <<'S'
#!/usr/bin/env bash
echo "какой-то чужой вывод без маркеров"
exit 0
S
chmod +x "$T"/stub*.sh "$T/sleeper.sh" "$T"/mut-*.sh

echo "== 0. статика: shellcheck (полный прогон, 4 файла) =="
if command -v shellcheck >/dev/null 2>&1; then
  sc=0
  for f in devh test.sh selfmutate.sh examples/check_bench.sh; do
    shellcheck "$HERE/$f" >/dev/null 2>&1 || { echo "  shellcheck findings в $f"; sc=1; }
  done
  [ "$sc" = 0 ] && pass "shellcheck чист" || fail "shellcheck чист"
else
  echo "  SKIP: shellcheck не установлен"
fi

echo "== 1. happy path: new → run → merge (--no-red, mutation 100%) =="
R="$(fresh r1)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "item one" --no-red "стаб-сценарий" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "RED пропущен (записанный обход)" "$T/out" && pass "записанный обход RED" || fail "записанный обход RED"
grep -q "killed 10 (100%)" .harness/logs/W-001-mutate.log && pass "mutation 100%" || fail "mutation 100%"
"$DEVH" merge >"$T/out" 2>&1 && pass "merge exit 0" || fail "merge exit 0"
[ "$(cat .harness/items/W-001/status)" = "done" ] && pass "статус done" || fail "статус done"
[ "$(git log --oneline -1)" != "$(git log --oneline origin/main~1)" ] && pass "main продвинут" || fail "main продвинут"
[ -s .harness/log.md ] && grep -q "fix-rounds=" .harness/log.md && pass "log.md + run-метрики" || fail "log.md + run-метрики"
"$DEVH" logs W-001 >"$T/out" 2>&1 && grep -q "agent.log" "$T/out" && pass "devh logs показывает логи" || fail "devh logs показывает логи"
grep -q "agent-r1" .harness/logs/W-001-timing.log && pass "timing: agent-r1" || fail "timing: agent-r1"
grep -q "verify-r1" .harness/logs/W-001-timing.log && pass "timing: verify-r1" || fail "timing: verify-r1"
if ls "$T/wt/"*-W-001 >/dev/null 2>&1; then fail "worktree убран"; else pass "worktree убран"; fi

echo "== 2. mutation-гейт: 20% → fix-раунд → 100% =="
R="$(fresh r2)"; base_env; rm -f "$T/mut-good"
export HARNESS_MUTATE="$T/mut-bad.sh {module}"
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "mut item" --no-red "стаб" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "раунд фикса" "$T/out" && pass "fix-раунд был" || fail "fix-раунд был"
grep -q "killed 10 (100%)" .harness/logs/W-001-mutate.log && pass "kill-rate 100%" || fail "kill-rate 100%"

echo "== 3. RED-пруф: падает до, зеленеет после =="
R="$(fresh r3)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "red item" >/dev/null
echo 'test -f done.txt' > .harness/items/W-001/red.cmd
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "OK red: падает" "$T/out" && grep -q "OK red: позеленел" "$T/out" && pass "RED-цикл" || fail "RED-цикл"

echo "== 4. BLOCKED: run падает, abort → open =="
R="$(fresh r4)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "blocked" --no-red "стаб" >/dev/null
if HARNESS_AGENT="$T/stub-blocked.sh" HARNESS_VERIFY='true' "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "run упал (BLOCKED)"; fi
grep -q "BLOCKED: не хватает данных" "$T/out" && pass "BLOCKED-детект по подписи" || fail "BLOCKED-детект по подписи"
[ "$(cat .harness/items/W-001/status)" = active ] && pass "статус active" || fail "статус active"
"$DEVH" abort >/dev/null 2>&1 && pass "abort" || fail "abort"
[ "$(cat .harness/items/W-001/status)" = open ] && pass "статус open" || fail "статус open"

echo "== 5. noop-агент: нет изменений → run и merge отказывают =="
R="$(fresh r5)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "noop" --no-red "стаб" >/dev/null
if HARNESS_AGENT="$T/stub-noop.sh" HARNESS_VERIFY='true' "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "run упал (нет изменений)"; fi
if "$DEVH" merge >"$T/out" 2>&1; then fail "merge должен упасть"; else pass "merge упал (нет коммитов)"; fi
"$DEVH" abort >/dev/null 2>&1

echo "== 6. WIP-лимит =="
R="$(fresh r6)"; base_env; cd "$R" || exit 1
HARNESS_MAX_ACTIVE=1 "$DEVH" init >/dev/null
printf 'a\n' | "$DEVH" new "a" --no-red x >/dev/null
"$DEVH" run >/dev/null 2>&1 && pass "первый run ок" || fail "первый run ок"
printf 'b\n' | "$DEVH" new "b" --no-red x >/dev/null
if HARNESS_MAX_ACTIVE=1 "$DEVH" run W-002 >"$T/out" 2>&1; then fail "второй run (W-002) должен быть отклонён"; else pass "WIP-лимит отклонил W-002"; fi
grep -q "WIP-лимит" "$T/out" && pass "сообщение WIP-лимита" || fail "сообщение WIP-лимита"

echo "== 7. watchdog-timeout: висящий агент убит =="
R="$(fresh r7)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "sleep" --no-red x >/dev/null
if HARNESS_AGENT="$T/sleeper.sh" HARNESS_AGENT_TIMEOUT=2 "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "timeout убил агента"; fi
grep -q "timeout=" "$T/out" && pass "watchdog-подпись в ошибке" || fail "watchdog-подпись в ошибке"
"$DEVH" abort >/dev/null 2>&1

echo "== 8. параллельные new: id не склеиваются (flock) =="
R="$(fresh r8)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
( printf 's1\n' | "$DEVH" new "t1" >/dev/null 2>&1 ) &
( printf 's2\n' | "$DEVH" new "t2" >/dev/null 2>&1 ) &
wait
[ -d .harness/items/W-001 ] && [ -d .harness/items/W-002 ] && pass "W-001 и W-002 существуют" || fail "параллельные new склеились"
mkdir -p .harness/items/W-008 .harness/items/W-009   # октал-ловушка: 009 как октал невалиден
printf 'next
' | "$DEVH" new "after octal ids" >/dev/null
[ -d .harness/items/W-010 ] && pass "next_id пережил W-008/W-009 (10#)" || fail "next_id: октал-ловушка на W-008/009"

echo "== 9. run.lock: merge не идёт при живом run =="
R="$(fresh r9)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "lock" --no-red x >/dev/null
"$DEVH" run >/dev/null 2>&1
bash -c 'exec 8>"$1"; flock -x 8; sleep 3' _ ".harness/items/W-001/run.lock" &
sleep 0.3
if "$DEVH" merge >"$T/out" 2>&1; then fail "merge должен быть отклонён"; else grep -q "run.lock" "$T/out" && pass "merge отклонён (run.lock)" || fail "merge отклонён не из-за лока"; fi
wait 2>/dev/null
"$DEVH" merge >/dev/null 2>&1 && pass "merge после освобождения" || fail "merge после освобождения"

echo "== 10. doctor =="
R="$(fresh r10)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
"$DEVH" doctor >"$T/out" 2>&1 && pass "doctor exit 0" || { fail "doctor exit 0"; cat "$T/out"; }
grep -q "fail=0" "$T/out" && pass "doctor чист" || fail "doctor чист"

echo "== 11. per-item агент: маршрутизация по сложности =="
R="$(fresh r11)"; base_env; export HARNESS_VERIFY='test -f done2.txt'; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "agent item" --no-red x >/dev/null
echo "$T/stub2.sh" > .harness/items/W-001/agent
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0 (override-агент)" || { fail "run exit 0 (override-агент)"; cat "$T/out"; }
grep -q "per-item" "$T/out" && pass "override подхвачен" || fail "override подхвачен"

echo "== 12. адаптация uwa: todo/NN.md + report.md =="
R="$(fresh r12)"; base_env; cd "$R" || exit 1
mkdir -p todo
printf '# Session 40 — Дистрибуция\n\nСоздай файл done.txt.\n' > todo/40.md
printf '# Отчет\n\n## Железные правила\n1. CI зелёный до коммита.\n' > todo/report.md
"$DEVH" init >/dev/null
grep -q "RULES='todo/report.md'" .harness/config && pass "init: RULES=report.md" || fail "init: RULES=report.md"
"$DEVH" new 40 >/dev/null && [ -d .harness/items/W-040 ] && pass "new 40 → W-040" || fail "new 40 → W-040"
echo 'test -f done.txt' > .harness/items/W-040/red.cmd
grep -q "Дистрибуция" .harness/items/W-040/title && pass "title из заголовка спеки" || fail "title из заголовка спеки"
"$DEVH" hint 40 | grep -q "todo/report.md" && pass "справочник в промпте" || fail "справочник в промпте"
"$DEVH" run 40 >"$T/out" 2>&1 && pass "run 40" || { fail "run 40"; cat "$T/out"; }
"$DEVH" merge 40 >/dev/null 2>&1 && pass "merge 40" || fail "merge 40"
grep -q "## Session 40" todo/report.md && pass "report.md дописан сессией" || fail "report.md дописан сессией"

echo "== 13. гигиена сообщения (MSG_DENY) + devh commit =="
R="$(fresh r13)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "deny item" --no-red x >/dev/null
if HARNESS_AGENT="$T/stub-deny.sh" "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть (гигиена)"; else pass "run упал (гигиена сообщения)"; fi
grep -q "devh commit" "$T/out" && pass "подсказка лечения" || fail "подсказка лечения"
if "$DEVH" commit >"$T/out2" 2>&1; then fail "commit должен отклонить грязный summary (2-я линия)"; else pass "commit отклонил грязный summary (2-я линия)"; fi
grep -q "всё ещё нарушает гигиену" "$T/out2" && pass "сообщение 2-й линии гигиены" || fail "сообщение 2-й линии гигиены"
printf 'feat: маркер добавлен\n\nдетали: стаб\n' > .harness/summaries/W-001.md
"$DEVH" commit >"$T/out" 2>&1 && pass "devh commit восстановил" || { fail "devh commit восстановил"; cat "$T/out"; }
"$DEVH" merge >/dev/null 2>&1 && pass "merge после commit" || fail "merge после commit"

echo "== 14. RED обязателен: без red.cmd и без --no-red run отказывает =="
R="$(fresh r14)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "no red" >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть (нет RED)"; else pass "run упал (нет RED)"; fi
grep -q "RED-пруф ОБЯЗАТЕЛЕН" "$T/out" && pass "сообщение про RED-пруф" || fail "сообщение про RED-пруф"
"$DEVH" abort >/dev/null 2>&1

echo "== 15. MUTATE обязателен: пустой MUTATE = блокер =="
R="$(fresh r15)"; base_env; cd "$R" || exit 1
HARNESS_MUTATE='' "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "m item" --no-red x >/dev/null
if env -u HARNESS_MUTATE "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть (MUTATE)"; else pass "run упал (MUTATE обязателен)"; fi
grep -q "MUTATE-гейт ОБЯЗАТЕЛЕН" "$T/out" && pass "сообщение про MUTATE" || fail "сообщение про MUTATE"
[ "$(cat .harness/items/W-001/status)" = open ] && pass "статус open (fail-fast до claim)" || fail "статус open"

echo "== 16. mutation env-блокер: нераспознанный инструмент = FAIL, не fix-раунд =="
R="$(fresh r16)"; base_env; export HARNESS_MUTATE="$T/mut-garbage.sh"; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "env item" --no-red x >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть (env)"; else pass "run упал (блокер среды)"; fi
grep -q "Блокер СРЕДЫ" "$T/out" && pass "блокер среды помечен" || fail "блокер среды помечен"
grep -q "раунд фикса" "$T/out" && fail "не должен быть fix-раунд" || pass "не fix-раунд (env ≠ работа агента)"
"$DEVH" abort >/dev/null 2>&1

echo "== 17. NO-IGNORED: ignored>0 = скрытый красный =="
R="$(fresh r17)"; base_env
export HARNESS_VERIFY='if [ -f ign-fix ]; then echo "3 passed; 0 failed; 0 ignored"; else echo "3 passed; 0 failed; 2 ignored"; fi; test -f done.txt'
export HARNESS_AGENT="$T/stub-ign.sh"
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "ign item" --no-red x >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "гейт NO-IGNORED упал" "$T/out" && pass "NO-IGNORED fix-раунд был" || fail "NO-IGNORED fix-раунд был"

echo "== 18. mutation: удалённый агентом файл пропускается, не env-блокер =="
cat > "$T/stub-del.sh" <<'S'
#!/usr/bin/env bash
rm -f "$HARNESS_WORKTREE/b.txt"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: рефактор с удалением\n\nдетали\n' > "$HARNESS_SUMMARY"
S
chmod +x "$T/stub-del.sh"
R="$(fresh r18)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "del item" --no-red x >/dev/null
HARNESS_AGENT="$T/stub-del.sh" "$DEVH" run >"$T/out" 2>&1 && pass "run exit 0 (удаление не блокер)" || { fail "run exit 0"; cat "$T/out"; }
grep -q "удалён агентом — мутации не требуется" .harness/logs/W-001-mutate.log && pass "удалённый файл пропущен" || fail "удалённый файл пропущен"

echo "== 19. resume: RED pre-check не ломает повторный run (работа уже в дереве) =="
cat > "$T/stub-blk-red.sh" <<'S'
#!/usr/bin/env bash
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'BLOCKED: первая попытка\n' > "$HARNESS_SUMMARY"
S
chmod +x "$T/stub-blk-red.sh"
R="$(fresh r19)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "resume item" >/dev/null
echo 'test -f done.txt' > .harness/items/W-001/red.cmd
if HARNESS_AGENT="$T/stub-blk-red.sh" "$DEVH" run >"$T/out" 2>&1; then fail "1-й run должен упасть"; else pass "1-й run BLOCKED (RED доказан на чистом HEAD)"; fi
HARNESS_AGENT="$T/stub.sh" "$DEVH" run >"$T/out" 2>&1 && pass "resume run exit 0" || { fail "resume run exit 0"; cat "$T/out"; }
grep -q "RED pre-check пропущен" "$T/out" && pass "pre-check пропущен на resume" || fail "pre-check пропущен на resume"
"$DEVH" merge >/dev/null 2>&1 && pass "merge после resume" || fail "merge после resume"

echo "== 20. mutation single-run (без {module}): kill-rate гейтит и эту ветку =="
cat > "$T/mut-bad2.sh" <<'S'
#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi
S
chmod +x "$T/mut-bad2.sh"
R="$(fresh r20)"; base_env; rm -f "$T/mut-good"; export HARNESS_MUTATE="$T/mut-bad2.sh"
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека
' | "$DEVH" new "single item" --no-red "стаб" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0 (single-run mutation)" || { fail "run exit 0"; cat "$T/out"; }
grep -q "раунд фикса" "$T/out" && pass "fix-раунд был (single-run)" || fail "fix-раунд был (single-run)"
grep -q "killed 10 (100%)" .harness/logs/W-001-mutate.log && pass "kill-rate 100% (single-run)" || fail "kill-rate 100% (single-run)"

echo
echo "ИТОГО: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
