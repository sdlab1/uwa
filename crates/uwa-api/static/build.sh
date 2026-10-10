#!/usr/bin/env bash
# Build the Svelte UI. Output goes to ./dist/ next to this script.
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
npm run --silent typecheck || true

echo "==> bundle"
npm run --silent build

echo "==> done: $(pwd)/dist/main.js"
