use crate::client::{BackendError, build_request_body, websocket_failed, websocket_message};
use crate::types::{CodexBackendEvent, CodexBackendEventStream, CodexBackendRequest};
use futures_util::{SinkExt as _, StreamExt as _};
use gateway_auth_codex::CodexAuthManager;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
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
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WebSocketSessionKey(String);

impl WebSocketSessionKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WebSocketSessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WebSocketChainId(String);

impl WebSocketChainId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn fresh() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct WebSocketRetryPolicy {
    pub max_recycles: usize,
    pub retryable: Vec<WebSocketErrorMatcher>,
    pub non_retryable: Vec<WebSocketErrorMatcher>,
}

impl Default for WebSocketRetryPolicy {
    fn default() -> Self {
        Self {
            max_recycles: 1,
            retryable: Vec::new(),
            non_retryable: Vec::new(),
        }
    }
}

impl WebSocketRetryPolicy {
    #[must_use]
    pub fn with_retryable(mut self, matcher: WebSocketErrorMatcher) -> Self {
        self.retryable.push(matcher);
        self
    }

    #[must_use]
    pub fn with_non_retryable(mut self, matcher: WebSocketErrorMatcher) -> Self {
        self.non_retryable.push(matcher);
        self
    }
}

#[derive(Clone, Debug)]
pub struct WebSocketErrorMatcher {
    pub variant: WebSocketErrorVariant,
    pub stage: Option<&'static str>,
    pub message_contains: Option<&'static str>,
    pub status: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebSocketErrorVariant {
    WebSocket,
    UnexpectedStatus,
    UnexpectedStatusWithBody,
    Any,
}

#[derive(Clone, Default)]
pub(crate) struct WebSocketSessionPool {
    sessions: Arc<Mutex<HashMap<WebSocketSessionKey, CodexWebSocketSession>>>,
}

pub struct PooledWebSocketEventStream {
    pub events: CodexBackendEventStream,
    pub websocket_chain_id: WebSocketChainId,
}

#[derive(Clone)]
struct CodexWebSocketSession {
    sender: mpsc::UnboundedSender<WebSocketCommand>,
    alive: Arc<AtomicBool>,
    websocket_chain_id: WebSocketChainId,
}

struct WebSocketCommand {
    body: serde_json::Value,
    events: mpsc::UnboundedSender<Result<CodexBackendEvent, BackendError>>,
}

type CodexWebSocketStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const WEBSOCKET_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

impl WebSocketSessionPool {
    pub(crate) fn has_live_session(&self, session_key: &WebSocketSessionKey) -> bool {
        self.live_websocket_chain_id(session_key).is_some()
    }

    pub(crate) fn live_websocket_chain_id(
        &self,
        session_key: &WebSocketSessionKey,
    ) -> Option<WebSocketChainId> {
        self.sessions
            .lock()
            .expect("websocket session mutex poisoned")
            .get(session_key)
            .filter(|session| session.is_alive())
            .map(|session| session.websocket_chain_id.clone())
    }

    pub(crate) async fn send_event_stream(
        &self,
        base_url: &str,
        auth: &CodexAuthManager,
        session_key: WebSocketSessionKey,
        req: CodexBackendRequest,
        policy: WebSocketRetryPolicy,
    ) -> Result<PooledWebSocketEventStream, BackendError> {
        let mut recycle_count = 0;

        loop {
            let mut stream = match self
                .send_attempt(base_url, auth, &session_key, req.clone())
                .await
            {
                Ok(stream) => stream,
                Err(err)
                    if should_recycle_websocket_error(&err, &policy)
                        && recycle_count < policy.max_recycles =>
                {
                    self.evict(&session_key);
                    recycle_count += 1;
                    tracing::warn!(
                        session_key = %session_key,
                        attempt = recycle_count,
                        error = %err,
                        "recycling websocket session before stream creation"
                    );
                    continue;
                }
                Err(err) => return Err(err),
            };

            let Some(first_item) = stream.events.next().await else {
                let err = BackendError::WebSocket {
                    stage: "read event",
                    message: "websocket stream ended before first event".to_string(),
                };
                if should_recycle_websocket_error(&err, &policy)
                    && recycle_count < policy.max_recycles
                {
                    self.evict(&session_key);
                    recycle_count += 1;
                    tracing::warn!(
                        session_key = %session_key,
                        attempt = recycle_count,
                        error = %err,
                        "recycling websocket session after empty first stream"
                    );
                    continue;
                }
                self.evict(&session_key);
                return Ok(PooledWebSocketEventStream {
                    events: futures_util::stream::once(async move { Err(err) }).boxed(),
                    websocket_chain_id: stream.websocket_chain_id,
                });
            };

            match first_item {
                Ok(first_event) => {
                    let tail_pool = self.clone();
                    let tail_key = session_key.clone();
                    let guarded_tail = stream
                        .events
                        .map(move |item| {
                            if let Err(err) = &item
                                && is_transport_lifecycle_error(err)
                            {
                                tail_pool.evict(&tail_key);
                            }
                            item
                        })
                        .boxed();
                    return Ok(PooledWebSocketEventStream {
                        events: futures_util::stream::once(async move { Ok(first_event) })
                            .chain(guarded_tail)
                            .boxed(),
                        websocket_chain_id: stream.websocket_chain_id,
                    });
                }
                Err(err)
                    if should_recycle_websocket_error(&err, &policy)
                        && recycle_count < policy.max_recycles =>
                {
                    self.evict(&session_key);
                    recycle_count += 1;
                    tracing::warn!(
                        session_key = %session_key,
                        attempt = recycle_count,
                        error = %err,
                        "recycling websocket session before first event"
                    );
                }
                Err(err) => {
                    if is_transport_lifecycle_error(&err) {
                        self.evict(&session_key);
                    }
                    return Ok(PooledWebSocketEventStream {
                        events: futures_util::stream::once(async move { Err(err) }).boxed(),
                        websocket_chain_id: stream.websocket_chain_id,
                    });
                }
            }
        }
    }

