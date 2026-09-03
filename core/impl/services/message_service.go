// Package services implements the core/domain/services use-case
// interfaces. MessageService here is the /v1/messages orchestrator: the
// 9-step flow in ARCHITECTURE_v2.md ("Request flow (POST /v1/messages)"),
// ported from crates/gateway-http-anthropic/src/lib.rs's unary flow
// (prepare_unary_message_flow.. commit_unary_result, lib.rs:1076-1547) and
// its streaming mirror (prepare_stream_message_flow..
// maybe_commit_stream_completion, lib.rs:2742-3318).
package services

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"strings"
	"time"

	"github.com/google/uuid"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/core/domain/claudecode"
	"github.com/Bytonomics/cld-gateway/core/domain/contextmgmt"
	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
	backendport "github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	stateport "github.com/Bytonomics/cld-gateway/core/domain/port/state"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
	translatorpkg "github.com/Bytonomics/cld-gateway/core/domain/translator"
	"github.com/Bytonomics/cld-gateway/core/domain/transport"
)

// contextKey namespaces the values MessageService reads off ctx. The
// handlers wave (not part of this task) is expected to populate these via
// WithClaudeSessionID / WithRequestID before calling Handle - typically from
// a Claude-Code-emitted session header and the request-id middleware
// respectively.
type contextKey string

const (
	claudeSessionIDContextKey contextKey = "cld_gateway_claude_session_id"
	requestIDContextKey       contextKey = "cld_gateway_request_id"
)

// WithClaudeSessionID attaches the Claude Code session id that scopes
// conversation-state branch selection for this request.
func WithClaudeSessionID(ctx context.Context, sessionID string) context.Context {
	return context.WithValue(ctx, claudeSessionIDContextKey, sessionID)
}

// ClaudeSessionIDFromContext reads back the id WithClaudeSessionID set. ok
// is false when absent or empty, which MessageService treats as "this turn
// is not a persisted Claude Code conversation" (no branch selection, no
// lease, no state commit) rather than an error - matching the shape of
// Rust's reject_missing_conversation_branch, which only errors when a
// session id IS present but branch preparation still fails.
func ClaudeSessionIDFromContext(ctx context.Context) (string, bool) {
	v, _ := ctx.Value(claudeSessionIDContextKey).(string)
	return v, v != ""
}

// WithRequestID attaches the per-request id used as the lease's request-id
// discriminant and for transport-selection diagnostics.
func WithRequestID(ctx context.Context, id core.RequestID) context.Context {
	return context.WithValue(ctx, requestIDContextKey, id)
}

// RequestIDFromContext reads back the id WithRequestID set, minting a new
// one when absent so Handle never operates without a lease discriminant.
func RequestIDFromContext(ctx context.Context) core.RequestID {
	if v, ok := ctx.Value(requestIDContextKey).(core.RequestID); ok && v != "" {
		return v
	}
	return core.NewRequestID()
}

// Deps are MessageService's constructor-injected collaborators. Every field
// is a port interface (or a concrete domain-owned type, per
// ARCHITECTURE_v2.md's "state stays core, not a pluggable port" call for
// ConversationRepo/ToolCallRepo) so app/initialize.go can wire one concrete
// backend at a time with zero edits to this file.
type Deps struct {
	Config        *config.Config
	Classifier    conversation.Classifier
	ContextMgmt   *contextmgmt.Manager
	Translator    translatorpkg.BackendTranslator
	Backend       backendport.Backend
	Selector      transport.Selector
	Leases        transport.LeaseStore
	Chains        transport.ChainRegistry
	Conversations stateport.ConversationRepo
	ToolCalls     stateport.ToolCallRepo
	Clock         stateport.Clock
}

// MessageService implements services.MessageService.
type MessageService struct {
	deps Deps
}

var _ services.MessageService = (*MessageService)(nil)

// New constructs a MessageService. deps.Classifier defaults to
// conversation.StructuralClassifier{} when nil.
func New(deps Deps) *MessageService {
	if deps.Classifier == nil {
		deps.Classifier = conversation.StructuralClassifier{}
	}
	return &MessageService{deps: deps}
}

