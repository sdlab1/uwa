#!/usr/bin/env bash
# dump.sh — создаёт Markdown‑дамп source‑кода проекта (только Rust).
# Режимы:
#   ./dump.sh            → полный дамп всего source (full_dump.md)
#   ./dump.sh <crate>    → дамп только указанного crate'а (<crate>_dump.md)
#   ./dump.sh percrate   → отдельный дамп для каждого crate'а
#   ./dump.sh stats      → показать статистику без создания дампа (по группам)
#
# В дамп включаются только файлы с расширениями *.rs и *.toml (Cargo.toml).
# Исключаются все артефакты сборки и служебные каталоги:
#   target, _build, deps, .git, node_modules, playwright-report,
#   test-results, cover, .pytest_cache, __pycache__ и любые подкаталоги
#   research/out, bt/bt_dataset, а также *.min.js и *.db (если появятся).
#
# Файлы пишутся в текущую директорию (рядом с dump.sh) без timestamp.
#
# Требует наличия файла repo.conf с алиасами вида:
#   MANIFEST=Cargo.toml
#   CARGO_LOCK=Cargo.lock
#   UWA_BROWSER_SRC=crates/uwa-browser/src
#   UWA_BROWSER_TOML=crates/uwa-browser/Cargo.toml
#   ... и т.д. для всех crate'ов.
#
# colors.sh не обязателен — оставлены пустые заглушки для совместимости.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Подгружаем алиасы из repo.conf в ассоциативный массив PATHS
if [[ ! -f "$SCRIPT_DIR/repo.conf" ]]; then
  echo "Error: repo.conf not found in $SCRIPT_DIR" >&2
  exit 1
