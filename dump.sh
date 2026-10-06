#!/usr/bin/env bash
# dump.sh - glues project_uwa Rust sources into Markdown dump file(s).
# Behavior (same as the original dump.sh.bak):
#   ./dump.sh            -> all three dumps: source_dump.md, tests_dump.md, full_dump.md
#   ./dump.sh source     -> only source_dump.md + colored stats
#   ./dump.sh tests      -> only tests_dump.md  + colored stats
#   ./dump.sh full       -> only full_dump.md   + colored stats
# Extra modes:
#   ./dump.sh percrate   -> separate dump for each crate (<crate>_dump.md);
#                           tests of a crate are included INTO its dump
#                           (see PERCRATE_TESTS_SEPARATE below to split them)
#   ./dump.sh stats      -> colored stats only, no dumps
#   ./dump.sh <crate>    -> dump of a single crate (name or path, e.g. uwa-bin)
#
# Dump files are written next to repo.conf, WITHOUT timestamp in the name.
# Only sources go into dumps: *.rs *.toml (no *.md - code and configs only).
# Service dirs (target, .git, node_modules, deps, ...) are always skipped.
# Colors come from colors.sh (auto-disabled when output is not a terminal).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# --- Colors -----------------------------------------------------------
# Shared muted scheme lives in colors.sh; it auto-disables when stdout
# is not a terminal. Empty fallback if the file is missing.
# shellcheck source=colors.sh
if [[ -f "$SCRIPT_DIR/colors.sh" ]]; then
  source "$SCRIPT_DIR/colors.sh"
else
  C_RESET='' C_DIM='' C_TITLE='' C_HDR='' C_HEADER='' C_EXT='' C_NUM='' C_TOT=''
  C_OK='' C_WARN='' C_ERR='' C_ACCENT=''
fi

# --- Per-crate dumps configuration ------------------------------------
# In percrate mode the tests of each crate are glued INTO its crate dump
# (<crate>_dump.md) - modular by design: set PERCRATE_TESTS_SEPARATE=1
# (env var) to emit them as separate <crate>_tests_dump.md files instead.
PERCRATE_TESTS_SEPARATE="${PERCRATE_TESTS_SEPARATE:-0}"

# --- repo.conf parser (strips inline comments after '=') ---------------
if [[ ! -f "$SCRIPT_DIR/repo.conf" ]]; then
  echo "Error: repo.conf not found in $SCRIPT_DIR" >&2
  exit 1
