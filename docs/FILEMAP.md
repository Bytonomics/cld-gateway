# FILEMAP: exact files, interfaces, public methods (golang_port)

Derived from the verified Rust public API surface (gateway-state 48 public
items; auth/net/backend/core/module APIs extracted 2026-08-22). Public
surface only — unexported internals decided during implementation.
GAP fixes (GAPS.md) marked ✱ where they add methods; pending owner approval.

## cmd/cld-gateway/main.go
- `func Main()` — argv: `serve` (default) | `login [openai|gemini]`;
  serve = auth preflight → config load → app.Initialize → app.RunServer.

## app/
**providers.go**
- `type Providers struct` — fields: Config, AuthService, MessageService,
  CountTokensService, ModelsService, AuthStatusService, TransportDiagnostics,
  ExchangeLog, Clock.
**initialize.go**
- `func Initialize(cfg *config.Config) (*Providers, error)` — manual
  constructor DI (repos → adapters → services).
**router.go**
- `func NewEcho(p *Providers) *echo.Echo` — pedantigoecho binder, Recover,
  RequestID, unary capture middleware, central HTTPErrorHandler.
- `func RunServer(e *echo.Echo, addr string) error` — ✱G3 graceful
  shutdown: signal.NotifyContext + Shutdown drain.
**routes_messages.go**
- `func MountMessagesAPI(e *echo.Echo, p *Providers)` — POST /v1/messages,
  POST /v1/messages/count_tokens.
**routes_meta.go**
- `func MountMetaAPI(...)` — GET /health (✱G11 uptime+config state),
  GET /v1/models, GET /auth/status, POST /auth/refresh.

## core/domain/errors/
**apperror.go**
- `type Code string`; const codes: `invalid_request_error`,
  `authentication_error`, `permission_error`, `not_found_error`,
  `rate_limit_error`, `api_error`, `overloaded_error`,
  `gateway_state_error`.
- `type AppError struct { Code Code; Message string; HTTPStatus int;
  Cause error }`
- `func (e *AppError) Error() string`; `func (e *AppError) Unwrap() error`
- `func New(Code, string, int) *AppError`; `func Wrap(error, Code, string, int) *AppError`
**anthropic.go**
- `func AnthropicPayload(err error) map[string]any` — serializes to
  `{"type":"error","error":{"type","message"}}`.

## core/domain/services/
**message_service.go**
- `type MessageService interface { Handle(ctx context.Context,
  req *dto.MessagesRequest) MessageResult }`
- `type MessageResult` — Unary(*dto.MessagesResponse) | Stream(<-chan
  SSEEvent, error) — consumed by the SSE writer goroutine.
**count_tokens_service.go**
- `type CountTokensService interface { Estimate(ctx, *dto.MessagesRequest) int64 }`
**models_service.go**
- `type ModelsService interface { List(ctx) (*dto.ModelList, error) }`
**auth_status_service.go**
- `type AuthStatusService interface { Status(ctx) (*dto.AuthStatus, error);
  Refresh(ctx) (*dto.AuthStatus, error) }`

## core/domain/dto/ (one file per family)
**messages.go** — `MessagesRequest`, `MessagesResponse`, `SystemBlock`,
`Message`, `ContentBlock`, `Tool`, `ToolChoice`, `OutputConfig`,
`Usage` (all with pedantigo `validate` tags; `var _ =
validator.Register(validator.New[MessagesRequest]())` etc.)
**sse.go** — `type SSEEvent struct { Event string; Data []byte }`
**models.go** — `Model`, `ModelList`
**auth.go** — `AuthStatus`, `AuthSnapshot`
**count_tokens.go** — `CountTokensResponse`

## core/domain/port/
**backend/backend.go**
- `type Capabilities struct { WebSocketDelta, ServerSideState bool }`
- `type Backend interface {
    SendUnary(ctx context.Context, req *BackendRequest) (*BackendResponse, error)
    SendStream(ctx context.Context, req *BackendRequest) (<-chan BackendEvent, error)
    Capabilities() Capabilities
    EvictSession(key SessionKey)
    HasLiveSession(key SessionKey) bool
    LiveChainID(key SessionKey) (ChainID, bool)
  }`