fi
declare -A PATHS
while IFS='=' read -r key value; do
  # Пропускаем пустые строки и комментарии
  [[ -z "$key" || "$key" =~ ^# ]] && continue
  # Удаляем ведущие и尾部 пробелы
  key=$(echo "$key" | xargs)
  value=$(echo "$value" | xargs)
  # Удаляем inline-комментарии после '#'
  value="${value%%#*}"
  value=$(echo "$value" | xargs)
  [[ -z "$value" ]] && continue
  PATHS[$key]="$value"
done < "$SCRIPT_DIR/repo.conf"

# Пустые заглушки для переменных цветов (оставляем для совместимости)
C_TITLE="" C_RESET="" C_DIM="" C_HDR="" C_EXT="" C_NUM="" C_TOT=""

# Расширения, которые хотим включать в дамп
EXTENSIONS=( -name '*.rs' -o -name '*.toml' )

# Каталоги и пути, которые всегда исключаем
EXCLUDE_DIRS=( -name target -o -name _build -o -name deps -o -name .git \
               -o -name node_modules -o -name playwright-report \
               -o -name test-results -o -name cover \
               -o -name .pytest_cache -o -name __pycache__ )

EXCLUDE_PATHS=( -path '*/research/out/*' -o -path '*/bt/*' \
                -o -name '*.min.js' -o -name '*.db' )

# Сборка списка файлов из одного алиаса (может быть файлом или каталогом)
collect_from_alias() {
  local alias_name="$1"
  local path="${PATHS[$alias_name]:-}"
  [[ -z "$path" ]] && return
  [[ -e "$path" ]] || return

  if [[ -f "$path" ]]; then
    echo "$path"
  else
    find "$path" \
      -type d \( "${EXCLUDE_DIRS[@]}" \) -prune -o \
      -type f \( "${EXTENSIONS[@]}" \) \
      -not \( "${EXCLUDE_PATHS[@]}" \) \
      -print
  fi
}

# Сборка файлов для группы (переименованный алиас, содержащий список имён других алиасов)
collect_group() {
  local -n group_ref="$1"
  local result=()
  for alias in "${group_ref[@]}"; do
    local path="${PATHS[$alias]:-}"
    [[ -n "$path" ]] || continue
    while IFS= read -r line; do
      [[ -n "$line" ]] && result+=("$line")
    done < <(collect_from_alias "$alias")
  done
  printf '%s\n' "${result[@]}" | sort -u
}

# Формируем анкор (id) для заголовка в markdown из пути
make_anchor() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9._-' '-'
}

# Вывод статистики по группе файлов
print_stats() {
  local title="$1" shift
  local files=("$@")
  if [[ ${#files[@]} -eq 0 ]]; then
    echo "  Нет файлов для группы '$title'"
    return
  fi
  declare -A ext_files ext_lines ext_bytes
  local total_lines=0 total_bytes=0
  for f in "${files[@]}"; do
    [[ -f "$f" ]] || continue
    local ext="${f##*.}"
    local lines bytes
    lines=$(wc -l < "$f")
    bytes=$(wc -c < "$f")
    ((ext_files[".$ext"]++))
    ((ext_lines[".$ext"]+=lines))
    ((ext_bytes[".$ext"]+=bytes))
    ((total_lines+=lines))
    ((total_bytes+=bytes))
  done
  local rule
  printf -v rule '%*s' 62 ''; rule="${rule// /─}"
  echo
  echo "  ${C_TITLE}${title}${C_RESET}"
  echo "  ${C_DIM}${rule}${C_RESET}"
  printf '  %s%-12s %8s %10s %12s%s\n' "$C_HDR" 'ext' 'files' 'lines' 'KB' "$C_RESET"
  for ext in "${!ext_files[@]}"; do
    printf '  %s%-12s%s %s%8d %10d %12s%s\n' \
      "$C_EXT" "$ext" "$C_RESET" "$C_NUM" "${ext_files[$ext]}" "${ext_lines[$ext]}" "$(awk "BEGIN {printf \"%.1f\", ${ext_bytes[$ext]/1024}}")" "$C_RESET"
  done < <(printf '%s\n' "${!ext_files[@]}" | sort)
  echo "  ${C_DIM}${rule}${C_RESET}"
  printf '  %s%-12s %8d %10d %12s%s\n' "$C_TOT" 'TOTAL' "$total_lines" "$total_bytes" "$(awk "BEGIN {printf \"%.1f\", $total_bytes/1024}")" "$C_RESET"
}

# Создание одного дампа файла
create_dump() {
  local dump_name="$1"   # без расширения _dump.md
  local -n files_ref="$2" # массив с путями
  local dump_file="$SCRIPT_DIR/${dump_name}_dump.md"
  {
    echo "# ${dump_name^} Dump"
    echo ""
    echo "_Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)_"
    echo ""
    echo "## Table of Contents"
    echo ""
  } > "$dump_file"

  local total_lines=0 total_bytes=0 files_count=0
  declare -A ext_lines ext_bytes ext_files
  for p in "${files_ref[@]}"; do
    [[ -f "$p" ]] || continue
    local anchor
    anchor=$(make_anchor "$p")
    echo "- [$p](#$anchor)" >> "$dump_file"
  done
  echo "" >> "$dump_file"

  for p in "${files_ref[@]}"; do
    [[ -f "$p" ]] || continue
    local anchor ext lang
    anchor=$(make_anchor "$p")
    ext="${p##*.}"
    case "$ext" in
      rs)   lang="rust" ;;
      toml) lang="toml" ;;
      *)    lang="" ;;
    esac
    {
      echo "## $p"
      echo ""
      echo "<a id=\"$anchor\"></a>"
      echo "\`\`\`$lang"
      cat "$p"
      echo "\`\`\`"
      echo ""
    } >> "$dump_file"

    local nlines nbytes
    nlines=$(wc -l < "$p")
    nbytes=$(wc -c < "$p")
    ((files_count++))
    ((total_lines+=nlines))
    ((total_bytes+=nbytes))
    ext_files[".$ext"]=$(( ${ext_files[".$ext"]:-0} + 1 ))
    ext_lines[".$ext"]=$(( ${ext_lines[".$ext"]:-0} + nlines ))
    ext_bytes[".$ext"]=$(( ${ext_bytes[".$ext"]:-0} + nbytes ))
  done

  {
    echo "## Stats"
    echo ""
    echo "- Files: $files_count"
    echo "- Lines: $total_lines"
    echo "- Bytes: $total_bytes"
    echo ""
    echo "### By extension"
    echo ""
    echo "| Ext | Files | Lines | Bytes |"
    echo "|---|---|---|---|"
    for ext in "${!ext_files[@]}"; do
      printf '| .%s | %s | %s | %s |\n' "${ext#.}" "${ext_files[$ext]}" "${ext_lines[$ext]}" "${ext_bytes[$ext]}"
    done | sort
  } >> "$dump_file"

  echo "=== $dump_name dump created: $dump_file ($files_count files) ==="
}

# ---------------------- Основная логика ----------------------
MODE="${1:-}"
case "$MODE" in
  ""|all|full)
    # Полный дамп всех источников + всех Cargo.toml + корневых файлов
    ALL_FILES=()
    # собрать src
    SRC_VARS=(UWA_BROWSER_SRC UWA_BIN_SRC UWA_CONFIG_SRC UWA_MCP_SRC UWA_API_SRC UWA_TOOLS_SRC UWA_EXTRACT_SRC)
