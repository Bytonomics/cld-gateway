# Architecture

This is a distillation of `golang_port/docs/ARCHITECTURE_v2.md` for people
reading the contributor docs first. When the two disagree, ARCHITECTURE_v2.md
is authoritative — this page tracks it, not the other way around.

## Scope: 1-to-n

Claude Code is the only inbound harness this gateway speaks — its request
normalization (command envelopes, slash-command promotion, directive
injection) is Claude-Code-specific by design, not a generic client
abstraction. The extension axis is entirely on the outbound side: many
backend providers can exist, exactly one is active at a time. There is no
harness port and no neutral intermediate representation to keep generic
across multiple inbound protocols — that complexity was deliberately not
built because nothing currently needs it.

## Layout

Single Go module (`github.com/Bytonomics/cld-gateway`), no workspace,
layered `smritea-cloud`-style:

```
cmd/cld-gateway/main.go     thin: argv (serve | login [vendor]), calls app
app/                        Providers struct (manual constructor DI),
                            initialize.go, router.go, routes_*.go
core/domain/                services/   use-case interfaces + DTOs
                            port/       backend/, auth/, state/ adapters
                            translator/ BackendTranslator iface + generic base
core/impl/                  services/   orchestrators
                            port/backend/codex/
                            port/auth/
                            translator/codex/
handlers/                   thin Echo handlers, constructor-injected
middleware/                 request-id, recovery, unary capture
observability/              exchange logs, redaction
```

The dependency rule is one-directional: `core/domain` defines interfaces
and DTOs with zero knowledge of any concrete backend, storage engine, or
HTTP framework; `core/impl` and the leaf `port/*` packages implement those
interfaces; `app` wires concrete implementations into the domain
interfaces via a manual constructor (`Providers`), not a DI framework or
service locator. Every implementation carries a compile-time interface
assertion (`var _ services.MessageService = (*MessageService)(nil)`) so a
signature drift fails the build, not a runtime lookup.

## Providers DI

`app.Initialize(cfg)` builds the dependency graph bottom-up — repos, then
adapters, then services — and returns a single `*Providers` struct that
`app.NewEcho` and the route mounters read from. There is exactly one
construction path; handlers and services never reach for a global.

## Request flow: `POST /v1/messages`

1. **Classify kind** — which conversation-kind bucket this turn falls
   into (main visible turn, subagent offshoot, permission classifier,
   etc.). Structural signals only; see [Design decisions](design-decisions.md)
   and [ADR-0010](adr/ADR-0010.md) for why the final signal set is still
   parked.
2. **Normalize Claude Code context** — resolve command envelopes, promote
   slash commands, inject directives. This is injection, not
   classification, and is exempt from the prompt-text rule for that
   reason (see [ADR-0006](adr/ADR-0006.md)).
3. **Context management** — apply configured pruning edits and hard
   limits to keep the request under the backend's effective limits.
4. **Resolve model** — apply the active backend's default/unsupported-model
   policy.
5. **Branch selection + transport identity** — resolve session, branch,
   conversation kind, model fingerprint, and reasoning effort into a
   single identity used for both state lookups and transport decisions.
6. **Translate** — hand the request to the active backend's
   `BackendTranslator` to produce a `BackendRequest`.
7. **Transport select** — choose WebSocket-delta transport if a live
   backend chain matches the stored checkpoint association for this
   identity, otherwise fall back to full SSE; acquire a turn lease before
   sending.
8. **Stream** — a single writer goroutine consumes the backend's event
   channel, writes each event to the client, and flushes immediately.
9. **Commit** — once the lease's gate allows it, conversation state
   commits (turn or offshoot checkpoints, the ledger, tool-call records),
   and the exchange is logged.

## SSE: single-writer model

Exactly one goroutine owns the HTTP response writer for a streaming
request. It reads off the backend event channel and writes+flushes SSE
bytes per event; response headers go out before the first write. No
middleware wraps the response writer on streaming routes — this is a
deliberate constraint (see [ADR-0004](adr/ADR-0004.md)) because a wrapped
writer that doesn't forward `http.Flusher` silently turns a streaming
response into a buffered one, which is exactly the bug class this design
rules out by construction.

## Logging: Option C (post-stream)

Exchange logging for streaming routes happens after the stream closes, not
per-event: the writer goroutine accumulates the events it sent and hands
them to the logger once the channel closes. Unary (non-streaming) routes
keep a normal request/response capture middleware, since there's no
streaming-writer conflict to avoid there. See [ADR-0004](adr/ADR-0004.md).

## Concurrency: the lease machine

Ported verbatim from the Rust implementation because its state machine is
what prevents an aborted client from silently committing a turn the user
never actually saw. A per-session mutex registry serializes access per
session, and a lease state machine tracks each in-flight turn:

```
in_flight → completed_committed
          → client_aborted_before_first_event
          → client_aborted_after_visible_output
          → backend_failed_before_commit
          → commit_suppressed_after_abort
```

Only `in_flight.AllowsCommit() == true`. Every other terminal state
refuses to commit. See [ADR-0008](adr/ADR-0008.md) — this is explicitly
not open for redesign as part of the port.

## Error model

`AppError` carries a `Code`, `Message`, `HTTPStatus`, and wrapped `Cause`,
and serializes through a central Echo `HTTPErrorHandler` into the
Anthropic error shape:

```json
{"type":"error","error":{"type":"<code>","message":"..."}}
```

Validation errors from request binding map to `invalid_request_error`
with the offending field path folded into the message. See
[ADR-0005](adr/ADR-0005.md).

## Config

Loaded via viper from `~/.gateway/config-dev.yml` (developer) or
`~/.gateway/config.yml` (packaged), overridable via `GATEWAY_CONFIG_PATH`
/ `GATEWAY_HOME`. `providers` is a map keyed by backend name with exactly
one entry carrying `active: true` — this is a deliberate restructuring
from the Rust config shape, not a straight port; see the user-facing
[Configuration reference](../docs/configuration/index.md) for the
schema itself.

## Where to go next

- [Design decisions](design-decisions.md) — why the shape above, not
  something else.
- [ADR index](adr/index.md) — the individual decision records.
- [Extending: backends](extending/backends.md) — how to add a second
  backend without touching core.
- [Testing](testing.md) — how the Rust test suite maps onto this layout.
