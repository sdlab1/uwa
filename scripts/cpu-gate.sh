#!/bin/bash
# CPU-гейт перед КАЖДЫМ мутантом (правило 2026-10-22, замеры — docs/rust_build.md §9.28).
#
# Использование (как rustc-wrapper; sccache-цепочка сохраняется):
#   CARGO_BUILD_RUSTC_WRAPPER=scripts/cpu-gate.sh cargo mutants --in-place ...
#
# Что делает:
#   1. меряет загрузку CPU (два сэмпла /proc/stat за 1 с);
#   2. если загрузка > 90% — ЖДЁТ 15 с и меряет заново (до 20 раз);
#   3. когда машина свободна — запускает настоящий rustc (через sccache, если есть).
#
# Решение кешируется на 60 с (/tmp/cpu-gate.stamp), чтобы ожидание не влезало
# в середину фазы сборки и не портило измерение build-времени мутанта.
#
# ВАЖНО: замерять надо busy = 100*(1 - dIdle/dTotal), а НЕ loadavg:
# loadavg считает и I/O-ожидание, и на этой машине (2 ядра/4 потока) он
# показывает 1.9 при полной загрузке. Ошибка разбора /proc/stat (idle вместо
# system) даёт «busy» ≈93 % всегда — см. §9.28, п.1 «ГРАБЛЯ».
set -u

STAMP="${CPU_GATE_STAMP:-/tmp/cpu-gate.stamp}"
LOG="${CPU_GATE_LOG:-/tmp/cpu-gate.log}"
THRESHOLD="${CPU_GATE_THRESHOLD:-90}"
WAIT_SECS="${CPU_GATE_WAIT:-15}"
MAX_WAITS="${CPU_GATE_MAX_WAITS:-20}"
TTL="${CPU_GATE_TTL:-60}"

busy() {
  read -r _ u1 n1 s1 i1 _ < /proc/stat
  sleep 1
  read -r _ u2 n2 s2 i2 _ < /proc/stat
  awk -v t1="$((u1 + n1 + s1 + i1))" -v i1="$i1" \
      -v t2="$((u2 + n2 + s2 + i2))" -v i2="$i2" \
      'BEGIN { if (t2 <= t1) { print "0.0"; exit } printf "%.1f", (t2 - t1 - (i2 - i1)) * 100 / (t2 - t1) }'
}

real_rustc() {
  if [ -x /home/user/.cargo/bin/sccache ]; then
    exec /home/user/.cargo/bin/sccache "$@"
  fi
  exec "$@"
}

now=$(date +%s)
if [ -f "$STAMP" ] && [ $((now - $(cat "$STAMP"))) -lt "$TTL" ]; then
  real_rustc "$@"
fi

waits=0
waited=0
while :; do
  p=$(busy)
  if awk -v x="$p" -v t="$THRESHOLD" 'BEGIN { exit !(x > t) }'; then
    if [ "$waits" -ge "$MAX_WAITS" ]; then
      echo "$(date +%FT%T) GATE give-up waited=${waited}s waits=$waits cpu=${p}%" >>"$LOG"
      break
    fi
    waits=$((waits + 1))
    waited=$((waited + WAIT_SECS))
    echo "$(date +%FT%T) GATE wait#$waits cpu=${p}%" >>"$LOG"
    sleep "$WAIT_SECS"
  else
    echo "$(date +%FT%T) GATE pass waited=${waited}s waits=$waits cpu=${p}%" >>"$LOG"
    break
  fi
done
date +%s >"$STAMP"
real_rustc "$@"