echo "SRC_VARS: ${SRC_VARS[*]}"
    while IFS= read -r line; do
      [[ -n "$line" ]] && ALL_FILES+=("$line")
    done < <(collect_group SRC_VARS)
    # собрать toml
    TOML_VARS=(UWA_BROWSER_TOML UWA_BIN_TOML UWA_CONFIG_TOML UWA_MCP_TOML UWA_API_TOML UWA_TOOLS_TOML UWA_EXTRACT_TOML)
    while IFS= read -r line; do
      [[ -n "$line" ]] && ALL_FILES+=("$line")
    done < <(collect_group TOML_VARS)
    # корневые файлы
    for alias in MANIFEST CARGO_LOCK; do
      path="${PATHS[$alias]:-}"
      [[ -n "$path" ]] && ALL_FILES+=("$path")
    done
    # Убираем дупликаты и сортируем
    IFS=$'\n' ALL_FILES=($(sort -u <<<"${ALL_FILES[*]}"))
    unset IFS
    create_dump "full" ALL_FILES
    ;;

  percrate)
echo "Entering percrate case"
    # Дамп для каждого crate отдельно
    # Список src переменных
    SRC_VARS=(UWA_BROWSER_SRC UWA_BIN_SRC UWA_CONFIG_SRC UWA_MCP_SRC UWA_API_SRC UWA_TOOLS_SRC UWA_EXTRACT_SRC)
echo "SRC_VARS: ${SRC_VARS[*]}"
    for src_var in "${SRC_VARS[@]}"; do
      # Убираем суффикс _SRC, чтобы получить базовое имя alias (в верхнем регистре)
      base_name="${src_var%_SRC}"
      # Имя для файла дампа в нижнем регистре
      crate_name="${base_name,,}"
      files=
      while IFS= read -r line; do
        [[ -n "$line" ]] && files+=("$line")
      done < <(collect_from_alias "$src_var")
      # Соответствующая переменная для Cargo.toml: base_name + _TOML
      toml_var="${base_name}_TOML"
      toml_path="${PATHS[$toml_var]:-}"
      if [[ -n "$toml_path" ]]; then
        while IFS= read -r line; do
          [[ -n "$line" ]] && files+=("$line")
        done < <(collect_from_alias "$toml_var")
      fi
      IFS=$'\n' files=($(sort -u <<<"${files[*]}"))
      unset IFS
      create_dump "$crate_name" files
    done
    ;;

  stats)
    # Показать статистику по группам без создания дампа
    SRC_VARS=(UWA_BROWSER_SRC UWA_BIN_SRC UWA_CONFIG_SRC UWA_MCP_SRC UWA_API_SRC UWA_TOOLS_SRC UWA_EXTRACT_SRC)
echo "SRC_VARS: ${SRC_VARS[*]}"
    SRC_FILES=()
    while IFS= read -r line; do
      [[ -n "$line" ]] && SRC_FILES+=("$line")
    done < <(collect_group SRC_VARS)
    TOML_VARS=(UWA_BROWSER_TOML UWA_BIN_TOML UWA_CONFIG_TOML UWA_MCP_TOML UWA_API_TOML UWA_TOOLS_TOML UWA_EXTRACT_TOML)
    TOML_FILES=()
    while IFS= read -r line; do
      [[ -n "$line" ]] && TOML_FILES+=("$line")
    done < <(collect_group TOML_VARS)
    ROOT_FILES=()
    for alias in MANIFEST CARGO_LOCK; do
      path="${PATHS[$alias]:-}"
      [[ -n "$path" ]] && ROOT_FILES+=("$path")
    done
    print_stats "SOURCE (src файлы)" "${SRC_FILES[@]}"
    print_stats "CONFIG (Cargo.toml crate'ов)" "${TOML_FILES[@]}"
    print_stats "ROOT (MANIFEST, CARGO_LOCK)" "${ROOT_FILES[@]}"
    ;;

  *)
    # Если аргумент не совпал со специальными режимами, трактуем как имя crate
    crate_arg="$MODE"
    # Попробуем найти алиас вида <crate>_SRC
    src_alias="${crate_arg^^}_SRC"
    toml_alias="${crate_arg^^}_TOML"
    files=
    src_path="${PATHS[$src_alias]:-}"
    if [[ -n "$src_path" ]]; then
      while IFS= read -r line; do
        [[ -n "$line" ]] && files+=("$line")
      done < <(collect_from_alias "$src_alias")
      toml_path="${PATHS[$toml_alias]:-}"
      if [[ -n "$toml_path" ]]; then
        while IFS= read -r line; do
          [[ -n "$line" ]] && files+=("$line")
        done < <(collect_from_alias "$toml_alias")
      fi
    else
      # Если алиаса нет, считаем, что аргумент — прямой путь
      if [[ -e "$crate_arg" ]]; then
        files=("$crate_arg")
      else
        echo "Error: unknown mode or crate '$crate_arg'. Valid modes: all, full, percrate, stats, или имя crate (например, uwa-browser)." >&2
        exit 2
      fi
    fi
    IFS=$'\n' files=($(sort -u <<<"${files[*]}"))
    unset IFS
    create_dump "$crate_arg" files
    ;;
esac
