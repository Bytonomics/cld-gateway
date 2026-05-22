// Crate: gatewayd (binary)
// Purpose: wire the HTTP server, middleware layers, and configuration into a runnable daemon.
// Allowed deps: gateway-http-anthropic, gateway-observability, gateway-core.
// Not allowed: implementing auth/backends directly (use the library crates).

#![forbid(unsafe_code)]

use gateway_http_anthropic::AppState;
use gateway_observability::middleware::{CaptureConfig, capture_http_exchange};
use tracing::info;
use tracing::warn;

mod tui_login;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    ensure_gateway_auth().await?;

    let config = CaptureConfig::default();
    let app = gateway_http_anthropic::router(AppState::from_env()).layer(
        axum::middleware::from_fn(move |req, next| {
            let config = config.clone();
            async move { capture_http_exchange(req, next, config).await }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ensure_gateway_auth() -> Result<(), Box<dyn std::error::Error>> {
    match gateway_auth_codex::load_credentials_default_path() {
        Ok(_) => {
            info!("gateway auth present");
            Ok(())
        }
        Err(gateway_auth_codex::CodexAuthError::AuthNotFound { .. }) => {
            warn!("gateway auth not found; starting interactive login");
            interactive_login().await?;
            Ok(())
        }
        Err(err) => {
            warn!("gateway auth check failed: {err}");
            Ok(())
        }
    }
}

async fn interactive_login() -> Result<(), Box<dyn std::error::Error>> {
    let selection = tui_login::login_menu()?;
    match selection {
        LoginSelection::Chatgpt => {
            gateway_auth_codex::login::login_with_chatgpt_and_write_default_auth_json().await?;
            println!("\nLogin successful.\n");
            Ok(())
        }
        LoginSelection::ApiKey => {
            let api_key = prompt_api_key()?;
            gateway_auth_codex::write_openai_api_key_default_path(&api_key)?;
            println!("\nAPI key saved.\n");
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginSelection {
    Chatgpt,
    ApiKey,
}

fn prompt_api_key() -> Result<String, Box<dyn std::error::Error>> {
    use std::io::Write as _;

    println!(
        "\nPaste your OpenAI API key. This enables /v1/models; /v1/messages still requires ChatGPT login.\n"
    );
    print!("OPENAI_API_KEY: ");
    std::io::stdout().flush()?;

    let mut key = String::new();
    std::io::stdin().read_line(&mut key)?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("empty API key".into());
    }
    Ok(key)
}
