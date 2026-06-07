// Crate: gatewayd (binary)
// Purpose: wire the HTTP server, middleware layers, and configuration into a runnable daemon.
// Allowed deps: gateway-http-anthropic, gateway-observability, gateway-core.
// Not allowed: implementing auth/backends directly (use the library crates).

#![forbid(unsafe_code)]

use gateway_core::config::load_gateway_config_default_path;
use gateway_http_anthropic::AppState;
use gateway_observability::middleware::{CaptureConfig, capture_http_exchange};
use tracing::info;
use tracing::warn;

mod login;
mod tui_login;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Login(Vendor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    OpenAI,
    Gemini,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let command = parse_command()?;

    match command {
        Command::Serve => run_serve().await?,
        Command::Login(vendor) => login::run_login(vendor).await?,
    }

    Ok(())
}

fn parse_command() -> Result<Command, Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    parse_command_from_args(&args)
}

fn parse_command_from_args(args: &[String]) -> Result<Command, Box<dyn std::error::Error>> {
    match args.len() {
        1 => {
            // No args: bare `cld-gateway` defaults to serve
            Ok(Command::Serve)
        }
        2 => {
            match args[1].as_str() {
                "serve" => Ok(Command::Serve),
                "login" => {
                    // `cld-gateway login` defaults to OpenAI
                    Ok(Command::Login(Vendor::OpenAI))
                }
                arg => Err(format!(
                    "unknown command '{arg}'; expected 'serve' or 'login [vendor]'"
                )
                .into()),
            }
        }
        3 => match args[1].as_str() {
            "login" => match args[2].as_str() {
                "openai" => Ok(Command::Login(Vendor::OpenAI)),
                "gemini" => Ok(Command::Login(Vendor::Gemini)),
                vendor => {
                    Err(format!("unknown vendor '{vendor}'; expected 'openai' or 'gemini'").into())
                }
            },
            "serve" => Err("too many arguments; expected 'serve' or 'login [vendor]'".into()),
            arg => {
                Err(format!("unknown command '{arg}'; expected 'serve' or 'login [vendor]'").into())
            }
        },
        _ => Err("too many arguments; expected 'serve' or 'login [vendor]'".into()),
    }
}

async fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    // Non-interactive auth preflight: attempt refresh if auth exists, but do not block startup.
    // For now, OpenAI is the default vendor in serve mode.
    auth_preflight_for_serve(Vendor::OpenAI).await;

    let gateway_config = load_gateway_config_default_path()?;
    let config = CaptureConfig::default();
    let app = gateway_http_anthropic::router(AppState::from_env()?).layer(
        axum::middleware::from_fn(move |req, next| {
            let config = config.clone();
            async move { capture_http_exchange(req, next, config).await }
        }),
    );

    let listen_addr = gateway_config.network.listen_addr;
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    info!("Listening on {listen_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn auth_preflight_for_serve(vendor: Vendor) {
    match vendor {
        Vendor::OpenAI => auth_preflight_openai().await,
        Vendor::Gemini => {
            info!("Gemini auth is not yet configured for serve mode; skipping preflight");
        }
    }
}

async fn auth_preflight_openai() {
    let auth_status = gateway_auth_codex::load_gateway_auth_status_default_path();

    match auth_status {
        Ok(Some(status)) if status.ready_for_messages() => {
            info!("gateway auth present; validating ChatGPT auth: {status:?}");
            let auth_manager = gateway_auth_codex::CodexAuthManager::default();
            match auth_manager.refresh_and_persist_default_path().await {
                Ok(snapshot) => {
                    info!(
                        "gateway auth health check succeeded for account_id={}",
                        snapshot.account_id
                    );
                }
                Err(err) => {
                    warn!("gateway auth health check failed; continuing without auth: {err}");
                }
            }
        }
        Ok(Some(status)) => {
            warn!("gateway auth present but incomplete: {status:?}; continuing without auth");
        }
        Ok(None) => {
            warn!("gateway auth not found; continuing without auth");
        }
        Err(err) => {
            warn!("gateway auth check failed: {err}; continuing without auth");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bare_gateway_defaults_to_serve() {
        let args = vec!["cld-gateway".to_string()];
        let result = parse_command_from_args(&args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Command::Serve);
    }

    #[test]
    fn test_serve_command() {
        let args = vec!["cld-gateway".to_string(), "serve".to_string()];
        let result = parse_command_from_args(&args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Command::Serve);
    }

    #[test]
    fn test_login_defaults_to_openai() {
        let args = vec!["cld-gateway".to_string(), "login".to_string()];
        let result = parse_command_from_args(&args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Command::Login(Vendor::OpenAI));
    }

    #[test]
    fn test_login_openai() {
        let args = vec![
            "cld-gateway".to_string(),
            "login".to_string(),
            "openai".to_string(),
        ];
        let result = parse_command_from_args(&args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Command::Login(Vendor::OpenAI));
    }

    #[test]
    fn test_login_gemini() {
        let args = vec![
            "cld-gateway".to_string(),
            "login".to_string(),
            "gemini".to_string(),
        ];
        let result = parse_command_from_args(&args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Command::Login(Vendor::Gemini));
    }

    #[test]
    fn test_invalid_vendor_error() {
        let args = vec![
            "cld-gateway".to_string(),
            "login".to_string(),
            "invalid".to_string(),
        ];
        let result = parse_command_from_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown vendor"));
    }

    #[test]
    fn test_unknown_command_error() {
        let args = vec!["cld-gateway".to_string(), "unknown".to_string()];
        let result = parse_command_from_args(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown command"));
    }

    #[test]
    fn test_too_many_args_error() {
        let args = vec![
            "cld-gateway".to_string(),
            "serve".to_string(),
            "extra".to_string(),
        ];
        let result = parse_command_from_args(&args);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("too many arguments")
        );
    }

    #[test]
    fn test_vendor_enum_properties() {
        // Confirm Vendor::OpenAI and Vendor::Gemini exist and are Copy/Clone/PartialEq
        let v1 = Vendor::OpenAI;
        let v2 = v1;
        assert_eq!(v1, v2);
        assert_ne!(v1, Vendor::Gemini);
    }
}
