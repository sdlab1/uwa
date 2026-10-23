#!/usr/bin/env bash
# Приёмка R-3/R-4/R-7/R-8 (канал devh msg): дедуп доставки, флуд уведомлений,
# дамп бэклога в watch, legacy-боксы, «watch крадёт чужие письма».
#
# Запуск: bash .harness/plan/flood_acceptance.sh [путь-к-devh]
# Изолированный DEVH_MSG_HOME => живая почта не страдает.
# Ожидаемый результат после фиксов: все строки PASS, exit 0.
set -u
DEV="${1:-/home/user/project_uwa/target/debug/devh}"
T="$(mktemp -d /tmp/flood-XXXXXX)"
export DEVH_MSG_HOME="$T"
export DEVH_AGENT=beam
export DEVH_MSG_NO_AUTO=1
FAIL=0
ok() { echo "PASS: $1"; }
bad() { echo "FAIL: $1"; FAIL=1; }
cnt() { grep -c '^ID:' "$1" 2>/dev/null || true; }

echo "== дом: $T =="

# --- алиас синонима -> канона -------------------------------------------
"$DEV" msg alias deepseek --for beam >/dev/null 2>&1

# --- A: 20 писем в синоним => ровно 20 писем в каноническом ящике --------
for i in $(seq 1 20); do
  "$DEV" msg send --to deepseek --subject "s$i" --body "b$i" >/dev/null 2>&1
done
N="$(cnt "$T/inbox/beam.log")"
U="$(grep '^ID:' "$T/inbox/beam.log" 2>/dev/null | sort -u | wc -l)"
[ "$N" = "20" ] && ok "A1 в inbox/beam.log ровно 20 писем" || bad "A1 в inbox/beam.log $N писем (ожидалось 20)"
[ "$U" = "20" ] && ok "A2 дублей доставки нет" || bad "A2 уникальных ID $U из 20 (R-3)"

# --- B: одно уведомление на пачку, без повторов ------------------------
unset DEVH_MSG_NO_AUTO
"$DEV" msg list >/dev/null 2>"$T/n1.err"
N1="$(wc -l < "$T/notified/beam.log" 2>/dev/null || echo 0)"
U1="$(sort -u "$T/notified/beam.log" 2>/dev/null | grep -c . || echo 0)"
[ "$N1" = "$U1" ] && ok "B1 notified без повторов ($N1 строк)" || bad "B1 notified $N1 строк / $U1 уникальных (R-4)"
"$DEV" msg list >/dev/null 2>>"$T/n1.err"
N2="$(wc -l < "$T/notified/beam.log" 2>/dev/null || echo 0)"
[ "$N2" = "$N1" ] && ok "B2 повторный вызов не добавил строк" || bad "B2 повторный вызов добавил $((N2-N1)) строк (R-4)"

# --- C: watch ЧУЖОГО агента: нет дампа бэклога и письма не съедены -----
"$DEV" msg send --to target --subject "чужая" --body "x" >/dev/null 2>&1
timeout 5 "$DEV" msg watch --agent target --consume >"$T/w.out" 2>&1
W="$(cnt "$T/w.out")"
[ "$W" = "0" ] && ok "C1 watch не вывалил бэклог на старте" || bad "C1 watch вывалил $W писем на старте (R-7)"
NT="$(cnt "$T/inbox/target.log")"
[ "$NT" = "1" ] && ok "C2 watch не съел чужие письма (--consume)" || bad "C2 после watch --consume в чужом ящике $NT писем (R-8)"

# --- D: --from-start (если флаг есть) ----------------------------------
if timeout 5 "$DEV" msg watch --agent target --from-start >"$T/w2.out" 2>&1; then
  W2="$(cnt "$T/w2.out")"
  [ "$W2" -ge 1 ] && ok "D1 --from-start печатает пачку ($W2)" || bad "D1 --from-start ничего не напечатал"
else
  echo "SKIP: D1 --from-start не поддержан (флаг появится вместе с R-7)"
fi

# --- E: legacy-боксы не учитываются -------------------------------------
cp "$T/inbox/beam.log" "$T/inbox/default.log" 2>/dev/null
cp "$T/inbox/beam.log" "$T/inbox/deepseek.log" 2>/dev/null
P="$("$DEV" msg pending 2>/dev/null | grep -c 'не прочитано' || true)"
[ "$P" -le 20 ] && ok "E1 legacy-боксы не учитываются (pending=$P)" || bad "E1 pending=$P (>20) — legacy-боксы в счёте (R-8)"
UPD="$(grep '^ID:' "$T/inbox/deepseek.log" 2>/dev/null | sort -u | wc -l)"
[ "$UPD" -le 1 ] && ok "E2 синонимный бокс не раздувает письма" || bad "E2 в синонимном боксе $UPD уникальных ID (R-8)"

echo "== итог: $([ $FAIL = 0 ] && echo OK || echo ЕСТЬ ПРОВАЛЫ) =="
echo "== мусор: $T =="
exit $FAIL
