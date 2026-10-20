#!/usr/bin/env bash
# A5-1 (red-green): в фазе RED пишет red.cmd, который ЗЕЛЁНЫЙ на чистом HEAD.
# Линковка с VERIFY сохранена («bash tests/all.sh; exit 0» — общий раннер),
# поэтому честный RED-pre обязан поймать: «уже зелёный — это не баг».
set -uo pipefail
red="$(dirname "$HARNESS_SPEC")/red.cmd"
case "$1" in *VERDICT*) echo "VERDICT: OK"; exit 0;; esac
if [ "${1#*ФАЗА RED}" != "$1" ]; then
  printf 'bash tests/all.sh; exit 0\n' > "$red"
  exit 0
fi
printf 'fix: валидация нечисловых аргументов в add\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
exit 0
