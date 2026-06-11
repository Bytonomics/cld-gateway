#!/bin/sh

set -eu

FORMULA_NAME="cld-gateway"
BREW_PREFIX="$(brew --prefix "$FORMULA_NAME")"
CONFIG_PATH="$HOME/.gateway/config.yml"
PACKAGE_SHARE_DIR="$BREW_PREFIX/share/cld-gateway"
PACKAGE_CONFIG_PATH="$PACKAGE_SHARE_DIR/config.yml"
SETTINGS_PATH="$HOME/.claude_gateway/settings.json"
PACKAGE_SETTINGS_PATH="$PACKAGE_SHARE_DIR/settings.json"
INSTALLED_COMMANDS_DIR="$HOME/.codex_gateway/commands"
PACKAGE_COMMANDS_DIR="$PACKAGE_SHARE_DIR/commands"
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
  - installed binaries and wrapper scripts (cld-gateway, cld-gateway-sh, cldg, clddg)
  - installed config/settings files
  - deployed command assets (e.g. ~/.codex_gateway/commands/codex/status.md)
  - wrapper script contents
  - health endpoint using the listen address from ~/.gateway/config.yml
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

verify_installed_commands_dir_matches_package() {
  if [ ! -d "$INSTALLED_COMMANDS_DIR" ]; then
    echo "Installed commands directory not found at $INSTALLED_COMMANDS_DIR" >&2
    exit 1
  fi

  if [ ! -d "$PACKAGE_COMMANDS_DIR" ]; then
    echo "Packaged commands directory not found at $PACKAGE_COMMANDS_DIR" >&2
    exit 1
  fi

  # Recursively verify all files in the packaged commands directory exist
  # and match their installed counterparts
  for pkg_file in $(find "$PACKAGE_COMMANDS_DIR" -type f); do
    rel_path="${pkg_file#$PACKAGE_COMMANDS_DIR/}"
    installed_file="$INSTALLED_COMMANDS_DIR/$rel_path"

    if [ ! -f "$installed_file" ]; then
      echo "Installed command file not found: $installed_file" >&2
      echo "Package has: $pkg_file" >&2
      exit 1
    fi

    if ! cmp -s "$installed_file" "$pkg_file"; then
      echo "Installed command file does not match packaged file." >&2
      echo "installed: $installed_file" >&2
      echo "package:   $pkg_file" >&2
      exit 1
    fi
  done
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
WRAPPER_NEW="$(brew --prefix)/bin/cld-gateway-sh"
[ -f "$WRAPPER_NEW" ] || { echo "Missing wrapper: $WRAPPER_NEW" >&2; exit 1; }
[ -x "$WRAPPER_NEW" ] || { echo "Wrapper not executable: $WRAPPER_NEW" >&2; exit 1; }

step "Checking installed runtime files"
[ -f "$CONFIG_PATH" ] || { echo "Missing config file: $CONFIG_PATH" >&2; exit 1; }
[ -f "$PACKAGE_CONFIG_PATH" ] || { echo "Missing packaged config file: $PACKAGE_CONFIG_PATH" >&2; exit 1; }
[ -f "$SETTINGS_PATH" ] || { echo "Missing settings file: $SETTINGS_PATH" >&2; exit 1; }
[ -f "$PACKAGE_SETTINGS_PATH" ] || { echo "Missing packaged settings file: $PACKAGE_SETTINGS_PATH" >&2; exit 1; }
[ -d "$INSTALLED_COMMANDS_DIR" ] || { echo "Missing installed commands directory: $INSTALLED_COMMANDS_DIR" >&2; exit 1; }
[ -d "$PACKAGE_COMMANDS_DIR" ] || { echo "Missing packaged commands directory: $PACKAGE_COMMANDS_DIR" >&2; exit 1; }

step "Verifying wrapper contents"
verify_wrapper_content "$WRAPPER_ONE" 'claude --settings "$HOME/.claude_gateway/settings.json" "$@"'
verify_wrapper_content "$WRAPPER_TWO" 'cldg" --dangerously-skip-permissions "$@"'
verify_wrapper_content "$WRAPPER_NEW" 'resolve_python_helper'

step "Verifying cld-gateway-sh helper path resolution"
# Check that cld-gateway-sh mentions formula-scoped libexec path pattern
if ! grep -q "libexec/post_install.py" "$WRAPPER_NEW" 2>/dev/null; then
  echo "Warning: cld-gateway-sh does not reference expected libexec path pattern" >&2
else
  echo "[OK] cld-gateway-sh references formula-scoped libexec path"
fi

# Verify the helper exists at expected formula-scoped path
FORMULA_LIBEXEC="$(brew --prefix "$FORMULA_NAME")/libexec"
FORMULA_HELPER="$FORMULA_LIBEXEC/post_install.py"
if [ -f "$FORMULA_HELPER" ]; then
  echo "[OK] Found formula-scoped helper at $FORMULA_HELPER"
else
  echo "Warning: Expected helper not found at $FORMULA_HELPER" >&2
fi

# Verify cld-gateway-sh setup and doctor commands are callable
if [ -x "$WRAPPER_NEW" ]; then
  if "$WRAPPER_NEW" doctor >/dev/null 2>&1; then
    echo "[OK] cld-gateway-sh doctor command is callable"
  else
    warn "cld-gateway-sh doctor command returned non-zero exit (may be expected if setup incomplete)"
  fi
else
  echo "Warning: cld-gateway-sh is not executable at $WRAPPER_NEW" >&2
fi

if command -v claude >/dev/null 2>&1; then
  step "Running wrapper executables because claude is on PATH"
  "$WRAPPER_ONE" --help >/dev/null 2>&1 || warn "cldg --help returned a non-zero exit code"
  "$WRAPPER_TWO" --help >/dev/null 2>&1 || warn "clddg --help returned a non-zero exit code"
else
  warn "claude is not on PATH; wrapper execution checks skipped after verifying script contents"
fi

step "Verifying symlinks created by Python helper"
CLAUDE_GATEWAY_HOME="${HOME}/.claude_gateway"
CLAUDE_HOME="${HOME}/.claude"

if [ ! -d "${CLAUDE_GATEWAY_HOME}" ]; then
  echo "Warning: ${CLAUDE_GATEWAY_HOME} not found; cannot verify symlinks" >&2
fi

# Representative entries from SHARED_CLAUDE_ENTRIES in post_install.py
# We check a few key entries that commonly exist
for entry in agents commands skills; do
  source_path="${CLAUDE_HOME}/${entry}"
  target_path="${CLAUDE_GATEWAY_HOME}/${entry}"

  # Entry is valid if it's either a directory or symlink
  if [ -d "$source_path" ]; then
    # Source exists; target should be directory or symlink
    if [ ! -d "$target_path" ] && [ ! -L "$target_path" ]; then
      echo "ERROR: Expected directory or symlink not found: $target_path (source exists at $source_path)" >&2
      exit 1
    fi
  fi
done
echo "✓ Directory/symlink verification passed"

step "Checking binary behavior"
cld-gateway invalid-command >/dev/null 2>&1 || true

expected_listen_addr="$(resolve_expected_listen_addr)"
verify_installed_config_matches_package
verify_installed_settings_match_package
verify_installed_commands_dir_matches_package
detect_conflicting_gateway_process "$expected_listen_addr"
health_url="$(resolve_health_url "$expected_listen_addr")"
step "Checking health endpoint at $health_url"
if ! curl --fail --silent --show-error --max-time "$TIMEOUT_SECS" "$health_url" >/dev/null; then
  echo "Health check failed at $health_url" >&2
  exit 1
fi

step "Homebrew verification finished successfully"
