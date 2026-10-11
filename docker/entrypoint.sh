#!/usr/bin/env bash
# Container entrypoint: bootstrap config, launch Chromium, hand off to uwa.
set -euo pipefail

CFG="/etc/uwa/uwa.toml"
if [ ! -f "$CFG" ]; then
  cp /etc/uwa/config.example.toml "$CFG"
  echo "→ created $CFG from example"
fi

# Bind inside the container. The example config binds to 127.0.0.1, which
# is unreachable from outside; rewrite to 0.0.0.0 unless the user already
# changed it. api_key becomes mandatory for a non-loopback bind, and the
# config loader enforces that — so set one if it's missing.
if grep -q '^bind *= *"127.0.0.1"' "$CFG"; then
  sed -i 's/^bind *= *"127.0.0.1"/bind = "0.0.0.0"/' "$CFG"
fi
if ! grep -qE '^api_key *= *"[^"]+"' "$CFG"; then
  KEY="${UWA_API_KEY:-$(head -c 24 /dev/urandom | base64 | tr -d '/+=')}"
  sed -i "s/^api_key *=.*/api_key = \"$KEY\"/" "$CFG"
  echo "→ generated API key: $KEY"
fi

# Launch Chromium in the background; uwa start will detect the CDP
# endpoint and reuse it instead of spawning its own.
chromium \
  --headless=new \
  --no-sandbox \
  --disable-gpu \
  --disable-dev-shm-usage \
  --remote-debugging-port=9222 \
  --user-data-dir=/var/lib/uwa/chromium-profile \
  --no-first-run \
  --no-default-browser-check \
  about:blank >/dev/null 2>&1 &

exec uwa start --quiet --no-browser "$@"