// turnPlan is everything steps 1-7 resolve, shared by the unary and
// streaming tails (steps 8-9).
type turnPlan struct {
	requestID       core.RequestID
	claudeSessionID string
	persisted       bool // conversation-state branch selection/commit applies to this turn

	kind              conversation.Kind
	resolution        config.ModelResolution
	reasoningEffort   string
	identity          conversation.Identity
	branchID          string
	commitTurn        bool // visible-main durable commit vs. offshoot checkpoint
	fingerprints      stateport.BranchFingerprintSet
	canonicalMsgs     any
	canonicalMsgHash  string
	contextMgmtReport contextmgmt.Report

	backendReq         *backendport.Request
	previousResponseID *string // populated by selectTransport, passed to Acquire
	leaseHeld          bool
	warnings           []dto.Warning
}

// Handle ports the 9-step flow. Steps 1-7 are shared (prepare); step 8/9
// diverge for unary vs. streaming per ARCHITECTURE_v2.md.
func (s *MessageService) Handle(ctx context.Context, req *dto.MessagesRequest) services.MessageResult {
	plan, workingReq, appErr := s.prepare(ctx, req)
	if appErr != nil {
		return services.MessageResult{Err: appErr}
	}

	if req.Stream {
		return s.handleStream(ctx, plan, workingReq)
	}
	return s.handleUnary(ctx, plan, workingReq)
}

// prepare runs steps 1-7: classify kind, normalize Claude Code context
// (for classification/branch fingerprinting - the translator normalizes
// again independently for translation, mirroring Rust's two call sites:
// lib.rs:3893 and translate.rs:73), apply context management, resolve
// model, select/create the conversation branch, translate, select
// transport, and acquire the main-turn lease.
func (s *MessageService) prepare(ctx context.Context, req *dto.MessagesRequest) (*turnPlan, *dto.MessagesRequest, error) {
	plan := &turnPlan{
		requestID:       RequestIDFromContext(ctx),
		claudeSessionID: "",
	}
	sessionID, hasSession := ClaudeSessionIDFromContext(ctx)
	plan.claudeSessionID = sessionID

	normalized := claudecode.NormalizeContext(req.System, req.Messages, s.deps.Config.Workflow.ClaudeCode)

	classifyReq := *req
	classifyReq.System = normalized.System
	classifyReq.Messages = normalized.Messages
	plan.kind = s.deps.Classifier.Classify(&classifyReq, normalized.ClientMetadata)

	workingReq := *req
	workingReq.System = normalized.System
	workingReq.Messages = normalized.Messages
	if s.deps.ContextMgmt != nil {
		_, plan.contextMgmtReport = s.deps.ContextMgmt.Apply(&workingReq)
	}

	plan.resolution = config.ResolveModel(s.deps.Config, req.Model)
	plan.reasoningEffort = reasoningEffortOf(req)

	plan.persisted = hasSession && s.deps.Config.Workflow.ConversationState.Enabled && s.deps.Conversations != nil

	plan.fingerprints = branchFingerprints(workingReq.Messages)
	plan.canonicalMsgs = canonicalValue(workingReq.Messages)
	plan.canonicalMsgHash = canonicalHash(workingReq.Messages)

	var selection stateport.BranchSelectionResult
	if plan.persisted {
		if _, err := s.deps.Conversations.EnsureSession(ctx, sessionID); err != nil {
			return nil, nil, apperr.Wrap(err, apperr.CodeGatewayState, "ensure claude session", 500)
		}
		turnScope := stateport.ConversationTurnScopeSide
		if plan.kind.IsVisibleMain() {
			turnScope = stateport.ConversationTurnScopeMain
		}
		sel, err := s.deps.Conversations.SelectOrCreateBranch(ctx, sessionID, stateport.BranchSelectionInput{
			ActiveCanonicalMessages: plan.canonicalMsgs,
			Fingerprints:            plan.fingerprints,
			TurnScope:               turnScope,
		})
		if err != nil {
			return nil, nil, apperr.Wrap(err, apperr.CodeGatewayState, "select conversation branch", 500)
		}
		selection = sel
		plan.branchID = sel.Branch.BranchID
		plan.commitTurn = plan.kind.IsVisibleMain()
	}

	plan.identity = conversation.Identity{
		ClaudeSessionID:          sessionID,
		BranchID:                 plan.branchID,
		Kind:                     plan.kind,
		ProviderModelFingerprint: plan.resolution.SelectedBackendModel,
		ReasoningEffort:          plan.reasoningEffort,
	}

	meta := translatorpkg.TranslateMeta{
		Model:           plan.resolution.SelectedBackendModel,
		ReasoningEffort: plan.reasoningEffort,
		ServiceTier:     config.ServiceTier(s.deps.Config),
	}
	backendReq, err := s.deps.Translator.TranslateRequest(ctx, &workingReq, meta)
	if err != nil {
		return nil, nil, apperr.Wrap(err, apperr.CodeInvalidRequest, "translate request", 400)
	}
	plan.backendReq = backendReq

	if plan.persisted {
		s.selectTransport(ctx, plan, selection)
	}

	if plan.persisted && plan.commitTurn {
		acquire := s.deps.Leases.Acquire(plan.identity, string(plan.requestID), plan.previousResponseID)
		if !acquire.Acquired {
			return nil, nil, apperr.New(apperr.CodeOverloaded, "a visible conversation turn for this branch is already in flight", 503)
		}
		plan.leaseHeld = true
	}

	return plan, &workingReq, nil
}

