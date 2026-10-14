#!/usr/bin/env bash
# devharness/test.sh — regression suite харнесса.
# Идея из старой системы (5b/REGRESSION-1): тесты харнесса — обязательны.
# Полностью самодостаточен: temp-репо + стаб-агенты; ничего не трогает вне /tmp.
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
  export HARNESS_MUTATE='' HARNESS_BENCH='' HARNESS_MUTATE_SCOPE='.'
  export HARNESS_MAX_ACTIVE=3 HARNESS_MAX_FIX=3 HARNESS_AGENT_TIMEOUT=60 HARNESS_LOG_DAYS=30
}

# стаб-агент: создаёт done.txt; на втором вызове включает мутационный "ремонт"
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
sleep 60
S
cat > "$T/fake-mutate.sh" <<'S'
#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi
S
chmod +x "$T"/stub*.sh "$T/sleeper.sh" "$T/fake-mutate.sh"

echo "== 1. happy path: new → run → merge =="
R="$(fresh r1)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "item one" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
"$DEVH" merge >"$T/out" 2>&1 && pass "merge exit 0" || fail "merge exit 0"
[ "$(cat .harness/items/W-001/status)" = done ] && pass "статус done" || fail "статус done"
[ "$(git log --oneline -1)" != "$(git log --oneline origin/main~1)" ] && pass "main продвинут" || fail "main продвинут"
[ -s .harness/log.md ] && pass "log.md записан" || fail "log.md записан"
[ ! -d "$T/wt/devh-W-001" ] && pass "worktree убран" || fail "worktree убран"

echo "== 2. mutation-гейт: 20% → fix-раунд → 100% =="
R="$(fresh r2)"; base_env
export HARNESS_MUTATE="$T/fake-mutate.sh {module}" HARNESS_MUTATE_SCOPE='.'
export T_MUT_GOOD="$T/mut-good"; rm -f "$T_MUT_GOOD"
cd "$R"; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "mut item" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "раунд фикса" "$T/out" && pass "fix-раунд был" || fail "fix-раунд был"
grep -q "killed 10 (100%)" .harness/logs/W-001-mutate.log && pass "kill-rate 100%" || fail "kill-rate 100%"

echo "== 3. RED-пруф: падает до, зеленеет после =="
R="$(fresh r3)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "red item" >/dev/null
echo 'test -f done.txt' > .harness/items/W-001/red.cmd
"$DEVH" run >"$T/out" 2>&1 && pass "run exit 0" || { fail "run exit 0"; cat "$T/out"; }
grep -q "OK red: падает" "$T/out" && grep -q "OK red: позеленел" "$T/out" && pass "RED-цикл" || fail "RED-цикл"

echo "== 4. BLOCKED: run падает, статус active, abort → open =="
R="$(fresh r4)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "blocked" >/dev/null
if HARNESS_AGENT="$T/stub-blocked.sh" HARNESS_VERIFY='true' "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "run упал (BLOCKED)"; fi
[ "$(cat .harness/items/W-001/status)" = active ] && pass "статус active" || fail "статус active"
"$DEVH" abort >/dev/null 2>&1 && pass "abort" || fail "abort"
[ "$(cat .harness/items/W-001/status)" = open ] && pass "статус open" || fail "статус open"

echo "== 5. noop-агент: нет изменений → run и merge отказывают =="
R="$(fresh r5)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "noop" >/dev/null
if HARNESS_AGENT="$T/stub-noop.sh" HARNESS_VERIFY='true' "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "run упал (нет изменений)"; fi
if "$DEVH" merge >"$T/out" 2>&1; then fail "merge должен упасть"; else pass "merge упал (нет коммитов)"; fi
"$DEVH" abort >/dev/null 2>&1

echo "== 6. WIP-лимит =="
R="$(fresh r6)"; base_env; cd "$R"
HARNESS_MAX_ACTIVE=1 "$DEVH" init >/dev/null
printf 'a\n' | "$DEVH" new "a" >/dev/null
"$DEVH" run >/dev/null 2>&1 && pass "первый run ок" || fail "первый run ок"
printf 'b\n' | "$DEVH" new "b" >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then fail "второй run должен быть отклонён"; else pass "WIP-лимит отклонил"; fi

echo "== 7. watchdog-timeout: висящий агент убит =="
R="$(fresh r7)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "sleep" >/dev/null
if HARNESS_AGENT="$T/sleeper.sh" HARNESS_AGENT_TIMEOUT=2 "$DEVH" run >"$T/out" 2>&1; then fail "run должен упасть"; else pass "timeout убил агента"; fi
"$DEVH" abort >/dev/null 2>&1

echo "== 8. параллельные new: id не склеиваются (flock) =="
R="$(fresh r8)"; base_env; cd "$R"
"$DEVH" init >/dev/null
( printf 's1\n' | "$DEVH" new "t1" >/dev/null 2>&1 ) &
( printf 's2\n' | "$DEVH" new "t2" >/dev/null 2>&1 ) &
wait
[ -d .harness/items/W-001 ] && [ -d .harness/items/W-002 ] && pass "W-001 и W-002 существуют" || fail "параллельные new склеились"

echo "== 9. run.lock: merge не идёт при живом run =="
R="$(fresh r9)"; base_env; cd "$R"
"$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "lock" >/dev/null
"$DEVH" run >/dev/null 2>&1
# держим run.lock из постороннего процесса 3 секунды
bash -c 'exec 8>"$1"; flock -x 8; sleep 3' _ ".harness/items/W-001/run.lock" &
sleep 0.3
if "$DEVH" merge >"$T/out" 2>&1; then fail "merge должен быть отклонён"; else grep -q "run.lock" "$T/out" && pass "merge отклонён (run.lock)" || fail "merge отклонён не из-за лока"; fi
wait 2>/dev/null
"$DEVH" merge >/dev/null 2>&1 && pass "merge после освобождения" || fail "merge после освобождения"

echo "== 10. doctor =="
R="$(fresh r10)"; base_env; cd "$R"
"$DEVH" init >/dev/null
"$DEVH" doctor >"$T/out" 2>&1 && pass "doctor exit 0" || { fail "doctor exit 0"; cat "$T/out"; }
grep -q "fail=0" "$T/out" && pass "doctor чист" || fail "doctor чист"

echo
echo "ИТОГО: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
