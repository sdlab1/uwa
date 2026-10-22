#!/bin/bash
# Приёмка мутационного гейта: прогон cargo-mutants с профилем мутантов и
# сверка вердиктов (правило §9.28, замеры 2026-10-22).
#
#   scripts/mutants-accept.sh                 # новый профиль (incremental+opt0), scope по умолчанию
#   scripts/mutants-accept.sh --profile old   # старый профиль (baseline для сверки)
#   scripts/mutants-accept.sh --profile both  # оба профиля подряд + сверка вердиктов
#   scripts/mutants-accept.sh --scope "devharness/src/core/escapes.rs"
#
# Профили:
#   new = CARGO_PROFILE_TEST_INCREMENTAL=true CARGO_PROFILE_TEST_OPT_LEVEL=0  (принятый, §9.28)
#   old = без env (как в командах аудита до §9.28) — baseline
#
# CPU-гейт (scripts/cpu-gate.sh): загрузка CPU >90 % → ждём 15 с перед фазой
# каждого мутанта. Отключается GATE=off.
#
# Сверка: вердикты (Caught/Missed/Unviable/Timeout) по ИМЕНИ мутанта
# сравниваются между прогонами; расхождения печатаются и пишутся в
# mutants-accept-report.json. Exit code: 0 — расхождений нет, 1 — есть.
set -u

PROFILE=new
SCOPE=""
EXTRA_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2;;
    --scope) SCOPE="$2"; shift 2;;
    --) shift; EXTRA_ARGS=("$@"); break;;
    *) EXTRA_ARGS+=("$1"); shift;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="$ROOT/scripts/cpu-gate.sh"
[ "${GATE:-on}" = "off" ] && GATE="" || true

case "$PROFILE" in
  new) PROFILE_ENV=(env CARGO_PROFILE_TEST_INCREMENTAL=true CARGO_PROFILE_TEST_OPT_LEVEL=0);;
  old) PROFILE_ENV=(env);;
  *) echo "неизвестный профиль: $PROFILE (new|old|both)" >&2; exit 2;;
esac

run_one() { # $1 = new|old
  local prof="$1"
  local env_args=(CARGO_PROFILE_TEST_INCREMENTAL=true CARGO_PROFILE_TEST_OPT_LEVEL=0)
  [ "$prof" = old ] && env_args=()
  local extra=()
  [ -n "$GATE" ] && extra=(CARGO_BUILD_RUSTC_WRAPPER="$GATE")
  # scope через запятую -> несколько -f (один прогон на набор файлов)
  local scope_args=()
  if [ -n "$SCOPE" ]; then
    local IFS=,
    for f in $SCOPE; do
      [ -n "$f" ] && scope_args+=(-f "$f")
    done
  fi
  echo "=== приёмка: профиль=$prof scope='${SCOPE:-ядро devh}' $(date +%T)"
  local log="mutants-accept-$prof.log"
  (
    cd "$ROOT" || exit 2
    env "${extra[@]}" "${env_args[@]}" \
      cargo-mutants mutants --in-place --test-tool nextest --baseline=skip \
      --timeout-multiplier 2 -p devh --config devharness/cargo-mutants.toml \
      "${scope_args[@]}" "${EXTRA_ARGS[@]}" 2>&1 | tee "$log"
  )
  local rc=${PIPESTATUS[0]}
  echo "=== профиль=$prof завершён rc=$rc $(date +%T); лог: $log"
  [ "$rc" = 0 ] || return "$rc"
  python3 "$ROOT/scripts/mutants-accept-report.py" "mutants.out/outcomes.json" >"mutants-accept-$prof.json"
  return 0
}

case "$PROFILE" in
  new) run_one new || exit $?;;
  old) run_one old || exit $?;;
  both)
    run_one old || exit $?
    run_one new || exit $?
    python3 "$ROOT/scripts/mutants-accept-report.py" mutants-accept-old.json mutants-accept-new.json
    exit $?
    ;;
esac
