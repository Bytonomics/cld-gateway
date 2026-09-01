# golang_port TEST PLAN (C2)

Purpose: close the test-coverage gap flagged as review finding C2 and lock in
the 26 functional fixes so they cannot silently regress. Behavior parity
source of truth is the Rust workspace under `crates/` and its fixtures.

## Principles

1. Test EXTERNAL behavior, not implementation details: inputs in -> HTTP/SSE
   bytes, channel events, on-disk state, or a port call out. No test asserts a
   private field or a stub's zero value.
2. Prefer the HIGHEST seam. Primary seam is the HTTP boundary
   (POST /v1/messages in, JSON/SSE out, disk state after). Drop to a package
   seam only for logic not reachable end-to-end (retry matchers, schema gate).
3. Every fixed finding (#1-#26) gets at least one REGRESSION test whose name
   references the finding, asserting the behavior the fix introduced AND the
   pre-fix failure mode.
4. Table-driven; ports mocked with hand-written fakes (no new mock framework).
5. Parity tests compare against Rust: reuse the Rust fixture corpus
   (crates/gateway-http-anthropic/tests/fixtures, translate/sse test tables) as
   golden files where they exist.
6. Determinism: no wall-clock (stateport.Clock fake), no network (httptest +
   in-process WS server), no ~/.gateway (t.TempDir() + GATEWAY_*/CLAUDE_GATEWAY_*
   /CLD_GATEWAY_* env overrides).

## Seams & shared harness (build first)

- testsupport (test-only helpers):
  - FakeBackend implementing port/backend.Backend: scriptable unary/stream
    events, LiveChainID/Capabilities, eviction spy.
  - FakeConversationRepo, FakeToolCallRepo, FakeAuthProvider, FakeClock,
    FakeDiagnosticSink, FakeExchangeLog: record calls for assertion.
  - MockCodexServer: httptest.Server speaking the Codex Responses SSE shape plus
    a coder/websocket upgrade endpoint, with programmable status/headers/frames/
    latency (401-once, stall, close-frame, ping).
  - NewTestApp(t, cfg, deps...): builds the Echo app with fakes injected.
- Golden loader: read expected_anthropic_*.jsonl / backend_stream_*.sse fixtures
  from the Rust tree; a -update flag regenerates Go-side goldens.

## P0 - fixed-finding regressions + correctness core

### P0-A core/domain/transport - lease (guards #2,#3) - NEW lease_test.go
- 6 states + AllowsCommit() only for InFlight.
- Acquire: first wins; second returns Busy with InFlightRequestID,
  PreviousResponseID, WebSocketChainID populated (#3).
- PromoteWebSocketChain: nil->true; missing/reqID-mismatch/non-InFlight->false;
  success sets chain (#3).
- ValidateForCommit 5-case matrix (#2): (none,none) accept; (some,some)= accept;
  (some,some)!= websocket_chain_id_mismatch; (some,none)
  missing_commit_websocket_chain_id; (none,some) unpromoted_websocket_chain_id;
  missing missing_active_lease; reqID request_id_mismatch; non-InFlight state
  string. Assert no mutation.
- Concurrency: N goroutines Acquire same key -> exactly one Acquired.

### P0-B core/impl/services - message_service orchestration (guards #2,#12,#14,#23) - NEW message_service_test.go
Drive Handle with fakes; assert port calls.
- Unary visible-main happy path: acquire -> PromoteWebSocketChain(live chain) ->
  ValidateForCommit accepts -> CommitTurn -> lease CompletedCommitted.
- COMMIT-SUPPRESSION (#2): fake backend returns a different live chain at commit
  than at promote -> CommitTurn NEVER called, lease CommitSuppressedAfterAbort,
  response still returned.
- Abort before-first-event vs after-visible-output -> correct terminal state; no
  commit on abort.
- Backend error -> BackendFailedBeforeCommit, no commit.
- Non-persisted turn -> no lease/branch/commit; tool calls still recorded.
- Offshoot -> CommitOffshootCheckpoint, no lease.
- CTX-MGMT REPORT (#12): applied edit -> response.ContextManagement set AND
  MessageResult.ContextManagementMetadata populated.
- TOOL-KIND (#14): custom call round-trips as custom_tool_call (not function);
  defaulted row resolves via GORM default function_call consistently.

### P0-C core/impl/port/backend/codex - wspool keepalive (guards #7) - NEW wspool_test.go
- IDLE SURVIVAL (#7): idle > keepalive interval, server pings -> session STILL
  live; LiveChainID unchanged; follow-up previous_response_id turn succeeds.
- RACE HARDENING (residual from #7 fix): interleave idle with command sends
  under -race -> no concurrent-Reader panic, no deadlock.
- Server close-frame while idle -> evicted, next turn re-establishes.
- Retry matchers: retryable {500,502,503} recycles up to MaxRecycles; non-
  retryable {400,401,403,422,429} does not; semantic body needles per Rust.

### P0-D core/impl/port/backend/codex - client + schema gate (guards #8,#13,#19,#20) - EXTEND client_test.go
- SCHEMA GATE (#8): tool params with optional props -> POSTed body has
  additionalProperties:false, required=all keys, optionals nullable (unary AND
  WS create body).
- 401 refresh-retry-once: 401 then 200 -> one refresh, one retry; headers right.
- LOGOUT ON PERMANENT REFRESH (#19): refresh returns ErrRefreshUnauthorized ->
  FakeAuthProvider.Logout(true) once.
- STREAM IDLE TIMEOUT (#13): server stalls -> DecodeEventStream emits terminal
  error frame within idle window; channel closes (no hang).
- client_metadata (#20): empty-but-present -> body has "client_metadata":{}.

### P0-E core/impl/port/state/conversation - fs (guards #4,#5,#6,#24,#25) - EXTEND fs_test.go
All against t.TempDir(); assert on-disk JSON matches Rust wire format.
- FindTurnCheckpoint (#4): two checkpoints same turn_id, different (count,hash)
  -> lookup by (count,hash) resolves correct one; non-matching pair misses.
- CROSS-PROCESS LOCK (#5): hold .session.lock from a second flock handle ->
  withSessionLock blocks until released (goroutine + timeout proves it).
- COMPACTION FINGERPRINTS (#6): ApplyCompaction with new fingerprints ->
  branch.json AND sparse checkpoint carry new compaction_summary_hash.
- RECONCILE EQUALITY (#24): messages JSON-equal but Go int vs disk float64 -> NO
  spurious InboundCanonicalSnapshotReconciled event.
- RETENTION OVERFLOW (#25): days=MaxInt64 -> returns 0 cleaned, no panic; normal
  days deletes only older-than-cutoff.
- Branch selection 5-action matrix; corruption fail-closed unless
  QuarantineAndReset.

### P0-F core/impl/port/state/toolcalls - gorm (guards #15) - NEW gorm_test.go
- RecordToolCall with CreatedAtUnixSeconds=0 -> stored created_at = fake clock,
  not 0 (#15).
- Upsert idempotency; tool_kind NOT NULL DEFAULT function_call; AutoMigrate add-
  column; exists/get round-trip with long-form kind.

### P0-G config (guards #9) - EXTEND config_test.go
- PROVIDER MAP PARSES (#9): providers.backends.custom + active: custom ->
  ResolveModel uses custom; legacy inline shape does NOT silently populate.
- Defaults when file missing; GATEWAY_CONFIG_PATH/GATEWAY_HOME; unsupported->
  default override; passthrough.

## P1 - untested domain logic

### P1-A core/domain/conversation - classifier + identity (guards #11, CLAUDE.md rules)
- Metadata read_only -> LocalControl.
- STRUCTURED-OUTPUT (#11): non-stream + output_config, no read-only meta ->
  VisibleMain (not offshoot/HookEvaluator).
- Bug-marked prompt-text detectors return documented kinds; a plain turn quoting
  "permission"/"authorize" with no structured signal -> VisibleMain (Slop-2
  word-gate stays deleted).
- Identity.Key/SessionKey/CheckpointKey exact format.

### P1-B core/domain/contextmgmt - Manager
- follow_request vs override_request; clear_tool_uses trigger+keep/exclude;
  clear_thinking; 3 hard limits; Report.ResponseValue/MetadataValue/IsEmpty.
  Parity with Rust applied-edit counts.

### P1-C core/domain/claudecode - envelope + context
- ParseCommandEnvelope tags; 22-command local-only table + stdout markers;
  inclusion metadata (extend_client_metadata).
- Slash-command promotion; packaged status body; directive injection; skill
  first-line rewrite (bug-marked). Confirm READ_ONLY_MARKERS stays deleted.

### P1-D core/impl/port/auth/codexauth - store/oauth/revoke
- Status.IsLoggedIn = ChatGPT+access+refresh+account_id (API-key -> not logged
  in) - drives preflight matrix (#10).
- RefreshAndPersist: 401 -> ErrRefreshUnauthorized; success rewrites tokens
  atomically (temp+rename, 0600); refreshErrorCode object/string/absent.
- AccountID from tokens then id_token JWT claim; WriteOpenAIAPIKey wholesale.

### P1-E core/impl/services - remaining services
- ModelsService: catalog from settings models[]; env-fallback slots; dedupe;
  missing/empty file -> CodeAPI error. CountTokens ceil(len/4). AuthStatusService
  maps status/refresh.
- stream_writer: FLUSH-BEFORE-COMPLETION (bytes reach a flush spy BEFORE input
  channel closes - the mandated streaming invariant); idle-timeout terminal
  event; Option-C log once at close; drains input on client-gone.
- translate_executor: status command JSON shape; post-result registry.

### P1-F core/impl/translator/codex + domain translator
- OpenAITranslator embeds Generic and satisfies interface; behavior test through
  the codex translator. Extend generic_test/sse_bridge_test with any event types
  missing from the golden tables.

## P2 - infra & wiring

### P2-A netpolicy
- Allowlist/denylist; Anthropic/Claude blocked even if explicitly allowed;
  localhost allowed; scheme reject; REDIRECT RE-CHECK (CheckRedirect denies a
  cross-host redirect to anthropic.com); GATEWAY_ALLOWED_OUTBOUND_HOSTS.

### P2-B middleware + core/domain/errors
- ErrorHandler: AppError->Anthropic shape with status; echo.HTTPError->
  invalid_request_error; HEAD->NoContent. Recover: panic->same shape;
  http.ErrAbortHandler re-panics. Capture unary tee; RequestID propagation.

### P2-C handlers (end-to-end via NewTestApp)
- POST /v1/messages unary: JSON body, session header threaded, exchange log
  AFTER response (#26).
- POST /v1/messages stream: SSE frames flushed; no Capture wrap; Option-C log at
  close.
- /v1/models, /auth/status, /auth/refresh, /health, /count_tokens happy+error ->
  Anthropic error shape.
- pedantigo binder: missing model / empty messages -> 400 invalid_request_error.

### P2-D app
- Initialize with conversation-state disabled/enabled; NewEcho mounts all 6
  routes; Capture only on meta+count_tokens, never /v1/messages.

### P2-E tui (lowest)
- RunLoginSelector transitions (arrow/number/enter/quit) via teatest, or unit-
  test the extracted pure reducer.

## Cross-cutting: end-to-end parity harness
One handlers-level suite replaying a fixed scenario matrix through NewTestApp
against MockCodexServer, diffing client-visible SSE/JSON against Rust goldens:
text turn, tool-use turn, web-search turn, structured output, multi-branch
continuation with previous_response_id reuse, 401 refresh, client-abort. Truest
whole-pipeline regression net; covers most findings transitively.

## Execution, CI, coverage
- Makefile: make test (unit, no network); make verify-test (RUN_MOCK_BACKEND=1,
  -tags=mockbackend, includes WS/httptest suites); both under -race.
- Coverage gate: fail CI under 85% for core/domain/transport, core/impl/services,
  core/impl/port/state/*, core/impl/port/backend/codex, config, netpolicy,
  core/domain/errors; advisory elsewhere. make cover prints per-package.
- -race nightly exercising P0-C (wspool) stress.

## Build sequence
1. Shared harness (testsupport, MockCodexServer, golden loader).
2. P0-A..P0-G (fixed-finding regressions; blocks any "fixes verified by tests").
3. Cross-cutting E2E parity harness.
4. P1 (domain logic).
5. P2 (infra/wiring/tui).

## Traceability: finding -> guarding suite
#1 -> CI make build off clean checkout (no replaces) | #2 -> P0-A,P0-B |
#3 -> P0-A | #4,#5,#6,#24,#25 -> P0-E | #7 -> P0-C | #8,#13,#19,#20 -> P0-D |
#9 -> P0-G | #10 -> P1-D (+ runServe preflight-error test) | #11 -> P1-A |
#12,#14,#23 -> P0-B | #15 -> P0-F | #16 -> observability test (new
transport_diag_test) | #17,#18,#21,#22 -> extend output_text_test/tool_calls_test
/new sse_test | #26 -> P2-C.
