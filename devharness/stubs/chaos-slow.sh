#!/usr/bin/env bash
# Стаб-агент A7 (chaos): как good.sh (честный фикс HEAD-дефекта + guard
# OBSERVE-инжекции), но спит 2с до работы — kill -9 в случайной точке
# попадает и в фазу агента (сирота добегает и пишет fix — это тоже хаос).
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in
  *VERDICT*) echo "VERDICT: OK"; exit 0;;
esac
case "$1" in
  *"add x 3"*) echo "stub: OBSERVE-точка в промпте — работаю (сон 2s перед фиксом)";;
  *) echo "stub: в промпте нет OBSERVE-точки — отказ" >&2; exit 3;;
esac
sleep 2
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
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: chaos-стаб (сон 2s)\n' > "$HARNESS_SUMMARY"
exit 0
