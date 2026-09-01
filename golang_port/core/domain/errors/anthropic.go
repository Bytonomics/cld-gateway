package errors

// AnthropicPayload serializes err into the Anthropic error response shape:
// {"type":"error","error":{"type":"<code>","message":"..."}}.
// Errors that are not *AppError are wrapped as api_error/500 first.
func AnthropicPayload(err error) map[string]any {
	appErr, ok := err.(*AppError)
	if !ok {
		appErr = Wrap(err, CodeAPI, err.Error(), 500)
	}

	return map[string]any{
		"type": "error",
		"error": map[string]any{
			"type":    string(appErr.Code),
			"message": appErr.Message,
		},
	}
}
