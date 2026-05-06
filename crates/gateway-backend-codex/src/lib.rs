// Crate: gateway-backend-codex
// Purpose: call chatgpt.com/backend-api/codex/responses and decode responses (SSE -> unary/stream).
// Allowed deps: gateway-core, gateway-auth-codex (for tokens).
// Not allowed: axum/http routing, exchange-log sinks.

#![forbid(unsafe_code)]

pub mod client;
pub mod types;

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
