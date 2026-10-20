#!/usr/bin/env bash
# Стаб-агент АТАКА 38 (A5b): фиксит calc.sh, но в том же файле оставляет
# грязное место (нецитируемое $1 → shellcheck SC2086) → LINT-гейт обязан
# прогнать НАСТОЯЩИЙ shellcheck по изменённым .sh и упасть. Ожидаем: rc=1,
# маркер LINT, лог lint с 'SC2086'. Функция pretty() не зовётся — VERIFY
# зелёный: доказательство, что гейт ловит стиль, а не падение тестов.
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in *"Ниже дифф"*) echo "VERDICT: OK"; exit 0;; esac
cat > "$wt/src/calc.sh" <<'EOF'
#!/usr/bin/env bash
add() {
  case "$1$2" in
    '' | *[!0-9]*) echo "ERR: non-numeric" >&2; return 1;;
  esac
  printf '%s\n' "$(($1 + $2))"
}
pretty() { echo $1; }
cmd=$1; shift
if [ "$cmd" = add ]; then add "$@"; fi
EOF
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб-атака lint\n' > "$HARNESS_SUMMARY"
exit 0
