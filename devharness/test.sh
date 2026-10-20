#!/usr/bin/env bash
# devharness/test.sh — regression suite харнесса.
# Идея из старой системы (5b/REGRESSION-1): тесты харнесса — обязательны.
# Полностью самодостаточен: temp-репо + стаб-агенты; ничего не трогает вне /tmp.
# shellcheck shell=bash
# shellcheck disable=SC2015  # ассерт-DSL: cond && pass || fail — pass/fail не падают

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
DEVH="${DEVH:-$HERE/devh}"
# CI-раннеры без git-identity; фиксируем тестовую — suite environment-независим
export GIT_AUTHOR_NAME=devh-test GIT_AUTHOR_EMAIL=devh-test@localhost
export GIT_COMMITTER_NAME=devh-test GIT_COMMITTER_EMAIL=devh-test@localhost
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
  export HARNESS_REVIEW=0 HARNESS_LINT=1
  # сброс гейтов, которые экспортят отдельные секции (иначе утекают в следующие)
  unset HARNESS_CRAP HARNESS_CRAP_SCOPE HARNESS_CRAP_MAX
  unset HARNESS_RED_MUTATE HARNESS_RED_MIN_KILL
}

smoke_reset_env() { # сброс утечек HARNESS_* между секциями: конфиг smoke-репо живёт в файле
  local k
  for k in $(env | sed -n 's/^\(HARNESS_[A-Z_]*\)=.*/\1/p'); do unset "$k"; done
}