// selectTransport ports select_message_transport (lib.rs:1144-1195): decide
// WS-delta vs. full-SSE reuse per invariant #2 and set previous_response_id
// on the outgoing backend request only when reuse is sanctioned. Also stores
// the previousResponseID on plan for later use by Acquire.
func (s *MessageService) selectTransport(ctx context.Context, plan *turnPlan, selection stateport.BranchSelectionResult) {
	hasCheckpoint := selection.Branch.OpenAiCheckpoint != nil
	previousResponseID := ""
	if hasCheckpoint {
		previousResponseID = selection.Branch.OpenAiCheckpoint.ResponseID
	}
	plan.previousResponseID = &previousResponseID

	sessionKey := plan.identity.SessionKey()
	liveChain, hasLiveChain := s.deps.Backend.LiveChainID(sessionKey)

	planIn := transport.Plan{
		Identity:           plan.identity,
		RequestID:          string(plan.requestID),
		HasCheckpoint:      hasCheckpoint,
		PreviousResponseID: previousResponseID,
		SessionKey:         sessionKey,
		HasLiveChain:       hasLiveChain,
		LiveChain:          liveChain,
	}
	decision, err := s.deps.Selector.Select(ctx, planIn)
	if err != nil {
		plan.warnings = append(plan.warnings, dto.Warning{
			Code:    "delta_calculation_failed",
			Message: "[CLD-Gateway] Could not determine whether an incremental update could be sent; sent full conversation history instead.",
		})
		return
	}
	if !decision.UseWS {
		if hasCheckpoint {
			plan.warnings = append(plan.warnings, dto.Warning{
				Code:    "delta_calculation_skipped",
				Message: "[CLD-Gateway] Sent full conversation history instead of an incremental update for this turn.",
			})
		}
		return
	}
	plan.backendReq.PreviousResponseID = &previousResponseID
}

func reasoningEffortOf(req *dto.MessagesRequest) string {
	if req.OutputConfig != nil && req.OutputConfig.Effort != nil && *req.OutputConfig.Effort != "" {
		return *req.OutputConfig.Effort
	}
	return "medium"
}

