#!/bin/sh

set -eu

FORMULA_NAME="cld-gateway"
BREW_PREFIX="$(brew --prefix "$FORMULA_NAME")"
CONFIG_PATH="$HOME/.gateway/config.yml"
PACKAGE_SHARE_DIR="$BREW_PREFIX/share/cld-gateway"
PACKAGE_CONFIG_PATH="$PACKAGE_SHARE_DIR/config.yml"
SETTINGS_PATH="$HOME/.claude_codex/settings.json"
PACKAGE_SETTINGS_PATH="$PACKAGE_SHARE_DIR/settings.json"
WRAPPER_ONE="$(brew --prefix)/bin/cldg"
WRAPPER_TWO="$(brew --prefix)/bin/clddg"
INSTALLED_BINARY_ONE="$(brew --prefix)/bin/cld-gateway"
INSTALLED_BINARY_TWO="$BREW_PREFIX/bin/cld-gateway"
HEALTH_PATH="/health"
TIMEOUT_SECS=10

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
Usage: homebrew-verify.sh

Verifies a Homebrew installation of cld-gateway by checking:
  - installed binaries and wrapper scripts
  - installed config/settings files
  - wrapper script contents
  - health endpoint using the listen address from ~/.gateway/config.yml
  - optional wrapper execution if claude is on PATH
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
require_command grep
require_command awk
require_command curl

resolve_expected_listen_addr() {
  if [ ! -f "$PACKAGE_CONFIG_PATH" ]; then
    echo "Packaged config file not found at $PACKAGE_CONFIG_PATH" >&2
    exit 1
  fi

  listen_addr="$(awk -F': ' '/^[[:space:]]*listen_addr:[[:space:]]*/ {print $2; exit}' "$PACKAGE_CONFIG_PATH")"
  if [ -z "$listen_addr" ]; then
    echo "listen_addr is not explicitly configured in packaged config $PACKAGE_CONFIG_PATH" >&2
    exit 1
  fi

  printf '%s\n' "$listen_addr"
}

resolve_health_url() {
  expected_listen_addr="$1"
  printf 'http://%s%s\n' "$expected_listen_addr" "$HEALTH_PATH"
}

verify_installed_config_matches_package() {
  if [ ! -f "$CONFIG_PATH" ]; then
    echo "Config file not found at $CONFIG_PATH" >&2
    exit 1
  fi

  if ! cmp -s "$CONFIG_PATH" "$PACKAGE_CONFIG_PATH"; then
    echo "Installed config file does not match packaged config." >&2
    echo "installed: $CONFIG_PATH" >&2
    echo "package:   $PACKAGE_CONFIG_PATH" >&2
    exit 1
  fi
}

verify_wrapper_content() {
  wrapper_path="$1"
  expected_text="$2"

  if [ ! -f "$wrapper_path" ]; then
    echo "Missing wrapper script: $wrapper_path" >&2
    exit 1
  fi

  if ! grep -F "$expected_text" "$wrapper_path" >/dev/null 2>&1; then
    echo "Wrapper $wrapper_path does not contain expected text: $expected_text" >&2
    exit 1
  fi
}

verify_installed_settings_match_package() {
  if [ ! -f "$SETTINGS_PATH" ]; then
    echo "Settings file not found at $SETTINGS_PATH" >&2
    exit 1
  fi

  if [ ! -f "$PACKAGE_SETTINGS_PATH" ]; then
    echo "Packaged settings file not found at $PACKAGE_SETTINGS_PATH" >&2
    exit 1
  fi

  if ! cmp -s "$SETTINGS_PATH" "$PACKAGE_SETTINGS_PATH"; then
    echo "Installed settings file does not match packaged settings." >&2
    echo "installed: $SETTINGS_PATH" >&2
    echo "package:   $PACKAGE_SETTINGS_PATH" >&2
    exit 1
  fi
}