- `type SessionKey string`; `func (s SessionKey) String() string`
- `type ChainID string`; `func (c ChainID) String() string`
**backend/types.go** — `BackendRequest`, `BackendResponse`, `BackendEvent`,
`EventStream` decode types (port of CodexBackendRequest/Response/Event).
**auth/auth.go**
- `type Provider interface {
    AccessToken(ctx context.Context) (Secret, error)
    AccountID(ctx context.Context) (string, error)
    RefreshAndPersist(ctx context.Context) (AuthSnapshot, error)
    Status(ctx context.Context) (*AuthStatus, error)
    Logout(ctx context.Context, revoke bool) error
  }`
**state/conversation.go**
- `type ConversationRepo interface {
    EnsureSession(ctx, sessionID string) (*ClaudeSessionMetadata, error)
    LoadSession(ctx, sessionID string) (*ClaudeSessionMetadata, error)
    LoadAllBranches(ctx, sessionID string) ([]BranchMetadata, error)
    CreateBranch(ctx, sessionID string, p BranchCreateParams) (BranchMetadata, error)
    SelectOrCreateBranch(ctx, sessionID string, in BranchSelectionInput) (BranchSelectionResult, error)
    LoadBranch(ctx, sessionID, branchID string) (*BranchMetadata, error)
    StoreBranch(ctx, sessionID string, b BranchMetadata) error
    AppendLedgerEvent(ctx, sessionID, branchID string, ev CanonicalLedgerEvent) error
    CommitTurn(ctx, sessionID, branchID string, p CommitTurnParams) (BranchMetadata, error)
    CommitOffshootCheckpoint(ctx, sessionID, branchID string, p CommitOffshootCheckpointParams) error
    ReconcileSnapshot(ctx, sessionID, branchID string, p ReconcileSnapshotParams) (BranchMetadata, error)
    ApplyCompaction(ctx, sessionID, branchID string, summaryHash string) (BranchMetadata, error)
    InvalidateCheckpoint(ctx, sessionID, branchID string) error
    RebuildBranchFromDisk(ctx, sessionID, branchID string) (BranchMetadata, error)
    FindTurnCheckpoint(ctx, sessionID, branchID, turnID string) (*TurnOpenAiCheckpoint, bool)
    CleanupSessionsOlderThan(ctx context.Context, days int) (int, error)
  }` — 1:1 port of the 20 ConversationStateStore methods
  (`crates/gateway-state/src/conversation.rs`).
**state/toolcalls.go**
- `type ToolCallRepo interface {
    EnsureSchema(ctx context.Context) error
    RecordToolCall(ctx, call StoredToolCall) error
    ToolCallExists(ctx, callID string) (bool, error)
    GetToolCall(ctx, callID string) (*StoredToolCall, error)
  }`
**state/types.go** — 16 structs/enums ported verbatim:
`ClaudeSessionMetadata`, `BranchFingerprintSet`, `OpenAiCheckpoint`,
`TurnOpenAiCheckpoint`, `OffshootOpenAiCheckpoint`, `BranchCheckpointRef`,
`BranchMetadata`, `BranchCreateParams`, `ConversationTurnScope` (Main/Side),
`BranchSelectionInput`, `BranchSelectionAction` (CreatedInitial/
ContinuedExisting/ForkedFromAncestor/CreatedAmbiguous/CreatedUnmatched),
`BranchSelectionResult`, `CommitTurnParams`,
`CommitOffshootCheckpointParams`, `ReconcileSnapshotParams`,
`SparseCheckpoint`(+Kind), `CanonicalLedgerEvent` (5 variants).
**state/clock.go** — `type Clock interface { Now() time.Time }`

## core/domain/translator/
**translator.go**
- `type BackendTranslator interface {
    TranslateRequest(ctx context.Context, in *dto.MessagesRequest, meta TranslateMeta) (*port.BackendRequest, error)
    TranslateResponseEvent(ev port.BackendEvent) ([]dto.SSEEvent, error)
    BuildUnaryResponse(events []port.BackendEvent) (*dto.MessagesResponse, error)
  }`
