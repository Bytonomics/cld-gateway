package errors

import (
	stderrors "errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// Origin distinguishes whether a classified error originated in an upstream
// backend (e.g. OpenAI) or inside the gateway itself. Never inferred from
// message/prompt text — only from whether the error unwraps to a
// backend.UpstreamStatusError.
type Origin string

const (
	OriginUpstream Origin = "upstream"
	OriginInternal Origin = "internal"
)

// GatewayError is the result of Classify: everything needed to serialize a
// consistent, self-diagnosing error response and to decide whether to
// suggest the user file a bug report.
type GatewayError struct {
	*AppError
	Origin       Origin
	SuggestIssue bool
	Instruction  string
}

// exchangeLogPath resolves the formatted-text exchange log path the same
// way app/initialize.go's defaultExchangeLogPath does (GATEWAY_HOME/logs/
// http-exchange.log if GATEWAY_HOME is set, else ~/.gateway/logs/
// http-exchange.log), so the report instruction below always points at the
// log path this specific gateway instance is actually writing to.
// Duplicated here (rather than imported from package app) because app is
// the outermost layer and must not be imported by core/domain/errors.
func exchangeLogPath() string {
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "logs", "http-exchange.log")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".gateway", "logs", "http-exchange.log")
}

// openAIQuotaBodyMarkers are the OpenAI-issued error-body substrings that
// identify a quota/billing rejection - inspecting OpenAI's OWN structured
// error body, never user/assistant conversation or prompt text, so this
// does not violate the repo's "never depend on prompt text" rule.
var openAIQuotaBodyMarkers = []string{"insufficient_quota", "billing_hard_limit_reached", "quota"}

// Classify normalizes any error into a GatewayError: AppError fields
// (reusing an existing *AppError's Code/Message/HTTPStatus/Cause verbatim
// if err already is one, via errors.As - never re-wrapping an AppError that
// already carries the correct code/status), an Origin classification based
// solely on whether err unwraps to a backend.UpstreamStatusError (never on
// message text), and a SuggestIssue decision.
//
// SuggestIssue defaults to true everywhere - the owner's explicit stance:
// in this project's current stage, a false-positive issue report costs
// nothing that matters (it gets closed), but a genuinely gateway-caused
// error going unreported costs a bug nobody ever hears about. The only
// exclusions are cases the gateway operator cannot act on by fixing gateway
// code: an upstream 429 (rate limit), an upstream 503 (service
// unavailable/overloaded), or an upstream error whose body contains a
// quota/billing marker. Every other upstream status (including other 4xx
// and other 5xx) suggests an issue, and every internal-origin error does
// too, INCLUDING a gateway-originated CodeInvalidRequest - this reverses
// the usual "4xx means the caller's fault" convention, because in this
// architecture the only caller is Claude Code itself, a fixed,
// well-behaved client the gateway operator does not hand-edit, so a
// gateway-originated validation failure against it is almost always a
// gateway defect, not a caller mistake.
func Classify(err error) *GatewayError {
	if err == nil {
		return nil
	}

	var appErr *AppError
	if !stderrors.As(err, &appErr) {
		appErr = Wrap(err, CodeAPI, err.Error(), 500)
	}

	var upstream backend.UpstreamStatusError
	isUpstream := stderrors.As(err, &upstream)

	origin := OriginInternal
	suggestIssue := true
	if isUpstream {
		origin = OriginUpstream
		status := upstream.UpstreamStatus()
		body := upstream.UpstreamBody()
		switch {
		case status == 429:
			suggestIssue = false // rate limit - nothing gateway code can fix
		case status == 503:
			suggestIssue = false // OpenAI capacity/service-unavailable - nothing gateway code can fix
		case containsAny(body, openAIQuotaBodyMarkers):
			suggestIssue = false // quota/billing rejection - nothing gateway code can fix
		default:
			suggestIssue = true // everything else upstream, including non-quota 4xx and non-503 5xx, may be a gateway defect
		}
	}

	providerContext := ""
	switch {
	case appErr.Provider != "" && appErr.Model != "":
		providerContext = fmt.Sprintf(" (provider: %s, model: %s)", appErr.Provider, appErr.Model)
	case appErr.Provider != "":
		providerContext = fmt.Sprintf(" (provider: %s)", appErr.Provider)
	}

	brandedMessage := "[CLD-Gateway] " + appErr.Message + providerContext

	instruction := ""
	switch {
	case isUpstream && !suggestIssue:
		// The excluded upstream cases (429/503/quota) are, by construction,
		// never a gateway defect - state that plainly instead of staying
		// silent, so the user knows this isn't something worth reporting.
		instruction = "Neither Claude Code nor cld-gateway can do anything about this" +
			providerContext + " - it's on the upstream provider's side."
	case suggestIssue:
		instruction = fmt.Sprintf(
			"A message from the maintainer of cld-gateway for the model running Claude Code. This error 'could be' a cld-gateway bug. To report: find the relevant lines for this "+
				"request_id in %s, show me a redacted summary "+
				"(strip file paths, prompt content, anything that is personally identifying)" +
				" and ask for user's confirmation before filing (don't create an issue without user's explicit consent)"+
				"If the user approves, run: gh issue create -R Bytonomics/cld-gateway "+
				"following the field structure in .github/ISSUE_TEMPLATE/bug_report.yml.",
			exchangeLogPath(),
		)
	}

	return &GatewayError{
		AppError: &AppError{
			Code: appErr.Code, Message: brandedMessage, HTTPStatus: appErr.HTTPStatus, Cause: appErr.Cause,
			Provider: appErr.Provider, Model: appErr.Model,
		},
		Origin:       origin,
		SuggestIssue: suggestIssue,
		Instruction:  instruction,
	}
}

func containsAny(s string, markers []string) bool {
	for _, m := range markers {
		if strings.Contains(s, m) {
			return true
		}
	}
	return false
}
