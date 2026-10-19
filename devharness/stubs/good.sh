#!/usr/bin/env bash
# Стаб-агент «хороший»: промпт в $1; env HARNESS_WORKTREE/HARNESS_SUMMARY
# (agent_env в run.rs). Детерминированный happy-path для smoke-сессий:
# чинит HEAD-дефект calc.sh (валидация нечисловых аргументов), пишет summary,
# на REVIEW-промпт (единственное слово-маркер «VERDICT» — только в
# prompt::review_prompt) отвечает вердиктом OK.
set -uo pipefail
wt=${HARNESS_WORKTREE:?}
case "$1" in
  *VERDICT*) echo "VERDICT: OK"; exit 0;;
esac
# Proof инжекции OBSERVE в промпт (agent_prompt → observe.md): если точки нет —
# агент отказывается работать (rc=3), smoke-сессия зелёной быть не может.
case "$1" in
  *"add x 3"*) echo "stub: OBSERVE-точка в промпте — работаю";;
  *) echo "stub: в промпте нет OBSERVE-точки — отказ" >&2; exit 3;;
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
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
exit 0
