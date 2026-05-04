// Crate: gateway-auth-codex
// Purpose: load/refresh Codex OAuth credentials from disk and provide safe auth snapshots.
// Allowed deps: gateway-core.
// Not allowed: axum/http routing, backend client code.

#![forbid(unsafe_code)]

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
