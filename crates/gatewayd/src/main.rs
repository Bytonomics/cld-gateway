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
    let forced_login_method = forced_login_method_from_env()?;
    let auth_status = gateway_auth_codex::load_gateway_auth_status_default_path();

    match forced_login_method {
        Some(ForcedLoginMethod::Chatgpt) => {
            ensure_forced_chatgpt_login(auth_status).await?;
            Ok(())
        }
        Some(ForcedLoginMethod::ApiKey) => {
            ensure_forced_api_key_login(auth_status).await?;
            Ok(())
        }
        None => {
            ensure_default_login(auth_status).await?;
            Ok(())
        }
    }
}

async fn ensure_default_login(
    auth_status: Result<
        Option<gateway_auth_codex::GatewayAuthStatus>,
        gateway_auth_codex::CodexAuthError,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    match auth_status {
        Ok(Some(status)) if status.ready_for_messages() => {
            validate_chatgpt_auth(status, None).await
        }
        Ok(Some(status)) => {
            warn!("gateway auth present but incomplete: {status:?}; starting interactive login");
            interactive_login(None).await
        }
        Ok(None) => {
            warn!("gateway auth not found; starting interactive login");
            interactive_login(None).await
        }
        Err(err) => {
            warn!("gateway auth check failed: {err}; starting interactive login");
            interactive_login(None).await
        }
    }
}

async fn ensure_forced_chatgpt_login(
    auth_status: Result<
        Option<gateway_auth_codex::GatewayAuthStatus>,
        gateway_auth_codex::CodexAuthError,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    match auth_status {
        Ok(Some(status))
            if matches!(
                status.login_method,
                gateway_auth_codex::GatewayLoginMethod::Chatgpt
            ) && status.ready_for_messages() =>
        {
            validate_chatgpt_auth(status, Some(ForcedLoginMethod::Chatgpt)).await
        }
        Ok(Some(status)) => {
            warn!("forced ChatGPT login requested; existing auth is incompatible: {status:?}");
            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
            interactive_login(Some(ForcedLoginMethod::Chatgpt)).await
        }
        Ok(None) => {
            warn!("forced ChatGPT login requested; no auth found");
            interactive_login(Some(ForcedLoginMethod::Chatgpt)).await
        }
        Err(err) => {
            warn!("gateway auth check failed: {err}; forcing ChatGPT login");
            interactive_login(Some(ForcedLoginMethod::Chatgpt)).await
        }
    }
}

async fn ensure_forced_api_key_login(
    auth_status: Result<
        Option<gateway_auth_codex::GatewayAuthStatus>,
        gateway_auth_codex::CodexAuthError,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    match auth_status {
        Ok(Some(status))
            if matches!(
                status.login_method,
                gateway_auth_codex::GatewayLoginMethod::ApiKey
            ) && status.has_openai_api_key =>
        {
            info!("gateway auth present: {status:?}");
            Ok(())
        }
        Ok(Some(status)) => {
            warn!("forced API key login requested; existing auth is incompatible: {status:?}");
            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
            interactive_login(Some(ForcedLoginMethod::ApiKey)).await
        }
        Ok(None) => {
            warn!("forced API key login requested; no auth found");
            interactive_login(Some(ForcedLoginMethod::ApiKey)).await
        }
        Err(err) => {
            warn!("gateway auth check failed: {err}; forcing API key login");
            interactive_login(Some(ForcedLoginMethod::ApiKey)).await
        }
    }
}

async fn validate_chatgpt_auth(
    status: gateway_auth_codex::GatewayAuthStatus,
    forced_login_method: Option<ForcedLoginMethod>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("gateway auth present; validating ChatGPT auth: {status:?}");
    let auth_manager = gateway_auth_codex::CodexAuthManager::default();
    match auth_manager.refresh_and_persist_default_path().await {
        Ok(snapshot) => {
            info!(
                "gateway auth health check succeeded for account_id={}",
                snapshot.account_id
            );
            Ok(())
        }
        Err(err) => {
            warn!("gateway auth health check failed; forcing login: {err}");
            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
            interactive_login(forced_login_method).await
        }
    }
}

async fn interactive_login(
    forced_login_method: Option<ForcedLoginMethod>,
) -> Result<(), Box<dyn std::error::Error>> {
    match forced_login_method {
        Some(ForcedLoginMethod::Chatgpt) => {
            gateway_auth_codex::login::login_with_chatgpt_and_write_default_auth_json().await?;
            println!("\nLogin successful.\n");
            Ok(())
        }
        Some(ForcedLoginMethod::ApiKey) => {
            let api_key = prompt_api_key()?;
            gateway_auth_codex::write_openai_api_key_default_path(&api_key)?;
            println!("\nAPI key saved.\n");
            Ok(())
        }
        None => {
            let selection = tui_login::login_menu()?;
            match selection {
                LoginSelection::Chatgpt => {
                    gateway_auth_codex::login::login_with_chatgpt_and_write_default_auth_json()
                        .await?;
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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginSelection {
    Chatgpt,
    ApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForcedLoginMethod {
    Chatgpt,
    ApiKey,
}

fn forced_login_method_from_env() -> Result<Option<ForcedLoginMethod>, Box<dyn std::error::Error>> {
    match std::env::var("GATEWAY_FORCED_LOGIN_METHOD") {
        Ok(value) if value.eq_ignore_ascii_case("chatgpt") => Ok(Some(ForcedLoginMethod::Chatgpt)),
        Ok(value) if value.eq_ignore_ascii_case("api") || value.eq_ignore_ascii_case("api_key") => {
            Ok(Some(ForcedLoginMethod::ApiKey))
        }
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Err(format!(
            "invalid GATEWAY_FORCED_LOGIN_METHOD value '{value}'; expected chatgpt or api"
        )
        .into()),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err.into()),
    }
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
