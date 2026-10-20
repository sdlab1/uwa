#!/usr/bin/env bash
# mk_smoke_repo.sh <dir> <stub-path> [wt-root]
# Микрорепо для smoke/adversarial/chaos-сессий харнесса: детерминировано,
# ноль LLM. Гейты прогоняют НАСТОЯЩИЕ судьи — стабы инструментов говорят на
# диалектах, которые парсят судьи:
#   MUTATE: "N mutants tested in T: C caught[, M missed]"   (gates::parse_mutation_output)
#   CRAP:   "CRAP: functions=N" + "file:line fun C=n cov=n" (gates::parse_crap_output)
#   REVIEW: "VERDICT: OK" | "VERDICT: ISSUES: ..."          (gates::parse_review_verdict)
# Тест-раннер говорит на диалекте cargo test с namespaced-id (якорь `::` —
# его требует gates::failing_tests, чтобы выделить УПАВШИЙ тест).
# На чистом HEAD падает add_non_numeric (нечисловой аргумент молча уходит в
# $(( ) — дефект спеки) → честный RED с test-id.
set -euo pipefail
DIR=$1 STUB=$2 WT=${3:-$DIR.wt}
BARE="$DIR.git"
rm -rf "$DIR" "$BARE"
git init -q --bare "$BARE"
git init -q "$DIR"
cd "$DIR"
git branch -M main
git config user.email t@t.local
git config user.name t
git remote add origin "$BARE"
mkdir -p src tests tools .smoke .harness
# скелет .harness как после devh init (агент пишет summary САМ в этот файл)
mkdir -p .harness/items .harness/summaries .harness/logs

cat > src/calc.sh <<'EOF'
#!/usr/bin/env bash
add() {
  # HEAD-дефект (его ловит RED): нечисловой аргумент молча уходит в $(( ).
  printf '%s\n' "$(($1 + $2))"
}
cmd=$1; shift
if [ "$cmd" = add ]; then add "$@"; fi
EOF

cat > tests/all.sh <<'EOF'
#!/usr/bin/env bash
# VERIFY-цель и red.cmd (RED-LINK требует общий раннер). Диалект cargo test.
fail=0
out=$(bash src/calc.sh add 2 3)
if [ "$out" = 5 ]; then echo "test calc::add_two ... ok"; else echo "test calc::add_two ... FAILED"; fail=1; fi
out=$(bash src/calc.sh add x 3 2>&1) || true
case "$out" in *ERR*) echo "test calc::add_non_numeric ... ok";; *) echo "test calc::add_non_numeric ... FAILED"; fail=1;; esac
if [ -f .smoke/ignored ]; then
  echo "test calc::add_extra ... ignored"
  echo "2 passed, 1 ignored"
else
  echo "2 passed"
fi
exit "$fail"
EOF

cat > tools/mutate.sh <<'EOF'
#!/usr/bin/env bash
# Диалект cargo-mutants summary. Режим из .smoke/mutate-mode (ok по умолчанию).
mode=$(cat .smoke/mutate-mode 2>/dev/null || echo ok)
case "$mode" in
  ok)       echo "5 mutants tested in 1s: 5 caught";;
  weak)     echo "5 mutants tested in 1s: 1 missed, 4 caught"; exit 2;;
  slow)     sleep 10; echo "5 mutants tested in 10s: 5 caught";;
  baseline) echo "cargo test failed in an unmutated tree"; exit 4;;
  *)        echo "5 mutants tested in 1s: 5 caught";;
esac
exit 0
EOF

cat > tools/crap.sh <<'EOF'
#!/usr/bin/env bash
# Диалект CRAP: "CRAP: functions=N" + строки "file:line fun C=n cov=n".
mode=$(cat .smoke/crap-mode 2>/dev/null || echo ok)
echo "CRAP: functions=2"
echo "src/calc.sh:3 add C=2 cov=90"
if [ "$mode" = violation ]; then echo "src/calc.sh:8 cmd C=15 cov=20"; fi
EOF

cat > .gitignore <<'EOF'
.harness/
.wt/
EOF

cat > .harness/config <<EOF
VERIFY=bash tests/all.sh
BOOTSTRAP=
AGENT=bash $STUB
AGENT_TIMEOUT=60
MAX_FIX=2
MAX_ACTIVE=2
MUTATE=bash tools/mutate.sh
MUTATE_SCOPE=src
MIN_KILL=90
CRAP=bash tools/crap.sh
CRAP_SCOPE=src
CRAP_MAX=12
LINT=1
REVIEW=1
NO_IGNORED=1
MSG_DENY=Session [0-9]+
WT_ROOT=$WT
LOG_DAYS=7
EOF

git add -A
git commit -qm init
git push -q -u origin main

# A6-нож (env): SMOKE_FREEZE=1 — FREEZE-гейт включён в конфиге микрорепо
# (код-дефолт false; adv_run умеет и выключить обратно через HARNESS_FREEZE=0).
# Обход allow-test-edit сюда НЕ кладётся: next_id = max(items)+1 — каталог
# W-001 до devh new сдвинул бы item на W-002; adv_run пишет обход ПОСЛЕ new.
if [ "${SMOKE_FREEZE:-0}" = 1 ]; then
  printf 'FREEZE=1\n' >> .harness/config
fi

# печатаем путь — вызывающий ловит: S=$(mk_smoke_repo.sh ...)
echo "$DIR"
