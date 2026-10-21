#!/usr/bin/env bash
# tools/mutants-run.sh — ЕДИНСТВЕННЫЙ правильный вход в мутационный аудит.
#
# Зачем: агент тысячи раз запускал `cargo mutants ...` руками, а оператор
# орал «где watcher». Ручной запуск обладает двумя пороками:
#   1) foreground без setsid — tool-таймаут убивает процесс ГРУППОЙ
#      (exit 127), прогон сбивается на 30–70% и итог недостоверный;
#   2) оператор не видит live-строки, только пустой лог.
# Скрипт делает единственное корректное: detached-запуск + watcher.
# НЕПРАВИЛЬНЫЙ путь перестаёт существовать как соблазн: больше нечего
# набирать руками.
#
# Использование:
#   tools/mutants-run.sh                     # ядро, канон (config)
#   tools/mutants-run.sh --diff /tmp/f03.diff # дельта по диффу (без config!)
#   tools/mutants-run.sh --once              # только снимок, без запуска
#   tools/mutants-run.sh --jobs 2            # проброс в cargo-mutants
#   tools/mutants-run.sh -p devh --in-place  # и т.п.
#
# Выход: 0 — прогон завершён, 0 missed; 3 — есть missed (дыра в тестах);
#        2 — нет запуска (для --once); иначе — код cargo-mutants.

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

WATCH="$HERE/mutants-watch.sh"
CONFIG="devharness/cargo-mutants.toml"
ONCE=0
DIFF=""
LOG="${MUTANTS_LOG:-/tmp/mutants-run.log}"
declare -a FWD=()   # проброс прочих аргументов в cargo-mutants

while [ $# -gt 0 ]; do
    case "$1" in
        --once)  ONCE=1; shift ;;
        --diff)  DIFF="$2"; shift 2 ;;
        --log)   LOG="$2"; shift 2 ;;
        -h|--help) awk '/^# ---usage---/{f=1;next} /^# ---cut---/{f=0} f' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)       FWD+=("$1"); shift ;;
    esac
done

# ---usage---
#   tools/mutants-run.sh                      # ядро, канон (config)
#   tools/mutants-run.sh --diff /tmp/f03.diff # дельта по диффу (без config)
#   tools/mutants-run.sh --once               # снимок стадии без запуска
#   tools/mutants-run.sh --jobs 2 -p devh     # проброс в cargo-mutants
#
# ОДНА команда вместо «cargo mutants руками» — набрать неправильно нельзя.
# ---cut---

# ---- уже идёт? тогда только watcher -------------------------------------
if pgrep -x cargo-mutants >/dev/null; then
    echo "(прогон уже идёт — только watcher)" >&2
    LIVE=1 HARNESS_COLOR=always STEP=1 INTERVAL=2 MUTANTS_LOG="$LOG" "$WATCH"
    exit $?
fi

if [ "$ONCE" = "1" ]; then
    LIVE=1 HARNESS_COLOR=always MUTANTS_LOG="$LOG" "$WATCH" --once || exit 2
    exit 0
fi

# ---- сборка аргументов: --in-diff ВСЕГДА без конфига -------------------
declare -a ARGS=( -p devh --in-place --test-tool nextest
                  --baseline=skip --timeout-multiplier 2 )
if [ -n "$DIFF" ]; then
    ARGS+=( --in-diff "$DIFF" )          # дельта: границы конфига сняты
else
    ARGS+=( --config "$CONFIG" )         # канон: только ядро
fi
ARGS+=( "${FWD[@]+"${FWD[@]}"}" )

rm -rf mutants.out
echo "запуск: cargo mutants ${ARGS[*]}" >&2
echo "лог:    $LOG (прогон detached, переживёт tool-таймаут)" >&2

# detached: setsid + собственная сессия => tool-таймаут не убьёт ГРУППОЙ
setsid bash -c "cd '$ROOT' && cargo mutants ${ARGS[*]} </dev/null >'$LOG' 2>&1" &
for _ in $(seq 1 90); do          ждём mutants.out до 90с
    [ -f mutants.out/outcomes.json ] && break
    sleep 1
done

# ---- watcher: единственный источник статуса для оператора -------------
LIVE=1 HARNESS_COLOR=always STEP=1 INTERVAL=2 MUTANTS_LOG="$LOG" "$WATCH" || true

# ---- вердикт -----------------------------------------------------------
python3 -u - mutants.out/outcomes.json <<'PY'
import json, sys
try:
    outs = json.load(open(sys.argv[1])).get("outcomes", [])
except Exception:
    sys.exit(2)
if not outs:
    sys.exit(2)
bad = [o for o in outs if o["summary"] == "MissedMutant"]
print("мутантов: %d | missed: %d" % (len(outs), len(bad)), file=sys.stderr)
sys.exit(3 if bad else 0)
PY