    fn evict(&self, session_key: &WebSocketSessionKey) {
        self.sessions
            .lock()
            .expect("websocket session mutex poisoned")
            .remove(session_key);
    }

    #[cfg(test)]
    pub(crate) fn contains_session(&self, session_key: &WebSocketSessionKey) -> bool {
        self.sessions
            .lock()
            .expect("websocket session mutex poisoned")
            .contains_key(session_key)
    }

    async fn send_attempt(
        &self,
        base_url: &str,
        auth: &CodexAuthManager,
        session_key: &WebSocketSessionKey,
        req: CodexBackendRequest,
    ) -> Result<PooledWebSocketEventStream, BackendError> {
        if let Some(session) = self.get_alive_session(session_key) {
            match session.send_event_stream(&req) {
                Ok(events) => {
                    let websocket_chain_id = session.websocket_chain_id.clone();
                    tracing::info!(
                        session_key = %session_key,
                        websocket_chain_id = %websocket_chain_id.as_str(),
                        previous_response_id = ?req.previous_response_id,
                        input_items = req.input.len(),
                        store = crate::types::STORE_RESPONSES_FOR_CONTINUATION,
                        "queued codex websocket response.create on live chain"
                    );
                    return Ok(PooledWebSocketEventStream {
                        events,
                        websocket_chain_id,
                    });
                }
                Err(err) => {
                    self.evict(session_key);
                    tracing::warn!(
                        session_key = %session_key,
                        error = %err,
                        "cached websocket session rejected request"
                    );
                    return Err(err);
                }
            }
        }

        if req.previous_response_id.is_some() {
            return Err(BackendError::WebSocket {
                stage: "queue response.create",
                message: "previous_response_id requires a live websocket session".to_string(),
            });
        }

        let session = open_session_with_refresh_retry(base_url, auth, req.clone()).await?;
        let events = session.send_event_stream(&req)?;
        let websocket_chain_id = session.websocket_chain_id.clone();
        tracing::info!(
            session_key = %session_key,
            websocket_chain_id = %websocket_chain_id.as_str(),
            previous_response_id = ?req.previous_response_id,
            input_items = req.input.len(),
            store = crate::types::STORE_RESPONSES_FOR_CONTINUATION,
            "opened codex websocket chain and queued response.create"
        );
        self.sessions
            .lock()
            .expect("websocket session mutex poisoned")
            .insert(session_key.clone(), session);
        Ok(PooledWebSocketEventStream {
            events,
            websocket_chain_id,
        })
    }

