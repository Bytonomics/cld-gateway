---
type: runbook
title: "Deferred-Work Audit Triage Handoff"
status: draft
tags:
  - audit
  - triage
  - technical-debt
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Deferred-Work Audit Triage Handoff

| Section | What it covers |
|---------|----------------|
| [Motivating incident](#motivating-incident) | The bug that triggered this audit |
| [Audit methodology](#audit-methodology) | How the sweep was run, and its known blind spot |
| [Confirmed open gaps](#confirmed-open-gaps) | The 6 real items requiring a decision |
| [Verified false positives](#verified-false-positives) | Matches that were checked and cleared |
| [Handoff instructions](#handoff-instructions) | What the next agent must do with all of the above |

## Motivating incident

A comment in `core/domain/translator/sse_bridge_test.go` read: "message_start is built by the
caller in Rust, outside sse_bridge.rs, so it is outside this port's scope." Accurate about Rust's
architecture — but during the Go port no caller ever picked up that responsibility. The gateway's
streaming responses to Claude Code never sent the `message_start` SSE event that Anthropic's
protocol requires as the first event of every stream.

Verified against the old Rust implementation: `old_rust/crates/gateway-http-anthropic/src/lib.rs#anthropic_stream_start_events`
(lines 2617-2641), called from `stream_messages` at line 2903 in the same file. Fixed in this
session:

- `core/domain/translator/sse_bridge.go#BuildStreamStartEvents` — new function building the start
  events.
- `core/impl/services/message_service.go#runStream` — now calls `BuildStreamStartEvents` and sends
  its result as the first event on every stream, before consuming any backend event.
- `core/domain/translator/sse_bridge_test.go#TestBuildStreamStartEventsShapeAndContent` and
  `#TestBuildStreamStartEventsIsSingleEvent` — new coverage.

This incident is why a full-repo audit for similar "someone else's responsibility" / "out of
scope" / deferred-work markers was run: to find any other place where a documented handoff was
never picked up.

## Audit methodology

Scope: every `*.go` file under the repo root, excluding `vendor/`, `old_rust/`, and `others/`.
Confirmed via `find` at exactly 87 files across 25 directories. The `find` file list was piped
directly into `grep`, so the searched-file count was mechanically proven equal to the enumerated
count (87), not just assumed.

Case-insensitive keyword list: `TODO`, `FIXME`, `XXX`, `HACK`, `out of scope`, `outside.*scope`,
`not implemented`, `unimplemented`, `not supported`, `not yet`, `deferred`, `for now`, `later`,
`punt`, `parked`, `placeholder`, `stub`, `not currently`.

**Known limitation** — state this explicitly before trusting the results below: this keyword list
did NOT catch the `message_start` incident itself in earlier passes of this investigation. That gap
was found by manually tracing the streaming lifecycle end-to-end and diffing against Rust, not by
grep. A keyword sweep only finds gaps that were honestly labeled with one of these words; it cannot
find a gap where the responsible party silently assumed someone else had it without saying so in a
comment. This method is not exhaustive — do not treat it as such.

## Confirmed open gaps

For each item: read the current code at the cited location, confirm the gap still exists (code may
have changed since this audit), then decide either (a) implement it now, or (b) leave it deferred
but confirm the comment/doc still accurately describes it. Do not assume any item below is still
accurate without re-checking.

1. **Graceful shutdown not implemented** — `cmd/cld-gateway/main.go` (marked `✱G3 (open gap, not
   yet owner-approved)` in `runServe`). Cross-reference `docs/GAPS.md` for the G3 entry and its
   approval status before implementing.
2. **Gemini backend login and serve-mode support unimplemented** — `cmd/cld-gateway/main.go`
   (`authPreflightForServe`'s `VendorGemini` case, and `runLogin`'s `VendorGemini` case). Accepted
   by the CLI surface but not implemented; explicitly documented as an intentionally out-of-scope
   backend for now in `CLAUDE.md`.
3. **WebSocket chain association is memory-only** — `core/domain/transport/chain_registry.go`
   (`TODO(✱G1, open gap per GAPS.md/FILEMAP.md)`). A persisted variant into branch metadata is not
   implemented. Cross-reference `docs/GAPS.md` G1.
4. **No TTL startup sweep for the lease store** — `core/domain/transport/lease.go`. Process-local
   only; the comment states this explicitly.
5. **`HookEvaluator` classification unimplemented** — `core/domain/conversation/classifier.go`.
   Explicitly blocked on `docs/classification-signal-redesign.md`, itself marked `Status: parked`
   ("do not implement until the golang_port design is finalised... Trigger to unpark: Owner confirms
   the golang_port architecture interview is complete and the slop inventory has been re-verified").
   Check whether that trigger condition has now been met before touching this.
6. **Hardcoded value that should be configurable** — `core/impl/port/backend/codex/wspool.go`
   (`TODO(G10, open gap)`). Cross-reference `docs/GAPS.md` G10.

## Verified false positives

These matched the audit's keyword search but were individually checked in full context and
confirmed to describe either completed work or normal (non-deferred) code flow. The next agent's
job here is lighter: confirm each is still accurate, and if so, optionally clean up the wording so
it doesn't read as a deferred-work marker to a future grep pass — this is exactly the class of
false alarm that makes keyword audits noisy. Low priority; a clarity improvement, not a functional
fix.

1. `core/domain/translator/generic.go` — "response-event mapping is intentionally out of scope for
   this file" — verified accurate: implemented in the same package's `core/domain/translator/sse_bridge.go#TranslateResponseEvent`
   and `core/domain/translator/sse_bridge.go#BuildUnaryResponse`, confirmed real and complete (the
   exact file just extended to fix the message_start incident above).
2. `core/impl/services/stream_writer.go` — "enforced by wiring in a later wave" — verified
   accurate: `app/routes_messages.go` confirms `/v1/messages` genuinely has no `middleware.Capture`
   mounted on it, matching what this comment promises.
3. `core/domain/port/state/toolcalls.go` and `core/domain/port/state/conversation.go` —
   "implementation lands in a later wave" — verified: both implementations exist and are wired
   (`core/impl/port/state/toolcalls/gorm.go`, `core/impl/port/state/conversation/fs.go`), not
   stubs.
4. `middleware/recovery.go`, `middleware/requestid.go`, `core/domain/translator/sse_bridge.go`
   (near the tool-call-kinds comment, pre-edit line number), `core/domain/translator/translator.go`,
   `core/impl/services/message_service.go` — all just the plain English word "later" describing
   normal sequencing within already-complete code (a value read later in the same function call, a
   wiring wave that already shipped). No deferred functionality, no action needed.
5. `core/domain/contextmgmt/manager.go` (`toolResultPlaceholder`, `thinkingPlaceholder` constants)
   — "placeholder" here is a real, active runtime constant (literal text substituted for cleared
   tool-result/thinking content during context management), not a stub or incomplete feature. No
   action needed.

## Handoff instructions

Re-verify every item above against current code before acting on it — files may have changed since
this audit was written. This document is a snapshot for handoff, not a live source of truth.

1. For each of the 6 [confirmed open gaps](#confirmed-open-gaps): decide and record — implement
   now, or confirm-and-leave-deferred.
2. For each [verified false positive](#verified-false-positives): optionally clean up the comment
   wording once confirmed still accurate.
3. If a similar sweep is needed again later, reuse the [audit methodology](#audit-methodology)
   above — but its documented keyword-only limitation means it must never be the sole method relied
   on to find this class of gap.