// handleUnary ports execute_unary_backend + commit_unary_result
// (lib.rs:1227-1547).
func (s *MessageService) handleUnary(ctx context.Context, plan *turnPlan, workingReq *dto.MessagesRequest) services.MessageResult {
	resp, err := s.deps.Backend.SendUnary(ctx, plan.backendReq)
	if err != nil {
		s.releaseLease(plan, transport.BackendFailedBeforeCommit)
		wrapped := apperr.Wrap(err, apperr.CodeAPI, err.Error(), 502)
		wrapped.Provider = s.deps.Config.Providers.Active
		wrapped.Model = plan.backendReq.Model
		return services.MessageResult{Err: wrapped}
	}

	// Promote WebSocket chain on the lease if one was acquired (only when persisted && commitTurn).
	// This must happen after the backend call succeeds and we can obtain the live chain.
	if plan.leaseHeld {
		if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok {
			s.deps.Leases.PromoteWebSocketChain(plan.identity, string(plan.requestID), &chain)
		}
	}

	events := decodeSSEBody(resp.Body)
	unary, err := s.deps.Translator.BuildUnaryResponse(events)
	if err != nil {
		s.releaseLease(plan, transport.BackendFailedBeforeCommit)
		return services.MessageResult{Err: apperr.Wrap(err, apperr.CodeAPI, "build response", 502)}
	}

	if v := plan.contextMgmtReport.ResponseValue(); v != nil {
		unary.ContextManagement = v
	}

	if ctx.Err() != nil {
		s.releaseLease(plan, transport.ClientAbortedAfterVisibleOutput)
		return services.MessageResult{Err: ctx.Err()}
	}

	// Validate lease before committing: gate check mirrors Rust's
	// validate_unary_lease_for_commit (lib.rs:1335-1349), which only gates
	// the visible-main commit path (a lease is only ever held for that
	// path); the offshoot-checkpoint path has no lease and commits
	// unconditionally (commit_unary_offshoot_result, lib.rs:1498-1537,
	// called regardless of the main lease's validation outcome).
	if plan.leaseHeld {
		var chainID *backendport.ChainID
		if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok {
			chainID = &chain
		}
		validation := s.deps.Leases.ValidateForCommit(plan.identity, string(plan.requestID), chainID)
		if !validation.Accepted {
			s.releaseLease(plan, transport.CommitSuppressedAfterAbort)
			unary.Warnings = plan.warnings
			return services.MessageResult{
				Unary:                     unary,
				ContextManagementMetadata: plan.contextMgmtReport.MetadataValue(),
			}
		}
	}

	s.commitTurn(ctx, plan, workingReq, unary)
	s.releaseLease(plan, transport.CompletedCommitted)

	unary.Warnings = plan.warnings
	return services.MessageResult{
		Unary:                     unary,
		ContextManagementMetadata: plan.contextMgmtReport.MetadataValue(),
	}
}

// handleStream ports prepare_stream_message_flow.. build_stream_sse
// (lib.rs:2742-2933) for producing SSE events, and
// maybe_commit_stream_completion / stream_lease_allows_commit
// (lib.rs:3154-3332) for the lease-gated commit: state is committed only
// when the lease is still in a commit-allowing state when the stream ends,
// never unconditionally.
func (s *MessageService) handleStream(ctx context.Context, plan *turnPlan, workingReq *dto.MessagesRequest) services.MessageResult {
	backendEvents, err := s.deps.Backend.SendStream(ctx, plan.backendReq)
	if err != nil {
		s.releaseLease(plan, transport.BackendFailedBeforeCommit)
		wrapped := apperr.Wrap(err, apperr.CodeAPI, err.Error(), 502)
		wrapped.Provider = s.deps.Config.Providers.Active
		wrapped.Model = plan.backendReq.Model
		return services.MessageResult{Err: wrapped}
	}

	out := make(chan dto.SSEEvent)
	go s.runStream(ctx, plan, workingReq, backendEvents, out)
	return services.MessageResult{
		Stream:                    out,
		ContextManagementMetadata: plan.contextMgmtReport.MetadataValue(),
	}
}

