#!/usr/bin/env bash
# A5-2 (red-wrong): red.cmd ЧЕСТНЫЙ (тот же раннер), но агент в фазе RED
# сажает в worktree «фикс», ломающий числовой путь → red падает НЕ по той
# причине (add_two вместо add_non_numeric). Ожидание: БЛОКЕР СПЕКИ (rc=1).
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
red="$(dirname "$HARNESS_SPEC")/red.cmd"
case "$1" in *VERDICT*) echo "VERDICT: OK"; exit 0;; esac
if [ "${1#*ФАЗА RED}" != "$1" ]; then
  printf 'bash tests/all.sh\n' > "$red"
  cat > "$wt/src/calc.sh" <<'CALC'
#!/usr/bin/env bash
add() {
  case "$1$2" in
    *[!0-9]*) echo "ERR: non-numeric" >&2; return 1;;
    *) echo "ERR: broken validation" >&2; return 1;;
  esac
}
cmd=$1; shift
if [ "$cmd" = add ]; then add "$@"; fi
CALC
  exit 0
fi
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
exit 0
