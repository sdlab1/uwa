#!/usr/bin/env bash
# uwa — one-command launcher.
#
#   ./start.sh              build (if needed), launch Chromium + uwa, open dashboard
#   ./start.sh --stop       stop whatever this script started
#   ./start.sh --no-browser don't open the dashboard
#   ./start.sh --no-chromium  reuse an existing Chromium (requires UWA_CHROMIUM_WS)
#   ./start.sh --rebuild    force a cargo rebuild
#   ./start.sh --port 9090  override the server port
#   ./start.sh -h           help
#
# Env overrides:
#   UWA_CONFIG      path to TOML           (default: ./uwa.toml)
#   UWA_CHROMIUM_WS CDP endpoint           (default: http://127.0.0.1:9222)
#   UWA_CHROME_BIN  Chromium binary        (auto-detected otherwise)
#   UWA_PORT        server port            (overridden by --port)
#   UWA_RUST_LOG    tracing filter         (default: info)

set -euo pipefail

# ============ ANSI ============
if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; RESET=$'\033[0m'
  RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; CYN=$'\033[36m'
else
  BOLD=""; DIM=""; RESET=""; RED=""; GRN=""; YEL=""; CYN=""
fi

step() { printf "%s→%s %s\n" "$CYN" "$RESET" "$1"; }
ok()   { printf "  %s✓%s %s\n" "$GRN" "$RESET" "$1"; }
warn() { printf "  %s⚠%s %s\n" "$YEL" "$RESET" "$1"; }
fail() { printf "  %s✗%s %s\n" "$RED" "$RESET" "$1" >&2; }
info() { printf "  %s%s%s\n" "$DIM" "$1" "$RESET"; }

# ============ Args ============
OPEN_BROWSER=1
START_CHROMIUM=1
FORCE_REBUILD=0
PORT_OVERRIDE=""

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      sed -n '2,/^set -euo pipefail$/p' "$0" | sed '/^set -euo/d; s/^# \{0,1\}//'
      exit 0 ;;
    --stop)
      STOP=1 ;;
    --no-browser)
      OPEN_BROWSER=0 ;;
    --no-chromium)
      START_CHROMIUM=0 ;;
    --rebuild)
      FORCE_REBUILD=1 ;;
    --port)
      shift
      PORT_OVERRIDE="${1:-}"
      [ -z "$PORT_OVERRIDE" ] && { fail "--port requires a value"; exit 2; } ;;
    *)
      fail "unknown argument: $1"
      echo "  Run ./start.sh --help"
      exit 2 ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

PID_DIR="$ROOT/.uwa"
UWA_PID_FILE="$PID_DIR/uwa.pid"
CHROME_PID_FILE="$PID_DIR/chromium.pid"
mkdir -p "$PID_DIR"

CONFIG="${UWA_CONFIG:-$ROOT/uwa.toml}"
CHROMIUM_WS="${UWA_CHROMIUM_WS:-http://127.0.0.1:9222}"

# ============ Helpers (must precede first use) ============
have() { command -v "$1" >/dev/null 2>&1; }

open_url() {
  local u="$1"
  case "$(uname -s)" in
    Darwin) open "$u" >/dev/null 2>&1 || true ;;
    Linux)  (xdg-open "$u" >/dev/null 2>&1 || true) ;;
    *)      true ;;
  esac
}