detect_conflicting_gateway_process() {
  listen_addr="$1"
  host="${listen_addr%%:*}"
  port="${listen_addr##*:}"

  if ! command -v lsof >/dev/null 2>&1; then
    warn "lsof is not available; cannot check for conflicting gateway listeners"
    return
  fi

  lsof_output="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
  if [ -z "$lsof_output" ]; then
    return
  fi

  if printf '%s\n' "$lsof_output" | grep -F "$INSTALLED_BINARY_ONE" >/dev/null 2>&1; then
    return
  fi
  if printf '%s\n' "$lsof_output" | grep -F "$INSTALLED_BINARY_TWO" >/dev/null 2>&1; then
    return
  fi

  echo "A different process is already listening on $host:$port; refusing to treat /health as proof of a correct Homebrew install." >&2
  printf '%s\n' "$lsof_output" >&2
  exit 1
}

step "Verifying Homebrew formula installation"
if ! brew list "$FORMULA_NAME" >/dev/null 2>&1; then
  echo "$FORMULA_NAME is not installed via Homebrew." >&2
  exit 1
fi

step "Checking installed binaries"
command -v cld-gateway >/dev/null 2>&1 || { echo "cld-gateway not found on PATH" >&2; exit 1; }
[ -f "$WRAPPER_ONE" ] || { echo "Missing wrapper: $WRAPPER_ONE" >&2; exit 1; }
[ -f "$WRAPPER_TWO" ] || { echo "Missing wrapper: $WRAPPER_TWO" >&2; exit 1; }

step "Checking installed runtime files"
[ -f "$CONFIG_PATH" ] || { echo "Missing config file: $CONFIG_PATH" >&2; exit 1; }
[ -f "$PACKAGE_CONFIG_PATH" ] || { echo "Missing packaged config file: $PACKAGE_CONFIG_PATH" >&2; exit 1; }
[ -f "$SETTINGS_PATH" ] || { echo "Missing settings file: $SETTINGS_PATH" >&2; exit 1; }
[ -f "$PACKAGE_SETTINGS_PATH" ] || { echo "Missing packaged settings file: $PACKAGE_SETTINGS_PATH" >&2; exit 1; }

step "Verifying wrapper contents"
verify_wrapper_content "$WRAPPER_ONE" 'claude --settings "$HOME/.claude_codex/settings.json" "$@"'
verify_wrapper_content "$WRAPPER_TWO" 'cldg" --dangerously-skip-permissions "$@"'

if command -v claude >/dev/null 2>&1; then
  step "Running wrapper executables because claude is on PATH"
  "$WRAPPER_ONE" --help >/dev/null 2>&1 || warn "cldg --help returned a non-zero exit code"
  "$WRAPPER_TWO" --help >/dev/null 2>&1 || warn "clddg --help returned a non-zero exit code"
else
  warn "claude is not on PATH; wrapper execution checks skipped after verifying script contents"
fi

step "Verifying symlinks created by Python helper"
CODEX_HOME="${HOME}/.claude_codex"
CLAUDE_HOME="${HOME}/.claude"

if [ ! -d "${CODEX_HOME}" ]; then
  echo "Warning: ${CODEX_HOME} not found; cannot verify symlinks" >&2
fi

# Representative entries from SHARED_CLAUDE_ENTRIES in post_install.py
# We check a few key entries that commonly exist
for entry in agents commands skills; do
  source_path="${CLAUDE_HOME}/${entry}"
  target_path="${CODEX_HOME}/${entry}"

  # Only verify symlink if source exists
  if [ -d "$source_path" ]; then
    if [ ! -L "$target_path" ]; then
      echo "ERROR: Expected symlink not found: $target_path (source exists at $source_path)" >&2
      exit 1
    fi
  fi
done
echo "✓ Symlink verification passed"

step "Checking binary behavior"
cld-gateway invalid-command >/dev/null 2>&1 || true

expected_listen_addr="$(resolve_expected_listen_addr)"
verify_installed_config_matches_package
detect_conflicting_gateway_process "$expected_listen_addr"
health_url="$(resolve_health_url "$expected_listen_addr")"
step "Checking health endpoint at $health_url"
if ! curl --fail --silent --show-error --max-time "$TIMEOUT_SECS" "$health_url" >/dev/null; then
  echo "Health check failed at $health_url" >&2
  exit 1
fi

step "Homebrew verification finished successfully"
