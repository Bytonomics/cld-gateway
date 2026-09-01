package app

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/contextmgmt"
	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	stateport "github.com/Bytonomics/cld-gateway/core/domain/port/state"
	translatorpkg "github.com/Bytonomics/cld-gateway/core/domain/translator"
	"github.com/Bytonomics/cld-gateway/core/domain/transport"
	"github.com/Bytonomics/cld-gateway/core/impl/port/auth/codexauth"
	codexbackend "github.com/Bytonomics/cld-gateway/core/impl/port/backend/codex"
	conversationstate "github.com/Bytonomics/cld-gateway/core/impl/port/state/conversation"
	toolcallsstate "github.com/Bytonomics/cld-gateway/core/impl/port/state/toolcalls"
	coreimplservices "github.com/Bytonomics/cld-gateway/core/impl/services"
	codextranslator "github.com/Bytonomics/cld-gateway/core/impl/translator/codex"
	"github.com/Bytonomics/cld-gateway/netpolicy"
	"github.com/Bytonomics/cld-gateway/observability"
)

// systemClock is the real-time stateport.Clock every store/service defaults
// to when Providers.Clock is not overridden for tests.
type systemClock struct{}

func (systemClock) Now() time.Time { return time.Now() }

// Initialize builds the full dependency graph for one gatewayd process:
// repos -> adapters -> services, in dependency order, mirroring
// ARCHITECTURE_v2.md's "manual constructor DI (no fx/wire, matching
// smritea-cloud style)". Every collaborator is wired through the
// core/domain port interfaces the earlier waves pinned; a second backend
// would only add a branch here.
func Initialize(cfg *config.Config) (*Providers, error) {
	clock := systemClock{}

	authProvider := codexauth.NewDefault()

	httpClient := netpolicy.New(cfg.Network.AllowedHosts)
	backendClient := codexbackend.New(codexbackend.Config{}, authProvider, httpClient)

	var conversations stateport.ConversationRepo
	if cfg.Workflow.ConversationState.Enabled {
		store, err := conversationstate.New(cfg.Workflow.ConversationState, clock)
		if err != nil {
			return nil, fmt.Errorf("init conversation state store: %w", err)
		}
		conversations = store
	}

	toolCalls, err := toolcallsstate.Open(toolcallsstate.DefaultToolCallsDBPath(), clock)
	if err != nil {
		return nil, fmt.Errorf("open tool calls store: %w", err)
	}

	transportDiag := observability.NewTransportDiagLog(observability.DefaultTransportDiagLogPath())
	exchangeLog := observability.NewFileExchangeLog(defaultExchangeLogPath())

	chainRegistry := transport.NewInMemoryChainRegistry(transportDiag)
	leaseStore := transport.NewInMemoryLeaseStore()
	selector := transport.NewSelector(chainRegistry, transportDiag)

	contextMgmt := contextmgmt.New(cfg.Workflow.ContextManagement)

	translator := codextranslator.New(
		cfg.Workflow.ClaudeCode,
		&toolCallKindLookup{repo: toolCalls},
		toolCalls,
		nil,
		nil,
		nil,
		"",
		clock,
	)

	messageService := coreimplservices.New(coreimplservices.Deps{
		Config:        cfg,
		Classifier:    conversation.StructuralClassifier{},
		ContextMgmt:   contextMgmt,
		Translator:    translator,
		Backend:       backendClient,
		Selector:      selector,
		Leases:        leaseStore,
		Chains:        chainRegistry,
		Conversations: conversations,
		ToolCalls:     toolCalls,
		Clock:         clock,
	})

	return &Providers{
		Config:               cfg,
		AuthService:          authProvider,
		MessageService:       messageService,
		CountTokensService:   coreimplservices.NewCountTokensService(),
		ModelsService:        coreimplservices.NewModelsService(""),
		AuthStatusService:    coreimplservices.NewAuthStatusService(authProvider),
		TransportDiagnostics: transportDiag,
		ExchangeLog:          exchangeLog,
		Clock:                clock,
	}, nil
}

// toolCallKindLookup adapts state.ToolCallRepo (persisted tool-call
// metadata) to translator.ToolCallKindLookup, so the Codex translator can
// resolve a call_id's backend tool-call kind (function/custom/tool_search/
// local_shell) from what MessageService recorded for a prior turn.
// ToolCallKindLookup's pinned signature (core/domain/translator/generic.go)
// carries no context parameter, so lookups here use context.Background().
type toolCallKindLookup struct {
	repo stateport.ToolCallRepo
}

func (l *toolCallKindLookup) ToolCallKind(callID string) (translatorpkg.ToolCallKind, bool) {
	if l == nil || l.repo == nil {
		return "", false
	}
	call, err := l.repo.GetToolCall(context.Background(), callID)
	if err != nil || call == nil || call.ToolKind == "" {
		return "", false
	}
	return translatorpkg.ToolCallKind(call.ToolKind), true
}

// defaultExchangeLogPath resolves the formatted-text exchange log path per
// ARCHITECTURE_v2.md invariant #8: GATEWAY_HOME/logs/http-exchange.log if
// GATEWAY_HOME is set, else ~/.gateway/logs/http-exchange.log.
func defaultExchangeLogPath() string {
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "logs", "http-exchange.log")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".gateway", "logs", "http-exchange.log")
}