port_from_config() {
  local f="$1"
  [ -f "$f" ] || { echo 8080; return; }
  # crude parse: first `port = N` under [server]
  awk '
    /^\[server\]/ { in_server = 1; next }
    /^\[/ { in_server = 0 }
    in_server && /^[[:space:]]*port[[:space:]]*=/ {
      gsub(/[^0-9]/, "", $0); print; exit
    }
  ' "$f" 2>/dev/null || echo 8080
}

# Is something speaking CDP at the given http://host:port ?
cdp_ready() {
  local url="$1"
  local rest="${url#*://}"
  local host="${rest%%:*}"
  local port="${rest##*:}"
  port="${port%%/*}"
  [ -z "$host" ] && host="127.0.0.1"
  if have curl; then
    curl -sf --max-time 1 "http://${host}:${port}/json/version" >/dev/null 2>&1
  else
    # Fallback: raw /dev/tcp probe
    (exec 3<>"/dev/tcp/${host}/${port}") 2>/dev/null
  fi
}

wait_http() {
  local url="$1" tries="${2:-40}" delay="${3:-0.25}"
  local i=0
  while [ $i -lt "$tries" ]; do
    if have curl; then
      curl -sf --max-time 1 "$url" >/dev/null 2>&1 && return 0
    fi
    i=$((i+1))
    sleep "$delay"
  done
  return 1
}

# ============ --stop ============
if [ "${STOP:-0}" = "1" ]; then
  stopped=0
  for label in uwa chromium; do
    f="$PID_DIR/${label}.pid"
    if [ -f "$f" ]; then
      pid="$(cat "$f" 2>/dev/null || true)"
      if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        sleep 0.3
        kill -9 "$pid" 2>/dev/null || true
        printf "  %s✓%s stopped %s (pid %s)\n" "$GRN" "$RESET" "$label" "$pid"
        stopped=1
      fi
      rm -f "$f"
    fi
  done
  if [ "$stopped" = "0" ]; then
    printf "  %s%s%s\n" "$DIM" "nothing to stop" "$RESET"
  fi
  # Also try the config-defined pid file if it exists (uwa writes one).
  if [ -f "$ROOT/uwa.pid" ]; then
    pid="$(cat "$ROOT/uwa.pid" 2>/dev/null || true)"
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    rm -f "$ROOT/uwa.pid"
  fi
  exit 0
fi

# ============ Banner ============
printf "\n%s%suwa%s %s· Universal Web API%s\n" \
  "$BOLD" "$CYN" "$RESET" "$DIM" "$RESET"

# ============ 1. Dependencies ============
step "Checking dependencies"

if ! have cargo; then
  fail "cargo not found"
  printf "  Install Rust: %shttps://rustup.rs%s\n" "$CYN" "$RESET"
  exit 1
fi
ok "cargo $(cargo --version | awk '{print $2}')"

if ! have npm; then
  fail "npm not found (required to build the UI)"
  printf "  Install Node.js 20+: %shttps://nodejs.org%s\n" "$CYN" "$RESET"
  exit 1
fi
ok "npm $(npm --version)"

if ! have curl; then
  warn "curl not found — using slower health checks"
fi

CHROME_BIN="${UWA_CHROME_BIN:-}"
if [ -z "$CHROME_BIN" ]; then
  for c in \
    google-chrome google-chrome-stable chromium chromium-browser \
    chrome \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
  do
    if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then
      CHROME_BIN="$c"; break
    fi
  done
fi

# ============ 2. Config ============
step "Preparing config"
if [ ! -f "$CONFIG" ]; then
  if [ -f "$ROOT/crates/uwa-bin/config.example.toml" ]; then
    cp "$ROOT/crates/uwa-bin/config.example.toml" "$CONFIG"
    ok "created $CONFIG from example"
    info "edit it later to change providers, selectors or the API key"
  else
    fail "missing $CONFIG and no example to copy from"
    exit 1
  fi
else
  ok "using $CONFIG"
fi

PORT="${PORT_OVERRIDE:-${UWA_PORT:-$(port_from_config "$CONFIG")}}"
[ -z "$PORT" ] && PORT=8080
BIND="127.0.0.1"
BASE_URL="http://${BIND}:${PORT}"

# If an instance is already up, don't start a second one.
if wait_http "$BASE_URL/healthz" 1 0.05; then
  warn "uwa is already running at $BASE_URL"
  if [ "$OPEN_BROWSER" = "1" ]; then
    info "opening dashboard…"
    open_url "$BASE_URL/"
  fi
  exit 0
fi

# ============ 3. Build ============
step "Building"

BIN="$ROOT/target/release/uwa"
NEED_BUILD=0
if [ "$FORCE_REBUILD" = "1" ] || [ ! -x "$BIN" ]; then
  NEED_BUILD=1
fi

# UI bundle: built by vite; the daemon can auto-build, but doing it now
# means the very first page load is instant.
UI_MAIN="$ROOT/crates/uwa-api/static/dist/main.js"
NEED_UI=0
if [ ! -f "$UI_MAIN" ]; then
  NEED_UI=1
fi

if [ "$NEED_BUILD" = "0" ] && [ "$NEED_UI" = "0" ]; then
  ok "already built (use --rebuild to force)"
else
  if [ "$NEED_BUILD" = "1" ]; then
    info "cargo build --release (this may take a few minutes the first time)"
    if ! cargo build --release -q 2>&1 | sed 's/^/  /'; then
      fail "cargo build failed"
      exit 1
    fi
    ok "daemon built"
  fi

  if [ "$NEED_UI" = "1" ]; then
    info "installing UI dependencies"
    ( cd "$ROOT/crates/uwa-api/static" && npm install --silent ) 2>&1 | sed 's/^/  /'
    info "bundling UI"
    ( cd "$ROOT/crates/uwa-api/static" && npm run --silent build ) 2>&1 | sed 's/^/  /'
    ok "UI bundled"
  fi
fi

# ============ 4. Chromium ============
CHROME_STARTED_BY_US=0
if [ "$START_CHROMIUM" = "1" ]; then
  step "Chromium"
  if cdp_ready "$CHROMIUM_WS"; then
    ok "reusing existing CDP at $CHROMIUM_WS"
  elif [ -n "$CHROME_BIN" ]; then
    # stable profile dir → logins survive restarts
    case "$(uname -s)" in
      Darwin) PROFILE="$HOME/Library/Caches/uwa/chromium-profile" ;;
      *)      PROFILE="${XDG_CACHE_HOME:-$HOME/.cache}/uwa/chromium-profile" ;;
    esac
    mkdir -p "$PROFILE"

    # derive host:port from CHROMIUM_WS for --remote-debugging-port
    rest="${CHROMIUM_WS#*://}"; cport="${rest##*:}"; cport="${cport%%/*}"
    [ -z "$cport" ] && cport=9222

    info "launching $CHROME_BIN on port $cport"
    # Background, detached, survives this shell's exit (but tracked by pid file).
    "$CHROME_BIN" \
      "--remote-debugging-port=${cport}" \
      "--user-data-dir=${PROFILE}" \
      --no-first-run --no-default-browser-check \
      --password-store=basic \
      "about:blank" >/dev/null 2>&1 &
    CHROME_PID=$!
    echo "$CHROME_PID" > "$CHROME_PID_FILE"
    CHROME_STARTED_BY_US=1

    if wait_http "${CHROMIUM_WS%/}/json/version" 40 0.25; then
      ok "Chromium ready (pid $CHROME_PID)"
    else
      fail "Chromium did not expose CDP at $CHROMIUM_WS"
      info "check that --remote-debugging-port is free and the profile isn't locked"
      exit 1
    fi
  else
    warn "no Chromium found"
    info "install Chrome/Chromium, or set UWA_CHROME_BIN=/path/to/chrome"
    info "or reuse an external browser via --no-chromium UWA_CHROMIUM_WS=…"
    exit 1
  fi