fi
declare -A PATHS
while IFS='=' read -r key value || [[ -n "$key" ]]; do
  [[ -z "$key" || "$key" == \#* ]] && continue
  key=$(echo "$key" | xargs)
  value=$(echo "$value" | xargs)
  value="${value%%#*}"
  value=$(echo "$value" | xargs)
  [[ -z "$value" ]] && continue
  PATHS[$key]="$value"
done < "$SCRIPT_DIR/repo.conf"

# --- Alias groups -----------------------------------------------------
# SOURCE_GROUP - application source: src/ of every crate + crates'
#               Cargo.toml + uwa-bin config template + root configs
#               (Cargo.toml, rust-toolchain.toml, cargo_dependencies2.toml).
#               (*.lock is NOT included - the dump is source only.)
# TESTS_GROUP  - integration tests (tests/) of every crate.
# NOTE: ALL_SRCS/ALL_TOMLS/ALL_TESTS are group aliases from repo.conf
# (space-separated alias lists); collect_files expands them recursively.
SOURCE_GROUP=(ALL_SRCS ALL_TOMLS UWA_BIN_CONFIG MANIFEST RUST_TOOLCHAIN CARGO_DEPS2)
TESTS_GROUP=(ALL_TESTS)

# Only source files are collected
EXTENSIONS=( -name '*.rs' -o -name '*.toml' )

# Service dirs - build artifacts and VCS, always skipped
EXCLUDE_DIRS=( -name target -o -name _build -o -name deps -o -name .git \
               -o -name node_modules -o -name playwright-report \
               -o -name test-results -o -name cover \
               -o -name .pytest_cache -o -name __pycache__ )

# --- collect_single_alias: files of one alias (file or dir) -----------
collect_single_alias() {
  local alias="$1"
  local path="${PATHS[$alias]:-}"
  [[ -z "$path" ]] && return 0
  [[ -e "$path" ]] || return 0

  if [[ -f "$path" ]]; then
    echo "$path"
  else
    find "$path" \
      -type d \( "${EXCLUDE_DIRS[@]}" \) -prune -o \
      -type f \( "${EXTENSIONS[@]}" \) \
      -print
  fi
}

# --- collect_files: collects files for an alias group ------------------
# The group is the NAME of a bash array holding alias names. An element
# is either a leaf alias (single path) or a group alias from repo.conf
# (space-separated alias list) - the latter is expanded recursively.
collect_files() {
  local -n group_ref=$1
  local alias sub path
  for alias in "${group_ref[@]}"; do
    path="${PATHS[$alias]:-}"
    if [[ -n "$path" && "$path" == *\ * ]]; then
      # nested group alias from repo.conf - expand into leaf aliases
      for sub in $path; do
        collect_single_alias "$sub"
      done
    else
      collect_single_alias "$alias"
    fi
  done | sort -u
}

# --- make_anchor: HTML anchor from a path ------------------------------
make_anchor() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9._-' '-'
}

# --- print_group_stats <TITLE> <GROUP_NAME> ----------------------------
# Aggregates group files by extension: files / lines / KB.
print_group_stats() {
  local title="$1" group="$2"
  local tmp; tmp="$(mktemp)"

  collect_files "$group" | while IFS= read -r f; do
    ext="${f##*.}"
    [[ -f "$f" ]] || continue
    lines="$(wc -l < "$f")"
    bytes="$(wc -c < "$f")"
    printf '%s\t%s\t%s\n' "$ext" "$lines" "$bytes"
  done |
  awk -F'\t' '
    { files[$1]++; lines[$1]+=$2; kb[$1]+=$3 }
    END { for (e in files) printf "%s\t%d\t%d\t%.1f\n", e, files[e], lines[e], kb[e]/1024 }
  ' | sort -t$'\t' -k3 -rn > "$tmp"

  local tf tl tkb
  tf="$(awk -F'\t' '{s+=$2} END{print s+0}' "$tmp")"
  tl="$(awk -F'\t' '{s+=$3} END{print s+0}' "$tmp")"
  tkb="$(awk -F'\t' '{s+=$4} END{printf "%.1f", s}' "$tmp")"

  local rule; printf -v rule '%*s' 62 ''; rule="${rule// /-}"

  echo
  echo "  ${C_TITLE}${title}${C_RESET}"
  echo "  ${C_DIM}${rule}${C_RESET}"
  printf '  %s%-12s %8s %10s %12s%s\n' "$C_HDR" 'ext' 'files' 'lines' 'KB' "$C_RESET"
  while IFS=$'\t' read -r ext nf nl kb; do
    printf '  %s%-12s%s %s%8d %10d %12s%s\n' \
      "$C_EXT" ".$ext" "$C_RESET" "$C_NUM" "$nf" "$nl" "$kb" "$C_RESET"
  done < "$tmp"
  echo "  ${C_DIM}${rule}${C_RESET}"
  printf '  %s%-12s %8d %10d %12s%s\n' "$C_TOT" 'TOTAL' "$tf" "$tl" "$tkb" "$C_RESET"

  print_top10_largest "$group"

  rm -f "$tmp"
}

# --- print_top10_largest <GROUP_NAME> ----------------------------------
# Top-10 largest files of a group: path + size.
print_top10_largest() {
  local group="$1"
  local all; all="$(mktemp)"
  local tmp; tmp="$(mktemp)"
  local rule; printf -v rule '%*s' 62 ''; rule="${rule// /-}"

  collect_files "$group" | while IFS= read -r f; do
    [[ -f "$f" ]] || continue
    bytes="$(wc -c < "$f")"
    printf '%s\t%s\n' "$bytes" "$f"
  done | sort -t$'\t' -k1 -rn > "$all"
  # NOTE: head reads from a file, not from the pipe - otherwise sort
  # catches SIGPIPE (code 141) and `set -euo pipefail` kills the script.
  head -n 10 "$all" > "$tmp"
  rm -f "$all"

  if [[ -s "$tmp" ]]; then
    echo
    echo "  ${C_TITLE}TOP 10 LARGEST FILES${C_RESET}"
    echo "  ${C_DIM}${rule}${C_RESET}"
    while IFS=$'\t' read -r bytes file; do
      kb=$(awk "BEGIN {printf \"%.1f\", $bytes/1024}")
      printf '  %s%-5s KB%s  %s\n' "$C_NUM" "$kb" "$C_RESET" "$file"
    done < "$tmp"
    echo "  ${C_DIM}${rule}${C_RESET}"
  fi

  rm -f "$tmp"
}

print_source_stats() { print_group_stats "SOURCE - src / Cargo.toml / config / manifest" SOURCE_GROUP; }
print_tests_stats()  { print_group_stats "TESTS - integration tests (tests/)"                   TESTS_GROUP; }

# --- build_dump <name> <title> <GROUP_NAME>... -------------------------
# Builds one dump: TOC + per-file code blocks + Stats section.
build_dump() {
  local mode="$1"     # dump file name without the _dump.md suffix
  local title="$2"    # H1 title inside the dump
  shift 2
  local groups=("$@")

  local ALL_FILES=()
  local g f
  for g in "${groups[@]}"; do
    while IFS= read -r f; do
      [[ -n "$f" ]] && ALL_FILES+=("$f")
    done < <(collect_files "$g")
  done

  local DUMP_FILE="$SCRIPT_DIR/${mode}_dump.md"
  {
    echo "# ${title}"
    echo ""
    echo "_Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)_"
    echo ""
    echo "## Table of Contents"
    echo ""
  } > "$DUMP_FILE"

  local p anchor
  for p in "${ALL_FILES[@]}"; do
    anchor=$(make_anchor "$p")
    echo "- [$p](#$anchor)" >> "$DUMP_FILE"
  done
  echo "" >> "$DUMP_FILE"

  local total_lines=0 total_bytes=0 files_dumped=0
  declare -A ext_lines ext_bytes ext_files
  for p in "${ALL_FILES[@]}"; do
    [[ -f "$p" ]] || continue
    anchor=$(make_anchor "$p")
    local ext lang
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
    } >> "$DUMP_FILE"

    local nlines nbytes
    nlines=$(wc -l < "$p")
    nbytes=$(wc -c < "$p")
    files_dumped=$((files_dumped + 1))
    total_lines=$((total_lines + nlines))
    total_bytes=$((total_bytes + nbytes))
    ext_files[$ext]=$(( ${ext_files[$ext]:-0} + 1 ))
    ext_lines[$ext]=$(( ${ext_lines[$ext]:-0} + nlines ))
    ext_bytes[$ext]=$(( ${ext_bytes[$ext]:-0} + nbytes ))
  done

  {
    echo "## Stats"
    echo ""
    echo "- Files: $files_dumped"
    echo "- Lines: $total_lines"
    echo "- Bytes: $total_bytes"
    echo ""
    echo "### By extension"
    echo ""
    echo "| Ext | Files | Lines | Bytes |"
    echo "|---|---|---|---|"
    local ext
    for ext in $(printf '%s\n' "${!ext_files[@]}" | sort); do
      echo "| .$ext | ${ext_files[$ext]} | ${ext_lines[$ext]} | ${ext_bytes[$ext]} |"
    done
  } >> "$DUMP_FILE"

  echo "${C_OK}=== $mode dump created: $DUMP_FILE ($files_dumped files) ===${C_RESET}"
}

