// Crate: gateway-observability
// Purpose: redaction + exchange logging + request-id plumbing (no business logic).
// Allowed deps: gateway-core.
// Not allowed: backend calls, auth refresh, Anthropic/Codex translation logic.

#![forbid(unsafe_code)]

pub mod exchange;
pub mod middleware;
pub mod paths;
pub mod redact;

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
