#!/usr/bin/env bash
# A5-4 (bad-summary): summary пишет, но первая строка «Session 9: …» —
# MSG_DENY='Session [0-9]+' обязан отказать коммиту (гигиена, rc=1).
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in *VERDICT*) echo "VERDICT: OK"; exit 0;; esac
cat > "$wt/src/calc.sh" <<'CALC'
#!/usr/bin/env bash
add() {
  case "$1$2" in
    '' | *[!0-9]*) echo "ERR: non-numeric" >&2; return 1;;
  esac
  printf '%s\n' "$(($1 + $2))"
}
cmd=$1; shift
if [ "$cmd" = add ]; then add "$@"; fi
CALC
printf 'Session 9: hotfix calc validation\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
exit 0