# --- expand_group_aliases: repo.conf group -> leaf alias NAMES --------
# Expands a group alias (space-separated alias list) into the names of
# its leaf aliases. Used to derive the crate list from ALL_SRCS, so
# adding a crate only requires a repo.conf edit - no script changes.
expand_group_aliases() {
  local group="$1"
  local value="${PATHS[$group]:-}"
  [[ -z "$value" ]] && return 0
  local item sub
  for item in $value; do
    sub="${PATHS[$item]:-}"
    if [[ -n "$sub" && "$sub" == *\ * ]]; then
      expand_group_aliases "$item"
    else
      echo "$item"
    fi
  done
}

# --- build_percrate_dumps ----------------------------------------------
# Separate dump for each crate. Modular layout:
#   crate dump group = [crate]_SRC + [crate]_TOML (+ config template)
#                      + [crate]_TESTS        <-- tests glued together
# To split tests into their own <crate>_tests_dump.md, set
# PERCRATE_TESTS_SEPARATE=1 (or: PERCRATE_TESTS_SEPARATE=1 ./dump.sh percrate).
build_percrate_dumps() {
  local alias base crate_name
  # Crate list is derived from the ALL_SRCS group alias of repo.conf -
  # a new crate is picked up automatically from repo.conf alone.
  for alias in $(expand_group_aliases ALL_SRCS); do
    base="${alias%_SRC}"
    crate_name="${base,,}"

    # modular part 1: crate code + manifests (+ config template)
    CRATE_GROUP=()
    [[ -n "${PATHS[${base}_SRC]:-}"  ]] && CRATE_GROUP+=("${base}_SRC")
    [[ -n "${PATHS[${base}_TOML]:-}" ]] && CRATE_GROUP+=("${base}_TOML")
    if [[ "$base" == "UWA_BIN" && -n "${PATHS[UWA_BIN_CONFIG]:-}" ]]; then
      CRATE_GROUP+=("UWA_BIN_CONFIG")
    fi

    if [[ "$PERCRATE_TESTS_SEPARATE" == "1" ]]; then
      # modular split: tests of the crate as a separate dump
      build_dump "$crate_name" "UWA ${crate_name//_/-} Dump" CRATE_GROUP
      if [[ -n "${PATHS[${base}_TESTS]:-}" ]]; then
        CRATE_TESTS_GROUP=("${base}_TESTS")
        build_dump "${crate_name}_tests" "UWA ${crate_name//_/-} Tests Dump" CRATE_TESTS_GROUP
      fi
    else
      # modular part 2: tests of the crate glued into the same dump
      [[ -n "${PATHS[${base}_TESTS]:-}" ]] && CRATE_GROUP+=("${base}_TESTS")
      build_dump "$crate_name" "UWA ${crate_name//_/-} Dump" CRATE_GROUP
    fi
  done
}

