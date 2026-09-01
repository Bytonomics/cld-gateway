#![forbid(unsafe_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tiny_http::{Response, Server};

use crate::auth_json::{AuthJson, Tokens};
use crate::{CodexAuthError, jwt, paths, persist};
use gateway_net::GatewayHttpClient;

const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_PORT: u16 = 1455;
const FALLBACK_PORT: u16 = 1457;

#[derive(Debug, Clone)]
struct PkceCodes {
    code_verifier: String,
    code_challenge: String,
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut bytes);

    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);

    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

fn build_authorize_url(redirect_uri: &str, pkce: &PkceCodes, state: &str) -> String {
    let mut query = vec![
        ("response_type".to_string(), "code".to_string()),
        ("client_id".to_string(), CLIENT_ID.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        (
            "scope".to_string(),
            "openid profile email offline_access api.connectors.read api.connectors.invoke"
                .to_string(),
        ),
        ("code_challenge".to_string(), pkce.code_challenge.clone()),
        ("code_challenge_method".to_string(), "S256".to_string()),
        ("id_token_add_organizations".to_string(), "true".to_string()),
        ("codex_cli_simplified_flow".to_string(), "true".to_string()),
        ("state".to_string(), state.to_string()),
        ("originator".to_string(), "gatewayd".to_string()),
    ];

    let qs = query
        .drain(..)
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(&v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{DEFAULT_ISSUER}/oauth/authorize?{qs}")
}

fn parse_query_params(raw_url: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some((_path, query)) = raw_url.split_once('?') else {
        return out;
    };
    for kv in query.split('&') {
        if kv.is_empty() {
            continue;
        }
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let key = urlencoding::decode(k)
            .unwrap_or_else(|_| k.into())
            .into_owned();
        let val = urlencoding::decode(v)
            .unwrap_or_else(|_| v.into())
            .into_owned();
        out.insert(key, val);
    }
    out
}

fn preferred_callback_port() -> u16 {
    std::env::var("CLD_GATEWAY_AUTH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn bind_callback_server() -> io::Result<(Server, u16)> {
    let preferred_port = preferred_callback_port();
    let preferred = format!("127.0.0.1:{preferred_port}");
    if let Ok(server) = Server::http(&preferred) {
        println!("Using OAuth callback port {preferred_port}");
        Ok((server, preferred_port))
    } else {
        let fallback = format!("127.0.0.1:{FALLBACK_PORT}");
        let server = Server::http(&fallback).map_err(io::Error::other)?;
        eprintln!(
            "Preferred OAuth callback port {preferred_port} unavailable; falling back to {FALLBACK_PORT}"
        );
        Ok((server, FALLBACK_PORT))
    }
}

async fn exchange_code_for_tokens(
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> Result<(String, String, String), CodexAuthError> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        #[serde(rename = "id_token")]
        id: String,
        #[serde(rename = "access_token")]
        access: String,
        #[serde(rename = "refresh_token")]
        refresh: String,
    }

    let http = GatewayHttpClient::default();
    let token_endpoint = format!("{DEFAULT_ISSUER}/oauth/token");
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(&pkce.code_verifier)
    );

    let res = http
        .post(&token_endpoint)
        .map_err(io::Error::other)?
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .execute()
        .await
        .map_err(io::Error::other)?;

    if !res.status().is_success() {
        return Err(CodexAuthError::LoginTokenExchangeFailed(
            res.status().as_u16(),
        ));
    }

    let parsed: TokenResponse = res.json().await.map_err(io::Error::other)?;
    Ok((parsed.id, parsed.access, parsed.refresh))
}

/// Runs an interactive “Sign in with `ChatGPT`” flow and writes `~/.gateway/auth.json`.
///
/// This matches Codex’s flow:
/// - browser OAuth authorize + localhost callback + PKCE
/// - exchange at `https://auth.openai.com/oauth/token`
/// - persist to `~/.gateway/auth.json`
///
/// # Errors
///
/// Returns an error if localhost callback binding fails, the browser callback times out, the
/// OAuth token exchange fails, or auth.json cannot be persisted.
pub async fn login_with_chatgpt_and_write_default_auth_json() -> Result<(), CodexAuthError> {
    let state = generate_state();
    let pkce = generate_pkce();
    let (server, port) = bind_callback_server()?;

    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let auth_url = build_authorize_url(&redirect_uri, &pkce, &state);

    println!(
        "\nFinish signing in via your browser\n\nIf the link doesn't open automatically, open the following link to authenticate:\n\n{auth_url}\n"
    );
    let _ = webbrowser::open(&auth_url);

    let (code, returned_state) = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_mins(15);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(CodexAuthError::LoginTimeout);
            }

            let maybe_req = server
                .recv_timeout(remaining.min(Duration::from_secs(10)))
                .map_err(io::Error::other)?;

            let Some(req) = maybe_req else {
                continue;
            };

            let url = req.url().to_string();
            if !url.starts_with("/auth/callback") {
                let _ = req.respond(
                    Response::from_string("<html><body>Not found</body></html>")
                        .with_status_code(404),
                );
                continue;
            }

            let params = parse_query_params(&url);
            let code = params.get("code").cloned().unwrap_or_default();
            let returned_state = params.get("state").cloned().unwrap_or_default();

            if code.is_empty() || returned_state.is_empty() {
                let _ = req.respond(
                    Response::from_string(
                        "<html><body><h3>Login failed</h3>Missing code/state.</body></html>",
                    )
                    .with_status_code(400),
                );
                return Err(CodexAuthError::LoginInvalidCallback);
            }

            let _ = req.respond(
                Response::from_string(
                    "<html><body><h3>Login complete</h3>You can close this tab.</body></html>",
                )
                .with_status_code(200),
            );
            return Ok((code, returned_state));
        }
    })
    .await
    .map_err(io::Error::other)??;

    if returned_state != state {
        return Err(CodexAuthError::LoginStateMismatch);
    }

    let (id_token, access_token, refresh_token) =
        exchange_code_for_tokens(&redirect_uri, &pkce, &code).await?;

    let account_id = jwt::extract_chatgpt_account_id_unverified(&id_token);

    let auth = AuthJson {
        auth_mode: Some("chatgpt".to_string()),
        openai_api_key: None,
        tokens: Some(Tokens {
            id_token: Some(id_token),
            access_token: Some(access_token),
            refresh_token: Some(refresh_token),
            account_id,
        }),
        last_refresh: None,
    };

    let value = serde_json::to_value(auth)?;
    let path = paths::default_auth_json_path();
    persist::atomic_write_json(&path, &value)?;
    Ok(())
}
