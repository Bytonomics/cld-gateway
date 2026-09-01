# RELEASE_INTEGRATION.md

Planning document only. Nothing under `scripts/release/`, `.github/`, or
`homebrew-tap/` is modified by this task — this is a checklist for the
release-pipeline owner to review and execute deliberately, not a diff to
apply now. Every file/line reference below was read directly, not
assumed.

## What's already true (verified) and doesn't need to change

The packaging step is already decoupled from the build step:
`scripts/release/cld_gateway_package/cli.py` requires `--entrypoint-bin`
as a prebuilt binary path and never invokes `cargo` itself — packaging
just copies that binary plus static assets (`config.yml`, `settings.json`,
`homebrew/post_install.py`, the `commands/` tree, the `bin/*` wrapper
scripts) into a canonical directory and archives it
(`scripts/release/cld_gateway_package/layout.py`,
`scripts/release/cld_gateway_package/archive.py`). This means the Go
migration's build-step swap is narrower than it might look: the packager
itself needs only two concrete changes (below), not a rewrite.

## Concrete changes needed

### 1. `.github/workflows/release.yml` — the `build` job

- Remove the **Install Rust toolchain** step (`dtolnay/rust-toolchain`)
  and both musl-only steps (**Install Zig**, **Install cargo-zigbuild**) —
  none are needed once `go build` cross-compiles natively.
- Add a Go toolchain step (`actions/setup-go`), pinned to the Go version
  in `golang_port/go.mod`.
- Replace the two **Build binary (macOS)** / **Build binary (Linux musl)**
  steps with one Go build step per matrix entry:
  ```sh
  CGO_ENABLED=0 GOOS=<goos> GOARCH=<goarch> \
    go build -C golang_port -o ../dist-bin/cld-gateway ./cmd/cld-gateway
  ```
  `<goos>`/`<goarch>` come from the target remap in the next item.
- Update the **Package archive** step's `--entrypoint-bin` path from
  `target/${{ matrix.target }}/release/cld-gateway` to wherever the Go
  build step above wrote its output (e.g. `dist-bin/cld-gateway`).
- Optional simplification worth flagging to the owner, not required:
  since Go cross-compiles from a single host, the 4-way `runs-on` matrix
  (`macos-latest` × 2, `ubuntu-latest` × 2) could collapse to a single
  `ubuntu-latest` job building all four `GOOS`/`GOARCH` pairs — cuts
  macOS runner minutes. Not required for correctness; a matrix that keeps
  4 parallel jobs is also fine and closer to today's shape.

### 2. `.github/workflows/release.yml` — the `tag-check` job

- Replace the version-match check
  (`grep -m1 '^version' Cargo.toml`) with whatever the Go module's single
  source-of-truth version becomes. **Open decision, not resolved here:**
  Go modules don't carry an inline semantic version the way
  `[workspace.package].version` does in `Cargo.toml`. Options: a plain
  `golang_port/VERSION` file, or a `const Version = "..."` in a Go source
  file read by both the CLI (`--version` output) and this workflow step
  (via a one-line `grep`/`sed`, same pattern as today). Whichever is
  chosen must be the *one* place both the release tag check and
  `scripts/release/cld_gateway_package/version.py` read from — see next
  item.

### 3. `scripts/release/cld_gateway_package/version.py`

`read_workspace_version()` currently parses `Cargo.toml`'s
`[workspace.package]` block with a regex. This is the one required code
change in the Python packager itself: point it at whatever version
source is chosen in item 2 above, keeping the same return contract
(`read_workspace_version() -> str`, raises `RuntimeError` if not found)
so `cli.py`'s call site (`version = read_workspace_version()`) doesn't
need to change.

### 4. `scripts/release/cld_gateway_package/targets.py` — target remap