else
  step "Chromium (skipped)"
  if cdp_ready "$CHROMIUM_WS"; then
    ok "external CDP reachable at $CHROMIUM_WS"
  else
    fail "UWA_CHROMIUM_WS=$CHROMIUM_WS is not responding"
    exit 1
  fi
fi

# ============ 5. Start uwa ============
step "Starting uwa"
info "config: $CONFIG"
info "server: $BASE_URL"

# Forward SIGINT/SIGTERM to uwa and clean up what we started.
UWA_PID=""
cleanup() {
  if [ -n "$UWA_PID" ] && kill -0 "$UWA_PID" 2>/dev/null; then
    kill "$UWA_PID" 2>/dev/null || true
    wait "$UWA_PID" 2>/dev/null || true
  fi
  if [ "$CHROME_STARTED_BY_US" = "1" ] && [ -f "$CHROME_PID_FILE" ]; then
    cpid="$(cat "$CHROME_PID_FILE" 2>/dev/null || true)"
    [ -n "$cpid" ] && kill "$cpid" 2>/dev/null || true
    rm -f "$CHROME_PID_FILE"
  fi
  rm -f "$UWA_PID_FILE"
}
trap cleanup EXIT INT TERM

UWA_RUST_LOG="${UWA_RUST_LOG:-info}"
# --port must beat uwa.toml: pass it through the env-override channel
# (UWA__SECTION__FIELD), which has precedence over the TOML file.
UWA__SERVER__PORT="$PORT" \
  RUST_LOG="$UWA_RUST_LOG" \
  "$BIN" --config "$CONFIG" &
UWA_PID=$!
echo "$UWA_PID" > "$UWA_PID_FILE"

# Wait for /healthz
if wait_http "$BASE_URL/healthz" 80 0.25; then
  ok "daemon ready (pid $UWA_PID)"
else
  fail "uwa did not answer at $BASE_URL/healthz"
  info "tail of the log above may show what went wrong"
  exit 1
fi

printf "\n%s%suwa is running%s\n" "$BOLD" "$GRN" "$RESET"
printf "  dashboard  %s%s/%s\n" "$CYN" "$BASE_URL" "$RESET"
printf "  openai api %s%s/v1%s\n" "$CYN" "$BASE_URL" "$RESET"
printf "  anthropic  %s%s/v1/messages%s\n" "$CYN" "$BASE_URL" "$RESET"
printf "  stop       %s./start.sh --stop%s\n\n" "$DIM" "$RESET"

if [ "$OPEN_BROWSER" = "1" ]; then
  open_url "$BASE_URL/"
fi

# Foreground: wait on uwa. Ctrl-C triggers cleanup.
wait "$UWA_PID"
