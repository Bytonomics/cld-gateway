#!/bin/sh

set -eu

TAP="bytonomics/tap"
FORMULA="$TAP/cld-gateway"

step() {
  printf '==> %s\n' "$1"
}

warn() {
  printf 'WARNING: %s\n' "$1" >&2
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "$1 is required." >&2
    exit 1
  fi
}

usage() {
  cat <<EOF
Usage: homebrew-reinstall.sh

Performs a clean Homebrew reinstall of bytonomics/tap/cld-gateway by:
  - uninstalling any existing cld-gateway formula
  - untapping and re-tapping bytonomics/tap
  - trusting bytonomics/tap and bytonomics/tap/cld-gateway
  - clearing cached cld-gateway downloads
  - forcing a fresh fetch and install
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

if [ "$#" -ne 0 ]; then
  echo "This script does not accept positional arguments." >&2
  usage >&2
  exit 1
fi

require_command brew
require_command find

step "Uninstalling existing cld-gateway formula if present"
brew uninstall cld-gateway >/dev/null 2>&1 || true

step "Refreshing bytonomics/tap"
brew untap "$TAP" >/dev/null 2>&1 || true
brew tap "$TAP"

step "Trusting $TAP and $FORMULA"
brew trust "$TAP"
brew trust --formula "$FORMULA"

step "Clearing cached cld-gateway downloads"
find "$(brew --cache)/downloads" -maxdepth 1 -name '*cld-gateway*' -delete 2>/dev/null || true

step "Fetching fresh formula artifacts"
brew fetch --force "$FORMULA"

step "Installing $FORMULA"
brew install "$FORMULA"

step "Homebrew reinstall finished"
printf 'Next step: cld-gateway-sh setup\n'
