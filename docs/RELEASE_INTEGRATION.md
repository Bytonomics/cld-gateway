---
type: explanation
title: "Rust-to-Go Release Pipeline Swap"
status: stable
tags:
  - release
  - go-port
  - ci
stale_after: 2027-05-01
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Rust-to-Go Release Pipeline Swap

| Section | What it covers |
|---------|----------------|
| [Outcome](#outcome) | The pipeline is fully migrated to Go; what changed |
| [Current pipeline](#current-pipeline) | Where each build/package/formula step lives today |
| [Formula smoke test](#formula-smoke-test) | What the Homebrew formula's `test do` block checks |

## Outcome

This plan proposed swapping the release pipeline's build step from `cargo`/Rust to `go build`,
while keeping the Python packager and Homebrew formula contract unchanged. The swap is complete:
`.github/workflows/release.yml` no longer installs a Rust toolchain or builds with `cargo` —
it uses the *actions/setup-go* GitHub Action, builds with `CGO_ENABLED=0 go build`, and the
tag-check job compares the release tag against a repo-root `VERSION` file (not `Cargo.toml`).

## Current pipeline

- `.github/workflows/release.yml` — `build` job compiles per `GOOS`/`GOARCH` matrix entry with
  `go build`, writing to a CI-only *dist-bin/* output directory (not present in the repo tree);
  the `tag-check` job diffs the release tag against `VERSION`.
- `scripts/release/cld_gateway_package/version.py` — `read_workspace_version()` reads `VERSION`
  directly (no `Cargo.toml` parsing).
- `scripts/release/cld_gateway_package/cli.py` — takes `--entrypoint-bin` as a prebuilt binary
  path; packaging copies that binary plus static assets (`config.yml`, `settings.json`,
  `scripts/release/cld_gateway_package/homebrew/post_install.py`, the
  `scripts/release/cld_gateway_package/commands/` tree, the `bin/*` wrapper scripts) into a
  canonical directory and archives it (`scripts/release/cld_gateway_package/layout.py`,
  `scripts/release/cld_gateway_package/archive.py`).
- `scripts/release/cld_gateway_package/targets.py` — `TARGET_SPECS` keys stay the original Rust
  target-triple strings (`aarch64-apple-darwin`, etc.) as the external archive/formula
  identifier, now mapped to Go `GOOS`/`GOARCH` pairs rather than Rust targets.
- `homebrew-tap/Formula/cld-gateway.rb` — unchanged structurally: `install` installs the same
  file set, `service do` runs `cld-gateway serve` with `GATEWAY_CONFIG_PATH` set. The `url`/
  `sha256` pairs are re-rendered per release by the tap's own publish workflow, triggered by the
  `repository-dispatch` step at the end of `.github/workflows/release.yml`.

## Formula smoke test

The formula's `test do` block checks that an unrecognized subcommand exits non-zero with the
literal substring `unknown command` in its combined output — `cmd/cld-gateway/main.go`'s argv
handling reproduces this. Beyond that baseline check, a real startup check (bind an ephemeral
port, `GET /health`, expect `200`, clean shutdown) and a `GATEWAY_CONFIG_PATH` resolution check
remain open ideas, not yet added to the formula; see `docs/GAPS.md` G11 for the related
`/health` config-state gap this would exercise.
