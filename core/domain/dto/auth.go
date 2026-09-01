package dto

// AuthStatus is the GET /auth/status response body, ported from
// auth_status_response (lib.rs:833-867). Field names/JSON keys mirror the
// Rust response; AccountID/LoginMethod are pointers because the Rust
// response serializes them as explicit `null` when absent, matching the
// nil-vs-set fields already carried by port/auth.Status.
type AuthStatus struct {
	LoggedIn         bool    `json:"logged_in"`
	ReadyForMessages bool    `json:"ready_for_messages,omitempty"`
	ReadyForModels   bool    `json:"ready_for_models,omitempty"`
	AccountID        *string `json:"account_id"`
	LoginMethod      *string `json:"login_method"`
	Source           string  `json:"source"`
	AuthRemediation  string  `json:"auth_remediation,omitempty"`
	ErrorType        string  `json:"error_type,omitempty"`
}

// AuthSnapshot is the POST /auth/refresh response body, ported from
// auth_refresh (lib.rs:869-880).
type AuthSnapshot struct {
	OK                   bool   `json:"ok"`
	AccountID            string `json:"account_id"`
	ExpiresAtUnixSeconds *int64 `json:"expires_at_unix_seconds"`
	Source               string `json:"source"`
}