# --- build_crate_dump <crate-or-path> ----------------------------------
# Dump of a single crate: [crate]_SRC + _TOML + _TESTS (+ config template).
# If no aliases match, the argument is used as a direct file/dir path.
build_crate_dump() {
  local crate_arg="$1"
  local stem
  stem="${crate_arg^^}"; stem="${stem//-/_}"

  if [[ -n "${PATHS[${stem}_SRC]:-}" ]]; then
    CRATE_GROUP=("${stem}_SRC")
    [[ -n "${PATHS[${stem}_TOML]:-}"  ]] && CRATE_GROUP+=("${stem}_TOML")
    [[ -n "${PATHS[${stem}_TESTS]:-}" ]] && CRATE_GROUP+=("${stem}_TESTS")
    if [[ "$stem" == "UWA_BIN" && -n "${PATHS[UWA_BIN_CONFIG]:-}" ]]; then
      CRATE_GROUP+=("UWA_BIN_CONFIG")
    fi
  elif [[ -e "$crate_arg" ]]; then
    # alias not found - register the path temporarily as an alias so the
    # same collection code handles it (file or dir)
    PATHS["__ARG_PATH__"]="$crate_arg"
    CRATE_GROUP=("__ARG_PATH__")
  else
    echo "Usage: $0 [all|tests|source|full|percrate|stats|<crate>] (default: all)" >&2
    exit 2
  fi

  # dump file name: lowercase, underscores (uwa-bin -> uwa_bin_dump.md)
  local dump_name="${crate_arg,,}"
  dump_name="${dump_name//-/_}"; dump_name="${dump_name//\//_}"
  build_dump "$dump_name" "UWA $crate_arg Dump" CRATE_GROUP
}

# --- What to generate ---------------------------------------------------
# Default (no args) - all three dumps, same as the original dump.sh.bak:
# source_dump.md, tests_dump.md, full_dump.md
MODE="${1:-all}"
case "$MODE" in
  all)      build_dump "source" "UWA Source Dump" SOURCE_GROUP; print_source_stats
            build_dump "tests"  "UWA Tests Dump"  TESTS_GROUP;  print_tests_stats
            build_dump "full"   "UWA Full Dump"   SOURCE_GROUP TESTS_GROUP ;;
  tests)    build_dump "tests"  "UWA Tests Dump"  TESTS_GROUP;  print_tests_stats ;;
  source)   build_dump "source" "UWA Source Dump" SOURCE_GROUP; print_source_stats ;;
  full)     build_dump "full"   "UWA Full Dump"   SOURCE_GROUP TESTS_GROUP
            print_source_stats; print_tests_stats ;;
  percrate) build_percrate_dumps ;;
  stats)    print_source_stats; print_tests_stats ;;
  *)        build_crate_dump "$MODE" ;;
esac