func (s *MessageService) runStream(ctx context.Context, plan *turnPlan, workingReq *dto.MessagesRequest, in <-chan backendport.Event, out chan<- dto.SSEEvent) {
	defer close(out)

	var accumulated []backendport.Event
	visibleOutputSent := false
	aborted := false

	// message_start must open every streaming response, unconditionally,
	// before anything derived from the backend is relayed - sent here,
	// not from inside the backend-event loop below, so its timing never
	// depends on how fast the backend responds (ports
	// anthropic_stream_start_events + build_stream_sse's ordering,
	// lib.rs:2617-2641,2902-2906; see translator.BuildStreamStartEvents's
	// doc comment for why TranslateResponseEvent cannot own this itself).
	msgID := "msg_" + uuid.NewString()
	for _, startEvent := range translatorpkg.BuildStreamStartEvents(msgID, workingReq.Model, plan.warnings) {
		select {
		case out <- startEvent:
			visibleOutputSent = true
		case <-ctx.Done():
			aborted = true
		}
		if aborted {
			break
		}
	}

	if aborted {
		s.releaseLease(plan, transport.ClientAbortedBeforeFirstEvent)
		return
	}

	for ev := range in {
		accumulated = append(accumulated, ev)

		sseEvents, err := s.deps.Translator.TranslateResponseEvent(ev)
		if err != nil {
			continue
		}
		for _, sse := range sseEvents {
			select {
			case out <- sse:
				visibleOutputSent = true
			case <-ctx.Done():
				aborted = true
			}
			if aborted {
				break
			}
		}
		if aborted {
			break
		}
	}

	if aborted || ctx.Err() != nil {
		if visibleOutputSent {
			s.releaseLease(plan, transport.ClientAbortedAfterVisibleOutput)
		} else {
			s.releaseLease(plan, transport.ClientAbortedBeforeFirstEvent)
		}
		return
	}

	// Promote WebSocket chain on the lease if one was acquired (only when persisted && commitTurn).
	// This must happen after the backend stream completes and we can obtain the live chain.
	if plan.leaseHeld {
		if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok {
			s.deps.Leases.PromoteWebSocketChain(plan.identity, string(plan.requestID), &chain)
		}
	}

	unary, err := s.deps.Translator.BuildUnaryResponse(accumulated)
	if err != nil {
		s.releaseLease(plan, transport.BackendFailedBeforeCommit)
		return
	}

	if v := plan.contextMgmtReport.ResponseValue(); v != nil {
		unary.ContextManagement = v
	}

	// Validate lease before committing: gate check mirrors Rust's
	// validate_unary_lease_for_commit (lib.rs:1335-1349), which only gates
	// the visible-main commit path (a lease is only ever held for that
	// path); the offshoot-checkpoint path has no lease and commits
	// unconditionally (commit_unary_offshoot_result, lib.rs:1498-1537,
	// called regardless of the main lease's validation outcome).
	if plan.leaseHeld {
		var chainID *backendport.ChainID
		if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok {
			chainID = &chain
		}
		validation := s.deps.Leases.ValidateForCommit(plan.identity, string(plan.requestID), chainID)
		if !validation.Accepted {
			s.releaseLease(plan, transport.CommitSuppressedAfterAbort)
			return
		}
	}

	s.commitTurn(ctx, plan, workingReq, unary)
	s.releaseLease(plan, transport.CompletedCommitted)
}

// releaseLease ports release_unary_lease / stream_lease_allows_commit's
// release path: transition the lease to its terminal state (if one is
// held) and release it. transition is a no-op when no lease was acquired
// for this turn (non-persisted or non-visible-main turns never acquire
// one).
func (s *MessageService) releaseLease(plan *turnPlan, transition transport.LeaseState) {
	if !plan.leaseHeld {
		return
	}
	_ = s.deps.Leases.Commit(plan.identity, string(plan.requestID), transition)
	s.deps.Leases.Release(plan.identity, string(plan.requestID))
}

