package errors

import (
	stderrors "errors"
	"fmt"
	"os"
	"path/filepath"

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

// Classify normalizes any error into a GatewayError: AppError fields
// (reusing an existing *AppError's Code/Message/HTTPStatus/Cause verbatim
// if err already is one, via errors.As - never re-wrapping an AppError that
// already carries the correct code/status), an Origin classification based
// solely on whether err unwraps to a backend.UpstreamStatusError (never on
// message text), and a SuggestIssue decision.
//
// SuggestIssue is false whenever Origin is upstream and the status is < 500
// - every non-5xx upstream status suppresses SuggestIssue, full stop; there
// is no quota-specific distinction in this logic (a 429 and a non-quota 4xx
// are treated identically). It is true when Origin is upstream and the
// status is >= 500 (a genuine upstream server error is still worth
// reporting, since it may indicate a gateway retry/handling bug, not just a
// transient upstream failure). It is true in every other case, INCLUDING
// when Origin is internal and the
// resulting AppError.Code is CodeInvalidRequest - this reverses the usual
// "4xx means the caller's fault" convention, because in this architecture
// the only caller is Claude Code itself, a fixed, well-behaved client the
// gateway operator does not hand-edit, so a gateway-originated validation
// failure against it is almost always a gateway defect, not a caller
// mistake.
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
		suggestIssue = false
		if status >= 500 {
			suggestIssue = true
		}
	}

	brandedMessage := "[CLD-Gateway] " + appErr.Message

	instruction := ""
	if suggestIssue {
		instruction = fmt.Sprintf(
			"This looks like a cld-gateway bug. To report: find the relevant lines for this "+
				"request_id in %s, show me a redacted summary "+
				"(strip file paths, prompt content, anything identifying), and ask my confirmation "+
				"before filing. If I approve, run: gh issue create -R Bytonomics/cld-gateway "+
				"following the field structure in .github/ISSUE_TEMPLATE/bug_report.yml.",
			exchangeLogPath(),
		)
	}

	return &GatewayError{
		AppError:     &AppError{Code: appErr.Code, Message: brandedMessage, HTTPStatus: appErr.HTTPStatus, Cause: appErr.Cause},
		Origin:       origin,
		SuggestIssue: suggestIssue,
		Instruction:  instruction,
	}
}
