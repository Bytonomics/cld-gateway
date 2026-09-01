# GAPS: Rust implementation review (verified findings)

Source: critical review of `crates/`, 2026-08-22. Every finding cites code.
These are candidates to FIX in the Go port — each needs an owner decision
before the file map freezes. None are silently folded in.

## Critical

### G1. Chain-checkpoint store lost on restart
- Where: `crates/gateway-http-anthropic/src/lib.rs:81-82`
  (`OpenAiChainCheckpointStore` = `Arc<Mutex<HashMap>>`).
- What: WS-chain ↔ response-id associations live only in process memory.
- Consequence: after any restart, incremental transport can't revalidate
  live chains → falls back to full SSE once per chain; correctness is
  preserved by design (checkpoint gate), only efficiency is lost.
- Go options: (a) accept — same behavior, safe fallback exists;
  (b) persist associations into the conversation-state branch JSON.
- Recommendation: (b) — cheap, matches existing branch metadata writes.

### G2. Main-turn leases lost on restart
- Where: `lib.rs:85-87` (`MainTurnLeaseStore`).
- What: in-flight lease map is memory-only.
- Consequence: a crash mid-turn leaves no lease; a retry could re-commit.
  In practice the single-user, single-process daemon rarely hits this.
- Go options: (a) accept; (b) persist lease with timestamp + crash-recovery
  sweep that clears leases older than a TTL at startup.
- Recommendation: (b) with TTL sweep — small, removes the crash window.

### G3. No graceful shutdown
- Where: `crates/gatewayd/src/main.rs:109-112` — bind and serve; no
  signal handling.
- Consequence: SIGTERM drops in-flight streams mid-turn.
- Go fix: signal.NotifyContext + http.Server.Shutdown with drain timeout;
  leases release on drain. Go makes this easy — do it.

### G4. Default backend timeout is None
- Where: `crates/gateway-backend-codex/src/client.rs:64`
  (`request_timeout: None`).
- Consequence: frozen upstream hangs a request forever.
- Go fix: default 120s unary / no header-timeout for streams (streams are
  event-driven; enforce idle-event timeout instead), configurable.

## High

### G5. Swallowed auth-cleanup errors
- Where: `client.rs:206,251` (`let _ = logout_with_revoke...`).
- Go fix: log the failure; keep going (logout is best-effort by design,
  but never silent).

### G6. Advisory file locking only
- Where: `crates/gateway-state/src/conversation.rs:1116-1134`.
- Consequence: two gateway processes can interleave writes.
- Reality: single-user daemon; brew service runs one instance.
- Go fix: keep flock-style advisory locking + document single-instance
  assumption; add a PID/lockfile check that refuses to start a second
  serve on the same port/state root.

### G7. Swallowed logging errors
- Where: `crates/gateway-observability/src/middleware.rs:135-136`.
- Go fix: slog-warn on append failure; disable-logging circuit breaker
  after N consecutive failures to avoid per-request error spam.

### G8. Blocking file IO on request path
- Where: conversation-state `std::fs` under async handlers.
- Go reality: goroutines make this a non-issue vs tokio's executor, but
  the per-session lock serializing disk writes remains.
- Go fix: keep per-session locks; writes stay small (JSON metadata + JSONL
  append). No exotic async FS needed.

## Medium

### G9. Unbounded exchange-log growth
- Where: `middleware.rs:239-251` — append-only JSONL/text.
- Go fix: size-based rotation (e.g. 50MB, keep 3) at the observability
  sink. Note: user's new formatted-text log format applies to the
  post-stream Option C writer too.

### G10. Hardcoded WS keepalive
- Where: `websocket_transport.rs:133` (20s).
- Go fix: config key under the backend's provider block.

### G11. Health endpoint is a stub
- Where: `lib.rs:821-823` — always ok.
- Go fix: `/health` returns process uptime + config load state (cheap);
  `/health/deep` optional later (auth validity, last backend contact).
  Keep `/health` itself lightweight for the brew service check.

## Low

### G12. No metrics
- Go fix: expvar or a small Prometheus endpoint on a separate localhost
  port later; not a launch blocker. Record request counts, transport
  decisions (already logged), WS pool size.

### G13. Error context
- Go fix: AppError already carries request correlation via middleware
  request-id; ensure slog fields include request_id everywhere.

## Decisions (owner, 2026-08-22)

| ID | Decision |
|---|---|
| G1 | OPEN — not approved yet |
| G2 | OPEN — not approved yet |
| G3 | OPEN — not approved yet |
| G4 | APPROVED — 120s unary default + idle-event timeout for streams, configurable |
| G5 | APPROVED — log auth-cleanup failures (never silent) |
| G6 | OPEN — not approved yet |
| G7 | APPROVED — log append failures + circuit breaker after N consecutive failures |
| G8 | APPROVED — keep per-session locks; goroutine-friendly IO; no exotic async FS |
| G9 | APPROVED — size-based rotation AND retention deleting old rotated files; keep the disk clean (applies to the formatted-text exchange log and JSONL sinks) |
| G10 | OPEN — not approved yet |
| G11 | OPEN — not approved yet |
| G12 | APPROVED (scope set) — optional OpenTelemetry endpoint, published later as an opt-in feature; design the observability package so an OTel exporter can be added without restructuring |
| G13 | note — covered by request_id in slog fields |