// commitTurn ports commit_unary_result / maybe_commit_stream_completion:
// commit the visible-main turn (ConversationRepo.CommitTurn) or record an
// offshoot checkpoint (ConversationRepo.CommitOffshootCheckpoint), then
// record any tool_use blocks in the response via ToolCallRepo. Only called
// once the lease-commit gate has already confirmed this turn may commit
// (releaseLease's caller only reaches here on the success path).
func (s *MessageService) commitTurn(ctx context.Context, plan *turnPlan, workingReq *dto.MessagesRequest, unary *dto.MessagesResponse) {
	if !plan.persisted || plan.branchID == "" {
		s.recordToolCalls(ctx, plan, unary)
		return
	}

	now := s.now()
	responseID := unary.ID
	fingerprint := plan.resolution.SelectedBackendModel
	compatFingerprint := requestCompatibilityFingerprint(s.deps.Config, plan.resolution, plan.backendReq)
	messageCount := uint64(len(workingReq.Messages))

	if plan.commitTurn {
		params := stateport.CommitTurnParams{
			TurnScope:                       stateport.ConversationTurnScopeMain,
			TurnID:                          "turn_" + core.NewRequestID().String(),
			Fingerprints:                    plan.fingerprints,
			ActiveCanonicalMessages:         plan.canonicalMsgs,
			ProviderResponseID:              strPtr(responseID),
			ProviderModelFingerprint:        strPtr(fingerprint),
			RequestCompatibilityFingerprint: strPtr(compatFingerprint),
			CanonicalMessageCount:           &messageCount,
			CanonicalPrefixHash:             strPtr(plan.canonicalMsgHash),
		}
		if plan.backendReq.PreviousResponseID != nil {
			params.PreviousResponseID = plan.backendReq.PreviousResponseID
		}
		if _, err := s.deps.Conversations.CommitTurn(ctx, plan.claudeSessionID, plan.branchID, params); err == nil {
			if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok && responseID != "" {
				s.deps.Chains.Associate(plan.identity, responseID, chain)
			}
		}
	} else if responseID != "" {
		params := stateport.CommitOffshootCheckpointParams{
			OffshootIdentity:                plan.identity.Key(),
			ProviderResponseID:              responseID,
			ProviderModelFingerprint:        fingerprint,
			RequestCompatibilityFingerprint: strPtr(compatFingerprint),
		}
		if plan.backendReq.PreviousResponseID != nil {
			params.PreviousResponseID = plan.backendReq.PreviousResponseID
		}
		_ = s.deps.Conversations.CommitOffshootCheckpoint(ctx, plan.claudeSessionID, plan.branchID, params)
		if chain, ok := s.deps.Backend.LiveChainID(plan.identity.SessionKey()); ok {
			s.deps.Chains.Associate(plan.identity, responseID, chain)
		}
	}

	_ = now
	s.recordToolCalls(ctx, plan, unary)
}

func (s *MessageService) recordToolCalls(ctx context.Context, plan *turnPlan, unary *dto.MessagesResponse) {
	if s.deps.ToolCalls == nil || unary == nil {
		return
	}
	reqIDStr := string(plan.requestID)
	// Retrieve tool call kinds from the translator that were extracted during
	// response processing. Keyed by call_id, values are canonical wire-format
	// strings like "function_call", "custom_tool_call", etc.
	toolCallKinds := s.deps.Translator.GetToolCallKinds()
	for _, block := range unary.Content {
		if block.BlockType != "tool_use" || block.ID == nil {
			continue
		}
		name := ""
		if block.Name != nil {
			name = *block.Name
		}
		// Retrieve the tool call kind for this call ID. Defaults to "function_call"
		// if not found (matching the database default).
		kind := "function_call"
		if k, ok := toolCallKinds[*block.ID]; ok {
			kind = k
		}
		_ = s.deps.ToolCalls.RecordToolCall(ctx, stateport.StoredToolCall{
			CallID:               *block.ID,
			ToolName:             name,
			ToolKind:             kind,
			RequestID:            &reqIDStr,
			CreatedAtUnixSeconds: s.now().Unix(),
		})
	}
}

func (s *MessageService) now() time.Time {
	if s.deps.Clock != nil {
		return s.deps.Clock.Now()
	}
	return time.Now()
}

func strPtr(s string) *string { return &s }

// canonicalValue mirrors canonical_message_value/canonical_content_value
// (lib.rs:4347-4398): field-by-field reconstruction of message content into
// a stable structural JSON projection suitable for storing as
// ActiveCanonicalMessages and for fingerprinting/hashing.
func canonicalValue(messages []dto.Message) any {
	var canonical []any
	for _, msg := range messages {
		canonical = append(canonical, canonicalMessageValue(msg))
	}
	return canonical
}

