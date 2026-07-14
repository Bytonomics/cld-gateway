#![forbid(unsafe_code)]

use crate::types::{
    CodexBackendEvent, CodexBackendEventStream, CodexBackendRequest, CodexBackendResponse,
};
use eventsource_stream::Eventsource as _;
use futures_util::{SinkExt as _, StreamExt as _};
use gateway_auth_codex::CodexAuthManager;
use gateway_core::format_error_chain;
use gateway_net::{GatewayHttpClient, NetworkPolicyError};
use reqwest::Response;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("request failed during {stage}: {message}")]
    RequestFailed {
        stage: &'static str,
        message: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("authentication failed during {stage}: {message}")]
    AuthFailed {
        stage: &'static str,
        message: String,
    },
    #[error("outbound request blocked during {stage}: {message}")]
    NetworkPolicy {
        stage: &'static str,
        message: String,
    },
    #[error("websocket failed during {stage}: {message}")]
    WebSocket {
        stage: &'static str,
        message: String,
    },
    #[error("unexpected response status {0}")]
    UnexpectedStatus(u16),
    #[error("unexpected response status {status}: {body}")]
    UnexpectedStatusWithBody { status: u16, body: String },
}

#[derive(Clone)]
pub struct CodexBackendClient {
    http: GatewayHttpClient,
    base_url: String,
    request_timeout: Option<Duration>,
}

#[derive(Clone)]
pub struct CodexWebSocketSession {
    sender: mpsc::UnboundedSender<WebSocketCommand>,
    alive: Arc<AtomicBool>,
}

struct WebSocketCommand {
    body: serde_json::Value,
    events: mpsc::UnboundedSender<Result<CodexBackendEvent, BackendError>>,
}

impl Default for CodexBackendClient {
    fn default() -> Self {
        Self {
            http: GatewayHttpClient::default(),
            base_url: "https://chatgpt.com".to_string(),
            request_timeout: None,
        }
    }
}

impl CodexBackendClient {
    #[must_use]
    pub fn with_base_url(mut self, base_url: &Url) -> Self {
        self.base_url = base_url.to_string();
        self
    }

    #[must_use]
    pub fn with_http_client(mut self, http: GatewayHttpClient) -> Self {
        self.http = http;
        self
    }

    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn http_client(&self) -> &GatewayHttpClient {
        &self.http
    }

