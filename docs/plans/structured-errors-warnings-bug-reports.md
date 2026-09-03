---
type: plan
title: "Plan: Structured Errors, Warnings, and Bug-Report Guidance"
status: draft
tags:
  - errors
  - warnings
  - observability
  - bug-reports
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# PLAN: Structured, self-diagnosing error responses + success-path warnings + bug-report guidance

| Section | What it covers |
|---------|----------------|
| [Problem Statement](#problem-statement) | Why current error/warning surfacing is inadequate |
| [Solution](#solution) | The classification function, branding, warnings, and logging fix |
| [User Stories](#user-stories) | The 21 stories this plan satisfies |
| [Implementation Decisions](#implementation-decisions) | The new classification function, its rules, and the Capture/stream_writer fixes |
| [Testing Decisions](#testing-decisions) | What each new/changed piece asserts, and how |
| [Success Criteria](#success-criteria) | The 6 conditions that mark this plan done |
| [Live Verification Scenarios](#live-verification-scenarios) | Real-`cldc`-session scenarios, pre- and post-fix |
| [Out of Scope](#out-of-scope) | What this plan explicitly does not cover |
| [Further Notes](#further-notes) | Bugs already fixed this session, and binding ADR constraints |

Version: 1.0 — consolidates the `/grill-me` interview and follow-up investigation (2026-09-02/03).
Canonical in-repo plan; not mirrored to the issue tracker (per owner correction — publishing to a
public GitHub issue is a distinct, higher-stakes action that needs its own explicit go-ahead, not
implied by "create this spec").

## Problem Statement

When a request from Claude Code to cld-gateway fails, the user sees a single opaque line
(`API Error: 502 backend request failed.`) with no signal on whether OpenAI, or the gateway itself,
is at fault, and no path to a log file or a way to report it. Every one of the gateway's
error-construction sites currently discards the real cause down to a handful of generic hardcoded
strings, and the exchange log — the only place the real detail ever lands — silently skips every
request that errors before a response is written, so even the operator debugging their own gateway
has nothing to go on. Separately, the gateway sometimes makes a real behavioral compromise (e.g.
falling back to a full-context transport instead of an incremental delta) and currently tells the
user nothing about it, even though the request still succeeds.

## Solution

Every error response the gateway sends to Claude Code carries the real cause, is clearly branded as
coming from cld-gateway (not Anthropic or OpenAI), points at the specific exchange-log entry via
request_id, and — when the failure isn't attributable to an OpenAI quota/billing limit or a failed
tool call — includes an instruction for Claude Code to review the relevant log lines with the user,
redact anything identifying, get explicit confirmation, and only then file a GitHub issue against
*Bytonomics/cld-gateway* following the repository's own issue-form structure. Success responses gain
a parallel, non-blocking `warnings` array for cases like a missed delta-transport optimization, so
the user learns about a degraded-but-successful outcome without it looking like a failure. This is
implemented once, at a single classification seam, and reused by every one of the six
Claude-Code-facing routes plus the mid-stream abort path, closing the exchange-log logging gap
(`Capture` middleware) found to affect four of those routes along the way.

## User Stories

1. As a `cldc`/`cldg` operator, I want a failed request's error message to state whether OpenAI
   rejected the call or the gateway broke internally, so that I don't waste time assuming it's my
   own request that's wrong.
2. As an operator, I want the error message to include the request_id and a pointer to
   `~/.gateway/logs/http-exchange.log`, so that I can find the exact exchange without guessing which
   of possibly many recent entries is mine.
3. As an operator, I want every gateway error to visibly say it's from cld-gateway, so that I never
   confuse a gateway-side failure with an Anthropic-side or OpenAI-side one.
4. As an operator hitting a likely gateway bug, I want Claude Code to offer to help me file a
   well-formed GitHub issue while the context is still fresh, so that I don't have to reconstruct
   the failure from memory later.
5. As an operator, I want to see the log excerpt and give explicit approval before anything is filed
   publicly, so that I retain control over what leaves my machine.
6. As an operator, I want Claude Code — not the gateway — to be the one that reviews the log excerpt
   for anything identifying before it's shown to me or filed, so that a human judgment call is made
   on content the gateway can't reliably scrub itself.
7. As an operator, I do NOT want to be prompted to file an issue when the failure is an OpenAI
   quota/billing limit, so that I'm not nagged about something that isn't a gateway defect and that
   I can't fix by reporting it.
8. As an operator, I do NOT want to be prompted to file an issue when the failure is a failed tool
   call, so that routine tool-execution failures (which already surface as normal `is_error` content
   blocks, not transport errors) don't generate report noise.
9. As an operator, I want a gateway-originated 4xx (e.g. a validation failure against Claude Code's
   own well-formed request) to also default to "possible bug," so that gateway-side
   request-shaping defects — the exact class of bug this investigation started with — aren't
   silently classified as "my fault."
10. As an operator, I want this same treatment on all six Claude-Code-facing routes (`/health`,
    `/v1/models`, `/auth/status`, `/auth/refresh`, `/v1/messages` unary+stream,
    `/v1/messages/count_tokens`), not just `/v1/messages`, so that a failure anywhere in the API
    surface is equally debuggable.
11. As an operator, I want every one of those routes' exchanges — success or failure — to actually
    appear in `http-exchange.log` with the real final status, so that the log is a reliable record
    instead of silently omitting errors or showing a false-positive 200.
12. As an operator, I want a streaming response that involved a known degradation (e.g. gateway
    couldn't compute an incremental delta and fell back to full context) to carry a warning I can
    see, so that I understand why a turn behaved differently than expected even though it
    "succeeded."
13. As an operator, since Claude Code's client always requests `stream: true` for real interactive
    turns, I want the warning delivery mechanism to actually work on the streaming path — not be
    deferred to a unary path that isn't exercised in practice.
14. As an operator, I want each warning to carry a stable machine-readable `code` alongside its
    human message, so that tooling (now or later) can key behavior off it instead of parsing free
    text.
15. As a maintainer, I want a GitHub issue form (`bug_report.yml`) in `.github/ISSUE_TEMPLATE/`
    defining the fields a good gateway bug report needs, so that anyone filing manually through the
    web UI gets the real, enforced structure GitHub provides.
16. As a maintainer, I want the classification logic to key off the existing
    `StatusError{Status,Body}` type — not off message text — so that upstream-vs-internal
    classification never depends on prompt or response content, honoring the repo's standing "never
    depend on prompt text" rule.
17. As a maintainer, I want the mid-stream abort error path (`stream_writer.go`'s
    `finalizeErrorEvent`, currently a second, divergent, hardcoded error shape) unified with the
    same classification logic used everywhere else, so that there is exactly one error-shape code
    path in the gateway, not two.
18. As an operator, I do NOT want the gateway to attempt its own content-level PII redaction (file
    paths, prompt text) beyond the secret/token redaction it already does, so that no false sense of
    security is created by a fragile regex layer standing in for real judgment.
19. As an operator, I do NOT want a `--template` flag relied on for `gh issue create`, since it does
    not support YAML issue forms (confirmed via `gh` CLI's own `--help` output and corroborating
    upstream *cli/cli* issues this session) — Claude Code composes the issue body itself, following
    the `.yml` form's field structure as an unenforced convention.
20. As a maintainer, I want the single new classification function to be the sole thing every
    error-serialization path calls, honoring ADR-0005's decision that a central error handler is the
    *only* place that serializes an error to the wire — not a second, competing implementation.
21. As a maintainer, I want the streaming warning to ride on the `message_start` event's `message`
    object (the same shape family as `MessagesResponse`), rather than a brand-new SSE event type,
    because that keeps the change inside the existing Anthropic SSE vocabulary the client already
    parses, instead of an unverified custom event.

## Implementation Decisions

- **New classification function, `core/domain/errors`** (exact name left to the implementer, e.g.
  `Classify(err error) *GatewayError` — avoid the bare word "Classify" if it risks confusion with the
  unrelated, pre-existing `ConversationRequestKind` classification concept in
  `core/domain/conversation/classifier.go`; a more specific name is preferred). Input: any `error`.
  Output: a value carrying — the existing `Code`/`HTTPStatus`/`Message` fields `AppError` already has
  (ADR-0005's closed 8-code set is unchanged, no new `error.type` values), plus an `Origin`
  distinction (upstream-OpenAI vs internal-gateway, derived solely from whether the error unwraps to
  the existing `StatusError{Status,Body}` type via `errors.As` — never from message-text inspection),
  a `SuggestIssue bool`, and the rendered instruction text to append when `SuggestIssue` is true. This
  extends, rather than replaces, the ADR-0005 `AppError` type and its single-serialization-point
  decision — consider recording the extension as a new ADR entry once implemented, per this
  project's one-decision-per-ADR convention.
- **`error.type` stays within Anthropic's 8 canonical values** (`invalid_request_error`,
  `authentication_error`, `billing_error`, `permission_error`, `not_found_error`,
  `request_too_large`, `rate_limit_error`, `api_error`, `timeout_error`, `overloaded_error`) — no
  gateway-specific type is introduced, since Claude Code's SDK has real hardcoded retry behavior
  keyed on these exact strings. Consistent with ADR-0005's already-closed set.
- **Quota/billing suppression rule**: `SuggestIssue` is false when the classified error is an
  upstream `StatusError` with status 429, or with an OpenAI-issued billing/quota error type in its
  body — this inspects OpenAI's own structured error body, not user/assistant conversation text, so
  it does not violate the no-prompt-text-dependence rule in the root `CLAUDE.md`.
- **Failed-tool-call exclusion requires no new code**: tool-call failures already surface as normal
  200 responses with `is_error` content blocks (`dto.ContentBlock.IsError`), never as a
  transport-level error, so they never reach the classification function at all.
- **4xx default reversed from convention**: any gateway-originated 4xx (not a passthrough of an
  OpenAI-issued 4xx, which is still classified as upstream via `StatusError`) defaults `SuggestIssue`
  to true, on the basis that the sole client in this architecture is Claude Code itself — a fixed,
  well-behaved caller the operator doesn't hand-edit — so a validation failure against it is a
  gateway defect far more often than a caller mistake.
- **Message branding**: every classified error and warning message is prefixed to unambiguously
  identify cld-gateway as the source, distinguishing it from an Anthropic-native or OpenAI-native
  error.
- **Instruction text shape** (when `SuggestIssue` is true): names the request_id, points at
  `~/.gateway/logs/http-exchange.log` (respecting `GATEWAY_HOME`/`CLD_GATEWAY_LOG_PATH` overrides
  where set), and instructs Claude Code to (1) find the relevant lines for that request_id, (2)
  summarize/redact anything identifying before showing the user, (3) get explicit user confirmation,
  (4) only then compose and run `gh issue create -R Bytonomics/cld-gateway` with a body following the
  `.github/ISSUE_TEMPLATE/bug_report.yml` field structure as an unenforced convention — no
  `--template` flag. No gateway-side PII redaction beyond the existing secret/token redaction in
  `observability/redact.go`; content-level judgment is explicitly left to Claude Code at report time.
- **New `.github/ISSUE_TEMPLATE/bug_report.yml`**: a GitHub issue form. Fields: request_id + pointer
  to the log line; gateway version + backend (codex/gemini) + run mode (dev/packaged, i.e.
  `cldc`/`cldg`); reproduction steps / expected vs actual; a log-excerpt field whose description
  explicitly calls out redaction/consent expectations. Authoritative reference structure for both
  Claude Code's headless convention-following path and a human filing manually through the web UI
  (where GitHub's client-side required-field enforcement actually applies).
- **Warnings — unary shape**: new field on `MessagesResponse`, `Warnings []Warning`, `omitempty`,
  following the existing precedent set by `MessagesResponse.ContextManagement`. `Warning` is a
  structured `{Code, Message}` pair, not a bare string.
- **Warnings — streaming shape (resolved this session)**: rides on the `message` object inside the
  `message_start` SSE event — `BuildStreamStartEvents` (`core/domain/translator/sse_bridge.go`,
  fixed this session to actually port Rust's `anthropic_stream_start_events`, `lib.rs:2617-2641`) gets
  a `warnings` field added to the `message` object it constructs, mirroring exactly how `Warnings`
  sits on `MessagesResponse` for the unary case. This was previously an open question — investigated
  and resolved during this session's follow-up work, contingent on the `message_start` fix landing
  first (it has: verified via fresh `build-check` + full `make test`, all green). Populated at
  whatever point in `MessageService.Handle`'s orchestration a known degradation occurs — the
  delta-calculation-miss example: `selectTransport` (`core/impl/services/message_service.go:245-270`)
  already resolves the WS-delta-vs-full-context decision synchronously, before `SendUnary`/
  `SendStream` is even called, so the warning is known well before `BuildStreamStartEvents` runs —
  this is not a mid-stream-discovery problem.
- **`middleware.Capture` fix**: today, `Capture` reads `c.Response().Status`/body immediately after
  `next(c)` returns, but Echo's own `ServeHTTP` only invokes `e.HTTPErrorHandler` — which sets the
  real status — strictly after the whole middleware chain (including `Capture`) has already returned,
  so any `Capture`-wrapped handler that returns a raw error (confirmed reachable in `/v1/models`,
  `/auth/status`, `/auth/refresh`, `/v1/messages/count_tokens`) gets logged as a false-positive
  200/empty-body success. Fix: `Capture` must itself trigger the same terminal
  error-handling/classification (reusing the new classification function) before reading final
  status/body, mirroring what `handlers/messages.go`'s `logError` (added earlier this investigation)
  already does for `/v1/messages`'s own non-middleware-based logging path.
- **`stream_writer.go`'s `finalizeErrorEvent` unification**: this currently hardcodes its own
  separate `{"type":"error","error":{"type":"api_error","message":"stream terminated: "+reason}}`
  shape, independent of the classification function. It should be rebuilt to go through the same
  classification function, so there is exactly one error-shape code path across unary, stream-setup,
  and mid-stream-abort cases — per ADR-0005's single-serialization-point intent, extended to cover
  the streaming exception ADR-0004 already carves out for the response writer itself.
- **Scope**: all six Claude-Code-facing routes get the classification function wired into their
  error path (`/health`, `/v1/models`, `/auth/status`, `/auth/refresh`, `/v1/messages` unary+stream,
  `/v1/messages/count_tokens`) — `/health` has no reachable error path today, so it's a no-op
  inclusion, not extra work.

## Testing Decisions

- Tests should assert on the observable output of the classification function and the response
  shapes it feeds — the resulting JSON's `type`/`error.type`/`error.message` content, the
  `SuggestIssue` decision, and the `warnings` array/field shape (both unary and the `message_start`
  event) — never on internal call sequencing or private state.
- **Classification function** (`core/domain/errors`): the primary test target. Given a plain
  internal `error`, a `*StatusError` with status 429, a `*StatusError` with a non-quota status/body,
  and a gateway-originated validation error, assert the resulting `Code`/`HTTPStatus`/`Origin`/
  `SuggestIssue`/message-branding are each correct. No HTTP server, no Echo context needed — pure
  function tests, same style as the existing table-driven tests in `core/domain/dto/messages_test.go`
  and `core/domain/translator/generic_test.go`.
- **`middleware.Capture`**: needs an `httptest`-driven Echo instance (no existing prior art in this
  package — new territory) asserting that a handler returning each of: a plain error, an `*AppError`,
  an `echo.HTTPError` (pedantigoecho binder validation failure shape) — each produces a logged
  `Entry` whose `Response.Status`/body matches what the client actually received, not a stale
  pre-error-handling default.
- **`MessageService.Handle`'s `Warnings`**: reuse the existing test seam and style already used by
  `core/impl/services`'s current tests (`TestCanonicalValue`, `TestBranchFingerprints`,
  `TestRequestCompatibilityFingerprint`, `TestExtractMessageText`) — call into the service with
  inputs engineered to force the known degradation path, assert the returned `MessageResult.Warnings`
  contains the expected `{Code, Message}`, both for the unary `MessagesResponse.Warnings` field and
  for the `message_start` event's `message.warnings` field on the streaming path. Follow the pattern
  already established by `TestBuildStreamStartEventsShapeAndContent` /
  `TestBuildStreamStartEventsIsSingleEvent` (added this session) for asserting on the emitted
  `message_start` shape.
- **`stream_writer.go`'s unified error path**: reuse/extend the existing `sse_bridge_test.go`
  conventions for asserting on emitted SSE event shapes.
- No test should assert on the literal instruction-text string sent to Claude Code beyond its
  structural pieces (request_id present, log path present, `gh issue create` present) — the exact
  wording is presentation, not a contract.

## Success Criteria

1. Every one of the six Claude-Code-facing routes, on failure, produces an `error.message` that:
   states origin (OpenAI-upstream vs gateway-internal), is prefixed to identify cld-gateway, includes
   the request_id, and — when `SuggestIssue` is true — includes the report instruction.
2. `~/.gateway/logs/http-exchange.log` contains one entry per request across all six routes, success
   or failure, with the real final HTTP status — zero silently-dropped entries, zero false-positive
   200s.
3. A streaming response that hit the delta-vs-full-context fallback carries a `warnings` entry on its
   `message_start` event; a unary response with the same fallback carries the equivalent entry on
   `MessagesResponse.Warnings`. A response with no degradation carries no `warnings` field
   (`omitempty` — the common case stays byte-identical to today's shape).
4. An OpenAI 429/quota response never sets `SuggestIssue`; a failed tool call never reaches the
   classification function at all (verified by absence, not a special-cased false branch).
5. `stream_writer.go`'s mid-stream abort path and `middleware.ErrorHandler`'s unary path produce the
   same `error.type`/`error.message` shape for the same underlying error — one code path, verified by
   a shared test case, not by inspection.
6. `.github/ISSUE_TEMPLATE/bug_report.yml` exists, is valid GitHub issue-form YAML (parses/renders in
   GitHub's own preview), and its field set matches the four categories decided above.

## Live Verification Scenarios

Every scenario below is driven through a real Claude Code session talking to a running `cldc`
gateway (`go run ./cmd/cld-gateway serve` on :6483) — never curl, never a synthetic request
constructed outside Claude Code. Testing the server in isolation only proves the gateway emits the
right bytes; it proves nothing about whether Claude Code actually surfaces them the way this plan
depends on, which is the entire point of testing live at all. No mock-backend harness exists today
(`RUN_MOCK_BACKEND=1`/`mockbackend` is documented in the Makefile but zero `.go` files implement
that build tag — confirmed by repo-wide grep), so where a scenario needs a specific failure on
demand, the lever is a deliberate, temporary, fully-reversible change to gateway's own local
state or code — never a hand-crafted request bypassing the real client. Each scenario should be run
once pre-fix (confirming it currently fails/misbehaves) and once post-fix.

1. **Real success, no warning** — send an ordinary `hi` through `cldc`. Confirm the response (or
   `message_start` for the streaming case, which is the actual path exercised — see user story 13)
   carries no `warnings` field at all. This is the regression guard: most turns must stay
   byte-identical to pre-plan behavior.
2. **Gateway-internal error (Origin: internal), guaranteed and reversible** — temporarily corrupt
   the refresh token in `~/.gateway/auth.json` (back up the file first), restart `cldc`, send `hi`.
   Traced precisely this session: `requestWithRefreshRetry` (`client.go:172-196`) hits a real 401
   from OpenAI, attempts `refreshOnUnauthorized` (`client.go:201-211`), which fails against the
   corrupted refresh token and returns a plain wrapped error — NOT a `*StatusError` — so this is
   guaranteed to classify as `Origin: internal`, not upstream, through a real "hi" typed in a real
   Claude Code session. Restore the backed-up `auth.json` afterward. Confirm `SuggestIssue: true`
   and the instruction text is present.
3. **Real upstream 4xx (Origin: upstream), guaranteed and reversible** — temporarily revert
   `core/domain/translator/generic.go`'s `Stream: true` back to `Stream: in.Stream`, restart `cldc`,
   send `hi`. Claude Code's own "retry without streaming" behavior sends `stream: false` on its
   retry; gateway forwards it as-is with the revert in place, and OpenAI rejects it with a real
   `400 {"detail":"Stream must be set to true"}` — this is not hypothetical, it is the exact upstream
   400 already observed live earlier this session, before the fix landed. Restore the one-line fix
   afterward. Confirm `Origin: upstream`, and confirm `SuggestIssue` follows the non-quota-4xx rule
   (false, per the resolved Classify logic — a real OpenAI-issued 4xx is not a gateway defect).
4. **Claude Code's own stream:false retry stays clean post-fix** — with the `Stream: true` fix in
   place (i.e. NOT scenario 3's temporary revert), send `hi` and confirm the retry no longer produces
   the `"Stream must be set to true"` 400 at all, and that any other real failure during a retry is
   classified and logged correctly rather than silently dropped.
5. **`/v1/models`, `/auth/status`, `/auth/refresh`, `/v1/messages/count_tokens` error paths** — reuse
   scenario 2's auth-corruption lever specifically against `/auth/refresh` (a real Claude Code
   `/status` or auth-check flow calls this), and confirm the `Capture`-logged exchange entry shows
   the real status, not a false-positive 200. This is the one most worth testing live rather than
   only via `httptest`, since the bug is specifically about real Echo middleware-ordering behavior,
   not just the classification function's own logic.
6. **Real upstream 5xx** — NOT reliably triggerable on demand even through a real client, since it
   requires OpenAI's own infrastructure to actually fail. Covered instead by the
   `core/domain/errors` unit tests constructing a `*StatusError` with a 5xx status directly — call
   out in code review that this scenario's live confirmation is deferred to the first time it
   naturally occurs, not blocked on before merging.
7. **Bug-report instruction, end to end** — trigger scenario 2 or 3 through the real Claude Code
   session and observe whether Claude Code actually: reads the log excerpt, shows it to the user,
   asks for confirmation, and only then proposes/runs `gh issue create`. This is explicitly the one
   item marked unverifiable-from-this-repo in Out of Scope — the live scenario here is how that
   eventually gets confirmed, just not as a merge-blocking test.

## Out of Scope

- Whether Claude Code actually follows a multi-step instruction embedded in an error string
  end-to-end — unverifiable from this repo, not something this plan's tests can cover.
- The exact wording/field list of `bug_report.yml` beyond the four field categories decided above —
  left to the implementer, refined at review time.
- The ~29 untested Claude-Code slash-command/status-command scenarios found in
  `core/domain/claudecode/` during this investigation (Rust `translate.rs` had named tests for them;
  the Go port's `core/domain/claudecode/` package has zero test files) — a real, separate gap, not
  part of this plan.
- The 6 confirmed open gaps and false positives recorded in
  `docs/runbooks/deferred-work-audit-triage.md` — a separate audit, not folded into this plan.
- Any change to `transport-decisions.jsonl` or its purpose — unrelated to this plan's error/warning
  surface.
- A GitHub Actions or bot-side check enforcing `bug_report.yml`'s fields server-side — confirmed this
  session that GitHub itself provides no such enforcement path via API/CLI (required-field validation
  is web-UI-only, and even there only on public repos) — out of scope because no such mechanism
  exists to build against.
- Any browser-opening / `--web` flow for issue filing — considered and explicitly rejected in favor
  of the headless, convention-following `gh issue create` path.

## Further Notes

- This plan closes over four concrete, verified bugs found during live investigation, three fixed
  already and confirmed via fresh `build-check` + full `make test` runs (all green, no `FAIL`):
  (1) the outgoing backend request's `stream` field was wired from the client's own `stream` flag
  instead of being hardcoded `true` like the Rust port (`core/domain/translator/generic.go`); (2)
  `message_service.go`'s backend-error branches discarded the real error text behind generic
  hardcoded strings, now surfacing `err.Error()`; (3) `handlers/messages.go` never logged the
  exchange at all when `MessageService.Handle` returned an error, now fixed via `logError`; (4) every
  streaming response was missing its `message_start` event entirely — a genuine Rust-parity gap, not
  a design choice — now fixed via `BuildStreamStartEvents`. This plan generalizes and completes that
  same class of fix — real-cause surfacing plus reliable logging — across the rest of the API
  surface, and adds the two genuinely new capabilities (issue-filing guidance, success-path warnings)
  on top of it, now that the `message_start` prerequisite for the streaming-warnings design is no
  longer blocked.
- Respects ADR-0004 (SSE single-writer goroutine, Option C post-stream logging — no per-event
  logging, no middleware wrapping the streaming response writer) and ADR-0005 (single `AppError`
  type, closed 8-code set, one serialization point) as binding constraints on the implementation
  approach above, not just as background reading.
- The repository is public (`git@github.com:Bytonomics/cld-gateway.git`), with an existing
  `ready-for-agent` GitHub label already present, for whenever this plan is actually mirrored to the
  issue tracker on explicit instruction — not done as part of writing this plan.
