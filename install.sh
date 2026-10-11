#!/usr/bin/env bash
# uwa installer — works without cargo-dist being run.
#
#   curl -fsSL https://github.com/sdlab1/uwa/releases/latest/download/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/sdlab1/uwa/main/install.sh | sh
#
# Flags (via env):
#   UWA_VERSION=0.1.0        pin a version (default: latest)
#   UWA_INSTALL_DIR=/path    install prefix (default: $HOME/.local/bin or /usr/local/bin)
#   UWA_NO_VERIFY=1          skip checksum verification (don't)
#
# What it does:
#   1. Detect OS + arch.
#   2. Fetch the release metadata from the GitHub API (or a pinned version).
#   3. Download the matching archive.
#   4. Verify SHA256 against the .sha256 sidecar.
#   5. Extract `uwa` and install it.

set -euo pipefail

REPO="sdlab1/uwa"
BIN="uwa"
VERSION="${UWA_VERSION:-latest}"
INSTALL_DIR="${UWA_INSTALL_DIR:-}"
NO_VERIFY="${UWA_NO_VERIFY:-0}"

# ---------- colours ----------
if [ -t 1 ]; then
  B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; D=$'\033[2m'; Z=$'\033[0m'
else
  B=""; G=""; Y=""; R=""; D=""; Z=""
fi
say()  { printf "%s→%s %s\n" "$B" "$Z" "$1"; }
ok()   { printf "  %s✓%s %s\n" "$G" "$Z" "$1"; }
warn() { printf "  %s⚠%s %s\n" "$Y" "$Z" "$1" >&2; }
die()  { printf "  %s✗%s %s\n" "$R" "$Z" "$1" >&2; exit 1; }

# ---------- detect ----------
detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64)   echo "x86_64-unknown-linux-musl" ;;
        aarch64|arm64)  echo "aarch64-unknown-linux-musl" ;;
        *) die "unsupported Linux arch: $arch" ;;
      esac ;;
    Darwin)
      case "$arch" in
        x86_64) echo "x86_64-apple-darwin" ;;
        arm64)  echo "aarch64-apple-darwin" ;;
        *) die "unsupported macOS arch: $arch" ;;
      esac ;;
    *) die "unsupported OS: $os (Windows: use the PowerShell installer)" ;;
  esac
}

have() { command -v "$1" >/dev/null 2>&1; }

# Prefer curl; fall back to wget.
fetch() {
  local url="$1" out="${2:-}"
  if have curl; then
    if [ -n "$out" ]; then curl -fsSL "$url" -o "$out"; else curl -fsSL "$url"; fi
  elif have wget; then
    if [ -n "$out" ]; then wget -qO "$out" "$url"; else wget -qO- "$url"; fi
  else
    die "need curl or wget"
  fi
}

sha256() {
  if have sha256sum; then sha256sum | awk '{print $1}'
  elif have shasum; then shasum -a 256 | awk '{print $1}'
  else die "need sha256sum or shasum"
  fi
}

# ---------- pick install dir ----------
pick_install_dir() {
  if [ -n "$INSTALL_DIR" ]; then echo "$INSTALL_DIR"; return; fi
  if [ -w /usr/local/bin ]; then echo "/usr/local/bin"; return; fi
  echo "$HOME/.local/bin"
}

# ---------- plan version ----------
resolve_tag() {
  if [ "$VERSION" = "latest" ]; then
    local json
    json="$(fetch "https://api.github.com/repos/$REPO/releases/latest")"
    echo "$json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
  else
    echo "$VERSION"
  fi
}

# ---------- main ----------
main() {
  say "Installing $BIN"

  local target tag
  target="$(detect_target)"
  ok "detected target: $target"

  tag="$(resolve_tag)"
  [ -n "$tag" ] || die "could not resolve version (is there a release?)"
  ok "version: $tag"

  local base archive url sum_url
  base="https://github.com/$REPO/releases/download/$tag"
  archive="$BIN-$tag-$target.tar.xz"
  url="$base/$archive"
  sum_url="$url.sha256"

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  say "Downloading $archive"
  fetch "$url" "$tmp/$archive" || die "download failed: $url"

  if [ "$NO_VERIFY" != "1" ]; then
    local expected actual
    if fetch "$sum_url" "$tmp/$archive.sha256"; then
      expected="$(awk '{print $1}' < "$tmp/$archive.sha256")"
      actual="$(sha256 < "$tmp/$archive")"
      [ "$expected" = "$actual" ] || die "checksum mismatch (expected $expected, got $actual)"
      ok "checksum verified"
    else
      warn "no checksum sidecar at $sum_url — skipping verification"
    fi
  fi

  say "Extracting"
  tar -xJf "$tmp/$archive" -C "$tmp"
  [ -f "$tmp/$BIN" ] || die "archive did not contain $BIN"

  local dest
  dest="$(pick_install_dir)"
  mkdir -p "$dest"
  install -m 0755 "$tmp/$BIN" "$dest/$BIN"

  ok "installed to $dest/$BIN"

  case ":$PATH:" in
    *":$dest:"*) ;;
    *) warn "add $dest to your PATH:"
       warn "  export PATH=\"$dest:\$PATH\"" ;;
  esac

  printf "\n%s%s is ready.%s Run:\n  %s%s start%s\n" \
    "$B" "$BIN" "$Z" "$B" "$BIN" "$Z"
}

main "$@"