    /// Sends a request to the Codex backend.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures or non-success HTTP status.
    pub async fn send(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<CodexBackendResponse, BackendError> {
        let res = self.send_streaming(req).await?;
        let status = res.status().as_u16();
        let body_text = res
            .text()
            .await
            .map_err(|source| request_failed("read response body", source))?;

        Ok(CodexBackendResponse {
            status,
            body: body_text,
        })
    }

    /// Sends a request to the Codex backend and returns the raw streaming response.
    ///
    /// This is used for consuming `text/event-stream` bodies (Day 11/13).
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures or non-success HTTP status.
    pub async fn send_streaming(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<Response, BackendError> {
        let url = format!(
            "{}/backend-api/codex/responses",
            self.base_url.trim_end_matches('/')
        );

        let body = build_request_body(req);
        let authorization = format!("Bearer {}", req.access_token.expose());

        let mut builder = self
            .http
            .post(&url)
            .map_err(|source| network_policy_failed("prepare request", &source))?
            .header("Authorization", &authorization)
            .header("chatgpt-account-id", &req.account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("Accept", "text/event-stream");

        if let Some(timeout) = self.request_timeout {
            builder = builder.timeout(timeout);
        }

        let res = builder
            .json(&body)
            .execute()
            .await
            .map_err(|source| request_failed("send request", source))?;

        let status = res.status().as_u16();
        if status >= 300 {
            let body = res
                .text()
                .await
                .ok()
                .map(|s| truncate_error_body(&s))
                .filter(|s| !s.trim().is_empty());
            if let Some(body) = body {
                return Err(BackendError::UnexpectedStatusWithBody { status, body });
            }
            return Err(BackendError::UnexpectedStatus(status));
        }

        Ok(res)
    }

    /// Day 8 contract: on 401, refresh once, retry once, then fail.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial request fails, refresh fails, or retry fails.
    pub async fn send_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<CodexBackendResponse, BackendError> {
        match self.send(&req).await {
            Ok(r) => Ok(r),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                // Re-load the access token after refresh.
                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.send(&req).await
            }
            Err(e) => Err(e),
        }
    }

    /// Day 8 contract (streaming): on 401, refresh once, retry once, then fail.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial request fails, refresh fails, or retry fails.
    pub async fn send_streaming_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<Response, BackendError> {
        match self.send_streaming(&req).await {
            Ok(r) => Ok(r),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.send_streaming(&req).await
            }
            Err(e) => Err(e),
        }
    }

    /// Converts the existing HTTP/SSE transport into backend events.
    #[must_use]
    pub fn response_to_event_stream(res: Response) -> CodexBackendEventStream {
        res.bytes_stream()
            .eventsource()
            .map(|item| {
                item.map(|event| CodexBackendEvent {
                    event: event.event,
                    data: event.data,
                })
                .map_err(|err| BackendError::WebSocket {
                    stage: "decode http event stream",
                    message: format!("{err}"),
                })
            })
            .boxed()
    }

    /// Sends an incremental request over the Codex Responses WebSocket.
    ///
    /// `previous_response_id` is intentionally supported here rather than on the HTTP path: the
    /// Codex backend accepts store=false continuation through WebSocket connection-local state.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket cannot be opened, the create frame cannot be sent, or the
    /// server rejects the upgrade.
    pub async fn send_websocket_event_stream(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<CodexBackendEventStream, BackendError> {
        let session = self.open_websocket_session(req).await?;
        session.send_event_stream(req)
    }

    /// WebSocket variant of the auth refresh retry contract.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial WebSocket request fails, refresh fails, or retry fails.
    pub async fn send_websocket_event_stream_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<CodexBackendEventStream, BackendError> {
        match self.send_websocket_event_stream(&req).await {
            Ok(stream) => Ok(stream),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.send_websocket_event_stream(&req).await
            }
            Err(err) => Err(err),
        }
    }

    fn websocket_request(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<tungstenite::handshake::client::Request, BackendError> {
        let url = websocket_url(&self.base_url)?;
        let authorization = format!("Bearer {}", req.access_token.expose());
        let mut request = url
            .into_client_request()
            .map_err(|source| websocket_failed("prepare request", source))?;
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            authorization
                .parse()
                .map_err(|err| websocket_message("prepare authorization header", err))?,
        );
        headers.insert(
            "chatgpt-account-id",
            req.account_id
                .parse()
                .map_err(|err| websocket_message("prepare account header", err))?,
        );
        headers.insert(
            "OpenAI-Beta",
            "responses=experimental"
                .parse()
                .map_err(|err| websocket_message("prepare beta header", err))?,
        );
        headers.insert(
            "originator",
            "codex_cli_rs"
                .parse()
                .map_err(|err| websocket_message("prepare originator header", err))?,
        );
        Ok(request)
    }

    /// Opens a reusable Codex Responses WebSocket session.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket cannot be opened or the server rejects the upgrade.
    pub async fn open_websocket_session(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<CodexWebSocketSession, BackendError> {
        let request = self.websocket_request(req)?;
        let (socket, _response) = connect_async(request)
            .await
            .map_err(|source| websocket_failed("connect", source))?;
        let (sender, receiver) = mpsc::unbounded_channel();
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(run_websocket_session(socket, receiver, Arc::clone(&alive)));
        Ok(CodexWebSocketSession { sender, alive })
    }

    /// Opens a reusable Codex Responses WebSocket session with one auth refresh retry on 401.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial WebSocket open fails, refresh fails, or retry fails.
    pub async fn open_websocket_session_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<CodexWebSocketSession, BackendError> {
        match self.open_websocket_session(&req).await {
            Ok(session) => Ok(session),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.open_websocket_session(&req).await
            }
            Err(err) => Err(err),
        }
    }
}

impl CodexWebSocketSession {
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Sends one `response.create` frame on this existing WebSocket session.
    ///
    /// # Errors
    ///
    /// Returns an error if the session task has already closed.
    pub fn send_event_stream(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<CodexBackendEventStream, BackendError> {
        if !self.is_alive() {
            return Err(BackendError::WebSocket {
                stage: "queue response.create",
                message: "websocket session is closed".to_string(),
            });
        }
        let (events, receiver) = mpsc::unbounded_channel();
        self.sender
            .send(WebSocketCommand {
                body: build_websocket_create_body(req),
                events,
            })
            .map_err(|_| BackendError::WebSocket {
                stage: "queue response.create",
                message: "websocket session is closed".to_string(),
            })?;
        Ok(unbounded_receiver_stream(receiver))
    }
}

type CodexWebSocketStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const WEBSOCKET_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

async fn run_websocket_session(
    mut socket: CodexWebSocketStream,
    mut receiver: mpsc::UnboundedReceiver<WebSocketCommand>,
    alive: Arc<AtomicBool>,
) {
    let mut keepalive = tokio::time::interval(WEBSOCKET_KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            command = receiver.recv() => command,
            item = socket.next() => {
                if handle_idle_websocket_item(item, &mut socket, &alive).await.is_err() {
                    return;
                }
                continue;
            }
            _ = keepalive.tick() => {
                if let Err(err) = socket.send(Message::Ping(Vec::new().into())).await {
                    alive.store(false, Ordering::Release);
                    tracing::debug!(
                        error = %err,
                        "websocket keepalive ping failed; marking session closed"
                    );
                    return;
                }
                continue;
            }
        };
        let Some(command) = command else {
            alive.store(false, Ordering::Release);
            return;
        };
        if let Err(err) = socket
            .send(Message::Text(command.body.to_string().into()))
            .await
        {
            alive.store(false, Ordering::Release);
            let _ = command
                .events
                .send(Err(websocket_failed("send response.create", err)));
            return;
        }

        forward_websocket_response_events(&mut socket, &command.events, &alive).await;
        if !alive.load(Ordering::Acquire) {
            return;
        }
    }
}

async fn forward_websocket_response_events(
    socket: &mut CodexWebSocketStream,
    events: &mpsc::UnboundedSender<Result<CodexBackendEvent, BackendError>>,
    alive: &AtomicBool,
) {
    while let Some(item) = socket.next().await {
        let event = match item {
            Ok(Message::Text(text)) => websocket_text_to_backend_event(&text),
            Ok(Message::Binary(bytes)) => match std::str::from_utf8(&bytes) {
                Ok(text) => websocket_text_to_backend_event(text),
                Err(err) => Err(websocket_message("decode binary event", err)),
            },
            Ok(Message::Close(_)) => {
                alive.store(false, Ordering::Release);
                let _ = events.send(Err(BackendError::WebSocket {
                    stage: "read event",
                    message: "websocket closed".to_string(),
                }));
                return;
            }
            Ok(Message::Ping(payload)) => {
                if let Err(err) = socket.send(Message::Pong(payload)).await {
                    alive.store(false, Ordering::Release);
                    let _ = events.send(Err(websocket_failed("reply pong", err)));
                    return;
                }
                continue;
            }
            Ok(Message::Pong(_) | Message::Frame(_)) => continue,
            Err(err) => {
                alive.store(false, Ordering::Release);
                let _ = events.send(Err(websocket_failed("read event", err)));
                return;
            }
        };

        let terminal = event
            .as_ref()
            .is_ok_and(|event| is_terminal_backend_event(&event.event));
        let _ = events.send(event);
        if terminal {
            return;
        }
    }
    alive.store(false, Ordering::Release);
    let _ = events.send(Err(BackendError::WebSocket {
        stage: "read event",
        message: "websocket closed".to_string(),
    }));
}

async fn handle_idle_websocket_item(
    item: Option<Result<Message, tungstenite::Error>>,
    socket: &mut CodexWebSocketStream,
    alive: &AtomicBool,
) -> Result<(), ()> {
    match item {
        Some(Ok(Message::Ping(payload))) => {
            socket.send(Message::Pong(payload)).await.map_err(|err| {
                alive.store(false, Ordering::Release);
                tracing::debug!(error = %err, "websocket idle pong failed");
            })
        }
        Some(Ok(Message::Pong(_) | Message::Frame(_) | Message::Text(_) | Message::Binary(_))) => {
            Ok(())
        }
        Some(Ok(Message::Close(close_frame))) => {
            alive.store(false, Ordering::Release);
            tracing::debug!(?close_frame, "websocket closed while idle");
            Err(())
        }
        Some(Err(err)) => {
            alive.store(false, Ordering::Release);
            tracing::debug!(error = %err, "websocket idle read failed");
            Err(())
        }
        None => {
            alive.store(false, Ordering::Release);
            tracing::debug!("websocket idle stream ended");
            Err(())
        }
    }
}

fn is_terminal_backend_event(event: &str) -> bool {
    matches!(
        event,
        "response.completed" | "response.failed" | "response.cancelled" | "error"
    )
}

fn unbounded_receiver_stream(
    mut receiver: mpsc::UnboundedReceiver<Result<CodexBackendEvent, BackendError>>,
) -> CodexBackendEventStream {
    futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx)).boxed()
}