// canonicalMessageValue mirrors canonical_message_value (lib.rs:4347-4352).
func canonicalMessageValue(message dto.Message) map[string]any {
	return map[string]any{
		"role":    message.Role,
		"content": canonicalContentValue(message.Content),
	}
}

// canonicalContentValue mirrors canonical_content_value (lib.rs:4354-4366).
func canonicalContentValue(content dto.Content) any {
	if content.Text != nil {
		// Text content becomes a single-element text block array
		return []any{
			map[string]any{
				"type": "text",
				"text": *content.Text,
			},
		}
	}
	// Blocks content: transform each block
	var blocks []any
	for _, block := range content.Blocks {
		blocks = append(blocks, canonicalContentBlockValue(block))
	}
	return blocks
}

// canonicalContentBlockValue mirrors canonical_content_block_value (lib.rs:4368-4397).
func canonicalContentBlockValue(block dto.ContentBlock) map[string]any {
	object := make(map[string]any)
	object["type"] = block.BlockType

	// Insert optional string fields
	if block.Text != nil {
		object["text"] = *block.Text
	}
	if block.ID != nil {
		object["id"] = *block.ID
	}
	if block.Name != nil {
		object["name"] = *block.Name
	}

	// Insert optional value fields
	if block.Input != nil {
		object["input"] = block.Input
	}
	if block.ToolUseID != nil {
		object["tool_use_id"] = *block.ToolUseID
	}
	if block.Content != nil {
		object["content"] = block.Content
	}

	// Insert is_error if present
	if block.IsError != nil {
		object["is_error"] = *block.IsError
	}

	// Insert source if present, stripping transient metadata
	if block.Source != nil {
		sourceValue, err := json.Marshal(block.Source)
		if err == nil {
			var sourceJSON any
			if err := json.Unmarshal(sourceValue, &sourceJSON); err == nil {
				object["source"] = stripTransientMessageMetadata(sourceJSON)
			}
		}
	}

	// Insert extra fields (except cache_control)
	for key, value := range block.Extra {
		if key != "cache_control" {
			var extraValue any
			if err := json.Unmarshal(value, &extraValue); err == nil {
				object[key] = stripTransientMessageMetadata(extraValue)
			}
		}
	}

	return object
}

// stripTransientMessageMetadata ports strip_transient_message_metadata
// (lib.rs:4425-4442): recursively drop "cache_control" keys before a
// canonical message snapshot is persisted to conversation state.
func stripTransientMessageMetadata(value any) any {
	switch v := value.(type) {
	case map[string]any:
		out := make(map[string]any, len(v))
		for k, val := range v {
			if k == "cache_control" {
				continue
			}
			out[k] = stripTransientMessageMetadata(val)
		}
		return out
	case []any:
		out := make([]any, len(v))
		for i, val := range v {
			out[i] = stripTransientMessageMetadata(val)
		}
		return out
	default:
		return v
	}
}

func canonicalHash(messages []dto.Message) string {
	value := canonicalValue(messages)
	encoded, err := json.Marshal(value)
	if err != nil {
		return ""
	}
	sum := sha256.Sum256(encoded)
	return hex.EncodeToString(sum[:])
}

// branchFingerprints ports branch_fingerprints_from_messages (lib.rs:4932-5001):
// extract text content from messages, hash various subsets for branch selection.
func branchFingerprints(messages []dto.Message) stateport.BranchFingerprintSet {
	var textMessages []string
	var lastUserText *string

	// Extract text content from each message, building "role:text" strings
	for _, message := range messages {
		text := extractMessageText(message)
		if strings.TrimSpace(text) == "" {
			continue
		}
		if message.Role == "user" {
			lastUserText = &text
		}
		textMessages = append(textMessages, message.Role+":"+strings.TrimSpace(text))
	}

	// Calculate recent tail: last 4 messages
	recentTail := ""
	if len(textMessages) > 0 {
		start := len(textMessages) - 4
		if start < 0 {
			start = 0
		}
		recentTail = strings.Join(textMessages[start:], "\n")
	}

	// Full transcript
	fullTranscript := strings.Join(textMessages, "\n")

	// Hash components
	var recentTailHash *string
	if recentTail != "" {
		h := sha256TextHex(recentTail)
		recentTailHash = &h
	}

	var lastUserMessageHash *string
	if lastUserText != nil && strings.TrimSpace(*lastUserText) != "" {
		h := sha256TextHex(*lastUserText)
		lastUserMessageHash = &h
	}

	var branchStateHash *string
	if fullTranscript != "" {
		h := sha256TextHex(fullTranscript)
		branchStateHash = &h
	}

	return stateport.BranchFingerprintSet{
		RecentMessageTailHash: recentTailHash,
		LastUserMessageHash:   lastUserMessageHash,
		BranchStateHash:       branchStateHash,
	}
}

