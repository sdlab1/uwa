#!/bin/bash
# Разовый замер загрузки CPU в процентах: busy = 100*(1 - dIdle/dTotal).
#
#   scripts/cpu-busy.sh          # один замер (1 с)
#   scripts/cpu-busy.sh 5        # среднее за 5 замеров
#
# Не путать с loadavg: на 2 ядрах/4 потоках loadavg 1.9 бывает при полной
# загрузке (load считает и I/O-ожидание). Для гейта мутаций нужен именно
# процент занятого CPU — см. docs/rust_build.md §9.28.
set -u
N="${1:-1}"
total=0
for _ in $(seq 1 "$N"); do
  read -r _ u1 n1 s1 i1 _ < /proc/stat
  sleep 1
  read -r _ u2 n2 s2 i2 _ < /proc/stat
  p=$(awk -v t1="$((u1 + n1 + s1 + i1))" -v i1="$i1" \
        -v t2="$((u2 + n2 + s2 + i2))" -v i2="$i2" \
        'BEGIN { if (t2 <= t1) { print 0; exit } printf "%.1f", (t2 - t1 - (i2 - i1)) * 100 / (t2 - t1) }')
  total=$(awk -v a="$total" -v b="$p" 'BEGIN { printf "%.1f", a + b }')
done
awk -v t="$total" -v n="$N" 'BEGIN { printf "%.1f%%\n", t / n }'
