# Design decisions

Distilled record of the design interview (2026-08-22) that produced
`ARCHITECTURE_v2.md`. Each item below has a corresponding ADR with fuller
rationale; this page is the fast-scan version.

## Scope stayed 1-to-n, not n-to-n

The temptation in porting a translation layer is to generalize both axes —
support multiple inbound harnesses *and* multiple outbound backends behind
a neutral intermediate representation. That was explicitly rejected. Only
Claude Code is an inbound harness today, its request shape is baked into
normalization logic that would need real design work to genericize, and
there's no second inbound client waiting to justify that work. The
extension axis that's real is outbound backends, so that's the only one
the port pays complexity for. See [ADR-0001](adr/ADR-0001.md).

## Port shapes, not files

The Rust crate boundaries (`gateway-core`, `gateway-auth-codex`,
`gateway-backend-codex`, `gateway-http-anthropic`, `gateway-state`,
`gateway-net`, `gateway-observability`) don't map onto Go packages
one-to-one — Go's `internal/` visibility and single-module conventions
work differently from a Cargo workspace. Instead, the port re-derives
package boundaries from the *interfaces* the Rust crates expose (ports,
services, adapters) using a layout already proven in another codebase
(`smritea-cloud`) rather than inventing a new one. See [ADR-0002](adr/ADR-0002.md).

## Translators extend by composition, not inheritance

`BackendTranslator` has one generic base (`GenericBackendTranslator`)
carrying the request/response shaping logic every backend needs, and each
concrete backend's translator embeds a pointer to it and overrides only
what's backend-specific. Go has no inheritance, so "override" here means
composition plus the embedding struct satisfying the same interface as
its embedded field would if promoted unchanged — a well-understood Go
idiom, not a workaround. See [ADR-0006](adr/ADR-0006.md).

## Conversation state stays core, not a port

Unlike backends, conversation state is *not* behind a pluggable interface
boundary meant for swapping implementations. It's core domain logic with
one filesystem-backed implementation, because the on-disk format
(branches, sparse checkpoints, JSONL ledger, retention, corruption policy)
is itself a load-bearing contract other tooling and the daemon's own crash
recovery depend on — pluggability here would just be indirection with one
implementation behind it. See [ADR-0007](adr/ADR-0007.md).

## The lease machine ports verbatim

The main-turn lease state machine is the one piece of concurrency logic in
this port that got zero redesign license. Its whole job is preventing an
aborted client from committing a turn nobody saw finish, and that's a
correctness property, not a performance one — there was no appetite for
"improving" it mid-port without a much closer audit than a language port
budget allows. See [ADR-0008](adr/ADR-0008.md).

## Library selections favored boring and pure-Go

Every library pick prioritized minimal C dependencies (so `CGO_ENABLED=0`
static binaries stay possible), an actively maintained Go idiom over a
port of the Rust library's exact API, and precedent from `smritea-cloud`
where one already existed. The one deliberate *removal* was OS keyring
support — auth is file-based on both sides, and the Rust keyring
integration was verified to be dead weight, not a feature actually
exercised. See [ADR-0009](adr/ADR-0009.md).

## Gap fixes were triaged, not blanket-applied

The Rust implementation review (`GAPS.md`) turned up thirteen findings.
Rather than porting all of them as "improvements" or none of them as "not
our problem," each got an explicit owner decision — some approved for the
Go port (graceful shutdown, backend timeouts, rotation, logging
failure-visibility), some left open pending further design work (lease
persistence, chain-checkpoint persistence, second-instance protection),
one scoped for later (metrics, opt-in only). See [ADR-0010](adr/ADR-0010.md).

## The exchange log became human-first

The Rust exchange log is JSONL. The Go port's primary exchange log is a
formatted-text format instead — `key: value` lines with a dashed
separator — because the dominant use case observed in practice is a human
reading recent entries directly, not a downstream JSON consumer. The
JSONL transport-decisions log stays JSONL because that one *is* consumed
by tooling. See [ADR-0011](adr/ADR-0011.md).

## Rust stays until parity, then a single deletion

The Go port and the Rust implementation coexist during development. Rust
is not deleted piecemeal as Go packages land — it stays as the release
daily-driver until the Go port reaches behavioral parity and has run as
the daily driver itself, at which point Rust is removed in one commit,
making rollback a single tag revert rather than a reconstruction. See
[ADR-0012](adr/ADR-0012.md).
