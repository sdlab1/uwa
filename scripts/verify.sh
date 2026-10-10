#!/usr/bin/env bash
# Verification protocol for uwa workspace.
#
# Run this after any large change. All steps must pass before merging.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> 1/10 cargo fmt"
cargo fmt --all -- --check

echo "==> 2/10 cargo build (default features)"
cargo build --workspace --locked

echo "==> 3/10 cargo build (all features)"
cargo build --workspace --all-features --locked

echo "==> 4/10 cargo clippy (all targets, all features, -D warnings)"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> 5/10 cargo test (default features)"
cargo test --workspace --locked 2>&1 | tee /tmp/verify-base.log
! grep -E '[1-9][0-9]* ignored' /tmp/verify-base.log || { echo "FAIL: ignored tests found"; exit 1; }

echo "==> 6/10 cargo test (all features)"
cargo test --workspace --all-features --locked 2>&1 | tee /tmp/verify-allfeat.log
! grep -E '[1-9][0-9]* ignored' /tmp/verify-allfeat.log || { echo "FAIL: ignored tests found"; exit 1; }

echo "==> 7/10 proptests (PROPTEST_CASES=5000)"
PROPTEST_CASES=5000 cargo test -p uwa-tools --test proptest_parser --locked
PROPTEST_CASES=5000 cargo test -p uwa-extract --test proptest_net --locked

echo "==> 8/10 feature-matrix builds"
for feat in \
    "uwa-api/metrics" \
    "uwa-bin/metrics" \
    "uwa-providers/snapshot" \
    "uwa-providers/fixture-server" \
    "uwa-browser/cdp" \
    "uwa-browser/nodriver"
do
    echo "   -> $feat"
    cargo build --workspace --features "$feat" --locked
done

echo "==> 9/10 dead code markers (only cfg_attr feature-gates allowed)"
if rg -n '^\s*fn _keep|#\[allow\(dead_code\)\]|let _ = \w+;' --type rust crates uwa-core 2>/dev/null | grep -v 'cfg_attr'; then
    echo "FAIL: dead code markers found"
    exit 1
fi

echo "==> 10/10 insta snapshot tests"
if command -v cargo-insta >/dev/null 2>&1; then
    cargo insta test --workspace --all-features 2>&1 | tee /tmp/verify-insta.log
    ! grep -E '[1-9][0-9]* ignored' /tmp/verify-insta.log || { echo "FAIL: ignored in insta"; exit 1; }
else
    echo "   cargo-insta not installed, skipping (run: cargo install cargo-insta --locked)"
fi

echo
echo "✅ verification complete"
