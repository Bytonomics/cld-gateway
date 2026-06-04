# Contributing to cld-gateway

Thank you for your interest in contributing to cld-gateway! This guide covers the tools and workflows you'll need for development.

---

## Prerequisites

### Required

- **Rust** 1.70+: Install from [rustup.rs](https://rustup.rs/)
- **uv**: Fast Python package manager and dependency resolver
  - Install: `curl -LsSf https://astral.sh/uv/install.sh | sh`
  - Or use Homebrew: `brew install uv`
  - Verify: `uv --version`

### Optional

- **Make**: Build automation (most development commands use `make` targets)
- **Zig** 0.14.0+: Required only if building for Linux musl targets
  - Install: https://ziglang.org/download/
  - Or install automatically by running a musl target build

### uv Setup for Development

#### Quick start

```sh
# Run release-tooling commands in the scripts/release project environment
uv run --project scripts/release pytest scripts/release/test/

# Run normal Rust/Make commands directly
make check
make fmt-fix
make clippy
make test
```

#### Understanding uv in this project

- The Python project lives under `scripts/release/`
- `scripts/release/uv.lock`: pinned dependencies for release/package tooling tests
- `scripts/release/pyproject.toml`: Python dependencies for the package builder test/tooling environment
- `uv run --project scripts/release ...`: runs commands in the scoped release-tooling environment
- If you want to pre-create/update that environment explicitly, use `uv sync --project scripts/release`

#### How uv is used in CI

The release workflow (`.github/workflows/release.yml`) uses uv to provision Python dependencies for the package builder:

```bash
uv run --project scripts/release python scripts/release/build_cld_gateway_package.py ...
```

This ensures the package builder runs in an isolated, reproducible Python environment.

---

## Development Workflow

### 1. Format and lint checks

Before committing, ensure your code passes formatting and linting:

```sh
make check          # Full checks (fmt + clippy + tests + release-tooling tests)
make fmt-check      # Check formatting (no changes)
make fmt-fix        # Auto-fix formatting
make clippy         # Lint with clippy
```

### 2. Running tests

```sh
# All tests
make test

# With wiremock-gated integration tests (some tests early-return unless set)
RUN_WIREMOCK=1 make verify-test

# Single crate
cargo test -p gateway-http-anthropic

# Single test by name (substring match)
cargo test -p gateway-http-anthropic streaming_bridge_matches_text_only_fixture
```

### 3. Building

```sh
# Debug build
cargo build -p gatewayd --bin cld-gateway

# Release build
cargo build --release -p gatewayd --bin cld-gateway

# Run locally with checks
make check && cargo run -p gatewayd --bin cld-gateway
```

---

## Pre-commit Hooks

This project uses pre-commit hooks (configured in `.pre-commit-config.yaml`) to enforce code standards automatically before commits.

### What runs on commit

1. Basic hygiene (trailing whitespace, EOF fixer, YAML/TOML checks)
2. Full verification suite (`make check`)

### If a hook modifies files

If a pre-commit hook modifies files during a commit attempt:

1. The commit is **aborted** (hook failures prevent commit creation)
2. Review the hook-modified files
3. Re-stage them: `git add -A`
4. Run `git commit` again (use a **new** commit, not `--amend`)

**Important:** Never use `git commit --amend` after a hook failure — that would amend the previous commit instead of creating a new one.

---

## Repository Structure

```
crates/
  ├── gatewayd/                    # Main daemon binary
  ├── gateway-core/                # Shared types and config
  ├── gateway-auth-codex/          # OAuth credential management
  ├── gateway-backend-codex/       # Upstream HTTP client
  ├── gateway-http-anthropic/      # Anthropic-compatible HTTP surface
  ├── gateway-state/               # Local persistence (SQLite)
  └── gateway-observability/       # Request/response logging

scripts/
  ├── release/                     # Package builder and release tools
  │   ├── pyproject.toml          # Python dependencies
  │   └── test/                   # Release workflow tests
  └── install/                     # Installation scripts

.github/
  ├── workflows/
  │   └── release.yml             # GitHub Actions release workflow
  └── scripts/
      └── build-cld-gateway-package-archive.sh

docs/                              # Documentation

Makefile                          # Development targets
RELEASE.md                        # Release playbook for maintainers
README.md                         # User guide
```

---

## Debugging Tips

### Exchange logs

When debugging requests/responses, check the exchange log:

```sh
tail -f ~/.gateway/logs/http-exchange.jsonl
```

Every proxied request has an `x-proxy-request-id` header you can use to correlate entries.

### Build failures

Use the documented Make targets directly:

```sh
make check
make test
RUN_WIREMOCK=1 make verify-test
```

### Runtime issues

Set environment variables to override default paths and ports:

```sh
# Use custom port and data directory
CLD_GATEWAY_LISTEN_ADDR=127.0.0.1:8081 \
GATEWAY_HOME=~/.gateway-dev \
cargo run -p gatewayd --bin cld-gateway
```

See `README.md` for a full list of environment variables.

---

## Codebase Philosophy

- **Small crates with explicit boundaries**: Each crate documents what it can and cannot import
- **Type safety**: Leverage Rust's type system; `Secret<T>` wraps sensitive values
- **Observability first**: All exchanges are logged with correlation IDs
- **Intentional no-ops**: Unsupported Anthropic fields are parsed but logged, see `UNSUPPORTED.md`

---

## Questions?

- See `CLAUDE.md` for automated agent-based development guidance
- Check `RELEASE.md` for release and version management details
- See `README.md` for runtime configuration and API endpoints
