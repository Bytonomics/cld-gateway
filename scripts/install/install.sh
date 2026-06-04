#!/bin/sh

set -eu

RELEASE="${CLD_GATEWAY_RELEASE:-latest}"
NON_INTERACTIVE="${CLD_GATEWAY_NON_INTERACTIVE:-false}"

BIN_DIR="${CLD_GATEWAY_INSTALL_DIR:-$HOME/.local/bin}"
BIN_PATH="$BIN_DIR/cld-gateway"

path_action="already"
path_profile=""
tmp_dir=""

step() {
  printf '==> %s\n' "$1"
}

warn() {
  printf 'WARNING: %s\n' "$1" >&2
}

normalize_version() {
  case "$1" in
    "" | latest)
      printf 'latest\n'
      ;;
    cld-gateway-v*)
      printf '%s\n' "${1#cld-gateway-v}"
      ;;
    v*)
      printf '%s\n' "${1#v}"
      ;;
    *)
      printf '%s\n' "$1"
      ;;
  esac
}

validate_version() {
  version="$1"

  if [ "$version" = "latest" ]; then
    return
  fi

  if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta)(\.[0-9]+)?)?$'; then
    echo "Invalid cld-gateway release version: $version. Expected latest or x.y.z[-alpha[.N]|-beta[.N]]." >&2
    exit 1
  fi
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --release)
        if [ "$#" -lt 2 ]; then
          echo "--release requires a value." >&2
          exit 1
        fi
        RELEASE="$2"
        shift
        ;;
      --install-dir)
        if [ "$#" -lt 2 ]; then
          echo "--install-dir requires a value." >&2
          exit 1
        fi
        BIN_DIR="$2"
        BIN_PATH="$BIN_DIR/cld-gateway"
        shift
        ;;
      --help | -h)
        cat <<EOF
Usage: install.sh [--release VERSION] [--install-dir DIR]

Environment:
  CLD_GATEWAY_RELEASE          Version to install; overridden by --release.
  CLD_GATEWAY_INSTALL_DIR      Directory to install the binary; overridden by --install-dir.
  CLD_GATEWAY_NON_INTERACTIVE  Set to 1, true, or yes to skip prompts.

Default install directory: \$HOME/.local/bin
EOF
        exit 0
        ;;
      *)
        echo "Unknown argument: $1" >&2
        exit 1
        ;;
    esac
    shift
  done
}

download_file() {
  url="$1"
  output="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -q -O "$output" "$url"
    return
  fi

  echo "curl or wget is required to install cld-gateway." >&2
  exit 1
}

download_text() {
  url="$1"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -q -O - "$url"
    return
  fi

  echo "curl or wget is required to install cld-gateway." >&2
  exit 1
}

release_url_for_asset() {
  asset="$1"
  resolved_version="$2"

  printf 'https://github.com/bytonomics/gateway/releases/download/cld-gateway-v%s/%s\n' "$resolved_version" "$asset"
}

package_archive_digest() {
  asset="$1"
  manifest_path="$2"

  digest="$(awk -v asset="$asset" '
    $2 == asset && $1 ~ /^[0-9a-fA-F]{64}$/ {
      print tolower($1)
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$manifest_path" 2>/dev/null || true)"

  if [ -z "$digest" ]; then
    echo "Could not find SHA-256 digest for $asset in cld-gateway-package_SHA256SUMS." >&2
    exit 1
  fi

  printf '%s\n' "$digest"
}

file_sha256() {
  path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
    return
  fi

  if command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$path" | sed 's/^.*= //'
    return
  fi

  echo "sha256sum, shasum, or openssl is required to verify the cld-gateway download." >&2
  exit 1
}

verify_archive_digest() {
  archive_path="$1"
  expected_digest="$2"
  actual_digest="$(file_sha256 "$archive_path")"

  if [ "$actual_digest" != "$expected_digest" ]; then
    echo "Downloaded cld-gateway archive checksum did not match expected digest." >&2
    echo "expected: $expected_digest" >&2
    echo "actual:   $actual_digest" >&2
    exit 1
  fi
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "$1 is required to install cld-gateway." >&2
    exit 1
  fi
}

resolve_version() {
  normalized_version="$(normalize_version "$RELEASE")"
  validate_version "$normalized_version"

  if [ "$normalized_version" != "latest" ]; then
    printf '%s\n' "$normalized_version"
    return
  fi

  release_json="$(download_text "https://api.github.com/repos/bytonomics/gateway/releases/latest")"
  resolved="$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name":[[:space:]]*"cld-gateway-v\([^"]*\)".*/\1/p' | head -n 1)"

  if [ -z "$resolved" ]; then
    echo "Failed to resolve the latest cld-gateway release version." >&2
    exit 1
  fi

  validate_version "$resolved"
  printf '%s\n' "$resolved"
}

pick_profile() {
  case "$os:${SHELL:-}" in
    darwin:*/zsh)
      printf '%s\n' "$HOME/.zprofile"
      ;;
    darwin:*/bash)
      printf '%s\n' "$HOME/.bash_profile"
      ;;
    linux:*/zsh)
      printf '%s\n' "$HOME/.zshrc"
      ;;
    linux:*/bash)
      printf '%s\n' "$HOME/.bashrc"
      ;;
    *)
      printf '%s\n' "$HOME/.profile"
      ;;
  esac
}