- `type TranslateMeta struct { Model, ReasoningEffort string;
  ServiceTier *string }`
**generic.go**
- `type GenericBackendTranslator struct { /* shared policy deps */ }`
- Shared methods (promoted to embedders): request-shaping basics,
  system→instructions assembly, tool-arg policy application
  (`ToolArgPolicy`), response-gate sanitization hooks.
**tool_arg_policy.go** — port of tool_arg_policy.rs:
`type PolicyEdit struct`; `func ApplyPolicies(...)`;
`func SanitizedToolArgsForKind(...)`.
**claude_response_gate.go** — port of claude_response_gate.rs:
`StructuredOutputSchemaFromConfig`, `CleanupStructuredOutputText...`,
`SanitizeResponseValue`, `SanitizeResponseText`.

> PROMPT-TEXT RULES (CLAUDE.md, binding): no prompt-text dependence, no
> message size/shape heuristics. Per AI_SLOP.md dispositions:
> - Slop 1 → deterministic metadata check only.
> - Slop 2 → deterministic checks (explicit request fields).
> - Slops 3 & 4 → detectors KEPT but carry a `// BUG(prompt-text):` marker;
>   revisit with real prompt samples.
> - Skill first-line check → carried as `// BUG(text-check):`; replace with
>   XML parsing of the skill payload when structure is confirmed.

## core/domain/claudecode/
**envelope.go** — port of claude_code_inclusion.rs (slop-free):
`type CommandEnvelope struct { Name, Body string }`;
`func ParseCommandEnvelope(text string) (*CommandEnvelope, bool)`;
`func ApplyInclusionPolicy(msgs []dto.Message) (InclusionResult)`;
`type InclusionResult struct { ReadOnly bool; LocalOnlyCommands []string }`
(structured tags + stdout markers only — no READ_ONLY_MARKERS).
**commands.go** — local-only command table + stdout markers; port of
`classify_claude_code_command` → `func ClassifyCommand(name string) Classification`.
**context.go** — port of claude_code_context.rs:
`func NormalizeContext(...)` (command promotion, directive injection,
skill-base-directory rewrite); `func GetPackagedCommandBody(name string) string`.

## core/domain/conversation/
**kind.go** — `type Kind string`; consts: VisibleMain, SubagentOffshoot,
PermissionClassifier, HookEvaluator, LocalControl, StatusOrAuxiliary,
UnknownOffshoot; `PersistenceReason()`, `IsVisibleMain()`.
⚠ FINAL SIGNALS PARKED (classification-signal-redesign.md); structural-only
interim.
**identity.go** — `type Identity struct { ClaudeSessionID, BranchID string;
Kind Kind; ProviderModelFingerprint, ReasoningEffort string }`;
`func (i Identity) Key() string`;
`func (i Identity) SessionKey() port.SessionKey`;
`func (i Identity) CheckpointKey(responseID string) string`.
**classifier.go** — `type Classifier interface { Classify(req
*dto.MessagesRequest, meta map[string]string) Kind }` (structural).

## core/domain/transport/
**selector.go** — `type Selector interface { Select(ctx, Plan) (Decision,
error) }`; `type Plan struct {...}`; `type Decision struct { UseWS bool;
Chain ChainID; Reason string }`.
**lease.go** — CORE, port verbatim:
`type LeaseState string` — InFlight, CompletedCommitted,
ClientAbortedBeforeFirstEvent, ClientAbortedAfterVisibleOutput,
BackendFailedBeforeCommit, CommitSuppressedAfterAbort;
`func (s LeaseState) AllowsCommit() bool`;
`type LeaseStore interface {
    Acquire(identity Identity, reqID string) (LeaseAcquire)
    Commit(identity Identity, reqID string, transition LeaseState) error
    Release(identity Identity, reqID string)
}` — ✱G2 optional persisted variant + TTL startup sweep.
**chain_registry.go** — `type ChainRegistry interface {
Associate(identity Identity, responseID string, chain ChainID);
Lookup(identity Identity, responseID string) (ChainID, bool) }`
— ✱G1 optional persisted variant into branch metadata.