fn request_failed(stage: &'static str, source: reqwest::Error) -> BackendError {
    let message = format_error_chain(&source);
    BackendError::RequestFailed {
        stage,
        message,
        source,
    }
}

fn network_policy_failed(stage: &'static str, source: &NetworkPolicyError) -> BackendError {
    BackendError::NetworkPolicy {
        stage,
        message: source.to_string(),
    }
}

fn websocket_failed<E>(stage: &'static str, source: E) -> BackendError
where
    E: std::fmt::Display,
{
    let message = source.to_string();
    if let Some(status) = websocket_status_from_error_message(&message) {
        return BackendError::UnexpectedStatus(status);
    }
    BackendError::WebSocket { stage, message }
}

fn websocket_message<E>(stage: &'static str, source: E) -> BackendError
where
    E: std::fmt::Display,
{
    BackendError::WebSocket {
        stage,
        message: source.to_string(),
    }
}

fn websocket_status_from_error_message(message: &str) -> Option<u16> {
    [401_u16, 403, 404, 409, 410, 422, 429, 500, 502, 503]
        .into_iter()
        .find(|status| message.contains(&status.to_string()))
}

fn websocket_url(base_url: &str) -> Result<String, BackendError> {
    let mut parsed = Url::parse(base_url).map_err(|err| websocket_message("parse url", err))?;
    let scheme = match parsed.scheme() {
        "https" => "wss",
        "http" => "ws",
        "wss" | "ws" => parsed.scheme(),
        other => {
            return Err(BackendError::WebSocket {
                stage: "prepare websocket url",
                message: format!("unsupported base url scheme: {other}"),
            });
        }
    }
    .to_string();
    parsed
        .set_scheme(&scheme)
        .map_err(|()| BackendError::WebSocket {
            stage: "prepare websocket url",
            message: format!("failed to set websocket scheme: {scheme}"),
        })?;
    parsed.set_path("/backend-api/codex/responses");
    parsed.set_query(None);
    Ok(parsed.to_string())
}

fn websocket_text_to_backend_event(text: &str) -> Result<CodexBackendEvent, BackendError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| BackendError::WebSocket {
            stage: "decode websocket event",
            message: err.to_string(),
        })?;
    let event = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("message")
        .to_string();
    Ok(CodexBackendEvent {
        event,
        data: text.to_string(),
    })
}

fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 8 * 1024;
    if body.len() <= MAX {
        body.to_string()
    } else {
        let mut s = body[..MAX].to_string();
        s.push_str("…(truncated)");
        s
    }
}

fn build_request_body(req: &CodexBackendRequest) -> serde_json::Value {
    // Body that matches Codex's "Responses-like" payload shape as closely as possible.
    let mut obj = serde_json::Map::new();
    obj.insert(
        "model".to_string(),
        serde_json::Value::String(req.model.clone()),
    );
    obj.insert(
        "instructions".to_string(),
        serde_json::Value::String(req.instructions.clone()),
    );
    obj.insert(
        "input".to_string(),
        serde_json::Value::Array(req.input.clone()),
    );
    obj.insert(
        "tools".to_string(),
        serde_json::Value::Array(req.tools.clone()),
    );
    obj.insert(
        "tool_choice".to_string(),
        serde_json::Value::String(req.tool_choice.clone()),
    );
    obj.insert(
        "parallel_tool_calls".to_string(),
        serde_json::Value::Bool(req.parallel_tool_calls),
    );
    if let Some(reasoning) = req.reasoning.clone() {
        obj.insert("reasoning".to_string(), reasoning);
    }
    if let Some(text) = req.text.clone() {
        obj.insert("text".to_string(), text);
    }
    // Backend contract: must be explicitly false.
    obj.insert("store".to_string(), serde_json::Value::Bool(req.store));
    // Backend returns SSE when stream=true.
    obj.insert("stream".to_string(), serde_json::Value::Bool(req.stream));
    obj.insert(
        "include".to_string(),
        serde_json::Value::Array(
            req.include
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    if let Some(service_tier) = req.service_tier.as_ref() {
        obj.insert(
            "service_tier".to_string(),
            serde_json::Value::String(service_tier.clone()),
        );
    }

    if let Some(meta) = req.client_metadata.as_ref() {
        let mut m = serde_json::Map::new();
        for (k, v) in meta {
            m.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        obj.insert("client_metadata".to_string(), serde_json::Value::Object(m));
    }

    serde_json::Value::Object(obj)
}

fn build_websocket_create_body(req: &CodexBackendRequest) -> serde_json::Value {
    let mut body = build_request_body(req);
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "type".to_string(),
            serde_json::Value::String("response.create".to_string()),
        );
        if let Some(previous_response_id) = req.previous_response_id.as_ref() {
            obj.insert(
                "previous_response_id".to_string(),
                serde_json::Value::String(previous_response_id.clone()),
            );
        }
        obj.remove("stream");
    }
    body
}

#[cfg(test)]
mod tests {
    use super::{
        CodexBackendClient, CodexWebSocketSession, WebSocketCommand, build_websocket_create_body,
        websocket_url,
    };
    use gateway_core::Secret;
    use std::sync::{Arc, atomic::AtomicBool};
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn default_client_has_no_total_request_timeout() {
        assert_eq!(CodexBackendClient::default().request_timeout(), None);
    }

    #[test]
    fn request_timeout_can_be_configured() {
        let timeout = Duration::from_secs(123);
        let client = CodexBackendClient::default().with_request_timeout(timeout);
        assert_eq!(client.request_timeout(), Some(timeout));
    }

    #[test]
    fn websocket_url_uses_codex_responses_path() {
        assert_eq!(
            websocket_url("https://chatgpt.com").expect("url"),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn websocket_create_body_uses_response_create_without_stream() {
        let req = crate::types::CodexBackendRequest {
            access_token: Secret::new("access_test".to_string()),
            account_id: "acct_test".to_string(),
            model: "gpt-5.5".to_string(),
            instructions: "You are helpful.".to_string(),
            input: Vec::new(),
            tools: Vec::new(),
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
            text: None,
            reasoning: None,
            previous_response_id: Some("resp_123".to_string()),
            store: false,
            stream: true,
            include: Vec::new(),
            service_tier: None,
            client_metadata: None,
        };

        let body = build_websocket_create_body(&req);
        assert_eq!(body["type"], "response.create");
        assert_eq!(body["previous_response_id"], "resp_123");
        assert_eq!(body.get("stream"), None);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn dead_websocket_session_rejects_reuse_without_queueing_request() {
        let (sender, mut receiver) = mpsc::unbounded_channel::<WebSocketCommand>();
        let session = CodexWebSocketSession {
            sender,
            alive: Arc::new(AtomicBool::new(false)),
        };
        let req = crate::types::CodexBackendRequest {
            access_token: Secret::new("access_test".to_string()),
            account_id: "acct_test".to_string(),
            model: "gpt-5.5".to_string(),
            instructions: "You are helpful.".to_string(),
            input: Vec::new(),
            tools: Vec::new(),
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
            text: None,
            reasoning: None,
            previous_response_id: Some("resp_123".to_string()),
            store: false,
            stream: true,
            include: Vec::new(),
            service_tier: None,
            client_metadata: None,
        };

        let Err(err) = session.send_event_stream(&req) else {
            panic!("dead session must not be reused");
        };

        assert!(err.to_string().contains("websocket session is closed"));
        assert!(receiver.try_recv().is_err());
    }
}
