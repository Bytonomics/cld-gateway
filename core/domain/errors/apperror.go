package errors

type Code string

const (
	CodeInvalidRequest Code = "invalid_request_error"
	CodeAuthentication Code = "authentication_error"
	CodePermission     Code = "permission_error"
	CodeNotFound       Code = "not_found_error"
	CodeRateLimit      Code = "rate_limit_error"
	CodeAPI            Code = "api_error"
	CodeOverloaded     Code = "overloaded_error"
	CodeGatewayState   Code = "gateway_state_error"
)

type AppError struct {
	Code       Code
	Message    string
	HTTPStatus int
	Cause      error
	// Provider and Model, when set, name the backend provider (e.g.
	// config.Providers.Active, "codex") and model in use for the request
	// that produced this error. Populated by the caller when known (e.g.
	// core/impl/services/message_service.go's backend-error wrap sites,
	// which have both in scope from plan.backendReq.Model and
	// s.deps.Config.Providers.Active) — never guessed or defaulted here.
	Provider string
	Model    string
}

func (e *AppError) Error() string {
	if e.Cause != nil {
		return e.Message + ": " + e.Cause.Error()
	}
	return e.Message
}

func (e *AppError) Unwrap() error {
	return e.Cause
}

func New(code Code, message string, httpStatus int) *AppError {
	return &AppError{Code: code, Message: message, HTTPStatus: httpStatus}
}

func Wrap(cause error, code Code, message string, httpStatus int) *AppError {
	return &AppError{Code: code, Message: message, HTTPStatus: httpStatus, Cause: cause}
}