    fn get_alive_session(
        &self,
        session_key: &WebSocketSessionKey,
    ) -> Option<CodexWebSocketSession> {
        let session = self
            .sessions
            .lock()
            .expect("websocket session mutex poisoned")
            .get(session_key)
            .cloned();
        match session {
            Some(session) if session.is_alive() => Some(session),
            Some(_) => {
                self.evict(session_key);
                None
            }
            None => None,
        }
    }
}

impl CodexWebSocketSession {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn send_event_stream(
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

async fn open_session_with_refresh_retry(
    base_url: &str,
    auth: &CodexAuthManager,
    mut req: CodexBackendRequest,
) -> Result<CodexWebSocketSession, BackendError> {
    match open_session(base_url, &req).await {
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

            open_session(base_url, &req).await
        }
        Err(err) => Err(err),
    }
}

async fn open_session(
    base_url: &str,
    req: &CodexBackendRequest,
) -> Result<CodexWebSocketSession, BackendError> {
    let request = websocket_request(base_url, req)?;
    let (socket, _response) = connect_async(request)
        .await
        .map_err(|source| websocket_failed("connect", source))?;
    let (sender, receiver) = mpsc::unbounded_channel();
    let alive = Arc::new(AtomicBool::new(true));
    let websocket_chain_id = WebSocketChainId::fresh();
    tokio::spawn(run_websocket_session(socket, receiver, Arc::clone(&alive)));
    Ok(CodexWebSocketSession {
        sender,
        alive,
        websocket_chain_id,
    })
}

fn websocket_request(
    base_url: &str,
    req: &CodexBackendRequest,
) -> Result<tungstenite::handshake::client::Request, BackendError> {
    let url = websocket_url(base_url)?;
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

fn should_recycle_websocket_error(err: &BackendError, policy: &WebSocketRetryPolicy) -> bool {
    if policy
        .non_retryable
        .iter()
        .any(|matcher| matcher.matches(err))
    {
        return false;
    }
    if is_default_non_retryable_error(err) {
        return false;
    }
    if policy.retryable.iter().any(|matcher| matcher.matches(err)) {
        return true;
    }
    is_default_retryable_error(err)
}

fn is_transport_lifecycle_error(err: &BackendError) -> bool {
    is_default_retryable_error(err)
}

fn is_default_retryable_error(err: &BackendError) -> bool {
    match err {
        BackendError::WebSocket { stage, message } => {
            matches!(
                *stage,
                "connect"
                    | "queue response.create"
                    | "send response.create"
                    | "reply pong"
                    | "read event"
            ) && retryable_websocket_message(message)
        }
        BackendError::UnexpectedStatus(status)
        | BackendError::UnexpectedStatusWithBody { status, .. } => retryable_status(*status),
        BackendError::RequestFailed { .. }
        | BackendError::AuthFailed { .. }
        | BackendError::NetworkPolicy { .. } => false,
    }
}

fn is_default_non_retryable_error(err: &BackendError) -> bool {
    match err {
        BackendError::WebSocket {
            stage: "decode websocket event",
            ..
        }
        | BackendError::RequestFailed { .. }
        | BackendError::AuthFailed { .. }
        | BackendError::NetworkPolicy { .. } => true,
        BackendError::UnexpectedStatus(status) => non_retryable_status(*status),
        BackendError::UnexpectedStatusWithBody { status, body } => {
            non_retryable_status(*status) || semantic_error_body(body)
        }
        BackendError::WebSocket { message, .. } => semantic_error_body(message),
    }
}

fn retryable_websocket_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "closed",
        "reset",
        "broken pipe",
        "without closing handshake",
        "connection aborted",
        "connection refused",
        "connection reset",
        "stream ended before first event",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn semantic_error_body(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    [
        "previous_response_id",
        "tool schema",
        "schema validation",
        "model",
        "invalid_request",
        "invalid request",
    ]
    .iter()
    .any(|needle| body.contains(needle))
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 500 | 502 | 503)
}

fn non_retryable_status(status: u16) -> bool {
    matches!(status, 400 | 401 | 403 | 422 | 429)
}

impl WebSocketErrorMatcher {
    fn matches(&self, err: &BackendError) -> bool {
        if !self.variant.matches(err) {
            return false;
        }
        if let Some(expected_stage) = self.stage {
            match err {
                BackendError::WebSocket { stage, .. } if *stage == expected_stage => {}
                _ => return false,
            }
        }
        if let Some(expected_status) = self.status {
            match err {
                BackendError::UnexpectedStatus(status)
                | BackendError::UnexpectedStatusWithBody { status, .. }
                    if *status == expected_status => {}
                _ => return false,
            }
        }
        if let Some(needle) = self.message_contains
            && !backend_error_text(err).contains(&needle.to_ascii_lowercase())
        {
            return false;
        }
        true
    }
}

impl WebSocketErrorVariant {
    fn matches(&self, err: &BackendError) -> bool {
        match self {
            Self::Any => true,
            Self::WebSocket => matches!(err, BackendError::WebSocket { .. }),
            Self::UnexpectedStatus => matches!(err, BackendError::UnexpectedStatus(_)),
            Self::UnexpectedStatusWithBody => {
                matches!(err, BackendError::UnexpectedStatusWithBody { .. })
            }
        }
    }
}

fn backend_error_text(err: &BackendError) -> String {
    match err {
        BackendError::WebSocket { message, .. }
        | BackendError::UnexpectedStatusWithBody { body: message, .. }
        | BackendError::RequestFailed {
            message,
            source: _,
            stage: _,
        }
        | BackendError::AuthFailed { message, .. }
        | BackendError::NetworkPolicy { message, .. } => message.to_ascii_lowercase(),
        BackendError::UnexpectedStatus(status) => status.to_string(),
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
        CodexWebSocketSession, WebSocketChainId, WebSocketCommand, WebSocketErrorMatcher,
        WebSocketErrorVariant, WebSocketRetryPolicy, WebSocketSessionKey, WebSocketSessionPool,
        build_websocket_create_body, should_recycle_websocket_error, websocket_url,
    };
    use crate::client::BackendError;
    use crate::types::STORE_RESPONSES_FOR_CONTINUATION;
    use gateway_core::Secret;
    use std::sync::{Arc, atomic::AtomicBool};
    use tokio::sync::mpsc;

    #[test]
    fn websocket_url_uses_codex_responses_path() {
        assert_eq!(
            websocket_url("https://chatgpt.com").expect("url"),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn websocket_create_body_uses_response_create_without_stream() {
        let req = test_request(Some("resp_123"));

        let body = build_websocket_create_body(&req);
        assert_eq!(body["type"], "response.create");
        assert_eq!(body["previous_response_id"], "resp_123");
        assert_eq!(body.get("stream"), None);
        assert_eq!(body["store"], STORE_RESPONSES_FOR_CONTINUATION);
    }

    #[test]
    fn dead_websocket_session_rejects_reuse_without_queueing_request() {
        let (sender, mut receiver) = mpsc::unbounded_channel::<WebSocketCommand>();
        let session = CodexWebSocketSession {
            sender,
            alive: Arc::new(AtomicBool::new(false)),
            websocket_chain_id: WebSocketChainId::new("chain-dead"),
        };

        let Err(err) = session.send_event_stream(&test_request(Some("resp_123"))) else {
            panic!("dead session must not be reused");
        };

        assert!(err.to_string().contains("websocket session is closed"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn pool_evicts_dead_session_before_reuse() {
        let pool = WebSocketSessionPool::default();
        let key = WebSocketSessionKey::new("session:branch");
        let (sender, _receiver) = mpsc::unbounded_channel::<WebSocketCommand>();
        pool.sessions.lock().expect("mutex").insert(
            key.clone(),
            CodexWebSocketSession {
                sender,
                alive: Arc::new(AtomicBool::new(false)),
                websocket_chain_id: WebSocketChainId::new("chain-dead"),
            },
        );

        assert!(pool.get_alive_session(&key).is_none());
        assert!(!pool.contains_session(&key));
    }

    #[test]
    fn retry_policy_defaults_retry_read_reset_before_first_event() {
        let err = BackendError::WebSocket {
            stage: "read event",
            message: "Connection reset without closing handshake".to_string(),
        };

        assert!(should_recycle_websocket_error(
            &err,
            &WebSocketRetryPolicy::default()
        ));
    }

    #[test]
    fn retry_policy_rejects_previous_response_id_errors() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: "previous_response_id resp_prev not found".to_string(),
        };

        assert!(!should_recycle_websocket_error(
            &err,
            &WebSocketRetryPolicy::default()
        ));
    }

    #[test]
    fn retry_policy_non_retryable_override_wins() {
        let err = BackendError::WebSocket {
            stage: "read event",
            message: "connection reset".to_string(),
        };
        let policy = WebSocketRetryPolicy::default().with_non_retryable(WebSocketErrorMatcher {
            variant: WebSocketErrorVariant::WebSocket,
            stage: Some("read event"),
            message_contains: Some("reset"),
            status: None,
        });

        assert!(!should_recycle_websocket_error(&err, &policy));
    }

    #[test]
    fn retry_policy_retryable_extension_is_honored() {
        let err = BackendError::WebSocket {
            stage: "custom stage",
            message: "custom retry".to_string(),
        };
        let policy = WebSocketRetryPolicy {
            max_recycles: 1,
            retryable: vec![WebSocketErrorMatcher {
                variant: WebSocketErrorVariant::WebSocket,
                stage: Some("custom stage"),
                message_contains: Some("custom retry"),
                status: None,
            }],
            non_retryable: Vec::new(),
        };

        assert!(should_recycle_websocket_error(&err, &policy));
    }

    fn test_request(previous_response_id: Option<&str>) -> crate::types::CodexBackendRequest {
        crate::types::CodexBackendRequest {
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
            previous_response_id: previous_response_id.map(str::to_string),
            stream: true,
            include: Vec::new(),
            service_tier: None,
            client_metadata: None,
        }
    }
}
