#!/usr/bin/env bash
# dev/check_bench.sh — машинная проверка criterion против commit-pinned baseline.json.
# Читает target/criterion/<bench>/new/estimates.json (mean point_estimate, нс),
# падает при превышении baseline × tolerance. Человекочитаемых выводов criterion
# ("time: [2.6 ms ...]") для гейта недостаточно — нужен именно estimates.json.
# Использование: bash dev/check_bench.sh  (cwd — корень репо; bench уже прогнан)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASE="$ROOT/gobot/native/gobot_compute/baseline.json"
CRIT="$ROOT/gobot/native/gobot_compute/target/criterion"
command -v jq >/dev/null 2>&1 || { echo "[bench-gate] ❌ нет jq"; exit 1; }
[ -f "$BASE" ] || { echo "[bench-gate] ❌ нет $BASE"; exit 1; }

tol=$(jq -r .tolerance "$BASE")
fail=0
for bench in $(jq -r '.benchmarks | keys[]' "$BASE"); do
  # criterion санитизирует '/' → '_' в именах директорий.
  dir="${bench//\//_}"
  est="$CRIT/$dir/new/estimates.json"
  [ -f "$est" ] || { echo "[bench-gate] ❌ нет estimates: $bench (bench прогнан?)"; fail=1; continue; }
  base_ms=$(jq -r --arg b "$bench" '.benchmarks[$b].ms' "$BASE")
  mean_ms=$(jq -r '.mean.point_estimate / 1000000' "$est")
  limit=$(awk -v b="$base_ms" -v t="$tol" 'BEGIN { printf "%.4f", b * t }')
  over=$(awk -v m="$mean_ms" -v l="$limit" 'BEGIN { print (m > l) ? 1 : 0 }')
  if [ "$over" = "1" ]; then
    echo "[bench-gate] ❌ $bench: ${mean_ms}мс > baseline ${base_ms}мс × ${tol} = ${limit}мс"
    fail=1
  else
    echo "[bench-gate] ✅ $bench: ${mean_ms}мс ≤ ${limit}мс (baseline ${base_ms}мс × ${tol})"
  fi
done
[ "$fail" = "0" ] || { echo "[bench-gate] регресс против baseline.json"; exit 1; }
echo "[bench-gate] гейт пройден: всё в допуске ×${tol} от pinned"
