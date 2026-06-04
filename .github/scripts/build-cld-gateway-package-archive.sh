#!/bin/sh
# Build a cld-gateway package archive using the Python package builder.
# Usage: build-cld-gateway-package-archive.sh --target <target> --entrypoint-bin <path> --archive-output <path>

set -eu

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

exec uv run --project "${REPO_ROOT}/scripts/release" python "${REPO_ROOT}/scripts/release/build_cld_gateway_package.py" "$@"
