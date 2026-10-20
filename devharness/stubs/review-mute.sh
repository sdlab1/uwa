#!/usr/bin/env bash
# Стаб-агент АТАКА 43 (A5b): работу делает честно, а на ревью-промпт молчит
# (rc=0 без строки VERDICT:) → ревьюер не отдал вердикт = блокер СРЕДЫ
# (промпт/модель/транспорт), не красный гейт агента: rc=2, маркер
# 'FAIL: review env-блокер' + 'вердикт не распознан'.
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in
  *"Ниже дифф"*) exit 0;;
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
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб-атака review-mute\n' > "$HARNESS_SUMMARY"
exit 0
