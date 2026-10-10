#!/usr/bin/env bash
# Build the UI. Run from anywhere; output goes to ./dist/main.js next to this
# script. The daemon serves that file at /static/dist/main.js.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v npm >/dev/null 2>&1; then
  echo "error: npm is required to build the UI" >&2
  exit 1
fi

if [ ! -d node_modules ]; then
  echo "==> installing dependencies"
  npm install --silent
fi

echo "==> typecheck"
npm run --silent typecheck

echo "==> bundle"
npm run --silent build

echo "==> done: $(pwd)/dist/main.js"