`TARGET_SPECS` and `HOST_RELEASE_TARGETS` currently key on Rust target
triples (`aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, ...) and
`TargetSpec` carries only `target` and `is_linux`. Recommended change:
add `goos: str` and `goarch: str` fields to `TargetSpec`, populated per
entry:

| Existing key (kept as archive/formula identifier) | `goos` | `goarch` |
|---|---|---|
| `aarch64-apple-darwin` | `darwin` | `arm64` |
| `x86_64-apple-darwin` | `darwin` | `amd64` |
| `aarch64-unknown-linux-musl` | `linux` | `arm64` |
| `x86_64-unknown-linux-musl` | `linux` | `amd64` |

**Recommendation: keep the existing triple strings as the external
identifier** (archive filenames, `--target` flag values, and the
Homebrew formula's URL construction all key on these today) even though
the `-unknown-linux-musl` suffix becomes technically inaccurate for a Go
build — a `CGO_ENABLED=0` Go binary isn't musl- or glibc-specific, it's
just static. Renaming the identifier is a bigger, separate change that
also touches `homebrew-tap/Formula/cld-gateway.rb`'s four `url` lines;
flag it to the owner as an optional follow-up, not part of this swap.
`_normalize_machine()`'s host-detection logic (mapping `platform.machine()`
output to `x86_64`/`aarch64`) is unaffected either way.

### 5. `scripts/release/cld_gateway_package/layout.py` and `cli.py` — cosmetic only

`--cargo-profile` (default `"release"`) is recorded verbatim into
`cld-gateway-package.json` as `"cargoProfile"`. Nothing currently
cross-checks this value against a real Cargo profile — `validate_package_dir`
only checks it's a non-empty string. No functional change is required,
but renaming the flag/field to something Go-appropriate (e.g.
`--build-mode` / `"buildMode"`) is worth doing for clarity as part of the
same change that touches `cli.py`'s help text. If renamed, grep both
`homebrew-tap/` and any other consumer of `cld-gateway-package.json` for
the `cargoProfile` key first — none was found in the files read for this
plan, but that should be re-verified at execution time, not assumed from
this document.

### 6. `homebrew-tap/Formula/cld-gateway.rb`

No structural change expected: `install` still installs the same file
set, `service do` still runs `cld-gateway serve` with
`GATEWAY_CONFIG_PATH` set, `caveats` text is unaffected. The four `url` /
`sha256` pairs are already re-rendered per release by the tap's own
publish workflow (triggered by the `repository-dispatch` at the end of
`.github/workflows/release.yml`) — confirm that render step
(`homebrew-tap/.github/scripts/render-formula.py`, not read as part of
this plan) doesn't hardcode anything Rust-specific before relying on it
unchanged.

## What the formula smoke test needs to check

The current `test do` block only checks one thing:

```ruby
output = shell_output("#{bin}/cld-gateway invalid-command 2>&1", 1)
assert_match "unknown command", output
```

For this to keep passing unmodified, **the Go CLI's argv handling
(`cmd/cld-gateway/main.go`) must reproduce this exact behavior**: an
unrecognized subcommand exits non-zero and its combined stdout+stderr
output contains the literal substring `unknown command`. This is a
concrete, testable requirement on the Go CLI implementation, not just a
release-pipeline concern — flag it to whoever implements `main.go`.

Recommended additions to the smoke test, beyond keeping the existing
check green (owner's call whether to add these now or later):

- **Static-link confirmation.** Since the entire point of
  `CGO_ENABLED=0` is a binary with no dynamic C library dependency, a
  smoke-test step that runs `file bin/cld-gateway` (or `ldd`, expecting
  "not a dynamic executable" on Linux) would catch a regression where the
  build accidentally picked up cgo (e.g. via a transitive dependency that
  isn't actually pure Go, contradicting the choices in
  `golang_port/docs/site/contributing/adr/ADR-0009.md`).
- **A real startup check.** `cld-gateway serve` bound to an ephemeral
  port, followed by a `GET /health` request expecting `200`, then a clean
  shutdown — verifies the binary doesn't just parse argv correctly but
  actually starts the HTTP service. Today's Rust-era smoke test doesn't
  do this either; it's a gap in both, worth closing during the swap
  rather than carrying forward.
- **Config path resolution.** Confirm `GATEWAY_CONFIG_PATH` (as set by
  the formula's own `service do` block) is honored — run with a temp
  config file and a value that's observable back (e.g. via `/health`
  reporting config-load state, once that's implemented per `GAPS.md`
  G11) to catch a regression in path resolution specifically in the
  packaged/Homebrew run mode.

## Explicitly out of scope for this document

- Actually editing `scripts/release/`, `.github/workflows/release.yml`,
  or `homebrew-tap/` — the owner reviews this checklist first.
- Deciding the Go version-source mechanism (item 2/3 above) — flagged as
  an open decision, not resolved here.
- The identifier-rename option in item 4 — flagged as optional follow-up,
  not part of the minimal swap.