add_to_path() {
  path_action="already"
  path_profile=""

  case ":$PATH:" in
    *":$BIN_DIR:"*)
      return
      ;;
  esac

  profile="$(pick_profile)"
  path_profile="$profile"
  begin_marker="# >>> cld-gateway installer >>>"
  end_marker="# <<< cld-gateway installer <<<"
  path_line="export PATH=\"$BIN_DIR:\$PATH\""

  if [ -f "$profile" ] && grep -F "$begin_marker" "$profile" >/dev/null 2>&1; then
    if grep -F "$path_line" "$profile" >/dev/null 2>&1; then
      path_action="configured"
      return
    fi

    if grep -F "$end_marker" "$profile" >/dev/null 2>&1; then
      rewrite_path_block "$profile" "$begin_marker" "$end_marker" "$path_line"
      path_action="updated"
      return
    fi
  fi

  append_path_block "$profile" "$begin_marker" "$end_marker" "$path_line"
  path_action="added"
}

append_path_block() {
  profile="$1"
  begin_marker="$2"
  end_marker="$3"
  path_line="$4"

  {
    printf '\n%s\n' "$begin_marker"
    printf '%s\n' "$path_line"
    printf '%s\n' "$end_marker"
  } >>"$profile"
}

rewrite_path_block() {
  profile="$1"
  begin_marker="$2"
  end_marker="$3"
  path_line="$4"
  tmp_profile="$tmp_dir/profile.$$.tmp"

  awk -v begin="$begin_marker" -v end="$end_marker" -v line="$path_line" '
    BEGIN {
      in_block = 0
      replaced = 0
    }
    $0 == begin {
      if (!replaced) {
        print begin
        print line
        print end
        replaced = 1
      }
      in_block = 1
      next
    }
    in_block {
      if ($0 == end) {
        in_block = 0
      }
      next
    }
    {
      print
    }
    END {
      if (in_block != 0) {
        exit 1
      }
    }
  ' "$profile" >"$tmp_profile"
  mv "$tmp_profile" "$profile"
}

print_launch_instructions() {
  case "$path_action" in
    added)
      step "Current terminal: export PATH=\"$BIN_DIR:\$PATH\" && cld-gateway"
      step "Future terminals: open a new terminal and run: cld-gateway"
      step "PATH was added to $path_profile"
      ;;
    updated)
      step "Current terminal: export PATH=\"$BIN_DIR:\$PATH\" && cld-gateway"
      step "Future terminals: open a new terminal and run: cld-gateway"
      step "PATH was updated in $path_profile"
      ;;
    configured)
      step "Current terminal: export PATH=\"$BIN_DIR:\$PATH\" && cld-gateway"
      step "Future terminals: open a new terminal and run: cld-gateway"
      step "PATH is already configured in $path_profile"
      ;;
    *)
      step "Current terminal: cld-gateway"
      step "Future terminals: open a new terminal and run: cld-gateway"
      ;;
  esac
}

# ── main ────────────────────────────────────────────────────────────────────

parse_args "$@"

require_command mktemp
require_command tar

case "$(uname -s)" in
  Darwin)
    os="darwin"
    ;;
  Linux)
    os="linux"
    ;;
  *)
    echo "install.sh supports macOS and Linux. Use install.ps1 on Windows." >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64 | amd64)
    arch="x86_64"
    ;;
  arm64 | aarch64)
    arch="aarch64"
    ;;
  *)
    echo "Unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

# On Apple Silicon running under Rosetta 2, prefer the native arm64 build.
if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
  if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
    arch="aarch64"
  fi
fi

if [ "$os" = "darwin" ]; then
  if [ "$arch" = "aarch64" ]; then
    target="aarch64-apple-darwin"
    platform_label="macOS (Apple Silicon)"
  else
    target="x86_64-apple-darwin"
    platform_label="macOS (Intel)"
  fi
else
  if [ "$arch" = "aarch64" ]; then
    target="aarch64-unknown-linux-musl"
    platform_label="Linux (ARM64)"
  else
    target="x86_64-unknown-linux-musl"
    platform_label="Linux (x64)"
  fi
fi

resolved_version="$(resolve_version)"
package_asset="cld-gateway-package-$target.tar.gz"
checksum_asset="cld-gateway-package_SHA256SUMS"
download_url="$(release_url_for_asset "$package_asset" "$resolved_version")"
checksum_url="$(release_url_for_asset "$checksum_asset" "$resolved_version")"

step "Installing cld-gateway"
step "Detected platform: $platform_label"
step "Resolved version: $resolved_version"

tmp_dir="$(mktemp -d)"
cleanup() {
  if [ -n "$tmp_dir" ]; then
    rm -rf "$tmp_dir"
  fi
}
trap cleanup EXIT INT TERM

archive_path="$tmp_dir/$package_asset"
checksum_path="$tmp_dir/$checksum_asset"

step "Downloading cld-gateway"
download_file "$checksum_url" "$checksum_path"
expected_digest="$(package_archive_digest "$package_asset" "$checksum_path")"
download_file "$download_url" "$archive_path"
verify_archive_digest "$archive_path" "$expected_digest"

step "Installing cld-gateway to $BIN_DIR"
mkdir -p "$BIN_DIR"
tar -xzf "$archive_path" -C "$tmp_dir" bin/cld-gateway
mv "$tmp_dir/bin/cld-gateway" "$BIN_PATH"
chmod 0755 "$BIN_PATH"

add_to_path

case "$path_action" in
  added | updated | configured)
    print_launch_instructions
    ;;
  *)
    step "$BIN_DIR is already on PATH"
    print_launch_instructions
    ;;
esac

printf 'cld-gateway %s installed successfully to %s.\n' "$resolved_version" "$BIN_PATH"
