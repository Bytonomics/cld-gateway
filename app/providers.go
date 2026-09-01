// Package app wires the concrete adapters built in earlier waves into the
// core/domain/services use-case interfaces handlers/ depends on, and mounts
// the Echo HTTP surface. See FILEMAP.md "app/" and ARCHITECTURE_v2.md
// "Layout".
package app

import (
	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/port/auth"
	stateport "github.com/Bytonomics/cld-gateway/core/domain/port/state"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
	"github.com/Bytonomics/cld-gateway/observability"
)

// Providers is the manually constructed dependency graph for one gatewayd
// process: the active gateway config plus every use-case service the
// handlers layer is constructor-injected with. Built by Initialize.
type Providers struct {
	Config *config.Config

	// AuthService is the raw auth.Provider (auth.json read/write, refresh,
	// PKCE login) shared by the Codex backend client and AuthStatusService.
	// It is distinct from AuthStatusService below, which is the
	// handlers-facing use case wrapping it.
	AuthService auth.Provider

	MessageService     services.MessageService
	CountTokensService services.CountTokensService
	ModelsService      services.ModelsService
	AuthStatusService  services.AuthStatusService

	TransportDiagnostics *observability.TransportDiagLog
	ExchangeLog          observability.ExchangeLog

	Clock stateport.Clock
}
