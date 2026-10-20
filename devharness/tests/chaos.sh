#!/usr/bin/env bash
# chaos.sh <iterations> [seed] — A7: kill -9 прогона в случайной точке.
# RED-инструмент харнесса: микрорепо (mk_smoke_repo.sh, ноль LLM, фазы
# растянуты — SMOKE_SLOW: агент спит 2с, мутатор 10с), devh run в фоне,
# kill -9 через sleep in [0..7]s — детерминированно от seed. После убийства:
# doctor обязан выжить (rc=0), инварианты (ниже), восстановление
# (abort + run до зелёного). Лог итерации: i | seed | sleep | стадия |
# rc | исход — воспроизводимо. Выход 1 = нарушен инвариант (находка:
# сид в rust_build.md + строка в RULES).
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DEVH=${DEVH:-$HERE/../devh}
ITERS=${1:?usage: chaos.sh <iterations> [seed]}
SEED=${2:-$RANDOM}
T=$(mktemp -d /tmp/devh-chaos.XXXXXX)
trap 'rm -rf "$T"' EXIT
MARKER="changed by cargo-mutants"
STUB="$HERE/../stubs/chaos-slow.sh"

# check_invariants <repo> <tag>: печатает CHAOS-INV строки, rc = их число
# (0 = чисто). Инварианты плана A7 — каждый ест реальный механизм:
# 1) status парсится (open/active/done) — atomic_write без torn-файлов;
# 2) fold(events.log) == status — A3-replay под расстрелом;
# 3) нет .harness/**/*.tmp — осиротевшие атомарные записи;
# 4) в worktree нет мутационных маркеров — урок ae9f619;
# 5) run.pid отсутствует или процесс жив; stale = НАХОДКА (лог, не fail):
#    это зона doctor, а не падение ассерта (план A7 явно).
check_invariants() {
  local S=$1 tag=$2 bad=0 st folded pid tmp
  st=$(tr -d '[:space:]' <"$S/.harness/items/W-001/status" 2>/dev/null || true)
  case "$st" in
    open | active | done) ;;
    *) echo "CHAOS-INV[$tag] 1: status не парсится ('$st') — torn-файл?"; bad=$((bad + 1)) ;;
  esac
  # fold(events.log): open --claimed--> active --merged--> done;
  # active --aborted--> open; мусорные строки пропускаем (как state::fold).
  folded=$(awk -F'|' '
    { ev=$NF; gsub(/^[ \t]+|[ \t]+$/, "", ev) }
    ev == "claimed" && s == 0 { s = 1; next }
    ev == "merged"  && s == 1 { s = 2; next }
    ev == "aborted" && s == 1 { s = 0; next }
    END { print (s == 2 ? "done" : s == 1 ? "active" : "open") }
  ' "$S/.harness/items/W-001/events.log" 2>/dev/null || echo open)
  if [ "$folded" != "$st" ]; then
    echo "CHAOS-INV[$tag] 2: events.log replay -> $folded, статус-файл $st — рассинхрон"
    bad=$((bad + 1))
  fi
  tmp=$(find "$S/.harness" -name '*.tmp' 2>/dev/null | head -3)
  if [ -n "$tmp" ]; then
    echo "CHAOS-INV[$tag] 3: осиротевшие атомарные записи: $(echo "$tmp" | tr '\n' ' ')"
    bad=$((bad + 1))
  fi
  if grep -rl "$MARKER" "$S.wt" --exclude-dir=.git >/dev/null 2>&1; then
    echo "CHAOS-INV[$tag] 4: мутационный маркер в worktree ($MARKER) — урок ae9f619"
    bad=$((bad + 1))
  fi
  pid=$(awk '{gsub(/[ \t]/, ""); print}' "$S/.harness/items/W-001/run.pid" 2>/dev/null || true)
  if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
    echo "CHAOS-PID[$tag]: stale run.pid $pid — находка doctor, не падение ассерта"
  fi
  return "$bad"
}

FAIL=0
for i in $(seq 1 "$ITERS"); do
  tag="$SEED/$i"
  S="$T/chaos$i"
  S=$(SMOKE_SLOW=1 bash "$HERE/mk_smoke_repo.sh" "$S" "$STUB" "$T/wt$i") || {
    echo "CHAOS-INV[$tag] 0: mk_smoke_repo упал"; FAIL=$((FAIL + 1)); continue
  }
  (cd "$S" && printf 'Сделай: валидацию нечисловых аргументов add.\nOBSERVE:\n- return: add x 3 -> ERR, rc=1\nRED: bash tests/all.sh\n' | "$DEVH" new "chaos $tag" >/dev/null 2>&1) || {
    echo "CHAOS-INV[$tag] 0: devh new упал"; FAIL=$((FAIL + 1)); continue
  }
  SLP=$(((SEED + i * 7) % 8)) # 0..7s — размазано по стадиям (детерминированно)
  (cd "$S" && exec "$DEVH" run W-001 >"$T/run-$i.log" 2>&1) &
  RUNPID=$!
  sleep "$SLP"
  if kill -9 "$RUNPID" 2>/dev/null; then
    wait "$RUNPID" 2>/dev/null; RC=$?; KILLED=yes
  else
    wait "$RUNPID" 2>/dev/null; RC=$?; KILLED=no # прогон успел завершиться сам
  fi
  STAGE=$(awk 'NF {last=$1} END {print (last ? last : "нет")}' "$S/.harness/logs/W-001-timing.log" 2>/dev/null || echo нет)
  # сироты добегают: агент убитого прогона живёт ещё ~2с (спит перед фиксом)
  # и пишет в worktree — ждём его смерти (до 8с), иначе abort/reset гонится
  # с его записью. Детерминированный хаос = смерть родителя, хвост — доезжает.
  for _ in $(seq 1 16); do
    pgrep -f "$STUB" >/dev/null 2>&1 || break
    sleep 0.5
  done
  (cd "$S" && "$DEVH" doctor >"$T/doctor-$i.log" 2>&1)
  DOC=$?
  [ "$DOC" = 0 ] || { echo "CHAOS-INV[$tag] d: doctor rc=$DOC после kill -9"; FAIL=$((FAIL + 1)); }
  (cd "$S" && "$DEVH" abort W-001 >/dev/null 2>&1)
  (cd "$S" && "$DEVH" run W-001 >"$T/resume-$i.log" 2>&1)
  REC=$?
  [ "$REC" = 0 ] || { echo "CHAOS-INV[$tag] r: recovery (abort + run) rc=$REC"; FAIL=$((FAIL + 1)); }
  BAD=0
  check_invariants "$S" "$tag" || BAD=$?
  [ "$BAD" = 0 ] || FAIL=$((FAIL + BAD))
  echo "chaos[$tag]: sleep=${SLP}s killed=$KILLED rc=$RC doctor=$DOC recovery=$REC stage=$STAGE inv=$BAD"
done
echo "chaos: $ITERS итераций, seed=$SEED, нарушений=$FAIL"
[ "$FAIL" = 0 ] || exit 1
exit 0
