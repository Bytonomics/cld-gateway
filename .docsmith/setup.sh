#!/usr/bin/env bash
# docsmith project bootstrap — installed by `docsmith scaffold`, run via
# `make setup` (or `bash .docsmith/setup.sh`).
#
# Idempotent + full: assumes a fresh machine where nothing is installed, yet
# every step is guarded so re-running never clobbers or duplicates anything.
# Provisions, in order: uv -> a doc-site venv with the mkdocs toolchain ->
# the pre-commit tool + git hooks.
#
# Assumptions: macOS/Linux with `bash`, `curl`, and network access.
set -euo pipefail

log() { printf '[docsmith setup] %s\n' "$*"; }

# Run from the project root (the directory containing .docsmith/).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SITE_VENV=".docsmith/site/.venv"
REQ_FILE=".docsmith/site-requirements.txt"

# --- 1. Ensure `uv` (installs only if missing) -------------------------------
if command -v uv >/dev/null 2>&1; then
	log "uv already installed ($(uv --version))"
else
	log "installing uv (astral.sh installer)..."
	curl -LsSf https://astral.sh/uv/install.sh | sh
fi
# Make a freshly-installed uv usable in this shell session.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
if ! command -v uv >/dev/null 2>&1; then
	log "ERROR: uv is not on PATH after install; open a new shell and re-run." >&2
	exit 1
fi

# --- 2. Doc-site venv + mkdocs toolchain (idempotent) ------------------------
if [ -f "$REQ_FILE" ]; then
	if [ ! -x "$SITE_VENV/bin/python" ]; then
		log "creating doc-site venv at $SITE_VENV ..."
		uv venv "$SITE_VENV"
	else
		log "doc-site venv already present at $SITE_VENV"
	fi
	log "installing/updating doc-site requirements from $REQ_FILE ..."
	uv pip install --python "$SITE_VENV/bin/python" -r "$REQ_FILE"
	if [ -x "$SITE_VENV/bin/mkdocs" ]; then
		log "mkdocs ready: $("$SITE_VENV/bin/mkdocs" --version 2>/dev/null || echo installed)"
	fi
else
	log "no $REQ_FILE found — skipping doc-site toolchain (site not enabled?)"
fi

# --- 3. pre-commit tool + git hooks (idempotent) -----------------------------
if command -v pre-commit >/dev/null 2>&1; then
	log "pre-commit already installed ($(pre-commit --version))"
else
	log "installing pre-commit (uv tool install)..."
	uv tool install pre-commit
	export PATH="$HOME/.local/bin:$PATH"
fi
if [ -f .pre-commit-config.yaml ] && command -v pre-commit >/dev/null 2>&1; then
	log "installing git pre-commit hook..."
	pre-commit install
else
	log "no .pre-commit-config.yaml (or pre-commit unavailable) — skipping hook install"
fi

log "done. Try: make validate  |  make docs-build  |  make docs-serve"
