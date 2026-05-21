// Crate: gatewayd (binary)
// Purpose: wire the HTTP server, middleware layers, and configuration into a runnable daemon.
// Allowed deps: gateway-http-anthropic, gateway-observability, gateway-core.
// Not allowed: implementing auth/backends directly (use the library crates).

#![forbid(unsafe_code)]

use gateway_http_anthropic::AppState;
use gateway_observability::middleware::{CaptureConfig, capture_http_exchange};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = CaptureConfig::default();
    let app = gateway_http_anthropic::router(AppState::default()).layer(axum::middleware::from_fn(
        move |req, next| {
            let config = config.clone();
            async move { capture_http_exchange(req, next, config).await }
        },
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
