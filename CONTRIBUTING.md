# Contributing to cld-gateway

Thank you for your interest in contributing to cld-gateway! This guide covers the tools and workflows you'll need for development.

---

## Prerequisites

### Required

- **Go** 1.24+: Install from [go.dev/dl](https://go.dev/dl/)
- **uv**: Fast Python package manager and dependency resolver
  - Install: `curl -LsSf https://astral.sh/uv/install.sh | sh`
  - Or use Homebrew: `brew install uv`
  - Verify: `uv --version`

### Optional

- **Make**: Build automation (most development commands use `make` targets)
- **golangci-lint**: Required for `make lint` / `make fmt-fix`
  - Install: https://golangci-lint.run/welcome/install/

### uv Setup for Development

#### Quick start

```sh
# Run release-tooling commands in the scripts/release project environment
uv run --project scripts/release pytest scripts/release/test/

# Run normal Go/Make commands directly
make check
make fmt-fix
make lint
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
make check          # Full checks (fmt + lint + tests + release-tooling tests)
make fmt-check      # Check formatting (no changes)
make fmt-fix        # Auto-fix formatting
make lint           # Lint with golangci-lint
```

### 2. Running tests

```sh
# All tests
make test

# With mock-backend-gated integration tests (some tests early-return unless set)
RUN_MOCK_BACKEND=1 make verify-test

# Single package
go test ./core/domain/translator/...

# Single test by name (substring match)
go test ./core/domain/translator/... -run TestSSEBridge
```

### 3. Building

```sh
# Compile-only check (no binary written)
make build-check

# Build the gateway binary
make build

# Run locally with checks
make check && ./bin/cld-gateway
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
cmd/cld-gateway/                  # Main daemon/CLI entrypoint
app/                               # Providers struct, manual constructor DI, router, routes
core/domain/                       # Use-case interfaces, DTOs, and ports (backend/auth/state/translator)
core/impl/                         # Concrete adapters and orchestrators (services, backend clients, translators)
handlers/                          # Thin Echo HTTP handlers
middleware/                        # Request ID, recovery, unary exchange capture
observability/                     # Exchange logs, redaction, transport diagnostics
netpolicy/                         # Outbound network allow/deny policy
config/                            # Viper-based config loading and model resolution
tui/                                # bubbletea login vendor picker

old_rust/                          # Frozen former Rust implementation, kept for reference only

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

docs/                              # Documentation (architecture, decisions, docs site content)

VERSION                           # Canonical release version (plain text, single line)
Makefile                          # Development targets
RELEASE.md                        # Release playbook for maintainers
README.md                         # User guide
```

---

## Debugging Tips

### Exchange logs

When debugging requests/responses, check the exchange log:

```sh
tail -f ~/.gateway/logs/http-exchange.log
```

Every proxied request has an `x-proxy-request-id` header you can use to correlate entries.

### Build failures

Use the documented Make targets directly:

```sh
make check
make test
RUN_MOCK_BACKEND=1 make verify-test
```

### Runtime issues

Set environment variables to override default paths and ports:

```sh
# Use custom port and data directory
CLD_GATEWAY_LISTEN_ADDR=127.0.0.1:8081 \
GATEWAY_HOME=~/.gateway-dev \
./bin/cld-gateway serve
```

See `README.md` for a full list of environment variables.

---

## Codebase Philosophy

- **Small packages with explicit boundaries**: Each package documents what it can and cannot import
- **Type safety**: `core.Secret` wraps sensitive values to prevent accidental logging
- **Observability first**: All exchanges are logged with correlation IDs
- **Intentional no-ops**: Unsupported Anthropic fields are parsed but logged, see `UNSUPPORTED.md`

---

## Questions?

- See `CLAUDE.md` for automated agent-based development guidance
- Check `RELEASE.md` for release and version management details
- See `README.md` for runtime configuration and API endpoints