// extractMessageText ports anthropic_message_text (lib.rs:5030-5040):
// extract text content from a message, joining text blocks with double newlines.
func extractMessageText(message dto.Message) string {
	if message.Content.Text != nil {
		return *message.Content.Text
	}
	var textParts []string
	for _, block := range message.Content.Blocks {
		if block.BlockType == "text" && block.Text != nil && *block.Text != "" {
			textParts = append(textParts, *block.Text)
		}
	}
	return strings.Join(textParts, "\n\n")
}

// sha256TextHex hashes text content to hex string (mirrors sha256_hex, lib.rs:5102-5106).
func sha256TextHex(text string) string {
	sum := sha256.Sum256([]byte(text))
	return hex.EncodeToString(sum[:])
}

// requestCompatibilityFingerprint ports request_compatibility_fingerprint
// (lib.rs:1813-1832): fold model, tools, reasoning, and service-tier into
// a compatibility hash for transport reuse decisions.
func requestCompatibilityFingerprint(
	cfg *config.Config,
	resolution config.ModelResolution,
	backendReq *backendport.Request,
) string {
	payload := map[string]any{
		"renderer_version":       "gateway_http_anthropic_transport_v1",
		"selected_backend_model": resolution.SelectedBackendModel,
		"instructions":           backendReq.Instructions,
		"tools":                  backendReq.Tools,
		"tool_choice":            backendReq.ToolChoice,
		"parallel_tool_calls":    backendReq.ParallelToolCalls,
		"text":                   backendReq.Text,
		"reasoning":              backendReq.Reasoning,
		"include":                backendReq.Include,
		"service_tier":           config.ServiceTier(cfg),
	}
	encoded, err := json.Marshal(payload)
	if err != nil {
		return ""
	}
	sum := sha256.Sum256(encoded)
	return hex.EncodeToString(sum[:])
}

// decodeSSEBody parses a fully-buffered EventSource-formatted response body
// (the shape backend.Backend.SendUnary returns per its "Body" field, since
// the Codex backend always answers with Accept: text/event-stream) into
// backend.Event frames. This mirrors codex.DecodeEventStream's framing
// rules (core/impl/port/backend/codex/sse.go) synchronously over an
// in-memory string rather than a channel/io.Reader, so this package does
// not import the codex backend package (core stays backend-agnostic; a
// second backend implementation would only need to also return an
// EventSource-formatted body, or this helper generalizes if it doesn't).
func decodeSSEBody(body string) []backendport.Event {
	var events []backendport.Event
	var dataLines []string

	flush := func() {
		if len(dataLines) == 0 {
			return
		}
		data := strings.Join(dataLines, "\n")
		dataLines = nil
		var probe struct {
			Type string `json:"type"`
		}
		_ = json.Unmarshal([]byte(data), &probe)
		events = append(events, backendport.Event{Type: probe.Type, Data: json.RawMessage(data)})
	}

	for _, line := range strings.Split(body, "\n") {
		line = strings.TrimSuffix(line, "\r")
		switch {
		case line == "":
			flush()
		case strings.HasPrefix(line, ":"):
			// SSE comment/keep-alive line; ignored.
		case strings.HasPrefix(line, "data:"):
			dataLines = append(dataLines, strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
		default:
			// Other SSE fields (event:, id:, retry:) ignored; Type comes from
			// the JSON payload, matching backend.Event's documented contract.
		}
	}
	flush()

	return events
}
