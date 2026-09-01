# Releasing

## The packager stays, the build step changes

The release pipeline's Python packager (`scripts/release/`) is retained
as-is — its job (assemble a canonical package directory: binary, config,
settings, commands, post-install helper; produce a checksummed archive per
platform; validate the result) doesn't change just because the binary
inside it is now built by `go build` instead of `cargo build`.

What changes is narrow and mechanical:

- The build step becomes `go build` with `CGO_ENABLED=0`, producing a
  static binary — this is what keeps the pure-Go library picks in
  [ADR-0009](adr/ADR-0009.md) (`glebarez/sqlite`, `coder/websocket`)
  load-bearing: a cgo dependency anywhere in the build graph would break
  static linking and reintroduce a runtime library dependency the
  packaged binary doesn't currently have.
- Target identifiers remap from Rust target triples
  (`aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, ...) to Go's
  `GOOS`/`GOARCH` pairs (`darwin/arm64`, `linux/amd64`, ...) — the archive
  naming and the Homebrew formula's per-platform URLs need to agree on
  whichever identifier scheme is chosen, in both places at once.
- Linux builds drop the Zig/`cargo-zigbuild` musl cross-compilation step
  entirely — Go cross-compiles natively via `GOOS`/`GOARCH` with no
  external toolchain, which removes an entire CI step rather than
  replacing it with a Go equivalent.

The full concrete checklist of exactly what to change, where, is in
`golang_port/docs/RELEASE_INTEGRATION.md` — that document is scoped as a
review-first plan, not something to execute directly against the release
pipeline without the pipeline owner's sign-off.

## Formula contract

The Homebrew formula's `install` block, its declared `service` invocation
(`cld-gateway serve` with `GATEWAY_CONFIG_PATH` set), and its `caveats`
text are all unaffected by the build-step change: it still installs
`bin/cld-gateway` (now a Go binary), `cld-gateway-sh`, `cldg`, `clddg`,
`config.yml`, `settings.json`, and the packaged `commands/` — the shape of
what gets installed doesn't change, only what produced the binary at the
center of it.

## Rollback story

Per [ADR-0012](adr/ADR-0012.md), Rust is retained until the Go port
reaches parity and has run as the daily driver. During that period, a
release tag can point at either implementation's build output — reverting
a bad Go release means reverting to the previous release tag, the same
rollback mechanism that already exists today, not a special Go-specific
procedure.

## See also

- [ADR-0012](adr/ADR-0012.md) — why Rust and Go coexist through the
  cutover instead of being deleted piecemeal.
- `golang_port/docs/RELEASE_INTEGRATION.md` — the concrete build-step
  migration checklist.
