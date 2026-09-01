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
