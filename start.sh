#!/usr/bin/env bash
# start.sh — build (if needed) and delegate to `uwa start`.
#
#   ./start.sh              build if needed, launch Chromium + uwa, open dashboard
#   ./start.sh --stop       stop whatever this script started
#   ./start.sh --rebuild    force a cargo rebuild before launching
#   ./start.sh -- <args>    forward the rest to `uwa start`
#
# Everything else (config bootstrapping, Chromium detection, URL printing,
# daemon lifecycle) lives in the Rust binary. This script exists only to
# cover the "I have the source, not the binary yet" case.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; RESET=$'\033[0m'
  GRN=$'\033[32m'; YEL=$'\033[33m'; RED=$'\033[31m'; CYN=$'\033[36m'
else
  BOLD=""; DIM=""; RESET=""; GRN=""; YEL=""; RED=""; CYN=""
fi

step() { printf "%s→%s %s\n" "$CYN" "$RESET" "$1"; }
ok()   { printf "  %s✓%s %s\n" "$GRN" "$RESET" "$1"; }
info() { printf "  %s%s%s\n" "$DIM" "$1" "$RESET"; }
fail() { printf "  %s✗%s %s\n" "$RED" "$RESET" "$1" >&2; }

BIN="$ROOT/target/release/uwa"
FORCE_REBUILD=0
PASSTHROUGH=()

while [ $# -gt 0 ]; do
  case "$1" in
    --rebuild) FORCE_REBUILD=1; shift ;;
    --) shift; PASSTHROUGH=("$@"); break ;;
    *) PASSTHROUGH+=("$1"); shift ;;
  esac
done

# A rebuild is triggered by --rebuild or by a missing binary. Otherwise we
# trust the existing build, even if it's stale, because the user might be
# running from a fresh checkout on a slow machine.
if [ "$FORCE_REBUILD" = "1" ] || [ ! -x "$BIN" ]; then
  step "Building"
  if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo not found — install Rust: https://rustup.rs"
    exit 1
  fi
  cargo build --release -q 2>&1 | sed 's/^/  /'
  ok "daemon built"

  if [ ! -f "$ROOT/crates/uwa-api/static/dist/main.js" ]; then
    step "Bundling UI"
    if ! command -v npm >/dev/null 2>&1; then
      fail "npm not found — install Node.js 20+: https://nodejs.org"
      exit 1
    fi
    ( cd "$ROOT/crates/uwa-api/static" \
        && [ -d node_modules ] || npm install --silent ) 2>&1 | sed 's/^/  /'
    ( cd "$ROOT/crates/uwa-api/static" && npm run --silent build ) 2>&1 | sed 's/^/  /'
    ok "UI bundled"
  fi
else
  info "using existing build (pass --rebuild to force)"
fi

# Delegate. `uwa start` handles Chromium, config, and the foreground daemon.
exec "$BIN" start "${PASSTHROUGH[@]}"