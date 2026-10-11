#!/usr/bin/env bash
# Full local check: rust tests, UI typecheck, UI build, UI E2E.
set -euo pipefail
cd "$(dirname "$0")"

step() { printf "\n\033[1m==> %s\033[0m\n" "$1"; }

step "cargo test"
cargo test --workspace --quiet

step "UI typecheck"
( cd crates/uwa-api/static && npm run --silent typecheck )

step "UI build"
( cd crates/uwa-api/static && npm run --silent build )

step "UI E2E"
( cd crates/uwa-api/static && npm run --silent test:e2e )

printf "\n\033[32mall green\033[0m\n"
