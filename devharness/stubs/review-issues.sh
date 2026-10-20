#!/usr/bin/env bash
# Стаб-агент АТАКА 42 (A5b): работу делает, но на ревью-промпт (единственный
# маркер «Ниже дифф» — prompt::review_prompt) каждый раунд отвечает
# «VERDICT: ISSUES: …» и НЕ правит замечания → REVIEW-гейт обязан упасть.
# Ожидаем: rc=1, маркер REVIEW, лог review с 'VERDICT: ISSUES'.
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in
  *"Ниже дифф"*) echo "VERDICT: ISSUES: add не валидирует пустую строку; добавь случай ''"; exit 0;;
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
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб-атака review\n' > "$HARNESS_SUMMARY"
exit 0