## core/domain/contextmgmt/
**manager.go** — port of context_management.rs:
`type Manager struct`; `func New(cfg config.ContextManagementConfig) *Manager`;
`func (m *Manager) Apply(req *dto.MessagesRequest) (*dto.MessagesRequest, Report)`;
`type Report struct`; `func (r Report) IsEmpty() bool`;
`ResponseValue()`, `MetadataValue()`.

## core/impl/
**services/message_service.go** — orchestrator implementing
services.MessageService; steps 1-9 per ARCHITECTURE_v2.
**services/stream_writer.go** — the single writer goroutine: owns
echo.Response, `WriteEvent(dto.SSEEvent) error` (write+flush),
✱G4 idle-event timeout; hands accumulated events to logger at close.
**services/models_service.go / auth_status_service.go / count_tokens.go**
**port/backend/codex/client.go** — `type Client struct`;
`func New(cfg Config, auth portauth.Provider, http *netpolicy.Client) *Client`;
implements port.Backend (SendUnary, SendStream, Capabilities,
EvictSession, HasLiveSession, LiveChainID) + refresh-retry-once;
✱G4 default 120s unary timeout.
**port/backend/codex/sse.go** — SSE decode → chan BackendEvent.
**port/backend/codex/wspool.go** — pooled WS sessions, keepalive
(✱G10 config), chain IDs, eviction.
**port/auth/codexauth/store.go** — implements port auth.Provider;
auth.json read/write, JWT exp, refresh, revoke; path resolution
(GATEWAY_AUTH_JSON_PATH / GATEWAY_HOME).
**port/auth/codexauth/login.go** — PKCE localhost-callback flow
(+ logout); API: `func RunLogin(ctx, opts LoginOpts) error`.
**port/state/conversation/fs.go** — implements ConversationRepo
(filesystem JSON/JSONL/checkpoints, per-session lock registry,
corruption policy, retention; ✱G6 second-instance lockfile).
**port/state/toolcalls/gorm.go** — implements ToolCallRepo via
GORM+glebarez; `func Open(dsn string) (*Store, error`.
**translator/codex/translator.go** — `type OpenAITranslator struct
{ *translator.GenericBackendTranslator }`; compile assert
`var _ translator.BackendTranslator = (*OpenAITranslator)(nil)`.

## handlers/
**messages.go** — `type MessagesHandler struct`;
`func NewMessagesHandler(s services.MessageService, l ExchangeLog) *MessagesHandler`;
`func (h *MessagesHandler) Post(c echo.Context) error` — bind (pedantigoecho),
call service, unary JSON or hand stream to writer goroutine (Option C log).
**count_tokens.go**, **models.go**, **health.go**, **auth.go** — same shape.

## middleware/
**requestid.go** — `func RequestID() echo.MiddlewareFunc`
**recovery.go** — `func Recover() echo.MiddlewareFunc` (→ AppError shape)
**capture.go** — unary-only exchange capture (NO writer wrapping on
streaming routes).

## observability/
**exchange.go** — `type ExchangeLog interface { Append(entry Entry) error }`
**format.go** — user-specified text format writer:
`key: value` lines + `------------------------------------` separator.
✱G9 rotation; ✱G7 failure logging.
**redact.go** — port of redaction.
**transport_diag.go** — JSONL sink for transport decisions
(`~/.gateway/logs/transport-decisions.jsonl`).

## config/
**config.go** — viper load; `type Config struct` (Workflow/Providers/
Network); `func Load(path string) (*Config, error)`;
`func DefaultPath() string`; providers map keyed by backend name with
`active: true` on one.
**models.go** — `func ResolveModel(c *Config, requested string) Resolution`;
`func ServiceTier(c *Config) *string`.

## core/ (root)
**ids.go** — `type RequestID string`; `func NewRequestID() RequestID`
**secret.go** — `type Secret string`; `func (s Secret) Expose() string`
**errors.go** — error-chain helper.

## tui/
**login.go** — bubbletea model for vendor picker; `func RunLoginSelector()
(Vendor, error)`.

## File count: 41 files.
