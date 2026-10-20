#!/usr/bin/env bash
# A8: агент флейк-приёмки = good.sh без OBSERVE-guard (инъекция точек
# доказывается секцией 39) + выживание в fix-раундах: фикс идемпотентный,
# поэтому повторное применение после флейко-Verify не убивает сессию.
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in
  *VERDICT*) echo "VERDICT: OK"; exit 0;;
esac
cat > "$wt/src/calc.sh" <<'EOF'
#!/usr/bin/env bash
add() {
  case "$1$2" in
    '' | *[!0-9]*) echo "ERR: non-numeric" >&2; return 1;;
  esac
  printf '%s\n' "$(($1 + $2))"
}
cmd=$1; shift
if [ "$cmd" = add ]; then add "$@"; fi
EOF
printf 'fix: валидацию нечисловых аргументов в add\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
exit 0