ADV_OK=0; ADV_N=0
install_tests() { # мини-раннер тестов в текущем temp-репо: RED = прогон тестов, а не grep
  mkdir -p t scripts
  cat > scripts/test.sh <<'EOF'
#!/usr/bin/env bash
pat="${1:-}"
n=0; bad=0
for f in t/*.sh; do
  [ -f "$f" ] || continue
  if [ -n "$pat" ] && [[ "$f" != *"$pat"* ]]; then continue; fi
  n=$((n+1))
  if bash "$f" >/dev/null 2>&1; then echo "ok   $f"; else echo "FAIL $f"; bad=$((bad+1)); fi
done
echo "$n tests, $bad failures"
exit "$bad"
EOF
  cat > t/feature_test.sh <<'EOF'
#!/usr/bin/env bash
[ -s done.txt ] || { echo "done.txt отсутствует"; exit 1; }
grep -qx marker done.txt || { echo "маркер не опубликован"; exit 1; }
EOF
  git add . >/dev/null 2>&1 && git commit -qm "тесты проекта"
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
cat > "$T/stub-red.sh" <<'S'
#!/usr/bin/env bash
red="$(dirname "$HARNESS_SPEC")/red.cmd"
if [ ! -f "$red" ]; then
  printf '%s\n' "$1" > "$(dirname "$red")/red-prompt.txt"
  printf '%s\n' "${HARNESS_VERIFY:-bash scripts/test.sh}" > "$red"
else
  [ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$T_MUT_GOOD"
  echo marker > "$HARNESS_WORKTREE/done.txt"
  grep -q "done2" "$HARNESS_SPEC" && echo marker > "$HARNESS_WORKTREE/done2.txt"
  printf 'feat: red.cmd написан и задача сделана\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
fi
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
cat > "$T/redmut.sh" <<'S'
#!/usr/bin/env bash
case "$1" in
  *second*) echo "Mutants: 10, Killed: 2 (20%)";;
  *) if bash -c "$1" >/dev/null 2>&1; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi;;
esac
S
cat > "$T/crap-ok.sh" <<'S'
#!/usr/bin/env bash
echo "src/lib.rs:10 parse_ok C=6 cov=45"
echo "src/bin.rs:20 run_cli C=3 cov=0"
echo "CRAP: functions=2"
S
cat > "$T/crap-fix.sh" <<'S'
#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then
  echo "src/lib.rs:10 parse_ok C=6 cov=45"
  echo "CRAP: functions=1"
else
  echo "src/big.rs:5 complex C=14 cov=100"
  echo "CRAP: functions=1"
fi
S
cat > "$T/crap-bad.sh" <<'S'
#!/usr/bin/env bash
echo "src/big.rs:5 complex C=14 cov=100"
echo "CRAP: functions=1"
S
cat > "$T/crap-garbage.sh" <<'S'
#!/usr/bin/env bash
echo "какой-то чужой вывод без маркеров"
S
cat > "$T/crap-bounds.sh" <<'S'
#!/usr/bin/env bash
echo "src/a.rs:1 edge C=12 cov=100"
echo "src/b.rs:2 zero C=3 cov=0"
echo "CRAP: functions=2"
S
cat > "$T/crap-99.sh" <<'S'
#!/usr/bin/env bash
echo "src/c.rs:3 tight C=12 cov=99"
echo "CRAP: functions=1"
S
cat > "$T/crap-zero.sh" <<'S'
#!/usr/bin/env bash
echo "CRAP: functions=0"
S
chmod +x "$T"/stub*.sh "$T/sleeper.sh" "$T"/mut-*.sh "$T"/crap-*.sh "$T/redmut.sh"

echo "== 0. статика: shellcheck (полный прогон, 11 файлов) =="
if command -v shellcheck >/dev/null 2>&1; then
  sc=0
  for f in devh test.sh selfmutate.sh examples/check_bench.sh stubs/good.sh tests/mk_smoke_repo.sh \
    stubs/red-green.sh stubs/red-wrong.sh stubs/no-summary.sh stubs/bad-summary.sh stubs/hang.sh; do
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
HARNESS_COLOR=always "$DEVH" doctor >"$T/out" 2>&1; grep -q $'\033\[3' "$T/out" && pass "HARNESS_COLOR=always даёт ANSI" || fail "HARNESS_COLOR=always даёт ANSI"
"$DEVH" doctor >"$T/out" 2>&1; ! grep -q $'\033' "$T/out" && pass "piped вывод без ANSI" || fail "piped вывод без ANSI"

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

echo "== 17. NO-IGNORED: незаявленный игнор = скрытый красный =="
R="$(fresh r17)"; base_env
export HARNESS_VERIFY='if [ -f ign-fix ]; then echo "test result: ok. 3 passed; 0 failed; 0 ignored"; else echo "test mod::hidden_one ... ignored"; echo "test mod::hidden_two ... ignored, причина"; echo "test result: ok. 1 passed; 2 ignored"; fi; test -f done.txt'
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

echo "== 21. bg/ps: detached-прогон и наблюдение за ним =="
R="$(fresh r21)"; base_env; export HARNESS_VERIFY='sleep 6; test -f done.txt'; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека
' | "$DEVH" new "bg item" --no-red x >/dev/null
"$DEVH" bg W-001 >"$T/out" 2>&1 && pass "bg вернул управление сразу" || { fail "bg вернул управление"; cat "$T/out"; }
grep -q "PID" "$T/out" && pass "bg печатает PID" || fail "bg печатает PID"
sleep 1
"$DEVH" ps >"$T/out" 2>&1; grep -q "W-001" "$T/out" && pass "ps видит прогон в полёте" || { fail "ps видит прогон"; cat "$T/out"; }
n=0; while [ -f .harness/items/W-001/run.pid ] && [ "$n" -lt 40 ]; do sleep 1; n=$((n+1)); done
[ ! -f .harness/items/W-001/run.pid ] && pass "прогон завершился (pid-файл убран trap-ом)" || fail "pid-файл завис"
[ "$(cat .harness/items/W-001/status)" = active ] && pass "bg-прогон дошёл до конца (active)" || fail "bg-прогон не дошёл"
"$DEVH" merge >/dev/null 2>&1 && pass "merge после bg" || fail "merge после bg"

echo "== 22. tickets: OnFlyAnalysis-наблюдения → items с red-идеей =="
R="$(fresh r22)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'crates/foo.rs:42 | дублирование логики с bar.rs | red-тест: тест на общий хелпер\nстрока без red-идеи — должна быть отклонена\n' > .harness/observations.md
"$DEVH" tickets >"$T/out" 2>&1 && grep -q "foo.rs:42" "$T/out" && pass "tickets: список показывает наблюдения" || fail "tickets: список"
"$DEVH" tickets --import >"$T/out" 2>&1 && pass "tickets --import ок" || { fail "tickets --import"; cat "$T/out"; }
[ -d .harness/items/W-001 ] && grep -q "RED-тест ОБЯЗАТЕЛЕН" .harness/items/W-001/spec.md && pass "тикет с red-идеей в спеке" || fail "тикет с red-идеей"
[ ! -s .harness/observations.md ] && pass "очередь наблюдений опустошена" || fail "очередь не опустошена"
grep -q "нет red-идеи" "$T/out" && pass "наблюдение без red-идеи отклонено" || fail "наблюдение без red-идеи не отклонено"

echo "== 23. LINT-гейт: авто-shellcheck на изменённых *.sh =="
cat > "$T/stub-lint.sh" <<'S'
#!/usr/bin/env bash
if [ -f "$HARNESS_WORKTREE/clean.sh" ]; then
  printf '#!/usr/bin/env bash\nx="$1"\necho "$x"\n' > "$HARNESS_WORKTREE/clean.sh"
else
  printf 'x=$1\necho $x\n' > "$HARNESS_WORKTREE/clean.sh"
fi
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: lint-фикс\n\nдетали\n' > "$HARNESS_SUMMARY"
S
chmod +x "$T/stub-lint.sh"
R="$(fresh r23)"; base_env; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека
' | "$DEVH" new "lint item" --no-red x >/dev/null
HARNESS_AGENT="$T/stub-lint.sh" "$DEVH" run >"$T/out" 2>&1 && pass "run ок после lint-фикс-раунда" || { fail "run ок после lint"; cat "$T/out"; }
grep -q "гейт LINT упал" "$T/out" && pass "LINT fix-раунд был" || fail "LINT fix-раунд был"

echo "== 24. REVIEW-гейт: self-review диффа вторым запросом =="
cat > "$T/stub-rev.sh" <<'S'
#!/usr/bin/env bash
if printf '%s' "$1" | grep -q "независимый ревьюер"; then
  if [ -f "$T_REV_OK" ]; then echo "VERDICT: OK"; else touch "$T_REV_OK"; echo "VERDICT: ISSUES: race в хелпере; нет edge-case теста"; fi
else
  echo marker > "$HARNESS_WORKTREE/done.txt"
  printf 'feat: review-фикс\n\nдетали\n' > "$HARNESS_SUMMARY"
fi
S
chmod +x "$T/stub-rev.sh"
R="$(fresh r24)"; base_env; export HARNESS_REVIEW=1 T_REV_OK="$T/rev-ok"; rm -f "$T/rev-ok"; cd "$R" || exit 1
"$DEVH" init >/dev/null
printf 'спека
' | "$DEVH" new "rev item" --no-red x >/dev/null
HARNESS_AGENT="$T/stub-rev.sh" "$DEVH" run >"$T/out" 2>&1 && pass "run ок после review-фикс-раунда" || { fail "run ок после review"; cat "$T/out"; }
grep -q "гейт REVIEW упал" "$T/out" && pass "REVIEW fix-раунд был" || fail "REVIEW fix-раунд был"
grep -q "VERDICT: OK" .harness/logs/W-001-review.log && pass "финальный вердикт OK в review-логе" || fail "финальный вердикт OK в review-логе"

echo "== 25. auto: папка спек → всё само (без RED в спеке его пишет агент) =="
R="$(fresh r25)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh feature'
cd "$R" || exit 1
install_tests
cat > t/second_test.sh <<'EOF'
#!/usr/bin/env bash
[ -s done2.txt ] || { echo "done2.txt отсутствует"; exit 1; }
grep -qx marker done2.txt || { echo "маркер не опубликован"; exit 1; }
EOF
git add -A >/dev/null 2>&1 && git commit -qm "второй тест"
"$DEVH" init >/dev/null
mkdir -p specs
printf '# Задача А без RED\nСоздай done.txt с маркером.\n' > specs/spec-a.md
printf '# Задача Б\nRED: bash scripts/test.sh second\nСоздай done2.txt с маркером.\n' > specs/spec-b.md
"$DEVH" auto --from specs >"$T/out" 2>&1 && pass "auto завершился rc=0" || { fail "auto rc"; cat "$T/out"; }
grep -q "импортировано 2 спек" "$T/out" && pass "импорт обеих спек" || fail "импорт обеих спек"
grep -q "RED-пруфа нет — его напишет агент" "$T/out" && pass "auto сообщил: red.cmd напишет агент" || fail "auto сообщил про red.cmd"
[ "$(cat .harness/items/W-001/status)" = "done" ] && pass "спека А без RED прошла цикл" || fail "спека А не прошла"
grep -qx 'bash scripts/test.sh feature' .harness/items/W-001/red.cmd && pass "W-001 red.cmd создан агентом: прогон тестов" || fail "W-001 red.cmd не тест"
grep -qx 'bash scripts/test.sh second' .harness/items/W-002/red.cmd && pass "W-002 RED из строки спеки сохранён (раннер совпал с VERIFY)" || fail "W-002 RED из спеки"
[ "$(cat .harness/items/W-002/status)" = "done" ] && pass "спека Б прошла цикл" || fail "спека Б не прошла"
grep -q "остался:" "$T/out" && fail "остаток должен быть пуст" || pass "остаток пуст (обе спеки закрыты)"

echo "== 26. CRAP-гейт (guardrail №2): score = C²·(1−cov)³ + C, порог 12 =="
R="$(fresh r26)"; base_env; export HARNESS_CRAP="$T/crap-ok.sh" HARNESS_CRAP_SCOPE='.' HARNESS_MAX_FIX=3
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap ok" --no-red x >/dev/null
"$DEVH" run >"$T/out" 2>&1 && grep -q "0 violations" .harness/logs/W-001-crap.log \
  && pass "CRAP зелёный: 0 violations в логе" || { fail "CRAP зелёный: 0 violations в логе"; cat "$T/out"; }

R="$(fresh r27)"; base_env; rm -f "$T/mut-good"; export HARNESS_CRAP="$T/crap-fix.sh" HARNESS_CRAP_SCOPE='.' HARNESS_MAX_FIX=3
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap fix" --no-red x >/dev/null
"$DEVH" run >"$T/out" 2>&1 && grep -q "гейт CRAP упал" "$T/out" && grep -q "0 violations" .harness/logs/W-001-crap.log \
  && pass "CRAP fix-раунд довёл до 0 violations" || { fail "CRAP fix-раунд довёл до 0 violations"; cat "$T/out"; }

R="$(fresh r28)"; base_env; export HARNESS_CRAP="$T/crap-garbage.sh" HARNESS_CRAP_SCOPE='.' HARNESS_MAX_FIX=3
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap env" --no-red x >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then
  fail "CRAP env-блокер ≠ fix-раунд, статус active"
else
  grep -q "Блокер СРЕДЫ" "$T/out" && ! grep -q "раунд фикса" "$T/out" && [ "$(cat .harness/items/W-001/status)" = active ] \
    && pass "CRAP env-блокер ≠ fix-раунд, статус active" || { fail "CRAP env-блокер ≠ fix-раунд, статус active"; cat "$T/out"; }
fi

R="$(fresh r29)"; base_env; rm -f "$T/mut-good"; export HARNESS_CRAP="$T/crap-bad.sh" HARNESS_CRAP_SCOPE='.'
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap max" --no-red x >/dev/null
if HARNESS_MAX_FIX=0 HARNESS_CRAP_MAX=12 "$DEVH" run >"$T/out" 2>&1; then
  fail "CRAP_MAX: score=14 красный при 12, зелёный при 20"
elif grep -q "score=14" .harness/logs/W-001-crap.log && HARNESS_MAX_FIX=3 HARNESS_CRAP_MAX=20 "$DEVH" run >"$T/out2" 2>&1; then
  pass "CRAP_MAX: score=14 красный при 12, зелёный при 20"
else
  fail "CRAP_MAX: score=14 красный при 12, зелёный при 20"; cat "$T/out" "$T/out2"
fi

R="$(fresh r30)"; base_env; export HARNESS_MAX_FIX=3 HARNESS_CRAP_SCOPE='.'
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap skip" --no-red x >/dev/null
ok1=0; HARNESS_CRAP="$T/crap-zero.sh" "$DEVH" run >"$T/out" 2>&1 && ok1=1
printf 'спека\n' | "$DEVH" new "crap skip2" --no-red x >/dev/null
if [ "$ok1" = 1 ] && HARNESS_CRAP="$T/crap-garbage.sh" HARNESS_CRAP_SCOPE=zzz "$DEVH" run W-002 >"$T/out2" 2>&1; then
  pass "CRAP: functions=0 и пустой scope — гейт пропущен, не env-блокер"
else
  fail "CRAP: functions=0 и пустой scope — гейт пропущен, не env-блокер"; cat "$T/out" "$T/out2"
fi

R="$(fresh r31)"; base_env; export HARNESS_CRAP="$T/crap-garbage.sh" HARNESS_CRAP_SCOPE='.' HARNESS_MAX_FIX=3
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap bypass" --no-red x --no-crap "инструмент недоступен" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && grep -q "CRAP пропущен (записанный обход): инструмент недоступен" "$T/out" \
  && pass "записанный обход CRAP (--no-crap) пропущен гейт" || { fail "записанный обход CRAP (--no-crap) пропущен гейт"; cat "$T/out"; }

R="$(fresh r32)"; base_env; export HARNESS_MAX_FIX=3
cd "$R" || exit 1; "$DEVH" init >/dev/null
printf 'спека\n' | "$DEVH" new "crap bounds" --no-red x >/dev/null
ok1=0; HARNESS_CRAP="$T/crap-bounds.sh" HARNESS_CRAP_SCOPE='.' "$DEVH" run >"$T/out" 2>&1 && ok1=1
if [ "$ok1" = 1 ] && HARNESS_CRAP="$T/crap-99.sh" HARNESS_CRAP_SCOPE='.' HARNESS_MAX_FIX=0 "$DEVH" run >"$T/out2" 2>&1; then
  fail "граница CRAP: 12@100 и 3@0 зелёные, 12@99 красный"
elif [ "$ok1" = 1 ]; then
  pass "граница CRAP: 12@100 и 3@0 зелёные, 12@99 красный"
else
  fail "граница CRAP: bounds-прогон должен пройти"; cat "$T/out"
fi

if HARNESS_CRAP="$T/crap-garbage.sh" "$DEVH" doctor >"$T/out" 2>&1; then
  fail "doctor ловит сломанный CRAP-инструмент"
else
  grep -q "FAIL" "$T/out" && grep -q "CRAP" "$T/out" \
    && pass "doctor ловит сломанный CRAP-инструмент" || { fail "doctor ловит сломанный CRAP-инструмент"; cat "$T/out"; }
fi

echo "== 27. NO-IGNORED allowlist: заявленный игнор пропускается, незаявленный = красный =="
# (a) заявленный в allowlist игнор → run зелёный
R="$(fresh r27a)"; base_env; export HARNESS_MAX_FIX=0
export HARNESS_VERIFY='echo "test suite::declared_slow ... ignored, сеть"; echo "test result: ok. 1 passed; 1 ignored"; test -f done.txt'
export HARNESS_AGENT="$T/stub.sh"
cd "$R" || exit 1; "$DEVH" init >/dev/null
mkdir -p scripts && printf 'declared_slow\n' > scripts/ignored-tests.txt
printf 'спека\n' | "$DEVH" new "ign allow ok" --no-red x >/dev/null
"$DEVH" run >"$T/out" 2>&1 && ! grep -q "NO-IGNORED" "$T/out" \
  && pass "allowlist: заявленный игнор НЕ красит NoIgnored" || { fail "allowlist: заявленный игнор НЕ красит NoIgnored"; cat "$T/out"; }
# (b) незаявленный игнор (allowlist есть, но имя не в нём) → красный
R="$(fresh r27b)"; base_env; export HARNESS_MAX_FIX=0
export HARNESS_VERIFY='echo "test suite::rogue_test ... ignored"; echo "test result: ok. 0 passed; 1 ignored"; test -f done.txt'
export HARNESS_AGENT="$T/stub.sh"
cd "$R" || exit 1; "$DEVH" init >/dev/null
mkdir -p scripts && printf 'declared_slow\n' > scripts/ignored-tests.txt
printf 'спека\n' | "$DEVH" new "ign allow bad" --no-red x >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then
  fail "allowlist: незаявленный игнор всё ещё красный"
else
  grep -q "NO-IGNORED" "$T/out" && grep -q "rogue_test" .harness/logs/W-001-verify.log \
    && pass "allowlist: незаявленный игнор красный, имя в verify-логе" || { fail "allowlist: незаявленный игнор красный, имя в verify-логе"; cat "$T/out"; }
fi

echo "== 28. RED пишет агент: дал только промпт → red-ТЕСТ его первая задача =="
R="$(fresh r33)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh'
cd "$R" || exit 1
install_tests
"$DEVH" init >/dev/null
printf 'спека: опубликуй done.txt с маркером\n' | "$DEVH" new "только промпт" >/dev/null
[ -f .harness/items/W-001/red.cmd ] && fail "red.cmd не должен быть до запуска" || pass "до запуска red.cmd нет (дал только промпт)"
"$DEVH" run >"$T/out" 2>&1 && pass "run зелёный без RED в спеке" || { fail "run зелёный без RED в спеке"; cat "$T/out"; }
grep -q "поручаю агенту написать его первой задачей" "$T/out" && pass "харнесс поручил red.cmd агенту" || fail "харнесс поручил red.cmd агенту"
grep -q "OK red: падает на HEAD" "$T/out" && pass "RED pre: тест падает на чистом HEAD" || fail "RED pre"
grep -q "OK red: позеленел" "$T/out" && pass "RED post: тест зелёный после правок" || fail "RED post"
grep -q "scripts/test.sh" .harness/items/W-001/red.cmd && pass "red.cmd — прогон тестового раннера" || fail "red.cmd не гоняет тесты"
grep -Eq '(^|[[:space:]])(grep|rg|test)[[:space:]-]' .harness/items/W-001/red.cmd && fail "red.cmd — это grep/проверка, а не тест" || pass "red.cmd — не grep и не проверка существования"
grep -q "ЗАПРЕЩЕНО вместо теста" .harness/items/W-001/red-prompt.txt && pass "red-author промпт запрещает grep/проверку существования" || fail "red-author промпт не запрещает grep"
grep -q "red-author" .harness/logs/W-001-timing.log && pass "стадия red-author в тайминге" || fail "стадия red-author"

echo "== 29. RED-LINK: red.cmd обязан гонять тот же раннер, что и VERIFY =="
R="$(fresh r34)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh' HARNESS_MAX_FIX=0
cd "$R" || exit 1
install_tests
"$DEVH" init >/dev/null
printf '# Задача\nRED: test -f done.txt\nСоздай done.txt с маркером.\n' | "$DEVH" new "чужой раннер" >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then
  fail "RED-LINK: несовпадение раннера должно быть красным"
else
  grep -q "RED-LINK" "$T/out" && pass "RED-LINK: жёсткий отказ при несовпадении раннера" || { fail "RED-LINK: жёсткий отказ"; cat "$T/out"; }
fi
grep -qx 'test -f done.txt' .harness/items/W-001/red.cmd && pass "RED-LINK: спека не переписана при отказе" || fail "RED-LINK: red.cmd спеки переписали"
[ "$(cat .harness/items/W-001/status)" != "done" ] && pass "RED-LINK: статус не done" || fail "RED-LINK: статус done"

R="$(fresh r35)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh' HARNESS_MAX_FIX=3
cd "$R" || exit 1
install_tests
"$DEVH" init >/dev/null
printf '# Задача\nRED: test -f done.txt\nСоздай done.txt с маркером.\n' | "$DEVH" new "чужой раннер 2" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "RED-LINK: после пере-авторинга run зелёный" || { fail "RED-LINK: run красный"; cat "$T/out"; }
grep -q "не совпадает с раннером VERIFY" "$T/out" && pass "RED-LINK: харнесс сообщил о несовпадении раннера" || fail "RED-LINK: нет сообщения о несовпадении"
grep -qx 'bash scripts/test.sh' .harness/items/W-001/red.cmd && pass "RED-LINK: red.cmd переписан раннером VERIFY" || fail "RED-LINK: red.cmd не переписан"
grep -q "КОРРЕКЦИЯ" .harness/items/W-001/red-prompt.txt && pass "RED-LINK: корректирующий промпт ушёл агенту" || fail "RED-LINK: нет коррекции в промпте"

echo "== 30. RED-MUT: контроль red-теста мутациями (RED_MUTATE) =="
R="$(fresh r36)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh'
cd "$R" || exit 1; install_tests; "$DEVH" init >/dev/null
printf 'спека: опубликуй done.txt с маркером\n' | "$DEVH" new "без red-mut" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && pass "RED-MUT выключен: run зелёный" || { fail "RED-MUT выключен: run красный"; cat "$T/out"; }
[ -f .harness/logs/W-001-redmut.log ] && fail "RED-MUT выключен: redmut-лог не должен появиться" || pass "RED-MUT выключен: redmut-лог не создан"

R="$(fresh r37)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh' HARNESS_RED_MUTATE="$T/redmut.sh '{test}'"
cd "$R" || exit 1; install_tests; "$DEVH" init >/dev/null
printf 'спека: опубликуй done.txt с маркером\n' | "$DEVH" new "red-mut ok" >/dev/null
"$DEVH" run >"$T/out" 2>&1 && grep -q "OK red-mutation" "$T/out" && grep -q "red-mutation: Mutants 10, killed 10" .harness/logs/W-001-redmut.log \
  && pass "RED-MUT: сильный red убивает мутантов" || { fail "RED-MUT: сильный red"; cat "$T/out"; }

R="$(fresh r38)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh' HARNESS_RED_MUTATE="$T/redmut.sh '{test}'"
cd "$R" || exit 1
install_tests
cat > t/second_test.sh <<'EOF'
#!/usr/bin/env bash
[ -s done2.txt ] || { echo "done2.txt отсутствует"; exit 1; }
grep -qx marker done2.txt || { echo "маркер не опубликован"; exit 1; }
EOF
git add -A >/dev/null 2>&1 && git commit -qm "второй тест"
"$DEVH" init >/dev/null
printf '# Задача\nRED: bash scripts/test.sh second\nСоздай done2.txt с маркером.\n' | "$DEVH" new "red-mut слабый" >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then
  fail "RED-MUT: слабый red должен быть красным"
else
  grep -q "RED-MUT" "$T/out" && pass "RED-MUT: слабый red отклонён" || { fail "RED-MUT: нет сообщения"; cat "$T/out"; }
  [ "$(cat .harness/items/W-001/status)" != "done" ] && pass "RED-MUT: статус не done" || fail "RED-MUT: статус done"
fi

R="$(fresh r39)"; base_env; export HARNESS_AGENT="$T/stub-red.sh" HARNESS_VERIFY='bash scripts/test.sh' HARNESS_RED_MUTATE="$T/redmut.sh"
cd "$R" || exit 1; install_tests; "$DEVH" init >/dev/null
printf 'спека: опубликуй done.txt с маркером\n' | "$DEVH" new "red-mut без {test}" >/dev/null
if "$DEVH" run >"$T/out" 2>&1; then
  fail "RED-MUT: отсутствие {test} должно быть красным"
else
  grep -q "нет шаблона {test}" "$T/out" && pass "RED-MUT: без {test} — отказ с подсказкой" || { fail "RED-MUT: нет подсказки про {test}"; cat "$T/out"; }
fi


echo "== 31. smoke: happy path через стаб-агента (микрорепо, ноль LLM, <60s) =="
SMOKE_T0=$SECONDS
smoke_reset_env
S="$(bash "$HERE/tests/mk_smoke_repo.sh" "$T/s31" "$HERE/stubs/good.sh" "$T/wt")"
[ -d "$S" ] && pass "smoke: микрорепо сгенерировано" || { fail "smoke: mk_smoke_repo (пусто: '$S')"; exit 1; }
cd "$S" || exit 1
printf 'Сделай: валидация нечисловых аргументов add.\nOBSERVE:\n- return: add x 3 -> ERR, rc=1\nRED: bash tests/all.sh\nRED-EXPECT: add_non_numeric\n' \
  | "$DEVH" new "smoke fix" >/dev/null 2>&1
grep -q "add_non_numeric" .harness/items/W-001/red-expect && pass "smoke: RED-EXPECT из спеки" || fail "smoke: RED-EXPECT из спеки"
grep -q "add x 3" .harness/items/W-001/observe.md && pass "smoke: OBSERVE-точки из спеки" || fail "smoke: OBSERVE-точки из спеки"
"$DEVH" run W-001 >"$T/out" 2>&1 && pass "smoke: run зелёный" || { fail "smoke: run зелёный"; cat "$T/out"; }
grep -q "| claimed" .harness/items/W-001/events.log && pass "smoke: claim в журнале (A3)" || fail "smoke: claim в журнале (A3)"
grep -q "calc::add_non_numeric ... FAILED" .harness/logs/W-001-red.log && pass "smoke: RED честный (упавший тест в логе)" || fail "smoke: RED честный"
grep -q "verify-r1" .harness/logs/W-001-timing.log && pass "smoke: timing verify" || fail "smoke: timing verify"
grep -q "OBSERVE-точка в промпте" .harness/logs/W-001-agent.log && pass "smoke: OBSERVE инжектится в промпт" || fail "smoke: OBSERVE инжектится"
[ "$(cat .harness/items/W-001/status)" = "active" ] && pass "smoke: статус active после run" || fail "smoke: статус active"
"$DEVH" merge W-001 >"$T/out" 2>&1 && pass "smoke: merge" || { fail "smoke: merge"; cat "$T/out"; }
grep -q "| merged" .harness/items/W-001/events.log && pass "smoke: merged в журнале" || fail "smoke: merged в журнале"
[ "$(cat .harness/items/W-001/status)" = "done" ] && pass "smoke: статус done" || fail "smoke: статус done"
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] && pass "smoke: main продвинут" || fail "smoke: main продвинут"
grep -q "add_non_numeric" "$T/out" 2>/dev/null && fail "smoke: merge не мясо" || pass "smoke: merge без RED-муса"
"$DEVH" doctor >"$T/out" 2>&1 && pass "smoke: doctor чист" || { fail "smoke: doctor чист"; cat "$T/out"; }
SMOKE_DT=$((SECONDS - SMOKE_T0))
echo "  smoke-цикл: ${SMOKE_DT}s"
[ "$SMOKE_DT" -lt 60 ] && pass "smoke: цикл < 60s" || fail "smoke: цикл < 60s (${SMOKE_DT}s)"

# ─── A5a: корпус атакующий (adversarial corpus) ───
# Идея: стабы-читеры говорят на РЕАЛЬНЫХ диалектах гейтов (как good.sh), но
# ломают РОВНО ОДНО допущение контракта. Судьи настоящие — проверяем, что
# гейт ловит именно этот обман, а не «случайно упал». Метрика kill-rate для
# отчёта: ADV_OK/ADV_N (поймано / запусков). Детерминированность: каждую
# атаку гоняем 3 раза на свежих микрорепо.

# adv_run <case> <stub> <sub> <timeout|-> <red:0|1> <expect:0|1>
# Микрорепо + один прогон devh run. Глобалы после вызова:
#   ADV_RC — код выхода прогона, ADV_DT — длительность (сек),
#   ADV_OUT — путь к логу прогона, ADV_DIR — путь к микрорепо.
adv_run() {
  local case=$1 stub=$2 sub=$3 tmo=$4 with_red=$5 with_expect=$6
  smoke_reset_env
  local S
  S="$(bash "$HERE/tests/mk_smoke_repo.sh" "$T/$case-$sub" "$HERE/stubs/$stub" "$T/$case-$sub.wt")"
  ADV_DIR="$S"
  if [ ! -d "$S" ]; then ADV_RC=99; ADV_OUT=""; return; fi
  cd "$S" || { ADV_RC=99; ADV_OUT=""; return; }
  local spec='Сделай: валидацию нечисловых аргументов add.
OBSERVE:
- return: add x 3 -> ERR, rc=1'
  # RED: в спеке = red.cmd пишет new (фаза автора red пропускается);
  # без RED: — red.cmd пишет агент в «ФАЗЕ RED» (атаки 32/33).
  # RED-EXPECT: = подпись причины провала (ключ RED_EXPECT/P7) — в спеке,
  # независимо от RED: (атака 33: зелёный тест удовлетворяет подпись).
  if [ "$with_red" = 1 ]; then spec="$spec
RED: bash tests/all.sh"; fi
  if [ "$with_expect" = 1 ]; then spec="$spec
RED-EXPECT: add_non_numeric"; fi
  printf '%s\n' "$spec" | "$DEVH" new "$case $sub" >/dev/null 2>&1
  local t0=$SECONDS
  if [ "$tmo" = "-" ]; then
    "$DEVH" run W-001 >"$T/adv-$case-$sub.log" 2>&1
  else
    HARNESS_AGENT_TIMEOUT="$tmo" "$DEVH" run W-001 >"$T/adv-$case-$sub.log" 2>&1
  fi
  ADV_RC=$?
  ADV_DT=$((SECONDS - t0))
  ADV_OUT="$T/adv-$case-$sub.log"
}

# adv_expect <case> <stub> <marker> <timeout|-> <red:0|1> <expect:0|1> [extra-grep] [max-dt]
# Три последовательных свежих прогона: rc=1 (не env-отговорка), маркер гейта,
# контрольная строка (extra), инварианты «обман не дошёл до main».
adv_expect() {
  local case=$1 stub=$2 marker=$3 tmo=$4 with_red=$5 with_expect=$6
  local extra=${7:-} maxdt=${8:-}
  local i
  for i in 1 2 3; do
    adv_run "$case" "$stub" "$i" "$tmo" "$with_red" "$with_expect"
    ADV_N=$((ADV_N+1))
    if [ "$ADV_RC" = 1 ] && [ -f "$ADV_OUT" ] && grep -q "$marker" "$ADV_OUT"; then
      ADV_OK=$((ADV_OK+1))
    fi
    if [ "$ADV_RC" = 1 ]; then pass "adv ${case}[$i]: обман пойман (rc=1)"; else
      fail "adv ${case}[$i]: rc=$ADV_RC, ожидался 1"
      [ -f "$ADV_OUT" ] && sed -n '1,25p' "$ADV_OUT"
    fi
    if [ -f "$ADV_OUT" ] && grep -q "$marker" "$ADV_OUT"; then
      pass "adv ${case}[$i]: маркер «$marker»"
    else fail "adv ${case}[$i]: нет маркера «$marker»"; fi
    if [ -n "$extra" ]; then
      if [ -f "$ADV_OUT" ] && grep -q "$extra" "$ADV_OUT"; then
        pass "adv ${case}[$i]: контроль «$extra»"
      else fail "adv ${case}[$i]: нет контроля «$extra»"; fi
    fi
    if [ -n "$maxdt" ]; then
      if [ "$ADV_DT" -lt "$maxdt" ]; then pass "adv ${case}[$i]: watchdog за ${ADV_DT}s (<$maxdt)"
      else fail "adv ${case}[$i]: dt=${ADV_DT}s, ожидался <$maxdt"; fi
    fi
    cd "$ADV_DIR" || exit 1
    [ "$(cat .harness/items/W-001/status)" != "done" ] && pass "adv ${case}[$i]: статус не done" || fail "adv ${case}[$i]: статус done"
    if grep -q "| merged" .harness/items/W-001/events.log 2>/dev/null; then
      fail "adv ${case}[$i]: merged в журнале"
    else pass "adv ${case}[$i]: без merged"; fi
    if [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ]; then
      pass "adv ${case}[$i]: main не сдвинут"
    else fail "adv ${case}[$i]: main сдвинут (обман ушёл в origin)"; fi
  done
}

echo "== 32. АТАКА red-green: red.cmd уже зелёный на чистом HEAD =="
adv_expect red-green red-green.sh "не баг" - 0 0

echo "== 33. АТАКА red-wrong: red падает НЕ ПО ТОЙ ПРИЧИНЕ =="
adv_expect red-wrong red-wrong.sh "НЕ ПО ТОЙ ПРИЧИНЕ" - 0 1 "calc::add_two"

echo "== 34. АТАКА no-summary: работа сделана, summary не написан =="
adv_expect no-summary no-summary.sh "не написал summary" - 1 1 "OK гейты"

echo "== 35. АТАКА bad-summary: summary с 'Session N' vs MSG_DENY =="
adv_expect bad-summary bad-summary.sh "гигиену" - 1 1 "OK гейты"

echo "== 36. АТАКА hang: висящий агент vs watchdog =="
adv_expect hang hang.sh "FAIL агент" 5 1 1 "watchdog" 30

echo "  adversarial corpus: поймано $ADV_OK/$ADV_N (kill-rate $(( ADV_N > 0 ? ADV_OK * 100 / ADV_N : 0 ))%)"
[ "$ADV_OK" = "$ADV_N" ] && pass "adv: все атаки пойманы ($ADV_OK/$ADV_N)" || fail "adv: корпус пойман не полностью ($ADV_OK/$ADV_N)"

echo
echo "ИТОГО: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
