#!/usr/bin/env bash
# colors.sh — общая приглушённая цветовая схема для *dump.sh и backup.sh.
# Подключается через: source "$SCRIPT_DIR/colors.sh"
# Отключается автоматически, если вывод не в терминал.
# Совместимость: dump.sh/devdump.sh используют C_HDR, backup.sh — C_HEADER
# и C_OK/C_WARN/C_ERR/C_ACCENT. Здесь определён суперсет обеих схем.

if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'
  C_DIM=$'\033[2m'
  C_TITLE=$'\033[38;5;109m'
  C_HDR=$'\033[38;5;245m'
  C_HEADER="$C_HDR"
  C_EXT=$'\033[38;5;179m'
  C_NUM=$'\033[38;5;108m'
  C_TOT=$'\033[38;5;146m'
  C_OK=$'\033[32m'
  C_WARN=$'\033[33m'
  C_ERR=$'\033[31m'
  C_ACCENT=$'\033[34m'
else
  C_RESET='' C_DIM='' C_TITLE='' C_HDR='' C_HEADER='' C_EXT='' C_NUM='' C_TOT=''
  C_OK='' C_WARN='' C_ERR='' C_ACCENT=''
fi
