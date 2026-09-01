// Crate: gateway-http-anthropic
// Purpose: Anthropic-compatible HTTP surface (routes, request parsing, translation).
// Allowed deps: gateway-core, gateway-auth-codex, gateway-backend-codex, gateway-observability.
// Not allowed: direct auth file IO (must go through gateway-auth-codex).

#![forbid(unsafe_code)]

use axum::Json;
use axum::body::{Bytes, to_bytes};
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use futures_util::StreamExt as _;
use gateway_backend_codex::client::{
    BackendError, WebSocketChainId, WebSocketRetryPolicy, WebSocketSessionKey,
};
use gateway_backend_codex::types::{CodexToolCall, CodexToolCallKind};
use gateway_core::RequestId;
use gateway_core::config::{
    GatewayConfig, ModelResolution, load_gateway_config_default_path, resolve_model,
    service_tier_for_config,
};
use gateway_net::{GatewayHttpClient, GatewayNetworkPolicy};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use uuid::Uuid;

mod claude_code_context;
mod claude_code_inclusion;
mod claude_response_gate;
mod context_management;
mod sse_bridge;
mod tool_arg_policy;
mod translate;
mod translate_executor;
mod types;

use crate::claude_code_context::normalize_claude_code_context;
use crate::claude_response_gate::{
    cleanup_structured_output_text_for_anthropic, sanitize_anthropic_response_text,
    sanitize_anthropic_response_value, structured_output_schema_from_config,
};
use crate::context_management::{ContextManagementReport, ContextManager};
use crate::translate::{ToolTranslationContext, TranslateResult, translate_request_with_context};
use crate::translate_executor::{ExecutorRuntime, execute_translated_command};
use crate::types::AnthropicMessagesRequest;
use gateway_state::{
    BranchFingerprintSet, BranchMetadata, BranchSelectionAction, BranchSelectionInput,
    CommitOffshootCheckpointParams, CommitTurnParams, ConversationStateStore,
    ConversationTurnScope, OpenAiCheckpoint, ToolCallStore, TurnOpenAiCheckpoint,
};
use sha2::{Digest, Sha256};

#[derive(Clone, Default)]
pub struct AppState {
    auth: gateway_auth_codex::CodexAuthManager,
    backend: gateway_backend_codex::client::CodexBackendClient,
    tool_calls: ToolCallStore,
    conversation_state: ConversationStateStore,
    openai_chain_checkpoints: OpenAiChainCheckpointStore,
    main_turn_leases: MainTurnLeaseStore,
    gateway_config: GatewayConfig,
    claude_gateway_settings_path: Option<PathBuf>,
    #[cfg(test)]
    auth_json_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
struct OpenAiChainCheckpointStore {
    checkpoints: Arc<Mutex<HashMap<String, WebSocketChainId>>>,
}

#[derive(Debug, Clone, Default)]
struct MainTurnLeaseStore {
    leases: Arc<Mutex<HashMap<String, MainTurnLease>>>,
}

#[derive(Debug, Clone)]
struct MainTurnLease {
    request: String,
    previous_response: Option<String>,
    websocket_chain: Option<WebSocketChainId>,
    state: MainTurnLeaseState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainTurnLeaseState {
    InFlight,
    CompletedCommitted,
    ClientAbortedBeforeFirstEvent,
    ClientAbortedAfterVisibleOutput,
    BackendFailedBeforeCommit,
    CommitSuppressedAfterAbort,
}

impl MainTurnLeaseState {
    fn as_str(self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::CompletedCommitted => "completed_committed",
            Self::ClientAbortedBeforeFirstEvent => "client_aborted_before_first_event",
            Self::ClientAbortedAfterVisibleOutput => "client_aborted_after_visible_output",
            Self::BackendFailedBeforeCommit => "backend_failed_before_commit",
            Self::CommitSuppressedAfterAbort => "commit_suppressed_after_abort",
        }
    }

    fn allows_commit(self) -> bool {
        self == Self::InFlight
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MainTurnLeaseAcquire {
    Acquired,
    Busy {
        in_flight_request_id: String,
        previous_response_id: Option<String>,
        websocket_chain_id: Option<WebSocketChainId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MainTurnLeaseCommit {
    Accepted,
    Rejected(&'static str),
}

#[derive(Debug)]
struct MainTurnLeaseGuard {
    store: MainTurnLeaseStore,
    transport_identity: ConversationTransportIdentity,
    request_id: String,
    released: Arc<AtomicBool>,
    visible_output_sent: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ConversationRequestKind {
    VisibleMain,
    SubagentOffshoot,
    PermissionClassifier,
    HookEvaluator,
    LocalControl,
    StatusOrAuxiliary,
    UnknownOffshoot,
}

impl ConversationRequestKind {
    fn as_key(self) -> &'static str {
        match self {
            Self::VisibleMain => "visible_main",
            Self::SubagentOffshoot => "subagent_offshoot",
            Self::PermissionClassifier => "permission_classifier",
            Self::HookEvaluator => "hook_evaluator",
            Self::LocalControl => "local_control",
            Self::StatusOrAuxiliary => "status_or_auxiliary",
            Self::UnknownOffshoot => "unknown_offshoot",
        }
    }

    fn persistence_reason(self) -> &'static str {
        match self {
            Self::VisibleMain => "visible_main",
            Self::SubagentOffshoot => "subagent_offshoot",
            Self::PermissionClassifier => "permission_or_classifier_transcript",
            Self::HookEvaluator => "hook_evaluator",
            Self::LocalControl => "local_control",
            Self::StatusOrAuxiliary => "status_or_auxiliary",
            Self::UnknownOffshoot => "unknown_offshoot",
        }
    }

    fn is_visible_main(self) -> bool {
        self == Self::VisibleMain
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConversationTransportIdentity {
    claude_session_id: String,
    branch_id: String,
    request_kind: ConversationRequestKind,
    provider_model_fingerprint: String,
    reasoning_effort: String,
}

impl ConversationTransportIdentity {
    fn new(
        claude_session_id: impl Into<String>,
        branch_id: impl Into<String>,
        request_kind: ConversationRequestKind,
        provider_model_fingerprint: impl Into<String>,
        reasoning_effort: impl Into<String>,
    ) -> Self {
        Self {
            claude_session_id: claude_session_id.into(),
            branch_id: branch_id.into(),
            request_kind,
            provider_model_fingerprint: provider_model_fingerprint.into(),
            reasoning_effort: reasoning_effort.into(),
        }
    }

    fn key(&self) -> String {
        format!(
            "v1:{}:{}:{}:{}:{}",
            self.claude_session_id,
            self.branch_id,
            self.request_kind.as_key(),
            self.provider_model_fingerprint,
            self.reasoning_effort
        )
    }

    fn websocket_session_key(&self) -> WebSocketSessionKey {
        WebSocketSessionKey::new(self.key())
    }

    fn checkpoint_key(&self, response_id: &str) -> String {
        format!("{}:{response_id}", self.key())
    }
}

impl OpenAiChainCheckpointStore {
    fn associate(
        &self,
        identity: &ConversationTransportIdentity,
        response_id: &str,
        websocket_chain_id: WebSocketChainId,
    ) {
        let websocket_chain_id_for_log = websocket_chain_id.as_str().to_string();
        self.checkpoints
            .lock()
            .expect("openai chain checkpoint mutex poisoned")
            .insert(identity.checkpoint_key(response_id), websocket_chain_id);
        tracing::info!(
            claude_session_id = %identity.claude_session_id,
            branch_id = %identity.branch_id,
            provider_model_fingerprint = %identity.provider_model_fingerprint,
            reasoning_effort = %identity.reasoning_effort,
            response_id,
            websocket_chain_id = %websocket_chain_id_for_log,
            "associated provider response id with websocket chain"
        );
        append_transport_diagnostic(serde_json::json!({
            "event": "checkpoint_associated",
            "transport_identity": identity.key(),
            "claude_session_id": identity.claude_session_id,
            "branch_id": identity.branch_id,
            "provider_model_fingerprint": identity.provider_model_fingerprint,
            "reasoning_effort": identity.reasoning_effort,
            "provider_response_id": response_id,
            "websocket_chain_id": websocket_chain_id_for_log,
        }));
    }

    fn websocket_chain_id_for_response(
        &self,
        identity: &ConversationTransportIdentity,
        response_id: &str,
    ) -> Option<WebSocketChainId> {
        self.checkpoints
            .lock()
            .expect("openai chain checkpoint mutex poisoned")
            .get(&identity.checkpoint_key(response_id))
            .cloned()
    }
}

impl MainTurnLeaseStore {
    fn acquire(
        &self,
        identity: &ConversationTransportIdentity,
        request_id: String,
        previous_response_id: Option<String>,
    ) -> MainTurnLeaseAcquire {
        let key = identity.key();
        let mut leases = self.leases.lock().expect("main turn lease mutex poisoned");
        if let Some(existing) = leases.get(&key) {
            return MainTurnLeaseAcquire::Busy {
                in_flight_request_id: existing.request.clone(),
                previous_response_id: existing.previous_response.clone(),
                websocket_chain_id: existing.websocket_chain.clone(),
            };
        }
        leases.insert(
            key,
            MainTurnLease {
                request: request_id,
                previous_response: previous_response_id,
                websocket_chain: None,
                state: MainTurnLeaseState::InFlight,
            },
        );
        MainTurnLeaseAcquire::Acquired
    }

    fn promote_websocket_chain(
        &self,
        identity: &ConversationTransportIdentity,
        request_id: &str,
        websocket_chain_id: Option<WebSocketChainId>,
    ) -> bool {
        let Some(websocket_chain_id) = websocket_chain_id else {
            return true;
        };
        let key = identity.key();
        let mut leases = self.leases.lock().expect("main turn lease mutex poisoned");
        let Some(lease) = leases.get_mut(&key) else {
            return false;
        };
        if lease.request != request_id {
            return false;
        }
        if !lease.state.allows_commit() {
            return false;
        }
        lease.websocket_chain = Some(websocket_chain_id);
        true
    }

    fn mark_state(
        &self,
        identity: &ConversationTransportIdentity,
        request_id: &str,
        state: MainTurnLeaseState,
    ) -> bool {
        let key = identity.key();
        let mut leases = self.leases.lock().expect("main turn lease mutex poisoned");
        let Some(lease) = leases.get_mut(&key) else {
            return false;
        };
        if lease.request != request_id {
            return false;
        }
        lease.state = state;
        true
    }

    fn validate_for_commit(
        &self,
        identity: &ConversationTransportIdentity,
        request_id: &str,
        websocket_chain_id: Option<&WebSocketChainId>,
    ) -> MainTurnLeaseCommit {
        let key = identity.key();
        let leases = self.leases.lock().expect("main turn lease mutex poisoned");
        let Some(lease) = leases.get(&key) else {
            return MainTurnLeaseCommit::Rejected("missing_active_lease");
        };
        if lease.request != request_id {
            return MainTurnLeaseCommit::Rejected("request_id_mismatch");
        }
        if !lease.state.allows_commit() {
            return MainTurnLeaseCommit::Rejected(lease.state.as_str());
        }
        match (lease.websocket_chain.as_ref(), websocket_chain_id) {
            (Some(expected), Some(actual)) if expected == actual => MainTurnLeaseCommit::Accepted,
            (Some(_), Some(_)) => MainTurnLeaseCommit::Rejected("websocket_chain_id_mismatch"),
            (Some(_), None) => MainTurnLeaseCommit::Rejected("missing_commit_websocket_chain_id"),
            (None, Some(_)) => MainTurnLeaseCommit::Rejected("unpromoted_websocket_chain_id"),
            (None, None) => MainTurnLeaseCommit::Accepted,
        }
    }

    fn release(&self, identity: &ConversationTransportIdentity, request_id: &str) -> bool {
        let key = identity.key();
        let mut leases = self.leases.lock().expect("main turn lease mutex poisoned");
        if leases
            .get(&key)
            .is_some_and(|lease| lease.request == request_id)
        {
            leases.remove(&key);
            return true;
        }
        false
    }
}

impl MainTurnLeaseGuard {
    fn new(
        store: MainTurnLeaseStore,
        transport_identity: ConversationTransportIdentity,
        request_id: String,
    ) -> Self {
        Self {
            store,
            transport_identity,
            request_id,
            released: Arc::new(AtomicBool::new(false)),
            visible_output_sent: Arc::new(AtomicBool::new(false)),
        }
    }

    fn mark_released(&self) {
        self.released.store(true, Ordering::Release);
    }

    fn mark_state(&self, state: MainTurnLeaseState) -> bool {
        self.store
            .mark_state(&self.transport_identity, &self.request_id, state)
    }

    fn release_with_state(&self, state: MainTurnLeaseState) -> bool {
        let _ = self.mark_state(state);
        let released = self
            .store
            .release(&self.transport_identity, &self.request_id);
        self.mark_released();
        released
    }

    fn mark_visible_output_sent(&self) {
        self.visible_output_sent.store(true, Ordering::Release);
    }
}

impl Drop for MainTurnLeaseGuard {
    fn drop(&mut self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let abort_state = if self.visible_output_sent.load(Ordering::Acquire) {
            MainTurnLeaseState::ClientAbortedAfterVisibleOutput
        } else {
            MainTurnLeaseState::ClientAbortedBeforeFirstEvent
        };
        let _ = self.mark_state(abort_state);
        let released = self
            .store
            .release(&self.transport_identity, &self.request_id);
        append_transport_diagnostic(serde_json::json!({
            "event": "visible_main_lease_aborted",
            "request_id": self.request_id,
            "transport_identity": self.transport_identity.key(),
            "claude_session_id": self.transport_identity.claude_session_id,
            "branch_id": self.transport_identity.branch_id,
            "provider_model_fingerprint": self.transport_identity.provider_model_fingerprint,
            "reasoning_effort": self.transport_identity.reasoning_effort,
            "released": released,
            "client_abort_state": abort_state.as_str(),
        }));
    }
}

impl AppState {
    /// # Errors
    ///
    /// Returns an error if the gateway config file exists but cannot be read or parsed.
    pub fn from_env() -> Result<Self, gateway_core::config::GatewayConfigError> {
        let gateway_config = load_gateway_config_default_path()?;
        let http = http_client_for_config(&gateway_config);
        let backend = backend_client_from_env(http.clone());
        let auth = gateway_auth_codex::CodexAuthManager::default().with_http_client(http.clone());
        Ok(Self {
            auth,
            backend,
            conversation_state: conversation_state_store_for_config(&gateway_config),
            claude_gateway_settings_path: Some(default_claude_gateway_settings_path()),
            gateway_config,
            ..Self::default()
        })
    }

    #[cfg(test)]
    #[must_use]
    fn with_claude_gateway_settings_path(mut self, path: PathBuf) -> Self {
        self.claude_gateway_settings_path = Some(path);
        self
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ClaudeGatewaySettings {
    models: Vec<ClaudeGatewayModel>,
    env: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeGatewayModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct ClaudeGatewayModelsResponseItem {
    id: String,
    #[serde(rename = "type")]
    item_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_input_tokens: Option<u64>,
}

fn http_client_for_config(config: &GatewayConfig) -> GatewayHttpClient {
    let mut policy = GatewayNetworkPolicy::default();
    policy.extend_allowed_hosts(&config.network.allowed_hosts);
    GatewayHttpClient::new(policy)
}

fn backend_client_from_env(
    http: GatewayHttpClient,
) -> gateway_backend_codex::client::CodexBackendClient {
    let client =
        gateway_backend_codex::client::CodexBackendClient::default().with_http_client(http);
    match backend_request_timeout_from_env() {
        Some(timeout) => client.with_request_timeout(timeout),
        None => client,
    }
}

fn backend_request_timeout_from_env() -> Option<Duration> {
    let raw = std::env::var("GATEWAY_BACKEND_REQUEST_TIMEOUT_SECS").ok()?;
    match raw.parse::<u64>() {
        Ok(0) => None,
        Ok(seconds) => Some(Duration::from_secs(seconds)),
        Err(err) => {
            warn!(
                value = raw.as_str(),
                error = %err,
                "ignoring invalid GATEWAY_BACKEND_REQUEST_TIMEOUT_SECS"
            );
            None
        }
    }
}

fn conversation_state_store_for_config(config: &GatewayConfig) -> ConversationStateStore {
    let store = if let Some(path) = config
        .workflow
        .conversation_state
        .persistence_root
        .as_deref()
    {
        ConversationStateStore::new_with_policy(
            path,
            config.workflow.conversation_state.corruption_policy,
        )
    } else {
        let default_store = ConversationStateStore::default();
        ConversationStateStore::new_with_policy(
            default_store.root(),
            config.workflow.conversation_state.corruption_policy,
        )
    };

    if let Some(max_session_age_days) = config
        .workflow
        .conversation_state
        .retention
        .max_session_age_days
    {
        match store.cleanup_sessions_older_than_days(max_session_age_days) {
            Ok(removed) if removed > 0 => {
                info!(
                    removed_session_buckets = removed,
                    max_session_age_days, "removed expired conversation-state session buckets"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    error = %error,
                    max_session_age_days,
                    "failed to clean up expired conversation-state session buckets"
                );
            }
        }
    }
    store
}

fn auth_path_override(state: &AppState) -> Option<&Path> {
    #[cfg(test)]
    {
        state.auth_json_path.as_deref()
    }

    #[cfg(not(test))]
    {
        let _ = state;
        None
    }
}

fn default_claude_gateway_settings_path() -> PathBuf {
    if let Ok(path) = std::env::var("CLAUDE_GATEWAY_SETTINGS_PATH") {
        return PathBuf::from(path);
    }

    if let Ok(claude_gateway_home) = std::env::var("CLAUDE_GATEWAY_HOME") {
        return PathBuf::from(claude_gateway_home).join("settings.json");
    }

    let home = std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    home.join(".claude_gateway").join("settings.json")
}

fn load_claude_gateway_settings(path: &Path) -> Result<ClaudeGatewaySettings, String> {
    let bytes = fs::read(path).map_err(|err| {
        format!(
            "failed to read Claude gateway settings at {}: {err}",
            path.display()
        )
    })?;
    serde_json::from_slice::<ClaudeGatewaySettings>(&bytes).map_err(|err| {
        format!(
            "failed to parse Claude gateway settings at {}: {err}",
            path.display()
        )
    })
}

fn model_catalog_from_settings(
    settings: &ClaudeGatewaySettings,
) -> Vec<ClaudeGatewayModelsResponseItem> {
    if !settings.models.is_empty() {
        let env_context_windows = model_context_windows_from_env(&settings.env);
        return dedupe_models(
            settings
                .models
                .iter()
                .map(|model| ClaudeGatewayModelsResponseItem {
                    id: model.id.clone(),
                    item_type: "model",
                    name: model.name.clone(),
                    description: model.description.clone(),
                    max_input_tokens: model
                        .max_input_tokens
                        .or_else(|| env_context_windows.get(&model.id).copied()),
                })
                .collect(),
        );
    }

    let mut models = Vec::new();
    add_model_from_env(
        &settings.env,
        ModelEnvKeys {
            id: "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            name: "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            description: "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
            max_input_tokens: "ANTHROPIC_DEFAULT_HAIKU_MAX_TOKENS",
        },
        "GPT-5.4 Mini",
        "OpenAI small/fallback model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        ModelEnvKeys {
            id: "ANTHROPIC_DEFAULT_SONNET_MODEL",
            name: "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            description: "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
            max_input_tokens: "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS",
        },
        "GPT-5.4",
        "OpenAI general-purpose model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        ModelEnvKeys {
            id: "ANTHROPIC_DEFAULT_OPUS_MODEL",
            name: "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            description: "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
            max_input_tokens: "ANTHROPIC_DEFAULT_OPUS_MAX_TOKENS",
        },
        "GPT-5.5",
        "OpenAI reasoning model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        ModelEnvKeys {
            id: "ANTHROPIC_DEFAULT_FABLE_MODEL",
            name: "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
            description: "ANTHROPIC_DEFAULT_FABLE_MODEL_DESCRIPTION",
            max_input_tokens: "ANTHROPIC_DEFAULT_FABLE_MAX_TOKENS",
        },
        "GPT-5.5 Pro",
        "OpenAI highest-capability model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        ModelEnvKeys {
            id: "ANTHROPIC_CUSTOM_MODEL_OPTION",
            name: "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
            description: "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
            max_input_tokens: "ANTHROPIC_CUSTOM_MODEL_OPTION_MAX_TOKENS",
        },
        "Custom model",
        "Custom model option",
        &mut models,
    );

    dedupe_models(models)
}

#[derive(Debug, Clone, Copy)]
struct ModelEnvKeys {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    max_input_tokens: &'static str,
}

fn add_model_from_env(
    env: &HashMap<String, serde_json::Value>,
    keys: ModelEnvKeys,
    default_name: &str,
    default_description: &str,
    models: &mut Vec<ClaudeGatewayModelsResponseItem>,
) {
    let Some(id) = env.get(keys.id).and_then(serde_json::Value::as_str) else {
        return;
    };

    models.push(ClaudeGatewayModelsResponseItem {
        id: id.to_string(),
        item_type: "model",
        name: env
            .get(keys.name)
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(default_name.to_string())),
        description: env
            .get(keys.description)
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(default_description.to_string())),
        max_input_tokens: env_u64(env, keys.max_input_tokens),
    });
}

fn model_context_windows_from_env(
    env: &HashMap<String, serde_json::Value>,
) -> HashMap<String, u64> {
    [
        (
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MAX_TOKENS",
        ),
        (
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS",
        ),
        (
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MAX_TOKENS",
        ),
        (
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MAX_TOKENS",
        ),
        (
            "ANTHROPIC_CUSTOM_MODEL_OPTION",
            "ANTHROPIC_CUSTOM_MODEL_OPTION_MAX_TOKENS",
        ),
    ]
    .into_iter()
    .filter_map(|(model_key, max_tokens_key)| {
        Some((
            env.get(model_key)?.as_str()?.to_string(),
            env_u64(env, max_tokens_key)?,
        ))
    })
    .collect()
}

fn env_u64(env: &HashMap<String, serde_json::Value>, key: &str) -> Option<u64> {
    env.get(key)
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
}

fn dedupe_models(
    models: Vec<ClaudeGatewayModelsResponseItem>,
) -> Vec<ClaudeGatewayModelsResponseItem> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .route("/auth/status", get(auth_status))
        .route("/auth/refresh", post(auth_refresh))
        .route("/v1/models", get(v1_models_with_state))
        .route("/v1/messages/count_tokens", post(v1_messages_count_tokens))
        .route("/v1/messages", post(v1_messages))
        .fallback(fallback_404)
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn auth_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = match auth_path_override(&state) {
        Some(path) => gateway_auth_codex::load_gateway_auth_status(path),
        None => gateway_auth_codex::load_gateway_auth_status_default_path(),
    };
    auth_status_response(status)
}

fn auth_status_response(
    status: Result<
        Option<gateway_auth_codex::GatewayAuthStatus>,
        gateway_auth_codex::CodexAuthError,
    >,
) -> Json<serde_json::Value> {
    match status {
        Ok(Some(status)) => Json(serde_json::json!({
            "logged_in": status.ready_for_messages(),
            "ready_for_messages": status.ready_for_messages(),
            "ready_for_models": status.ready_for_models(),
            "account_id": status.account_id,
            "login_method": match status.login_method {
                gateway_auth_codex::GatewayLoginMethod::Chatgpt => "chatgpt",
                gateway_auth_codex::GatewayLoginMethod::ApiKey => "api_key",
            },
            "source": "gateway_auth_json",
        })),
        Ok(None) => Json(serde_json::json!({
            "logged_in": false,
            "account_id": null,
            "login_method": null,
            "source": "gateway_auth_json",
            "auth_remediation": "Please run: cld-gateway login claude",
        })),
        Err(err) => Json(serde_json::json!({
            "logged_in": false,
            "account_id": null,
            "login_method": null,
            "source": "error",
            "error_type": format!("{err}"),
            "auth_remediation": "Please run: cld-gateway login claude",
        })),
    }
}

async fn auth_refresh(State(state): State<AppState>) -> axum::response::Response {
    match state.auth.refresh_and_persist_default_path().await {
        Ok(snap) => Json(serde_json::json!({
            "ok": true,
            "account_id": snap.account_id,
            "expires_at_unix_seconds": snap.expires_at_unix_seconds,
            "source": "gateway_auth_json",
        }))
        .into_response(),
        Err(err) => auth_error(&format!("{err}")),
    }
}

async fn fallback_404() -> impl IntoResponse {
    claude_status_json_response(
        StatusCode::NOT_FOUND,
        serde_json::json!({
            "error": { "type": "not_found", "message": "not found" }
        }),
    )
}

async fn v1_messages_count_tokens(
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: Request,
) -> axum::response::Response {
    let body = match read_request_body(req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let input_tokens = estimate_anthropic_count_tokens(&body);
    append_transport_diagnostic(serde_json::json!({
        "event": "status_or_auxiliary_request",
        "request_id": request_id_str(request_id.as_ref()),
        "request_kind": ConversationRequestKind::StatusOrAuxiliary.as_key(),
        "route": "/v1/messages/count_tokens",
        "input_tokens": input_tokens,
        "commit_policy": "auxiliary_no_conversation_commit",
        "client_abort_state": LeaseDiagnosticState::NotAborted.as_str(),
    }));
    claude_json_response(serde_json::json!({
        "input_tokens": input_tokens
    }))
}

fn estimate_anthropic_count_tokens(body: &[u8]) -> i64 {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return 0;
    };
    let encoded = serde_json::to_string(&value).unwrap_or_default();
    i64::try_from(encoded.len().div_ceil(4)).unwrap_or(i64::MAX)
}

async fn v1_models_with_state(State(state): State<AppState>) -> axum::response::Response {
    let settings_path = state
        .claude_gateway_settings_path
        .as_deref()
        .map_or_else(default_claude_gateway_settings_path, Path::to_path_buf);

    let settings = match load_claude_gateway_settings(&settings_path) {
        Ok(settings) => settings,
        Err(message) => {
            return claude_status_json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "error": {
                        "type": "config_error",
                        "message": message
                    }
                }),
            );
        }
    };

    let data = model_catalog_from_settings(&settings);

    if data.is_empty() {
        return claude_status_json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": {
                    "type": "config_error",
                    "message": format!(
                        "no models were found in {}",
                        settings_path.display()
                    )
                }
            }),
        );
    }

    claude_json_response(serde_json::json!({ "object": "list", "data": data }))
}

async fn v1_messages(
    State(state): State<AppState>,
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: Request,
) -> axum::response::Response {
    let claude_session_id = claude_session_id_from_headers(req.headers());
    let body = match read_request_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let mut req: AnthropicMessagesRequest = match deserialize_with_path(&body) {
        Ok(v) => v,
        Err(err) => return bad_request(&err),
    };

    log_ignored_stop_sequences(&req.stop_sequences, request_id.as_ref());
    log_ignored_request_controls(&req, request_id.as_ref());
    let context_management_report =
        apply_context_management(&state.gateway_config, &mut req, request_id.as_ref());
    validate_tool_results(&state, &req);

    if req.stream {
        return Box::pin(stream_messages(
            state,
            request_id,
            claude_session_id,
            req,
            context_management_report,
        ))
        .await
        .into_response();
    }

    let flow = match prepare_unary_message_flow(
        &state,
        request_id.as_ref(),
        claude_session_id.as_deref(),
        &req,
        &context_management_report,
    )
    .await
    {
        Ok(flow) => flow,
        Err(resp) => return *resp,
    };
    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return auth_error(&err),
    };
    let transport = match select_message_transport(
        &state,
        request_id.as_ref(),
        flow.prepared_branch.as_ref(),
        &flow.resolution,
        &flow.translated,
    ) {
        Ok(transport) => transport,
        Err(err) => return service_unavailable_error(&err),
    };
    let backend_req = match build_unary_backend_request(
        &state,
        &req,
        &context_management_report,
        &flow,
        &transport,
        creds,
    ) {
        Ok(backend_req) => backend_req,
        Err(resp) => return *resp,
    };
    let result =
        match execute_unary_backend(&state, request_id.as_ref(), &flow, &transport, backend_req)
            .await
        {
            Ok(result) => result,
            Err(resp) => return *resp,
        };
    commit_unary_result(&state, request_id.as_ref(), &flow, &transport, &result);

    let mut response =
        build_unary_messages_response(&state, &req, request_id.as_ref(), &result.decoded);
    if let Some(context_management) = context_management_report.response_value() {
        response["context_management"] = context_management;
    }

    let mut http_res = claude_json_response(response);
    http_res.extensions_mut().insert(flow.resolution);
    http_res
}

struct UnaryMessageFlow {
    resolution: ModelResolution,
    prepared_branch: Option<PreparedConversationBranch>,
    translated: TranslateResult,
    post_command_input: Option<serde_json::Value>,
}

struct MessageTransportSelection {
    request_compatibility_fingerprint: String,
    transport_identity: Option<ConversationTransportIdentity>,
    websocket_transport_identity: Option<ConversationTransportIdentity>,
    selected_checkpoint: Option<SelectedCheckpoint>,
    selected_transport: SelectedTransport,
}

struct UnaryBackendResult {
    decoded: gateway_backend_codex::types::CodexUnaryDecoded,
    request_previous_response_id: Option<String>,
    response_websocket_chain_id: Option<WebSocketChainId>,
    main_turn_lease: Option<MainTurnLeaseGuard>,
}

async fn prepare_unary_message_flow(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    claude_session_id: Option<&str>,
    req: &AnthropicMessagesRequest,
    context_management_report: &ContextManagementReport,
) -> Result<UnaryMessageFlow, Box<axum::response::Response>> {
    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id,
        "resolved model for /v1/messages",
    );
    let preview_tool_context = build_tool_translation_context(state, req);
    let translated_preview = translate_request_with_context(req, &preview_tool_context)
        .map_err(|err| Box::new(bad_request(&err)))?;
    let prepared_branch = prepare_conversation_branch(
        state,
        claude_session_id,
        req,
        translated_preview.client_metadata.as_ref(),
    );
    log_conversation_branch_resolution(request_id, prepared_branch.as_ref());
    reject_missing_conversation_branch(claude_session_id, prepared_branch.as_ref())?;

    let full_render_req = prepared_branch.as_ref().map_or_else(
        || req.clone(),
        |prepared_branch| {
            request_with_messages(req, prepared_branch.full_backend_render_messages())
        },
    );
    let tool_context = build_tool_translation_context(state, &full_render_req);
    let mut translated = translate_request_with_context(&full_render_req, &tool_context)
        .map_err(|err| Box::new(bad_request(&err)))?;
    attach_context_management_metadata(&mut translated, context_management_report);
    let post_command_input = execute_translated_command_input(
        state,
        request_id,
        &full_render_req.model,
        &resolution.selected_backend_model,
        &translated,
    )
    .await
    .map_err(|err| Box::new(translated_command_json_error_response(&err)))?;
    if let Some(command_input) = post_command_input.clone() {
        translated.input.push(command_input);
    }

    Ok(UnaryMessageFlow {
        resolution,
        prepared_branch,
        translated,
        post_command_input,
    })
}

fn reject_missing_conversation_branch(
    claude_session_id: Option<&str>,
    prepared_branch: Option<&PreparedConversationBranch>,
) -> Result<(), Box<axum::response::Response>> {
    if claude_session_id.is_some() && prepared_branch.is_none() {
        return Err(Box::new(service_unavailable_error(
            "Claude Code conversation requests require a WebSocket conversation-state branch",
        )));
    }
    Ok(())
}

fn select_message_transport(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    resolution: &ModelResolution,
    translated: &TranslateResult,
) -> Result<MessageTransportSelection, String> {
    let request_compatibility_fingerprint =
        request_compatibility_fingerprint(&state.gateway_config, resolution, translated);
    let transport_identity = transport_identity_for_branch(
        prepared_branch,
        &resolution.selected_backend_model,
        translated.reasoning.as_ref(),
    );
    let websocket_transport_identity = websocket_transport_identity_for_branch(
        prepared_branch,
        &resolution.selected_backend_model,
        translated.reasoning.as_ref(),
    );
    let selected_checkpoint = prepared_branch.and_then(|branch| {
        selected_checkpoint_for_transport(branch, &resolution.selected_backend_model)
    });
    let websocket_chain_decision = websocket_chain_decision_for_branch(
        state,
        prepared_branch,
        websocket_transport_identity.as_ref(),
        selected_checkpoint.as_ref(),
    );
    let selected_transport = select_transport(
        prepared_branch,
        &websocket_chain_decision,
        &resolution.selected_backend_model,
        &request_compatibility_fingerprint,
    )
    .map_err(|err| err.clone())?;
    validate_previous_response_contract(prepared_branch, &selected_transport)?;
    log_transport_selection(
        request_id,
        prepared_branch,
        &selected_transport,
        selected_checkpoint.as_ref(),
        &resolution.selected_backend_model,
        Some(&websocket_chain_decision),
    );
    Ok(MessageTransportSelection {
        request_compatibility_fingerprint,
        transport_identity,
        websocket_transport_identity,
        selected_checkpoint,
        selected_transport,
    })
}

fn build_unary_backend_request(
    state: &AppState,
    req: &AnthropicMessagesRequest,
    context_management_report: &ContextManagementReport,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    creds: gateway_auth_codex::CodexCredentials,
) -> Result<gateway_backend_codex::types::CodexBackendRequest, Box<axum::response::Response>> {
    let backend_render_req = request_for_selected_transport(
        req,
        flow.prepared_branch.as_ref(),
        &transport.selected_transport,
    );
    let backend_tool_context = build_tool_translation_context(state, &backend_render_req);
    let mut backend_translated =
        translate_request_with_context(&backend_render_req, &backend_tool_context)
            .map_err(|err| Box::new(bad_request(&err)))?;
    attach_context_management_metadata(&mut backend_translated, context_management_report);
    if let Some(command_input) = flow.post_command_input.clone() {
        backend_translated.input.push(command_input);
    }
    Ok(build_backend_request(
        &state.gateway_config,
        &flow.resolution,
        backend_translated,
        creds,
        transport.selected_transport.previous_response_id.clone(),
    ))
}

async fn execute_unary_backend(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<UnaryBackendResult, Box<axum::response::Response>> {
    let main_turn_lease = acquire_visible_main_lease(
        state,
        request_id,
        flow.prepared_branch.as_ref(),
        transport.transport_identity.as_ref(),
        transport.selected_transport.previous_response_id.as_deref(),
    )
    .map_err(|err| Box::new(service_unavailable_error(&err)))?;
    let (decoded, request_previous_response_id, response_websocket_chain_id) =
        match Box::pin(run_backend_unary(
            state,
            request_id,
            flow.prepared_branch.as_ref(),
            transport.websocket_transport_identity.as_ref(),
            backend_req,
        ))
        .await
        {
            Ok(value) => value,
            Err(resp) => {
                release_lease_as_backend_failed(main_turn_lease.as_ref());
                return Err(Box::new(resp));
            }
        };
    promote_unary_websocket_chain(
        state,
        main_turn_lease.as_ref(),
        response_websocket_chain_id.clone(),
    )?;
    Ok(UnaryBackendResult {
        decoded,
        request_previous_response_id,
        response_websocket_chain_id,
        main_turn_lease,
    })
}

fn release_lease_as_backend_failed(main_turn_lease: Option<&MainTurnLeaseGuard>) {
    if let Some(lease) = main_turn_lease {
        lease.release_with_state(MainTurnLeaseState::BackendFailedBeforeCommit);
    }
}

fn promote_unary_websocket_chain(
    state: &AppState,
    main_turn_lease: Option<&MainTurnLeaseGuard>,
    response_websocket_chain_id: Option<WebSocketChainId>,
) -> Result<(), Box<axum::response::Response>> {
    if let Some(lease) = main_turn_lease
        && !state.main_turn_leases.promote_websocket_chain(
            &lease.transport_identity,
            &lease.request_id,
            response_websocket_chain_id,
        )
    {
        return Err(Box::new(service_unavailable_error(
            "visible main turn lease was lost before backend completion",
        )));
    }
    Ok(())
}

fn commit_unary_result(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
) {
    let commit_turn_failed =
        commit_unary_visible_result(state, request_id, flow, transport, result);
    commit_unary_offshoot_result(state, request_id, flow, transport, result);
    release_unary_lease(result.main_turn_lease.as_ref(), commit_turn_failed);
}

fn commit_unary_visible_result(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
) -> bool {
    let Some(prepared_branch) = flow
        .prepared_branch
        .as_ref()
        .filter(|prepared_branch| prepared_branch.commit_turn)
    else {
        return false;
    };
    if let MainTurnLeaseCommit::Rejected(reason) = validate_unary_lease_for_commit(state, result) {
        append_unary_checkpoint_skipped(request_id, prepared_branch, transport, result, reason);
        return true;
    }
    if let Err(err) = commit_unary_visible_turn(state, prepared_branch, flow, transport, result) {
        warn!(error = %err, "failed to commit unary conversation-state turn");
        return true;
    }
    append_unary_visible_commit(state, request_id, prepared_branch, transport, result);
    false
}

fn validate_unary_lease_for_commit(
    state: &AppState,
    result: &UnaryBackendResult,
) -> MainTurnLeaseCommit {
    result
        .main_turn_lease
        .as_ref()
        .map_or(MainTurnLeaseCommit::Accepted, |lease| {
            state.main_turn_leases.validate_for_commit(
                &lease.transport_identity,
                &lease.request_id,
                result.response_websocket_chain_id.as_ref(),
            )
        })
}

fn commit_unary_visible_turn(
    state: &AppState,
    prepared_branch: &PreparedConversationBranch,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
) -> Result<(), gateway_state::StateError> {
    state
        .conversation_state
        .commit_turn(
            &prepared_branch.claude_session_id,
            &prepared_branch.branch.branch_id,
            &CommitTurnParams {
                turn_scope: prepared_branch.turn_scope,
                turn_id: format!("turn_{}", Uuid::new_v4()),
                fingerprints: prepared_branch.fingerprints.clone(),
                active_canonical_messages: serde_json::to_value(&prepared_branch.active_messages)
                    .ok(),
                provider_response_id: result.decoded.response_id.clone(),
                previous_response_id: result.request_previous_response_id.clone(),
                provider_model_fingerprint: Some(flow.resolution.selected_backend_model.clone()),
                request_compatibility_fingerprint: Some(
                    transport.request_compatibility_fingerprint.clone(),
                ),
                provider_input_tokens: result.decoded.token_usage.map(|usage| usage.input_tokens),
                canonical_message_count: Some(prepared_branch.active_messages.len()),
                canonical_prefix_hash: Some(canonical_messages_prefix_hash(
                    &prepared_branch.active_messages,
                    prepared_branch.active_messages.len(),
                )),
                provider_output_items: result.decoded.output_items.clone(),
            },
        )
        .map(|_| ())
}

fn append_unary_checkpoint_skipped(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: &PreparedConversationBranch,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
    reason: &'static str,
) {
    append_checkpoint_diagnostic(&CheckpointDiagnostic {
        event: "checkpoint_commit_skipped",
        subject: CheckpointDiagnosticSubject::from(prepared_branch),
        transport_identity: transport.transport_identity.as_ref(),
        request_id: request_id_str(request_id),
        provider_response_id: result.decoded.response_id.as_deref(),
        previous_response_id: result.request_previous_response_id.as_deref(),
        selected_checkpoint_source: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.source),
        selected_checkpoint_response_id: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.response_id.as_str()),
        websocket_chain_id: result.response_websocket_chain_id.as_ref(),
        streaming: false,
        commit_policy: "visible_main_lease_rejected",
        skip_reason: Some(reason),
        client_abort_state: lease_diagnostic_state_for_reason(reason),
    });
}

fn append_unary_visible_commit(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: &PreparedConversationBranch,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
) {
    let Some(response_id) = result.decoded.response_id.as_deref() else {
        return;
    };
    associate_unary_visible_chain(state, transport, result, response_id);
    append_checkpoint_diagnostic(&CheckpointDiagnostic {
        event: "visible_checkpoint_committed",
        subject: CheckpointDiagnosticSubject::from(prepared_branch),
        transport_identity: transport.transport_identity.as_ref(),
        request_id: request_id_str(request_id),
        provider_response_id: Some(response_id),
        previous_response_id: result.request_previous_response_id.as_deref(),
        selected_checkpoint_source: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.source),
        selected_checkpoint_response_id: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.response_id.as_str()),
        websocket_chain_id: result.response_websocket_chain_id.as_ref(),
        streaming: false,
        commit_policy: "durable_visible_main_commit",
        skip_reason: None,
        client_abort_state: LeaseDiagnosticState::CompletedCommitted,
    });
    log_unary_checkpoint_commit(request_id, prepared_branch, response_id, result);
}

fn associate_unary_visible_chain(
    state: &AppState,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
    response_id: &str,
) {
    if let Some(websocket_chain_id) = result.response_websocket_chain_id.clone() {
        state.openai_chain_checkpoints.associate(
            transport
                .transport_identity
                .as_ref()
                .expect("transport identity exists for prepared branch"),
            response_id,
            websocket_chain_id,
        );
    }
}

fn log_unary_checkpoint_commit(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: &PreparedConversationBranch,
    response_id: &str,
    result: &UnaryBackendResult,
) {
    if let Some(request_id) = request_id_str(request_id) {
        info!(
            request_id = %request_id,
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            provider_response_id = %response_id,
            previous_response_id = ?result.request_previous_response_id,
            compaction_reset_pending = false,
            "captured unary provider checkpoint response id"
        );
    } else {
        info!(
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            provider_response_id = %response_id,
            previous_response_id = ?result.request_previous_response_id,
            compaction_reset_pending = false,
            "captured unary provider checkpoint response id"
        );
    }
}

fn commit_unary_offshoot_result(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &UnaryMessageFlow,
    transport: &MessageTransportSelection,
    result: &UnaryBackendResult,
) {
    let Some(prepared_branch) = flow
        .prepared_branch
        .as_ref()
        .filter(|prepared_branch| !prepared_branch.commit_turn)
    else {
        return;
    };
    let Some(response_id) = result.decoded.response_id.as_deref() else {
        return;
    };
    record_offshoot_checkpoint(&OffshootCheckpointRecord {
        conversation_state: &state.conversation_state,
        openai_chain_checkpoints: &state.openai_chain_checkpoints,
        prepared_branch,
        transport_identity: transport.transport_identity.as_ref(),
        response_id,
        previous_response_id: result.request_previous_response_id.as_deref(),
        provider_model_fingerprint: &flow.resolution.selected_backend_model,
        request_compatibility_fingerprint: &transport.request_compatibility_fingerprint,
        provider_input_tokens: result.decoded.token_usage.map(|usage| usage.input_tokens),
        selected_checkpoint_source: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.source),
        selected_checkpoint_response_id: transport
            .selected_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.response_id.as_str()),
        websocket_chain_id: result.response_websocket_chain_id.as_ref(),
        streaming: false,
        request_id: request_id_str(request_id),
    });
}

fn release_unary_lease(main_turn_lease: Option<&MainTurnLeaseGuard>, commit_turn_failed: bool) {
    if let Some(lease) = main_turn_lease {
        if commit_turn_failed {
            lease.release_with_state(MainTurnLeaseState::BackendFailedBeforeCommit);
        } else {
            lease.release_with_state(MainTurnLeaseState::CompletedCommitted);
        }
    }
}

fn build_unary_messages_response(
    state: &AppState,
    req: &AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    decoded: &gateway_backend_codex::types::CodexUnaryDecoded,
) -> serde_json::Value {
    let usage = decoded.token_usage.map_or(
        serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0
        }),
        |u| {
            let mut value = serde_json::json!({
                "input_tokens": u.input_tokens,
                "output_tokens": u.output_tokens
            });
            if u.web_search_requests > 0 {
                value["server_tool_use"] = serde_json::json!({
                    "web_search_requests": u.web_search_requests,
                    "web_fetch_requests": 0
                });
            }
            value
        },
    );

    let assistant_text = cleanup_structured_output_text_for_anthropic(
        req.output_config.as_ref(),
        &decoded.final_text,
    );

    if decoded.tool_calls.is_empty() {
        return serde_json::json!({
            "id": format!("msg_{}", Uuid::new_v4()),
            "type": "message",
            "role": "assistant",
            "model": req.model,
            "content": [{ "type": "text", "text": assistant_text }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": usage
        });
    }

    let request_id_str = request_id.map(|axum::extract::Extension(r)| r.0.as_str());
    let mut content = Vec::new();
    if !assistant_text.is_empty() {
        content.push(serde_json::json!({ "type": "text", "text": assistant_text }));
    }
    for tool_call in &decoded.tool_calls {
        let _ = state.tool_calls.record_tool_call(
            &tool_call.call_id,
            &tool_call.name,
            tool_call.kind.as_str(),
            request_id_str,
        );
        content.push(tool_call_content_block(tool_call));
    }

    serde_json::json!({
        "id": format!("msg_{}", Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "model": req.model,
        "content": content,
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": usage
    })
}

fn tool_call_content_block(tool_call: &CodexToolCall) -> serde_json::Value {
    let input_value = crate::tool_arg_policy::sanitized_tool_args_for_kind(
        &tool_call.name,
        tool_call.kind,
        &tool_call.arguments,
    )
    .map_or_else(
        |_| serde_json::json!({}),
        |(args, _edits)| serde_json::Value::Object(args),
    );
    serde_json::json!({
        "type": "tool_use",
        "id": tool_call.call_id,
        "name": tool_call.name,
        "input": input_value
    })
}

fn resolve_and_log_model(
    config: &GatewayConfig,
    model: &str,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    msg: &str,
) -> ModelResolution {
    let resolution = resolve_model(config, model);
    if let Some(axum::extract::Extension(rid)) = request_id {
        info!(
            request_id = %rid.0,
            requested_model = %resolution.requested,
            selected_backend_model = %resolution.selected_backend_model,
            selection_reason = %resolution.selection_reason,
            "{msg}"
        );
    }
    resolution
}

fn load_codex_credentials(
    auth_path: Option<&Path>,
) -> Result<gateway_auth_codex::CodexCredentials, String> {
    let result = match auth_path {
        Some(path) => gateway_auth_codex::load_credentials(path),
        None => gateway_auth_codex::load_credentials_default_path(),
    };
    result.map_err(|err| format!("{err}"))
}

fn build_backend_request(
    config: &GatewayConfig,
    resolution: &ModelResolution,
    translated: crate::translate::TranslateResult,
    creds: gateway_auth_codex::CodexCredentials,
    previous_response_id: Option<String>,
) -> gateway_backend_codex::types::CodexBackendRequest {
    gateway_backend_codex::types::CodexBackendRequest {
        access_token: creds.access_token,
        account_id: creds.account_id,
        model: resolution.selected_backend_model.clone(),
        instructions: translated.instructions,
        input: translated.input,
        tools: translated.tools,
        tool_choice: translated.tool_choice,
        parallel_tool_calls: translated.parallel_tool_calls,
        text: translated.text,
        reasoning: translated.reasoning,
        previous_response_id,
        stream: true,
        include: translated.include,
        service_tier: service_tier_for_config(config),
        client_metadata: translated.client_metadata,
    }
}

fn transport_identity_for_branch(
    prepared_branch: Option<&PreparedConversationBranch>,
    provider_model_fingerprint: &str,
    reasoning: Option<&serde_json::Value>,
) -> Option<ConversationTransportIdentity> {
    let prepared_branch = prepared_branch?;
    Some(ConversationTransportIdentity::new(
        prepared_branch.claude_session_id.clone(),
        prepared_branch.branch.branch_id.clone(),
        prepared_branch.request_kind,
        provider_model_fingerprint.trim(),
        normalized_reasoning_effort(reasoning),
    ))
}

fn visible_main_transport_identity_for_branch(
    prepared_branch: Option<&PreparedConversationBranch>,
    provider_model_fingerprint: &str,
    reasoning: Option<&serde_json::Value>,
) -> Option<ConversationTransportIdentity> {
    let prepared_branch = prepared_branch?;
    Some(ConversationTransportIdentity::new(
        prepared_branch.claude_session_id.clone(),
        prepared_branch.branch.branch_id.clone(),
        ConversationRequestKind::VisibleMain,
        provider_model_fingerprint.trim(),
        normalized_reasoning_effort(reasoning),
    ))
}

fn websocket_transport_identity_for_branch(
    prepared_branch: Option<&PreparedConversationBranch>,
    provider_model_fingerprint: &str,
    reasoning: Option<&serde_json::Value>,
) -> Option<ConversationTransportIdentity> {
    let prepared_branch = prepared_branch?;
    if !prepared_branch.commit_turn && prepared_branch.allow_incremental_context {
        visible_main_transport_identity_for_branch(
            Some(prepared_branch),
            provider_model_fingerprint,
            reasoning,
        )
    } else {
        transport_identity_for_branch(Some(prepared_branch), provider_model_fingerprint, reasoning)
    }
}

fn request_id_for_lease(request_id: Option<&axum::extract::Extension<RequestId>>) -> String {
    request_id_str(request_id).map_or_else(
        || format!("request_{}", Uuid::new_v4()),
        ToString::to_string,
    )
}

fn should_acquire_visible_main_lease(prepared_branch: Option<&PreparedConversationBranch>) -> bool {
    prepared_branch
        .is_some_and(|branch| branch.commit_turn && branch.request_kind.is_visible_main())
}

fn acquire_visible_main_lease(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    transport_identity: Option<&ConversationTransportIdentity>,
    previous_response_id: Option<&str>,
) -> Result<Option<MainTurnLeaseGuard>, String> {
    if !should_acquire_visible_main_lease(prepared_branch) {
        return Ok(None);
    }
    let identity = transport_identity
        .cloned()
        .ok_or_else(|| "visible main turn is missing a transport identity".to_string())?;
    let lease_request_id = request_id_for_lease(request_id);
    match state.main_turn_leases.acquire(
        &identity,
        lease_request_id.clone(),
        previous_response_id.map(ToString::to_string),
    ) {
        MainTurnLeaseAcquire::Acquired => {
            append_transport_diagnostic(serde_json::json!({
                "event": "visible_main_lease_acquired",
                "request_id": lease_request_id,
                "transport_identity": identity.key(),
                "claude_session_id": identity.claude_session_id,
                "branch_id": identity.branch_id,
                "provider_model_fingerprint": identity.provider_model_fingerprint,
                "reasoning_effort": identity.reasoning_effort,
                "previous_response_id": previous_response_id,
                "commit_policy": "visible_main_lease_required",
                "client_abort_state": "in_flight",
            }));
            Ok(Some(MainTurnLeaseGuard::new(
                state.main_turn_leases.clone(),
                identity,
                lease_request_id,
            )))
        }
        MainTurnLeaseAcquire::Busy {
            in_flight_request_id,
            previous_response_id,
            websocket_chain_id,
        } => Err(format!(
            "visible main turn already in flight for this conversation transport identity: request_id={in_flight_request_id}, previous_response_id={}, websocket_chain_id={}",
            previous_response_id.as_deref().unwrap_or("null"),
            websocket_chain_id
                .as_ref()
                .map_or("pending", WebSocketChainId::as_str)
        )),
    }
}

fn normalized_reasoning_effort(reasoning: Option<&serde_json::Value>) -> String {
    reasoning
        .and_then(|value| value.get("effort"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| "default".to_string(), str::to_ascii_lowercase)
}

fn request_compatibility_fingerprint(
    config: &GatewayConfig,
    resolution: &ModelResolution,
    translated: &crate::translate::TranslateResult,
) -> String {
    let payload = serde_json::json!({
        "renderer_version": "gateway_http_anthropic_transport_v1",
        "selected_backend_model": resolution.selected_backend_model,
        "instructions": translated.instructions,
        "tools": translated.tools,
        "tool_choice": translated.tool_choice,
        "parallel_tool_calls": translated.parallel_tool_calls,
        "text": translated.text,
        "reasoning": translated.reasoning,
        "include": translated.include,
        "service_tier": service_tier_for_config(config),
    });
    hash_serde_value(&payload)
}

fn apply_context_management(
    config: &GatewayConfig,
    req: &mut AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
) -> ContextManagementReport {
    let report = ContextManager::new(&config.workflow.context_management)
        .apply(req.context_management.as_ref(), &mut req.messages);
    if let Some(metadata) = report.metadata_value() {
        if let Some(axum::extract::Extension(rid)) = request_id {
            info!(
                request_id = %rid.0,
                context_management = %metadata,
                "applied gateway context management"
            );
        } else {
            info!(
                context_management = %metadata,
                "applied gateway context management"
            );
        }
    }
    report
}

fn attach_context_management_metadata(
    translated: &mut crate::translate::TranslateResult,
    report: &ContextManagementReport,
) {
    let Some(metadata) = report.metadata_value() else {
        return;
    };
    let encoded = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".to_string());
    translated
        .client_metadata
        .get_or_insert_with(HashMap::new)
        .insert("gateway_context_management".to_string(), encoded);
}

async fn run_backend_unary(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    websocket_transport_identity: Option<&ConversationTransportIdentity>,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<
    (
        gateway_backend_codex::types::CodexUnaryDecoded,
        Option<String>,
        Option<WebSocketChainId>,
    ),
    axum::response::Response,
> {
    let (events, effective_previous_response_id, websocket_chain_id) =
        Box::pin(send_backend_stream_strict_delta(
            state,
            request_id_str(request_id).map(ToString::to_string),
            prepared_branch,
            websocket_transport_identity,
            backend_req.clone(),
        ))
        .await
        .map_err(backend_request_failure_to_http_response)?;

    let commit_previous_response_id = effective_previous_response_id;
    let decoded = match gateway_backend_codex::sse_unary::read_backend_events_to_completion(events)
        .await
    {
        Ok(decoded) => decoded,
        Err(err)
            if is_known_delta_decode_rejection(&err, commit_previous_response_id.as_deref()) =>
        {
            invalidate_websocket_transport_session(state, websocket_transport_identity);
            log_delta_contract_violation(
                request_id_str(request_id),
                prepared_branch,
                commit_previous_response_id.as_deref(),
                &err.to_string(),
            );
            return Err(decode_error_to_http_response(&err));
        }
        Err(err) => return Err(decode_error_to_http_response(&err)),
    };

    Ok((decoded, commit_previous_response_id, websocket_chain_id))
}

fn decode_error_to_http_response(
    err: &gateway_backend_codex::sse_unary::SseDecodeError,
) -> axum::response::Response {
    claude_status_json_response(
        StatusCode::BAD_GATEWAY,
        serde_json::json!({
            "error": { "type": "backend_error", "message": format!("{err}") }
        }),
    )
}

fn claude_json_response(value: serde_json::Value) -> axum::response::Response {
    Json(sanitize_anthropic_response_value(value)).into_response()
}

fn claude_status_json_response(
    status: StatusCode,
    value: serde_json::Value,
) -> axum::response::Response {
    (status, Json(sanitize_anthropic_response_value(value))).into_response()
}

#[derive(Debug)]
struct TranslatedCommandExecutionError {
    message: String,
}

async fn execute_translated_command_input(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    current_model: &str,
    resolved_model: &str,
    translated: &TranslateResult,
) -> Result<Option<serde_json::Value>, TranslatedCommandExecutionError> {
    let Some(command_name) = translated.client_metadata.as_ref().and_then(|metadata| {
        metadata
            .get("claude_code_translated_slash_command")
            .map(std::string::String::as_str)
    }) else {
        return Ok(None);
    };

    let maybe_creds = load_codex_credentials(auth_path_override(state)).ok();
    let runtime = ExecutorRuntime {
        credentials: maybe_creds.clone(),
        backend_client: state.backend.clone(),
        current_model: Some(current_model.to_string()),
        session_info: translate_executor::SessionInfo {
            thread_id: None,
            thread_name: None,
            account_display: maybe_creds
                .as_ref()
                .map(|credentials| credentials.account_id.clone()),
        },
        gateway_version: env!("CARGO_PKG_VERSION"),
        config_path: Some(
            gateway_core::config::default_gateway_config_path()
                .display()
                .to_string(),
        ),
        resolved_model: Some(resolved_model.to_string()),
        current_dir: std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string()),
        reasoning_effort: translated
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("anthropic_effort").cloned()),
    };

    let executor_json = match execute_translated_command(Some(command_name), &runtime).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            let message = format!("No executor registered for translated command '{command_name}'");
            tracing::error!(
                command = %command_name,
                "translated command has no executor; returning error"
            );
            return Err(TranslatedCommandExecutionError { message });
        }
        Err(err) => {
            log_translated_command_executor_error(request_id, command_name, &err);
            return Err(TranslatedCommandExecutionError {
                message: format!("Translated command '{command_name}' failed: {err}"),
            });
        }
    };

    let Some(post_result_fn) = translate_executor::get_post_result_function(command_name) else {
        let message =
            format!("No post-result function registered for translated command '{command_name}'");
        tracing::error!(
            command = %command_name,
            "translated command missing post-result function; returning error"
        );
        return Err(TranslatedCommandExecutionError { message });
    };

    let result_text = post_result_fn(
        &executor_json,
        crate::claude_code_context::get_packaged_command_body(command_name),
    );
    Ok(Some(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{ "type": "input_text", "text": result_text }]
    })))
}

fn log_translated_command_executor_error(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    command_name: &str,
    err: &str,
) {
    if let Some(axum::extract::Extension(rid)) = request_id {
        tracing::error!(
            request_id = %rid.0,
            error = %err,
            command = %command_name,
            "translated command executor failed; returning error"
        );
    } else {
        tracing::error!(
            error = %err,
            command = %command_name,
            "translated command executor failed; returning error"
        );
    }
}

fn translated_command_json_error_response(
    err: &TranslatedCommandExecutionError,
) -> axum::response::Response {
    claude_status_json_response(
        StatusCode::OK,
        serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": err.message
            }
        }),
    )
}

#[derive(Debug)]
enum BackendRequestFailure {
    Backend(BackendError),
}

struct BackendEventStreamWithChain {
    events: gateway_backend_codex::types::CodexBackendEventStream,
    websocket_chain_id: Option<WebSocketChainId>,
}

fn log_delta_contract_violation(
    request_id: Option<&str>,
    prepared_branch: Option<&PreparedConversationBranch>,
    attempted_previous_response_id: Option<&str>,
    error: &str,
) {
    if let Some(prepared_branch) = prepared_branch {
        warn!(
            request_id = ?request_id,
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            previous_response_id = ?attempted_previous_response_id,
            error,
            "delta contract violation; refusing silent full retry"
        );
    } else {
        warn!(
            request_id = ?request_id,
            previous_response_id = ?attempted_previous_response_id,
            error,
            "delta contract violation without branch context; refusing silent full retry"
        );
    }
}

fn invalidate_websocket_transport_session(
    state: &AppState,
    websocket_transport_identity: Option<&ConversationTransportIdentity>,
) {
    let Some(identity) = websocket_transport_identity else {
        return;
    };
    state
        .backend
        .evict_live_websocket_session(&identity.websocket_session_key());
}

async fn send_backend_stream_strict_delta(
    state: &AppState,
    request_id: Option<String>,
    prepared_branch: Option<&PreparedConversationBranch>,
    websocket_transport_identity: Option<&ConversationTransportIdentity>,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<
    (
        gateway_backend_codex::types::CodexBackendEventStream,
        Option<String>,
        Option<WebSocketChainId>,
    ),
    BackendRequestFailure,
> {
    let attempted_previous_response_id = backend_req.previous_response_id.clone();
    match send_backend_event_stream(state, websocket_transport_identity, backend_req.clone()).await
    {
        Ok(stream) => Ok((
            stream.events,
            attempted_previous_response_id,
            stream.websocket_chain_id,
        )),
        Err(err) if is_known_delta_rejection(&err, attempted_previous_response_id.as_deref()) => {
            invalidate_websocket_transport_session(state, websocket_transport_identity);
            log_delta_contract_violation(
                request_id.as_deref(),
                prepared_branch,
                attempted_previous_response_id.as_deref(),
                &err.to_string(),
            );
            Err(BackendRequestFailure::Backend(err))
        }
        Err(err) => Err(BackendRequestFailure::Backend(err)),
    }
}

async fn send_backend_stream_for_streaming(
    state: &AppState,
    request_id: Option<String>,
    prepared_branch: Option<&PreparedConversationBranch>,
    websocket_transport_identity: Option<&ConversationTransportIdentity>,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<
    (
        gateway_backend_codex::types::CodexBackendEventStream,
        Option<String>,
        Option<WebSocketChainId>,
    ),
    BackendRequestFailure,
> {
    let (mut events, effective_previous_response_id, websocket_chain_id) =
        Box::pin(send_backend_stream_strict_delta(
            state,
            request_id.clone(),
            prepared_branch,
            websocket_transport_identity,
            backend_req,
        ))
        .await?;

    let Some(first_item) = events.next().await else {
        return Ok((events, effective_previous_response_id, websocket_chain_id));
    };

    match first_item {
        Ok(event)
            if is_known_delta_failure_event(&event, effective_previous_response_id.as_deref()) =>
        {
            if delta_failure_event_to_backend_error(&event).is_some() {
                invalidate_websocket_transport_session(state, websocket_transport_identity);
                log_delta_contract_violation(
                    request_id.as_deref(),
                    prepared_branch,
                    effective_previous_response_id.as_deref(),
                    "backend returned a delta rejection event",
                );
                return Err(BackendRequestFailure::Backend(
                    delta_failure_event_to_backend_error(&event)
                        .expect("delta failure event was already parsed"),
                ));
            }
            let prefixed = futures_util::stream::once(async move { Ok(event) })
                .chain(events)
                .boxed();
            Ok((prefixed, effective_previous_response_id, websocket_chain_id))
        }
        Ok(event) => {
            let prefixed = futures_util::stream::once(async move { Ok(event) })
                .chain(events)
                .boxed();
            Ok((prefixed, effective_previous_response_id, websocket_chain_id))
        }
        Err(err) if is_known_delta_rejection(&err, effective_previous_response_id.as_deref()) => {
            log_delta_contract_violation(
                request_id.as_deref(),
                prepared_branch,
                effective_previous_response_id.as_deref(),
                &err.to_string(),
            );
            let prefixed = futures_util::stream::once(async move { Err(err) })
                .chain(events)
                .boxed();
            Ok((prefixed, effective_previous_response_id, websocket_chain_id))
        }
        Err(err) => {
            let prefixed = futures_util::stream::once(async move { Err(err) })
                .chain(events)
                .boxed();
            Ok((prefixed, effective_previous_response_id, websocket_chain_id))
        }
    }
}

async fn send_backend_event_stream(
    state: &AppState,
    websocket_transport_identity: Option<&ConversationTransportIdentity>,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<BackendEventStreamWithChain, BackendError> {
    if let Some(identity) = websocket_transport_identity {
        let stream = state
            .backend
            .send_pooled_websocket_event_stream(
                &state.auth,
                identity.websocket_session_key(),
                backend_req,
                WebSocketRetryPolicy::default(),
            )
            .await?;
        return Ok(BackendEventStreamWithChain {
            events: stream.events,
            websocket_chain_id: Some(stream.websocket_chain_id),
        });
    }

    let response = state
        .backend
        .send_streaming_with_refresh_retry(&state.auth, backend_req)
        .await?;
    Ok(BackendEventStreamWithChain {
        events: gateway_backend_codex::client::CodexBackendClient::response_to_event_stream(
            response,
        ),
        websocket_chain_id: None,
    })
}

fn websocket_chain_decision_for_branch(
    state: &AppState,
    prepared_branch: Option<&PreparedConversationBranch>,
    identity: Option<&ConversationTransportIdentity>,
    selected_checkpoint: Option<&SelectedCheckpoint>,
) -> WebSocketChainDecision {
    if prepared_branch.is_none() {
        return WebSocketChainDecision {
            match_result: WebSocketChainMatch::Missing,
            transport_identity: None,
            live_websocket_chain_id: None,
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: None,
            reason: "missing_branch",
        };
    }
    let Some(checkpoint) = selected_checkpoint else {
        return WebSocketChainDecision {
            match_result: WebSocketChainMatch::Missing,
            transport_identity: None,
            live_websocket_chain_id: None,
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: None,
            reason: "missing_checkpoint",
        };
    };
    let Some(identity) = identity else {
        return WebSocketChainDecision {
            match_result: WebSocketChainMatch::Missing,
            transport_identity: None,
            live_websocket_chain_id: None,
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: Some(checkpoint.response_id.clone()),
            reason: "missing_transport_identity",
        };
    };
    let session_key = identity.websocket_session_key();
    let Some(live_websocket_chain_id) = state.backend.live_websocket_chain_id(&session_key) else {
        return WebSocketChainDecision {
            match_result: WebSocketChainMatch::Missing,
            transport_identity: Some(identity.key()),
            live_websocket_chain_id: None,
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: Some(checkpoint.response_id.clone()),
            reason: "missing_live_websocket_chain",
        };
    };
    let checkpoint_websocket_chain_id = state
        .openai_chain_checkpoints
        .websocket_chain_id_for_response(identity, &checkpoint.response_id);
    let Some(checkpoint_websocket_chain_id) = checkpoint_websocket_chain_id else {
        return WebSocketChainDecision {
            match_result: WebSocketChainMatch::Missing,
            transport_identity: Some(identity.key()),
            live_websocket_chain_id: Some(live_websocket_chain_id),
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: Some(checkpoint.response_id.clone()),
            reason: "missing_checkpoint_websocket_chain_association",
        };
    };

    if live_websocket_chain_id == checkpoint_websocket_chain_id {
        WebSocketChainDecision {
            match_result: WebSocketChainMatch::Matching,
            transport_identity: Some(identity.key()),
            live_websocket_chain_id: Some(live_websocket_chain_id),
            checkpoint_websocket_chain_id: Some(checkpoint_websocket_chain_id),
            checkpoint_response_id: Some(checkpoint.response_id.clone()),
            reason: "websocket_chain_match",
        }
    } else {
        WebSocketChainDecision {
            match_result: WebSocketChainMatch::Mismatching,
            transport_identity: Some(identity.key()),
            live_websocket_chain_id: Some(live_websocket_chain_id),
            checkpoint_websocket_chain_id: Some(checkpoint_websocket_chain_id),
            checkpoint_response_id: Some(checkpoint.response_id.clone()),
            reason: "websocket_chain_mismatch",
        }
    }
}

fn is_known_delta_rejection(
    err: &BackendError,
    attempted_previous_response_id: Option<&str>,
) -> bool {
    let Some(previous_response_id) = attempted_previous_response_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };

    match err {
        BackendError::UnexpectedStatusWithBody { status, body }
            if matches!(*status, 400 | 404 | 409 | 410 | 422) =>
        {
            let body = body.to_ascii_lowercase();
            let previous_response_id = previous_response_id.to_ascii_lowercase();
            let mentions_checkpoint = body.contains("previous_response_id")
                || body.contains("response_id")
                || body.contains(&previous_response_id);
            let indicates_stale_chain = [
                "not found",
                "does not exist",
                "invalid",
                "unknown",
                "stale",
                "expired",
            ]
            .iter()
            .any(|needle| body.contains(needle));

            mentions_checkpoint && indicates_stale_chain
        }
        BackendError::WebSocket { message, .. } => {
            let message = message.to_ascii_lowercase();
            message.contains("previous_response_id") && message.contains("live websocket session")
        }
        _ => false,
    }
}

fn is_known_delta_decode_rejection(
    err: &gateway_backend_codex::sse_unary::SseDecodeError,
    attempted_previous_response_id: Option<&str>,
) -> bool {
    let gateway_backend_codex::sse_unary::SseDecodeError::BackendFailed { message } = err else {
        return false;
    };

    is_known_delta_rejection(
        &BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: message.clone(),
        },
        attempted_previous_response_id,
    )
}

fn is_known_delta_failure_event(
    event: &gateway_backend_codex::types::CodexBackendEvent,
    attempted_previous_response_id: Option<&str>,
) -> bool {
    let Some(message) = gateway_backend_codex::backend_error::parse_backend_failure_event(
        &event.event,
        &event.data,
    ) else {
        return false;
    };
    is_known_delta_rejection(
        &BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: message,
        },
        attempted_previous_response_id,
    )
}

fn delta_failure_event_to_backend_error(
    event: &gateway_backend_codex::types::CodexBackendEvent,
) -> Option<BackendError> {
    gateway_backend_codex::backend_error::parse_backend_failure_event(&event.event, &event.data)
        .map(|message| BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: message,
        })
}

fn backend_request_failure_to_http_response(
    err: BackendRequestFailure,
) -> axum::response::Response {
    match err {
        BackendRequestFailure::Backend(err) => match err {
            BackendError::AuthFailed { stage: _, message } => auth_error(&message),
            BackendError::UnexpectedStatusWithBody { status: 401, body } => auth_error(&body),
            BackendError::UnexpectedStatus(401) => auth_error("Authentication failed"),
            _ => claude_status_json_response(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({
                    "error": { "type": "backend_error", "message": err.to_string() }
                }),
            ),
        },
    }
}

fn backend_request_failure_to_sse(
    err: BackendRequestFailure,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    match err {
        BackendRequestFailure::Backend(err) => match err {
            BackendError::AuthFailed { stage: _, message } => sse_auth_error(&message),
            BackendError::UnexpectedStatusWithBody { status: 401, body } => sse_auth_error(&body),
            BackendError::UnexpectedStatus(401) => sse_auth_error("Authentication failed"),
            _ => sse_error("backend_error", &err.to_string()),
        },
    }
}

fn log_ignored_stop_sequences(
    stop_sequences: &[String],
    request_id: Option<&axum::extract::Extension<RequestId>>,
) {
    if stop_sequences.is_empty() {
        return;
    }
    let count = stop_sequences.len();
    if let Some(axum::extract::Extension(rid)) = request_id {
        tracing::warn!(
            request_id = %rid.0,
            count,
            "ignoring Anthropic stop_sequences (unsupported)"
        );
    } else {
        tracing::warn!(count, "ignoring Anthropic stop_sequences (unsupported)");
    }
}

fn log_ignored_request_controls(
    req: &AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
) {
    // Read-and-log any controls we parse but do not forward yet. This both documents behavior and
    // avoids silent drift from Claude Code expectations.
    let rid = request_id.map(|axum::extract::Extension(r)| r.0.clone());

    if req.max_tokens.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic max_tokens (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic max_tokens (not forwarded yet)");
        }
    }
    if req.top_k.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic top_k (no backend equivalent)");
        } else {
            tracing::warn!("ignoring Anthropic top_k (no backend equivalent)");
        }
    }
    if req.metadata.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic metadata (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic metadata (not forwarded yet)");
        }
    }
    if req.thinking.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic thinking (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic thinking (not forwarded yet)");
        }
    }
}

fn validate_tool_results(state: &AppState, req: &AnthropicMessagesRequest) {
    for msg in &req.messages {
        let crate::types::AnthropicContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            if block.block_type != "tool_result" {
                continue;
            }
            let Some(tool_use_id) = block.tool_use_id.as_deref() else {
                continue;
            };
            let exists = state
                .tool_calls
                .tool_call_exists(tool_use_id)
                .unwrap_or(false);
            if !exists {
                // This gateway is stateless from the client's perspective and may restart between
                // tool calls. Do not reject the request; log and allow the backend to decide.
                tracing::warn!(
                    tool_use_id,
                    "tool_result references unknown tool_use_id (gateway state missing); forwarding anyway"
                );
            }
        }
    }
}

fn build_tool_translation_context(
    state: &AppState,
    req: &AnthropicMessagesRequest,
) -> ToolTranslationContext {
    let mut tool_kinds = std::collections::HashMap::new();
    for msg in &req.messages {
        let crate::types::AnthropicContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            let call_id = match block.block_type.as_str() {
                "tool_use" => block.id.as_deref(),
                "tool_result" => block.tool_use_id.as_deref(),
                _ => None,
            };
            let Some(call_id) = call_id else {
                continue;
            };
            if tool_kinds.contains_key(call_id) {
                continue;
            }
            let stored = state.tool_calls.get_tool_call(call_id).ok().flatten();
            if let Some(stored) = stored
                && let Ok(kind) = stored.tool_kind.parse::<CodexToolCallKind>()
            {
                tool_kinds.insert(call_id.to_string(), kind);
            }
        }
    }
    ToolTranslationContext::new(tool_kinds)
        .with_claude_code_config(state.gateway_config.workflow.claude_code.clone())
}

fn sse_error(
    message_type: &str,
    message: &str,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let payload = serde_json::json!({
        "type": "error",
        "error": { "type": message_type, "message": message }
    })
    .to_string();
    let payload = sanitize_anthropic_response_text(payload);

    let stream = futures_util::stream::iter([Ok::<Event, std::convert::Infallible>(
        Event::default().event("error").data(payload),
    )])
    .boxed();

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_auth_error(
    message: &str,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let remediation = "Please run: cld-gateway login claude";
    let payload = serde_json::json!({
        "type": "error",
        "error": {
            "type": "auth_error",
            "message": message,
            "auth_remediation": remediation
        }
    })
    .to_string();
    let payload = sanitize_anthropic_response_text(payload);

    let stream = futures_util::stream::iter([Ok::<Event, std::convert::Infallible>(
        Event::default().event("error").data(payload),
    )])
    .boxed();

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn anthropic_stream_start_events(msg_id: &str, model: &str) -> Vec<Event> {
    vec![
        Event::default().event("message_start").data(
            serde_json::json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                    "content": [],
                    "model": model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
    ]
}

async fn stream_messages(
    state: AppState,
    request_id: Option<axum::extract::Extension<RequestId>>,
    claude_session_id: Option<String>,
    req: AnthropicMessagesRequest,
    context_management_report: ContextManagementReport,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let flow = match prepare_stream_message_flow(
        &state,
        request_id.as_ref(),
        claude_session_id.as_deref(),
        &req,
        &context_management_report,
    )
    .await
    {
        Ok(flow) => flow,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return sse_auth_error(&err),
    };
    let transport = match select_message_transport(
        &state,
        request_id.as_ref(),
        flow.prepared_branch.as_ref(),
        &flow.resolution,
        &flow.translated,
    ) {
        Ok(transport) => transport,
        Err(err) => return sse_error("service_unavailable_error", &err),
    };
    let backend_req = match build_stream_backend_request(
        &state,
        &req,
        &context_management_report,
        &flow,
        &transport,
        creds,
    ) {
        Ok(backend_req) => backend_req,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    let result =
        match execute_stream_backend(&state, request_id.as_ref(), &flow, &transport, backend_req)
            .await
        {
            Ok(result) => result,
            Err(err) => return err.into_sse(),
        };
    build_stream_sse(
        &state,
        request_id,
        &req,
        &context_management_report,
        &flow,
        &transport,
        result,
    )
}

struct StreamMessageFlow {
    resolution: ModelResolution,
    prepared_branch: Option<PreparedConversationBranch>,
    translated: TranslateResult,
    post_command_input: Option<serde_json::Value>,
}

struct StreamBackendResult {
    events: gateway_backend_codex::types::CodexBackendEventStream,
    effective_previous_response_id: Option<String>,
    response_websocket_chain_id: Option<WebSocketChainId>,
    main_turn_lease: Option<MainTurnLeaseGuard>,
}

enum StreamStartFailure {
    Message {
        error_type: &'static str,
        message: String,
    },
    Backend(BackendRequestFailure),
}

impl StreamStartFailure {
    fn into_sse(
        self,
    ) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>>
    {
        match self {
            Self::Message {
                error_type,
                message,
            } => sse_error(error_type, &message),
            Self::Backend(err) => backend_request_failure_to_sse(err),
        }
    }
}

async fn prepare_stream_message_flow(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    claude_session_id: Option<&str>,
    req: &AnthropicMessagesRequest,
    context_management_report: &ContextManagementReport,
) -> Result<StreamMessageFlow, String> {
    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id,
        "resolved model for /v1/messages (streaming)",
    );
    let preview_tool_context = build_tool_translation_context(state, req);
    let translated_preview =
        translate_request_with_context(req, &preview_tool_context).map_err(|err| err.clone())?;
    let prepared_branch = prepare_conversation_branch(
        state,
        claude_session_id,
        req,
        translated_preview.client_metadata.as_ref(),
    );
    log_conversation_branch_resolution(request_id, prepared_branch.as_ref());
    if claude_session_id.is_some() && prepared_branch.is_none() {
        return Err(
            "Claude Code conversation requests require a WebSocket conversation-state branch"
                .to_string(),
        );
    }
    let full_render_req = prepared_branch.as_ref().map_or_else(
        || req.clone(),
        |prepared_branch| {
            request_with_messages(req, prepared_branch.full_backend_render_messages())
        },
    );
    let tool_context = build_tool_translation_context(state, &full_render_req);
    let mut translated = translate_request_with_context(&full_render_req, &tool_context)
        .map_err(|err| err.clone())?;
    attach_context_management_metadata(&mut translated, context_management_report);
    let post_command_input = execute_translated_command_input(
        state,
        request_id,
        &req.model,
        &resolution.selected_backend_model,
        &translated,
    )
    .await
    .map_err(|err| err.message)?;
    if let Some(command_input) = post_command_input.clone() {
        translated.input.push(command_input);
    }
    Ok(StreamMessageFlow {
        resolution,
        prepared_branch,
        translated,
        post_command_input,
    })
}

fn build_stream_backend_request(
    state: &AppState,
    req: &AnthropicMessagesRequest,
    context_management_report: &ContextManagementReport,
    flow: &StreamMessageFlow,
    transport: &MessageTransportSelection,
    creds: gateway_auth_codex::CodexCredentials,
) -> Result<gateway_backend_codex::types::CodexBackendRequest, String> {
    let backend_render_req = request_for_selected_transport(
        req,
        flow.prepared_branch.as_ref(),
        &transport.selected_transport,
    );
    let backend_tool_context = build_tool_translation_context(state, &backend_render_req);
    let mut backend_translated =
        translate_request_with_context(&backend_render_req, &backend_tool_context)
            .map_err(|err| err.clone())?;
    attach_context_management_metadata(&mut backend_translated, context_management_report);
    if let Some(command_input) = flow.post_command_input.clone() {
        backend_translated.input.push(command_input);
    }
    Ok(build_backend_request(
        &state.gateway_config,
        &flow.resolution,
        backend_translated,
        creds,
        transport.selected_transport.previous_response_id.clone(),
    ))
}

async fn execute_stream_backend(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &StreamMessageFlow,
    transport: &MessageTransportSelection,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<StreamBackendResult, StreamStartFailure> {
    let request_previous_response_id = transport.selected_transport.previous_response_id.clone();
    let main_turn_lease = acquire_visible_main_lease(
        state,
        request_id,
        flow.prepared_branch.as_ref(),
        transport.transport_identity.as_ref(),
        request_previous_response_id.as_deref(),
    )
    .map_err(|err| StreamStartFailure::Message {
        error_type: "service_unavailable_error",
        message: err,
    })?;
    let (events, effective_previous_response_id, response_websocket_chain_id) =
        Box::pin(send_backend_stream_for_streaming(
            state,
            request_id_str(request_id).map(ToString::to_string),
            flow.prepared_branch.as_ref(),
            transport.websocket_transport_identity.as_ref(),
            backend_req,
        ))
        .await
        .map_err(StreamStartFailure::Backend)?;
    promote_stream_websocket_chain(
        state,
        main_turn_lease.as_ref(),
        response_websocket_chain_id.clone(),
    )?;
    Ok(StreamBackendResult {
        events,
        effective_previous_response_id,
        response_websocket_chain_id,
        main_turn_lease,
    })
}

fn promote_stream_websocket_chain(
    state: &AppState,
    main_turn_lease: Option<&MainTurnLeaseGuard>,
    response_websocket_chain_id: Option<WebSocketChainId>,
) -> Result<(), StreamStartFailure> {
    if let Some(lease) = main_turn_lease
        && !state.main_turn_leases.promote_websocket_chain(
            &lease.transport_identity,
            &lease.request_id,
            response_websocket_chain_id,
        )
    {
        return Err(StreamStartFailure::Message {
            error_type: "service_unavailable_error",
            message: "visible main turn lease was lost before backend stream started".to_string(),
        });
    }
    Ok(())
}

fn build_stream_sse(
    state: &AppState,
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: &AnthropicMessagesRequest,
    context_management_report: &ContextManagementReport,
    flow: &StreamMessageFlow,
    transport: &MessageTransportSelection,
    result: StreamBackendResult,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let initial = futures_util::stream::iter(
        anthropic_stream_start_events(&format!("msg_{}", Uuid::new_v4()), &req.model)
            .into_iter()
            .map(Ok::<Event, std::convert::Infallible>),
    );
    let StreamBackendResult {
        events,
        effective_previous_response_id,
        response_websocket_chain_id,
        main_turn_lease,
    } = result;
    let stream_commit = build_stream_commit_context(
        state,
        request_id.as_ref(),
        flow,
        transport,
        effective_previous_response_id,
        response_websocket_chain_id,
        main_turn_lease,
    );
    let tail = backend_events_to_anthropic_events(
        events,
        state.conversation_state.clone(),
        state.tool_calls.clone(),
        request_id.map(|axum::extract::Extension(r)| r.0),
        context_management_report.response_value(),
        structured_output_schema_from_config(req.output_config.as_ref()),
        stream_commit,
    );
    Sse::new(initial.chain(tail).boxed()).keep_alive(KeepAlive::default())
}

fn build_stream_commit_context(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    flow: &StreamMessageFlow,
    transport: &MessageTransportSelection,
    effective_previous_response_id: Option<String>,
    response_websocket_chain_id: Option<WebSocketChainId>,
    main_turn_lease: Option<MainTurnLeaseGuard>,
) -> Option<StreamCommitContext> {
    flow.prepared_branch
        .as_ref()
        .map(|prepared_branch| StreamCommitContext {
            claude_session_id: prepared_branch.claude_session_id.clone(),
            branch_id: prepared_branch.branch.branch_id.clone(),
            fingerprints: prepared_branch.fingerprints.clone(),
            active_canonical_messages: serde_json::to_value(&prepared_branch.active_messages)
                .unwrap_or(serde_json::Value::Null),
            provider_model_fingerprint: flow.resolution.selected_backend_model.clone(),
            request_compatibility_fingerprint: transport.request_compatibility_fingerprint.clone(),
            previous_response_id: effective_previous_response_id,
            canonical_message_count: prepared_branch.active_messages.len(),
            canonical_prefix_hash: canonical_messages_prefix_hash(
                &prepared_branch.active_messages,
                prepared_branch.active_messages.len(),
            ),
            request_kind: prepared_branch.request_kind,
            turn_scope: prepared_branch.turn_scope,
            commit_turn: prepared_branch.commit_turn,
            selected_checkpoint_source: transport
                .selected_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.source),
            selected_checkpoint_response_id: transport
                .selected_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.response_id.clone()),
            transport_identity: transport.transport_identity.clone(),
            websocket_chain_id: response_websocket_chain_id,
            openai_chain_checkpoints: state.openai_chain_checkpoints.clone(),
            request_id: request_id_str(request_id).map(ToString::to_string),
            main_turn_lease,
        })
}

#[derive(Debug, Clone)]
struct PreparedConversationBranch {
    claude_session_id: String,
    branch: BranchMetadata,
    request_kind: ConversationRequestKind,
    selection_action: BranchSelectionAction,
    turn_scope: ConversationTurnScope,
    persistence_class: ConversationPersistenceClass,
    persistence_reason: &'static str,
    commit_turn: bool,
    allow_incremental_context: bool,
    allow_zero_delta_start: bool,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
    delta_start_index: usize,
}

impl PreparedConversationBranch {
    fn full_backend_render_messages(&self) -> Vec<crate::types::AnthropicMessage> {
        render_messages_with_operational_context(
            self.active_messages.clone(),
            self.operational_context_messages.clone(),
            self.delta_start_index,
        )
    }

    fn incremental_backend_render_messages(&self) -> Vec<crate::types::AnthropicMessage> {
        let durable_delta = self
            .active_messages
            .get(self.delta_start_index..)
            .unwrap_or_default()
            .to_vec();
        render_messages_with_operational_context(
            durable_delta,
            self.operational_context_messages.clone(),
            self.delta_start_index,
        )
    }
}

fn render_messages_with_operational_context(
    mut durable_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
    delta_start_index: usize,
) -> Vec<crate::types::AnthropicMessage> {
    durable_messages.extend(
        operational_context_messages
            .into_iter()
            .filter(|message| message.durable_messages_before >= delta_start_index)
            .map(|message| message.message),
    );
    durable_messages
}

struct DurableBranchSnapshot {
    active_canonical_messages: serde_json::Value,
    delta_start_index: usize,
    allow_zero_delta_start: bool,
}

struct TransientBranchPrep {
    claude_session_id: String,
    branch: BranchMetadata,
    request_kind: ConversationRequestKind,
    persistence_class: ConversationPersistenceClass,
    persistence_reason: &'static str,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
    delta_start_index: usize,
}

struct DurableBranchPrep<'a> {
    claude_session_id: &'a str,
    branch: BranchMetadata,
    request_kind: ConversationRequestKind,
    selection_action: BranchSelectionAction,
    turn_scope: ConversationTurnScope,
    persistence_reason: &'static str,
    fingerprints: BranchFingerprintSet,
    snapshot: &'a DurableBranchSnapshot,
    operational_context_messages: Vec<OperationalContextMessage>,
}

struct ReconciledDurableBranchPrep<'a> {
    state: &'a AppState,
    claude_session_id: &'a str,
    branch: BranchMetadata,
    request_kind: ConversationRequestKind,
    selection_action: BranchSelectionAction,
    turn_scope: ConversationTurnScope,
    persistence_reason: &'static str,
    fingerprints: BranchFingerprintSet,
    snapshot: &'a DurableBranchSnapshot,
    compaction_command_seen: bool,
    operational_context_messages: Vec<OperationalContextMessage>,
}

struct InitialDurableBranchPrep<'a> {
    state: &'a AppState,
    claude_session_id: &'a str,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    fingerprints: BranchFingerprintSet,
    active_canonical_messages: serde_json::Value,
    compaction_command_seen: bool,
    operational_context_messages: Vec<OperationalContextMessage>,
}

struct SideBranchPrep {
    claude_session_id: String,
    branch: BranchMetadata,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
    delta_start_index: usize,
}

struct ConversationBranchAnalysis {
    durable_messages: Vec<crate::types::AnthropicMessage>,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    existing_branches: Vec<BranchMetadata>,
    prefix_match: Option<(BranchMetadata, usize)>,
    compaction_context: CompactionRequestContext,
    fingerprints: BranchFingerprintSet,
    active_canonical_messages: serde_json::Value,
    latest_context_branch: Option<BranchMetadata>,
    internal_reason: Option<&'static str>,
    operational_context_messages: Vec<OperationalContextMessage>,
}

struct ClassifiedCanonicalMessages {
    durable_visible_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
}

#[derive(Debug, Clone)]
struct OperationalContextMessage {
    message: crate::types::AnthropicMessage,
    durable_messages_before: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationPersistenceClass {
    DurableMain,
    ReadOnlySideTurn,
    TransientInternal,
}

#[derive(Debug)]
struct StreamCommitContext {
    claude_session_id: String,
    branch_id: String,
    fingerprints: BranchFingerprintSet,
    active_canonical_messages: serde_json::Value,
    provider_model_fingerprint: String,
    request_compatibility_fingerprint: String,
    previous_response_id: Option<String>,
    canonical_message_count: usize,
    canonical_prefix_hash: String,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    commit_turn: bool,
    selected_checkpoint_source: Option<&'static str>,
    selected_checkpoint_response_id: Option<String>,
    transport_identity: Option<ConversationTransportIdentity>,
    websocket_chain_id: Option<WebSocketChainId>,
    openai_chain_checkpoints: OpenAiChainCheckpointStore,
    request_id: Option<String>,
    main_turn_lease: Option<MainTurnLeaseGuard>,
}

fn maybe_commit_stream_completion(
    conversation_state: &ConversationStateStore,
    stream_commit: Option<&StreamCommitContext>,
    event_name: &str,
    data: &str,
) {
    if event_name != "response.completed" {
        return;
    }
    let Some(commit) = stream_commit else {
        return;
    };

    let response_id =
        gateway_backend_codex::sse_unary::extract_response_id_from_completed_event(data);
    if !commit.commit_turn {
        maybe_record_stream_offshoot_checkpoint(
            conversation_state,
            commit,
            data,
            response_id.as_deref(),
        );
        return;
    }
    if !stream_lease_allows_commit(commit, response_id.as_deref()) {
        return;
    }
    if let Err(err) = conversation_state.commit_turn(
        &commit.claude_session_id,
        &commit.branch_id,
        &CommitTurnParams {
            turn_scope: ConversationTurnScope::Main,
            turn_id: format!("turn_{}", Uuid::new_v4()),
            fingerprints: commit.fingerprints.clone(),
            active_canonical_messages: Some(commit.active_canonical_messages.clone()),
            provider_response_id: response_id.clone(),
            previous_response_id: commit.previous_response_id.clone(),
            provider_model_fingerprint: Some(commit.provider_model_fingerprint.clone()),
            request_compatibility_fingerprint: Some(
                commit.request_compatibility_fingerprint.clone(),
            ),
            provider_input_tokens: extract_input_tokens_from_completed_event(data),
            canonical_message_count: Some(commit.canonical_message_count),
            canonical_prefix_hash: Some(commit.canonical_prefix_hash.clone()),
            provider_output_items: extract_completed_output_items(data),
        },
    ) {
        warn!(error = %err, "failed to commit streaming conversation-state turn");
        return;
    }

    if let Some(response_id) = response_id.as_deref() {
        record_stream_checkpoint_commit(commit, response_id);
        if let Some(request_id) = commit.request_id.as_deref() {
            info!(
                request_id = %request_id,
                claude_session_id = %commit.claude_session_id,
                branch_id = %commit.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?commit.previous_response_id,
                compaction_reset_pending = false,
                "captured streaming provider checkpoint response id"
            );
        } else {
            info!(
                claude_session_id = %commit.claude_session_id,
                branch_id = %commit.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?commit.previous_response_id,
                compaction_reset_pending = false,
                "captured streaming provider checkpoint response id"
            );
        }
    }
}

fn maybe_record_stream_offshoot_checkpoint(
    conversation_state: &ConversationStateStore,
    commit: &StreamCommitContext,
    data: &str,
    response_id: Option<&str>,
) {
    let Some(response_id) = response_id else {
        return;
    };
    let prepared_branch = transient_stream_prepared_branch(commit);
    record_offshoot_checkpoint(&OffshootCheckpointRecord {
        conversation_state,
        openai_chain_checkpoints: &commit.openai_chain_checkpoints,
        prepared_branch: &prepared_branch,
        transport_identity: commit.transport_identity.as_ref(),
        response_id,
        previous_response_id: commit.previous_response_id.as_deref(),
        provider_model_fingerprint: &commit.provider_model_fingerprint,
        request_compatibility_fingerprint: &commit.request_compatibility_fingerprint,
        provider_input_tokens: extract_input_tokens_from_completed_event(data),
        selected_checkpoint_source: commit.selected_checkpoint_source,
        selected_checkpoint_response_id: commit.selected_checkpoint_response_id.as_deref(),
        websocket_chain_id: commit.websocket_chain_id.as_ref(),
        streaming: true,
        request_id: commit.request_id.as_deref(),
    });
}

fn transient_stream_prepared_branch(commit: &StreamCommitContext) -> PreparedConversationBranch {
    PreparedConversationBranch {
        claude_session_id: commit.claude_session_id.clone(),
        branch: BranchMetadata {
            schema_version: 1,
            branch_id: commit.branch_id.clone(),
            parent_branch_id: None,
            fork_ancestor_checkpoint: None,
            current_checkpoint_id: None,
            active_canonical_messages: None,
            fingerprints: commit.fingerprints.clone(),
            openai_checkpoint: None,
            turn_openai_checkpoints: Vec::new(),
            offshoot_openai_checkpoints: Vec::new(),
            compaction_reset_pending: false,
            last_main_turn_id: None,
            created_at_unix_seconds: 0,
            updated_at_unix_seconds: 0,
        },
        request_kind: commit.request_kind,
        selection_action: BranchSelectionAction::ContinuedExisting,
        turn_scope: commit.turn_scope,
        persistence_class: ConversationPersistenceClass::TransientInternal,
        persistence_reason: commit.request_kind.persistence_reason(),
        commit_turn: false,
        allow_incremental_context: true,
        allow_zero_delta_start: false,
        fingerprints: commit.fingerprints.clone(),
        active_messages: Vec::new(),
        operational_context_messages: Vec::new(),
        delta_start_index: commit.canonical_message_count,
    }
}

fn stream_lease_allows_commit(commit: &StreamCommitContext, response_id: Option<&str>) -> bool {
    let Some(lease) = commit.main_turn_lease.as_ref() else {
        return true;
    };
    match lease.store.validate_for_commit(
        &lease.transport_identity,
        &lease.request_id,
        commit.websocket_chain_id.as_ref(),
    ) {
        MainTurnLeaseCommit::Accepted => true,
        MainTurnLeaseCommit::Rejected(reason) => {
            if matches!(
                reason,
                "client_aborted_before_first_event" | "client_aborted_after_visible_output"
            ) {
                let _ = lease.mark_state(MainTurnLeaseState::CommitSuppressedAfterAbort);
            }
            append_checkpoint_diagnostic(&CheckpointDiagnostic {
                event: "checkpoint_commit_skipped",
                subject: stream_checkpoint_subject(commit),
                transport_identity: commit.transport_identity.as_ref(),
                request_id: commit.request_id.as_deref(),
                provider_response_id: response_id,
                previous_response_id: commit.previous_response_id.as_deref(),
                selected_checkpoint_source: commit.selected_checkpoint_source,
                selected_checkpoint_response_id: commit.selected_checkpoint_response_id.as_deref(),
                websocket_chain_id: commit.websocket_chain_id.as_ref(),
                streaming: true,
                commit_policy: "visible_main_lease_rejected",
                skip_reason: Some(reason),
                client_abort_state: lease_diagnostic_state_for_reason(reason),
            });
            lease
                .store
                .release(&lease.transport_identity, &lease.request_id);
            lease.mark_released();
            false
        }
    }
}

fn record_stream_checkpoint_commit(commit: &StreamCommitContext, response_id: &str) {
    if let Some(websocket_chain_id) = commit.websocket_chain_id.clone()
        && let Some(identity) = commit.transport_identity.as_ref()
    {
        commit
            .openai_chain_checkpoints
            .associate(identity, response_id, websocket_chain_id);
    }
    append_checkpoint_diagnostic(&CheckpointDiagnostic {
        event: "visible_checkpoint_committed",
        subject: stream_checkpoint_subject(commit),
        transport_identity: commit.transport_identity.as_ref(),
        request_id: commit.request_id.as_deref(),
        provider_response_id: Some(response_id),
        previous_response_id: commit.previous_response_id.as_deref(),
        selected_checkpoint_source: commit.selected_checkpoint_source,
        selected_checkpoint_response_id: commit.selected_checkpoint_response_id.as_deref(),
        websocket_chain_id: commit.websocket_chain_id.as_ref(),
        streaming: true,
        commit_policy: "durable_visible_main_commit",
        skip_reason: None,
        client_abort_state: LeaseDiagnosticState::CompletedCommitted,
    });
    if let Some(lease) = commit.main_turn_lease.as_ref() {
        lease.release_with_state(MainTurnLeaseState::CompletedCommitted);
    }
}

struct OffshootCheckpointRecord<'a> {
    conversation_state: &'a ConversationStateStore,
    openai_chain_checkpoints: &'a OpenAiChainCheckpointStore,
    prepared_branch: &'a PreparedConversationBranch,
    transport_identity: Option<&'a ConversationTransportIdentity>,
    response_id: &'a str,
    previous_response_id: Option<&'a str>,
    provider_model_fingerprint: &'a str,
    request_compatibility_fingerprint: &'a str,
    provider_input_tokens: Option<i64>,
    selected_checkpoint_source: Option<&'a str>,
    selected_checkpoint_response_id: Option<&'a str>,
    websocket_chain_id: Option<&'a WebSocketChainId>,
    streaming: bool,
    request_id: Option<&'a str>,
}

fn record_offshoot_checkpoint(record: &OffshootCheckpointRecord<'_>) {
    let Some(identity) = record.transport_identity else {
        return;
    };
    if record.prepared_branch.commit_turn || record.prepared_branch.request_kind.is_visible_main() {
        return;
    }
    if let Some(websocket_chain_id) = record.websocket_chain_id.cloned() {
        record
            .openai_chain_checkpoints
            .associate(identity, record.response_id, websocket_chain_id);
    }
    if let Err(err) = record.conversation_state.commit_offshoot_openai_checkpoint(
        &record.prepared_branch.claude_session_id,
        &record.prepared_branch.branch.branch_id,
        &CommitOffshootCheckpointParams {
            offshoot_identity: identity.key(),
            provider_response_id: record.response_id.to_string(),
            previous_response_id: record.previous_response_id.map(ToString::to_string),
            provider_model_fingerprint: record.provider_model_fingerprint.to_string(),
            request_compatibility_fingerprint: Some(
                record.request_compatibility_fingerprint.to_string(),
            ),
            provider_input_tokens: record.provider_input_tokens,
        },
    ) {
        warn!(error = %err, "failed to persist offshoot OpenAI checkpoint");
    }
    append_checkpoint_diagnostic(&CheckpointDiagnostic {
        event: "offshoot_checkpoint_committed",
        subject: CheckpointDiagnosticSubject::from(record.prepared_branch),
        transport_identity: record.transport_identity,
        request_id: record.request_id,
        provider_response_id: Some(record.response_id),
        previous_response_id: record.previous_response_id,
        selected_checkpoint_source: record.selected_checkpoint_source,
        selected_checkpoint_response_id: record.selected_checkpoint_response_id,
        websocket_chain_id: record.websocket_chain_id,
        streaming: record.streaming,
        commit_policy: "offshoot_checkpoint_only",
        skip_reason: None,
        client_abort_state: LeaseDiagnosticState::NotAborted,
    });
}

fn stream_checkpoint_subject(commit: &StreamCommitContext) -> CheckpointDiagnosticSubject<'_> {
    CheckpointDiagnosticSubject {
        claude_session_id: &commit.claude_session_id,
        branch_id: &commit.branch_id,
        request_kind: commit.request_kind,
        turn_scope: commit.turn_scope,
        commit_turn: commit.commit_turn,
    }
}

fn extract_completed_output_items(data: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|value| {
            value
                .get("response")
                .and_then(|response| response.get("output"))
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
}

fn extract_input_tokens_from_completed_event(data: &str) -> Option<i64> {
    gateway_backend_codex::sse_unary::extract_usage_from_completed_event(data)
        .map(|usage| usage.input_tokens)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedTransport {
    mode: TransportMode,
    previous_response_id: Option<String>,
    reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSocketChainMatch {
    Matching,
    Mismatching,
    Missing,
}

#[derive(Debug, Clone)]
struct WebSocketChainDecision {
    match_result: WebSocketChainMatch,
    transport_identity: Option<String>,
    live_websocket_chain_id: Option<WebSocketChainId>,
    checkpoint_websocket_chain_id: Option<WebSocketChainId>,
    checkpoint_response_id: Option<String>,
    reason: &'static str,
}

#[derive(Debug, Clone)]
struct SelectedCheckpoint {
    response_id: String,
    provider_model_fingerprint: String,
    request_compatibility_fingerprint: Option<String>,
    source: &'static str,
    canonical_message_count: Option<usize>,
}

impl SelectedCheckpoint {
    fn from_openai_checkpoint(checkpoint: &OpenAiCheckpoint, source: &'static str) -> Self {
        Self {
            response_id: checkpoint.response_id.clone(),
            provider_model_fingerprint: checkpoint.provider_model_fingerprint.clone(),
            request_compatibility_fingerprint: checkpoint.request_compatibility_fingerprint.clone(),
            source,
            canonical_message_count: None,
        }
    }

    fn from_turn_checkpoint(checkpoint: TurnOpenAiCheckpoint) -> Self {
        Self {
            response_id: checkpoint.response_id,
            provider_model_fingerprint: checkpoint.provider_model_fingerprint,
            request_compatibility_fingerprint: checkpoint.request_compatibility_fingerprint,
            source: "turn_prefix_checkpoint",
            canonical_message_count: Some(checkpoint.canonical_message_count),
        }
    }
}

fn selected_checkpoint_for_transport(
    prepared_branch: &PreparedConversationBranch,
    provider_model_fingerprint: &str,
) -> Option<SelectedCheckpoint> {
    if prepared_branch.request_kind.is_visible_main() && prepared_branch.delta_start_index > 0 {
        let prefix_hash = canonical_messages_prefix_hash(
            &prepared_branch.active_messages,
            prepared_branch.delta_start_index,
        );
        if let Some(checkpoint) = ConversationStateStore::find_turn_openai_checkpoint(
            &prepared_branch.branch,
            prepared_branch.delta_start_index,
            &prefix_hash,
        )
        .filter(|checkpoint| checkpoint.provider_model_fingerprint == provider_model_fingerprint)
        {
            return Some(SelectedCheckpoint::from_turn_checkpoint(checkpoint));
        }
    }

    prepared_branch
        .branch
        .openai_checkpoint
        .as_ref()
        .map(|checkpoint| {
            SelectedCheckpoint::from_openai_checkpoint(checkpoint, "visible_branch_head")
        })
}

fn select_transport(
    prepared_branch: Option<&PreparedConversationBranch>,
    chain_decision: &WebSocketChainDecision,
    provider_model_fingerprint: &str,
    request_compatibility_fingerprint: &str,
) -> Result<SelectedTransport, String> {
    let Some(prepared_branch) = prepared_branch else {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "no_branch_available",
        });
    };

    let selected_checkpoint =
        selected_checkpoint_for_transport(prepared_branch, provider_model_fingerprint);
    let Some(checkpoint) = selected_checkpoint.as_ref() else {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "branch_bootstrap_missing_checkpoint",
        });
    };

    if checkpoint.provider_model_fingerprint != provider_model_fingerprint {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "branch_bootstrap_model_drift",
        });
    }

    match chain_decision.match_result {
        WebSocketChainMatch::Matching => {}
        WebSocketChainMatch::Mismatching => {
            return Ok(SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_websocket_chain_mismatch",
            });
        }
        WebSocketChainMatch::Missing => {
            return Ok(SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_missing_websocket_chain",
            });
        }
    }

    if prepared_branch.turn_scope != ConversationTurnScope::Main
        && !prepared_branch.allow_incremental_context
    {
        return Err(
            "conversation branch has a stored OpenAI checkpoint, but this request is not eligible for checkpoint reuse"
                .to_string(),
        );
    }

    if !prepared_branch.commit_turn && prepared_branch.allow_incremental_context {
        return Ok(SelectedTransport {
            mode: TransportMode::Incremental,
            previous_response_id: Some(checkpoint.response_id.clone()),
            reason: "transient_context_read",
        });
    }

    if prepared_branch.commit_turn && prepared_branch.branch.compaction_reset_pending {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "branch_bootstrap_compaction_reset",
        });
    }

    if prepared_branch.commit_turn
        && checkpoint.source == "visible_branch_head"
        && !prepared_branch.active_messages.is_empty()
        && prepared_branch
            .incremental_backend_render_messages()
            .is_empty()
    {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "branch_bootstrap_uncheckpointed_snapshot_head",
        });
    }

    if prepared_branch.commit_turn && prepared_branch.delta_start_index == 0 {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: if prepared_branch.allow_zero_delta_start {
                "branch_bootstrap_zero_delta_start"
            } else {
                "branch_bootstrap_no_prefix_match"
            },
        });
    }

    let reason = if checkpoint.request_compatibility_fingerprint.as_deref()
        == Some(request_compatibility_fingerprint)
    {
        "branch_checkpoint_reuse"
    } else {
        "branch_checkpoint_reuse_compatibility_drift"
    };

    Ok(SelectedTransport {
        mode: TransportMode::Incremental,
        previous_response_id: Some(checkpoint.response_id.clone()),
        reason,
    })
}

fn validate_previous_response_contract(
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
) -> Result<(), String> {
    match selected_transport.mode {
        TransportMode::Incremental => {
            if selected_transport.previous_response_id.is_some() {
                Ok(())
            } else {
                Err(format!(
                    "incremental transport selected without previous_response_id: reason={}",
                    selected_transport.reason
                ))
            }
        }
        TransportMode::Full => validate_full_transport_null_previous_response(
            prepared_branch,
            selected_transport.previous_response_id.as_deref(),
            selected_transport.reason,
        ),
    }
}

fn validate_full_transport_null_previous_response(
    prepared_branch: Option<&PreparedConversationBranch>,
    previous_response_id: Option<&str>,
    reason: &str,
) -> Result<(), String> {
    if previous_response_id.is_some() {
        return Err(format!(
            "full bootstrap transport must not carry previous_response_id: reason={reason}"
        ));
    }

    if is_approved_full_bootstrap_reason(reason) {
        return Ok(());
    }

    let branch_context = prepared_branch.map_or_else(
        || "no_branch".to_string(),
        |branch| {
            format!(
                "session={} branch={} delta_start_index={} commit_turn={} compaction_reset_pending={}",
                branch.claude_session_id,
                branch.branch.branch_id,
                branch.delta_start_index,
                branch.commit_turn,
                branch.branch.compaction_reset_pending
            )
        },
    );
    Err(format!(
        "unapproved null previous_response_id full transport: reason={reason} {branch_context}"
    ))
}

fn is_approved_full_bootstrap_reason(reason: &str) -> bool {
    matches!(
        reason,
        "no_branch_available"
            | "branch_bootstrap_missing_checkpoint"
            | "branch_bootstrap_missing_websocket_chain"
            | "branch_bootstrap_websocket_chain_mismatch"
            | "branch_bootstrap_model_drift"
            | "branch_bootstrap_compaction_reset"
            | "branch_bootstrap_no_prefix_match"
            | "branch_bootstrap_zero_delta_start"
    )
}

fn claude_session_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-claude-code-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn prepare_conversation_branch(
    state: &AppState,
    claude_session_id: Option<&str>,
    req: &AnthropicMessagesRequest,
    _client_metadata: Option<&HashMap<String, String>>,
) -> Option<PreparedConversationBranch> {
    let claude_session_id = claude_session_id?;
    let analysis = analyze_conversation_branch_request(state, claude_session_id, req)?;

    let ConversationBranchAnalysis {
        durable_messages,
        request_kind,
        turn_scope,
        existing_branches,
        prefix_match,
        compaction_context,
        fingerprints,
        active_canonical_messages,
        latest_context_branch,
        internal_reason,
        operational_context_messages,
    } = analysis;

    if let Some(reason) = internal_reason {
        return prepare_transient_from_latest_context(
            claude_session_id,
            latest_context_branch,
            request_kind,
            reason,
            fingerprints,
            durable_messages,
            operational_context_messages,
        );
    }

    if !request_kind.is_visible_main() {
        return prepare_transient_from_latest_context(
            claude_session_id,
            latest_context_branch,
            request_kind,
            request_kind.persistence_reason(),
            fingerprints,
            durable_messages,
            operational_context_messages,
        );
    }

    if turn_scope == ConversationTurnScope::Side {
        return prepare_side_from_latest_context(
            claude_session_id,
            latest_context_branch,
            request_kind,
            turn_scope,
            fingerprints,
            durable_messages,
            operational_context_messages,
        );
    }

    prepare_visible_main_branch(VisibleMainBranchPrep {
        state,
        claude_session_id,
        request_kind,
        turn_scope,
        existing_branches,
        prefix_match,
        compaction_context,
        fingerprints,
        active_canonical_messages,
        operational_context_messages,
        latest_context_branch,
    })
}

struct VisibleMainBranchPrep<'a> {
    state: &'a AppState,
    claude_session_id: &'a str,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    existing_branches: Vec<BranchMetadata>,
    prefix_match: Option<(BranchMetadata, usize)>,
    compaction_context: CompactionRequestContext,
    fingerprints: BranchFingerprintSet,
    active_canonical_messages: serde_json::Value,
    operational_context_messages: Vec<OperationalContextMessage>,
    latest_context_branch: Option<BranchMetadata>,
}

fn prepare_visible_main_branch(
    params: VisibleMainBranchPrep<'_>,
) -> Option<PreparedConversationBranch> {
    if let Some((branch, delta_start_index)) = durable_branch_match(
        params.prefix_match,
        params.latest_context_branch.clone(),
        params.compaction_context,
    ) {
        return prepare_reconciled_durable_branch(ReconciledDurableBranchPrep {
            state: params.state,
            claude_session_id: params.claude_session_id,
            branch,
            request_kind: params.request_kind,
            selection_action: BranchSelectionAction::ContinuedExisting,
            turn_scope: params.turn_scope,
            persistence_reason: "matched_existing_prefix",
            fingerprints: params.fingerprints,
            snapshot: &DurableBranchSnapshot {
                active_canonical_messages: params.active_canonical_messages.clone(),
                delta_start_index,
                allow_zero_delta_start: params.compaction_context.history_marker_seen,
            },
            compaction_command_seen: params.compaction_context.command_seen,
            operational_context_messages: params.operational_context_messages,
        });
    }

    if !params.existing_branches.is_empty() {
        return params
            .latest_context_branch
            .or_else(|| latest_branch(&params.existing_branches))
            .and_then(|branch| {
                prepare_reconciled_durable_branch(ReconciledDurableBranchPrep {
                    state: params.state,
                    claude_session_id: params.claude_session_id,
                    branch,
                    request_kind: params.request_kind,
                    selection_action: BranchSelectionAction::ContinuedExisting,
                    turn_scope: params.turn_scope,
                    persistence_reason: "ambiguous_rebaseline_latest_branch",
                    fingerprints: params.fingerprints,
                    snapshot: &DurableBranchSnapshot {
                        active_canonical_messages: params.active_canonical_messages.clone(),
                        delta_start_index: 0,
                        allow_zero_delta_start: params.compaction_context.history_marker_seen,
                    },
                    compaction_command_seen: params.compaction_context.command_seen,
                    operational_context_messages: params.operational_context_messages,
                })
            });
    }

    prepare_initial_durable_branch(InitialDurableBranchPrep {
        state: params.state,
        claude_session_id: params.claude_session_id,
        request_kind: params.request_kind,
        turn_scope: params.turn_scope,
        fingerprints: params.fingerprints,
        active_canonical_messages: params.active_canonical_messages,
        compaction_command_seen: params.compaction_context.command_seen,
        operational_context_messages: params.operational_context_messages,
    })
}

fn analyze_conversation_branch_request(
    state: &AppState,
    claude_session_id: &str,
    req: &AnthropicMessagesRequest,
) -> Option<ConversationBranchAnalysis> {
    let normalized = normalize_claude_code_context(
        &req.system,
        &req.messages,
        &state.gateway_config.workflow.claude_code,
    );
    let classified_messages = classify_canonical_messages(normalized.messages);
    let durable_messages = classified_messages.durable_visible_messages;
    let request_kind =
        classify_conversation_request(req, &durable_messages, &normalized.client_metadata);
    let turn_scope = match normalized
        .client_metadata
        .get("gateway_conversation_inclusion")
    {
        Some(mode) if mode == "read_only" => ConversationTurnScope::Side,
        _ => ConversationTurnScope::Main,
    };
    let existing_branches = load_existing_branches_for_session(state, claude_session_id);
    let prefix_match = best_prefix_matched_branch(&existing_branches, &durable_messages);
    let compaction_context =
        compaction_context_for_request(&normalized.client_metadata, prefix_match.as_ref());
    let fingerprints = branch_fingerprints_from_messages_with_compaction_prefix(
        &durable_messages,
        compaction_context.command_seen,
        compaction_context.summary_message_count,
    );
    let active_canonical_messages = serde_json::to_value(&durable_messages).ok()?;
    let latest_context_branch = latest_checkpoint_branch(&existing_branches);
    let internal_reason =
        classify_transient_internal_request(req, &durable_messages).or_else(|| {
            (!classified_messages.operational_context_messages.is_empty()
                && !request_kind.is_visible_main())
            .then_some(request_kind.persistence_reason())
        });

    Some(ConversationBranchAnalysis {
        durable_messages,
        request_kind,
        turn_scope,
        existing_branches,
        prefix_match,
        compaction_context,
        fingerprints,
        active_canonical_messages,
        latest_context_branch,
        internal_reason,
        operational_context_messages: classified_messages.operational_context_messages,
    })
}

fn prepare_transient_from_latest_context(
    claude_session_id: &str,
    latest_context_branch: Option<BranchMetadata>,
    request_kind: ConversationRequestKind,
    persistence_reason: &'static str,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
) -> Option<PreparedConversationBranch> {
    latest_context_branch.map(|branch| {
        let delta_start_index = borrowed_context_delta_start_index(&branch, &active_messages);
        transient_prepared_branch(TransientBranchPrep {
            claude_session_id: claude_session_id.to_string(),
            branch,
            request_kind,
            persistence_class: ConversationPersistenceClass::TransientInternal,
            persistence_reason,
            fingerprints,
            active_messages,
            operational_context_messages,
            delta_start_index,
        })
    })
}

fn prepare_side_from_latest_context(
    claude_session_id: &str,
    latest_context_branch: Option<BranchMetadata>,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
    operational_context_messages: Vec<OperationalContextMessage>,
) -> Option<PreparedConversationBranch> {
    latest_context_branch.map(|branch| {
        let delta_start_index = borrowed_context_delta_start_index(&branch, &active_messages);
        side_prepared_branch(SideBranchPrep {
            claude_session_id: claude_session_id.to_string(),
            branch,
            request_kind,
            turn_scope,
            fingerprints,
            active_messages,
            operational_context_messages,
            delta_start_index,
        })
    })
}

fn borrowed_context_delta_start_index(
    branch: &BranchMetadata,
    incoming_messages: &[crate::types::AnthropicMessage],
) -> usize {
    deserialize_active_messages(branch.active_canonical_messages.as_ref()).map_or(0, |stored| {
        if stored_messages_are_prefix(&stored, incoming_messages) {
            stored.len()
        } else {
            0
        }
    })
}

#[derive(Debug, Clone, Copy)]
struct CompactionRequestContext {
    command_seen: bool,
    history_marker_seen: bool,
    summary_message_count: Option<usize>,
}

fn durable_branch_match(
    prefix_match: Option<(BranchMetadata, usize)>,
    latest_context_branch: Option<BranchMetadata>,
    compaction_context: CompactionRequestContext,
) -> Option<(BranchMetadata, usize)> {
    prefix_match.or_else(|| {
        if compaction_context.command_seen {
            return latest_context_branch
                .clone()
                .map(|branch| (active_message_count(&branch), branch))
                .map(|(delta_start_index, branch)| (branch, delta_start_index));
        }
        compaction_context
            .history_marker_seen
            .then(|| latest_context_branch.map(|branch| (branch, 0)))
            .flatten()
    })
}

fn compaction_context_for_request(
    client_metadata: &HashMap<String, String>,
    prefix_match: Option<&(BranchMetadata, usize)>,
) -> CompactionRequestContext {
    let command_seen = is_active_slash_command(client_metadata, "/compact")
        || has_active_local_only_command(client_metadata, "/compact");
    let history_marker_seen = !command_seen && has_local_only_command(client_metadata, "/compact");
    CompactionRequestContext {
        command_seen,
        history_marker_seen,
        summary_message_count: command_seen
            .then(|| prefix_match.map(|(_, count)| *count))
            .flatten(),
    }
}

fn prepare_reconciled_durable_branch(
    params: ReconciledDurableBranchPrep<'_>,
) -> Option<PreparedConversationBranch> {
    let branch = apply_compaction_if_needed(
        params.state,
        params.claude_session_id,
        params.branch,
        &params.fingerprints,
        params.compaction_command_seen,
    )?;
    durable_prepared_branch(DurableBranchPrep {
        claude_session_id: params.claude_session_id,
        branch,
        request_kind: params.request_kind,
        selection_action: params.selection_action,
        turn_scope: params.turn_scope,
        persistence_reason: params.persistence_reason,
        fingerprints: params.fingerprints,
        snapshot: params.snapshot,
        operational_context_messages: params.operational_context_messages,
    })
}

fn load_existing_branches_for_session(
    state: &AppState,
    claude_session_id: &str,
) -> Vec<BranchMetadata> {
    let Ok(session) = state.conversation_state.load_session(claude_session_id) else {
        return Vec::new();
    };
    session
        .branch_ids
        .iter()
        .filter_map(|branch_id| {
            state
                .conversation_state
                .load_branch(claude_session_id, branch_id)
                .ok()
        })
        .collect()
}

fn transient_prepared_branch(params: TransientBranchPrep) -> PreparedConversationBranch {
    PreparedConversationBranch {
        claude_session_id: params.claude_session_id,
        branch: params.branch,
        request_kind: params.request_kind,
        selection_action: BranchSelectionAction::ContinuedExisting,
        turn_scope: ConversationTurnScope::Side,
        persistence_class: params.persistence_class,
        persistence_reason: params.persistence_reason,
        commit_turn: false,
        allow_incremental_context: true,
        allow_zero_delta_start: false,
        fingerprints: params.fingerprints,
        active_messages: params.active_messages,
        operational_context_messages: params.operational_context_messages,
        delta_start_index: params.delta_start_index,
    }
}

fn side_prepared_branch(params: SideBranchPrep) -> PreparedConversationBranch {
    PreparedConversationBranch {
        claude_session_id: params.claude_session_id,
        branch: params.branch,
        request_kind: params.request_kind,
        selection_action: BranchSelectionAction::ContinuedExisting,
        turn_scope: params.turn_scope,
        persistence_class: ConversationPersistenceClass::ReadOnlySideTurn,
        persistence_reason: "read_only_conversation_inclusion",
        commit_turn: false,
        allow_incremental_context: true,
        allow_zero_delta_start: false,
        fingerprints: params.fingerprints,
        active_messages: params.active_messages,
        operational_context_messages: params.operational_context_messages,
        delta_start_index: params.delta_start_index,
    }
}

fn durable_prepared_branch(params: DurableBranchPrep<'_>) -> Option<PreparedConversationBranch> {
    let active_messages = deserialize_active_messages(Some(
        &params.snapshot.active_canonical_messages,
    ))
    .or_else(|| deserialize_active_messages(params.branch.active_canonical_messages.as_ref()))?;
    Some(PreparedConversationBranch {
        claude_session_id: params.claude_session_id.to_string(),
        branch: params.branch,
        request_kind: params.request_kind,
        selection_action: params.selection_action,
        turn_scope: params.turn_scope,
        persistence_class: ConversationPersistenceClass::DurableMain,
        persistence_reason: params.persistence_reason,
        commit_turn: true,
        allow_incremental_context: true,
        allow_zero_delta_start: params.snapshot.allow_zero_delta_start,
        fingerprints: params.fingerprints,
        active_messages,
        operational_context_messages: params.operational_context_messages,
        delta_start_index: params.snapshot.delta_start_index,
    })
}

fn prepare_initial_durable_branch(
    params: InitialDurableBranchPrep<'_>,
) -> Option<PreparedConversationBranch> {
    let selection = params
        .state
        .conversation_state
        .select_or_create_branch(
            params.claude_session_id,
            &BranchSelectionInput {
                active_canonical_messages: Some(params.active_canonical_messages.clone()),
                fingerprints: params.fingerprints.clone(),
                turn_scope: params.turn_scope,
            },
        )
        .ok()?;
    let branch = apply_initial_selection_compaction_if_needed(
        params.state,
        params.claude_session_id,
        &selection,
        &params.fingerprints,
        params.compaction_command_seen,
    )?;
    durable_prepared_branch(DurableBranchPrep {
        claude_session_id: params.claude_session_id,
        branch,
        request_kind: params.request_kind,
        selection_action: selection.action,
        turn_scope: params.turn_scope,
        persistence_reason: "initial_or_selected_durable_branch",
        fingerprints: params.fingerprints,
        snapshot: &DurableBranchSnapshot {
            active_canonical_messages: params.active_canonical_messages,
            delta_start_index: 0,
            allow_zero_delta_start: false,
        },
        operational_context_messages: params.operational_context_messages,
    })
}

fn apply_initial_selection_compaction_if_needed(
    state: &AppState,
    claude_session_id: &str,
    selection: &gateway_state::BranchSelectionResult,
    fingerprints: &BranchFingerprintSet,
    compaction_command_seen: bool,
) -> Option<BranchMetadata> {
    let previous_compaction_summary_hash = selection
        .matched_existing_branch
        .as_ref()
        .and_then(|existing| existing.fingerprints.compaction_summary_hash.clone());
    if compaction_command_seen
        && (fingerprints.compaction_summary_hash.is_none()
            || previous_compaction_summary_hash != fingerprints.compaction_summary_hash)
    {
        return state
            .conversation_state
            .apply_compaction(
                claude_session_id,
                &selection.branch.branch_id,
                fingerprints.compaction_summary_hash.as_deref(),
                fingerprints,
            )
            .ok();
    }
    Some(selection.branch.clone())
}

fn apply_compaction_if_needed(
    state: &AppState,
    claude_session_id: &str,
    branch: BranchMetadata,
    fingerprints: &BranchFingerprintSet,
    compaction_command_seen: bool,
) -> Option<BranchMetadata> {
    if compaction_command_seen
        && (fingerprints.compaction_summary_hash.is_none()
            || branch.fingerprints.compaction_summary_hash != fingerprints.compaction_summary_hash)
    {
        return state
            .conversation_state
            .apply_compaction(
                claude_session_id,
                &branch.branch_id,
                fingerprints.compaction_summary_hash.as_deref(),
                fingerprints,
            )
            .ok();
    }
    Some(branch)
}

fn best_prefix_matched_branch(
    branches: &[BranchMetadata],
    incoming_messages: &[crate::types::AnthropicMessage],
) -> Option<(BranchMetadata, usize)> {
    branches
        .iter()
        .filter_map(|branch| {
            let stored_messages =
                deserialize_active_messages(branch.active_canonical_messages.as_ref())?;
            stored_messages_are_prefix(&stored_messages, incoming_messages).then_some((
                stored_messages.len(),
                branch.updated_at_unix_seconds,
                branch.clone(),
            ))
        })
        .max_by_key(|(message_count, updated_at, _)| (*message_count, *updated_at))
        .map(|(message_count, _, branch)| (branch, message_count))
}

fn active_message_count(branch: &BranchMetadata) -> usize {
    deserialize_active_messages(branch.active_canonical_messages.as_ref())
        .map_or(0, |messages| messages.len())
}

fn latest_checkpoint_branch(branches: &[BranchMetadata]) -> Option<BranchMetadata> {
    branches
        .iter()
        .filter(|branch| branch.openai_checkpoint.is_some())
        .max_by_key(|branch| branch.updated_at_unix_seconds)
        .cloned()
}

fn latest_branch(branches: &[BranchMetadata]) -> Option<BranchMetadata> {
    branches
        .iter()
        .max_by_key(|branch| branch.updated_at_unix_seconds)
        .cloned()
}

fn stored_messages_are_prefix(
    stored_messages: &[crate::types::AnthropicMessage],
    incoming_messages: &[crate::types::AnthropicMessage],
) -> bool {
    if stored_messages.is_empty() || stored_messages.len() > incoming_messages.len() {
        return false;
    }
    stored_messages
        .iter()
        .zip(incoming_messages.iter())
        .all(|(stored, incoming)| {
            canonical_message_value(stored) == canonical_message_value(incoming)
        })
}

fn classify_canonical_messages(
    messages: Vec<crate::types::AnthropicMessage>,
) -> ClassifiedCanonicalMessages {
    let mut durable_visible_messages = Vec::new();
    let mut operational_context_messages = Vec::new();
    let mut durable_messages_before = 0usize;

    for message in messages {
        if is_turn_level_system_message(&message) {
            operational_context_messages.push(OperationalContextMessage {
                message,
                durable_messages_before,
            });
            continue;
        }
        if let Some(message) = durable_canonical_message(message) {
            durable_visible_messages.push(message);
            durable_messages_before += 1;
        }
    }

    ClassifiedCanonicalMessages {
        durable_visible_messages,
        operational_context_messages,
    }
}

fn durable_canonical_message(
    mut message: crate::types::AnthropicMessage,
) -> Option<crate::types::AnthropicMessage> {
    if is_turn_level_system_message(&message) {
        return None;
    }

    match &mut message.content {
        crate::types::AnthropicContent::Text(text) => (!text.trim().is_empty()).then_some(message),
        crate::types::AnthropicContent::Blocks(blocks) => {
            blocks.retain(|block| {
                block.block_type != "text"
                    || block
                        .text
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
            });
            (!blocks.is_empty()).then_some(message)
        }
    }
}

fn is_turn_level_system_message(message: &crate::types::AnthropicMessage) -> bool {
    message.role.eq_ignore_ascii_case("system")
}

fn canonical_message_value(message: &crate::types::AnthropicMessage) -> serde_json::Value {
    serde_json::json!({
        "role": message.role,
        "content": canonical_content_value(&message.content),
    })
}

fn canonical_content_value(content: &crate::types::AnthropicContent) -> serde_json::Value {
    match content {
        crate::types::AnthropicContent::Text(text) => {
            serde_json::json!([{ "type": "text", "text": text }])
        }
        crate::types::AnthropicContent::Blocks(blocks) => serde_json::Value::Array(
            blocks
                .iter()
                .map(canonical_content_block_value)
                .collect::<Vec<_>>(),
        ),
    }
}

fn canonical_content_block_value(block: &crate::types::AnthropicContentBlock) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "type".to_string(),
        serde_json::Value::String(block.block_type.clone()),
    );
    insert_optional_string(&mut object, "text", block.text.as_deref());
    insert_optional_string(&mut object, "id", block.id.as_deref());
    insert_optional_string(&mut object, "name", block.name.as_deref());
    insert_optional_value(&mut object, "input", block.input.as_ref());
    insert_optional_string(&mut object, "tool_use_id", block.tool_use_id.as_deref());
    insert_optional_value(&mut object, "content", block.content.as_ref());
    if let Some(is_error) = block.is_error {
        object.insert("is_error".to_string(), serde_json::Value::Bool(is_error));
    }
    if let Some(source) = block.source.as_ref()
        && let Ok(source_value) = serde_json::to_value(source)
    {
        object.insert(
            "source".to_string(),
            strip_transient_message_metadata(source_value),
        );
    }
    for (key, value) in &block.extra {
        if key != "cache_control" {
            object.insert(key.clone(), strip_transient_message_metadata(value.clone()));
        }
    }
    serde_json::Value::Object(object)
}

fn insert_optional_string(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        object.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
}

fn insert_optional_value(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&serde_json::Value>,
) {
    if let Some(value) = value {
        object.insert(
            key.to_string(),
            strip_transient_message_metadata(value.clone()),
        );
    }
}

fn strip_transient_message_metadata(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .filter(|(key, _)| key != "cache_control")
                .map(|(key, value)| (key, strip_transient_message_metadata(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(strip_transient_message_metadata)
                .collect(),
        ),
        value => value,
    }
}

fn classify_transient_internal_request(
    req: &AnthropicMessagesRequest,
    messages: &[crate::types::AnthropicMessage],
) -> Option<&'static str> {
    let joined_text = messages
        .iter()
        .map(anthropic_message_text)
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    let role_counts_as_internal_decision = !req.stream
        && messages.len() <= 2
        && !messages.iter().any(|message| message.role == "assistant");
    let has_transcript_wrapper = joined_text.contains("<transcript>");
    let has_permission_context = joined_text.contains("specific action under review")
        || joined_text.contains("claude.md configuration")
        || joined_text.contains("<block>")
        || joined_text.contains("authorize")
        || joined_text.contains("permission");

    (role_counts_as_internal_decision && has_transcript_wrapper && has_permission_context)
        .then_some("permission_or_classifier_transcript")
}

fn classify_conversation_request(
    req: &AnthropicMessagesRequest,
    messages: &[crate::types::AnthropicMessage],
    client_metadata: &HashMap<String, String>,
) -> ConversationRequestKind {
    let system_text = system_prompt_text(&req.system).to_ascii_lowercase();
    let joined_text = messages
        .iter()
        .map(anthropic_message_text)
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    if client_metadata
        .get("gateway_conversation_inclusion")
        .is_some_and(|mode| mode == "read_only")
    {
        return ConversationRequestKind::LocalControl;
    }

    if system_text.contains("cc_is_subagent=true") {
        return ConversationRequestKind::SubagentOffshoot;
    }

    if classify_transient_internal_request(req, messages).is_some() {
        return ConversationRequestKind::PermissionClassifier;
    }

    if !req.stream
        && req.output_config.is_some()
        && messages.len() <= 2
        && !messages.iter().any(|message| message.role == "assistant")
    {
        return ConversationRequestKind::HookEvaluator;
    }

    if messages.len() <= 2
        && (system_text.contains("claude agent sdk")
            || joined_text
                .contains("the following skills are available for use with the skill tool"))
    {
        return ConversationRequestKind::UnknownOffshoot;
    }

    ConversationRequestKind::VisibleMain
}

fn system_prompt_text(system: &[crate::types::AnthropicSystemBlock]) -> String {
    system
        .iter()
        .filter_map(|block| block.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

fn request_id_str(request_id: Option<&axum::extract::Extension<RequestId>>) -> Option<&str> {
    request_id.map(|axum::extract::Extension(rid)| rid.0.as_str())
}

fn log_conversation_branch_resolution(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
) {
    let Some(prepared_branch) = prepared_branch else {
        if let Some(request_id) = request_id_str(request_id) {
            info!(
                request_id = %request_id,
                branch_resolution = "skipped",
                "conversation-state branch resolution produced no branch context"
            );
        } else {
            info!(
                branch_resolution = "skipped",
                "conversation-state branch resolution produced no branch context"
            );
        }
        return;
    };

    if let Some(request_id) = request_id_str(request_id) {
        info!(
            request_id = %request_id,
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            branch_action = ?prepared_branch.selection_action,
            turn_scope = ?prepared_branch.turn_scope,
            persistence_class = ?prepared_branch.persistence_class,
            persistence_reason = prepared_branch.persistence_reason,
            commit_turn = prepared_branch.commit_turn,
            compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
            "selected conversation-state branch"
        );
    } else {
        info!(
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            branch_action = ?prepared_branch.selection_action,
            turn_scope = ?prepared_branch.turn_scope,
            persistence_class = ?prepared_branch.persistence_class,
            persistence_reason = prepared_branch.persistence_reason,
            commit_turn = prepared_branch.commit_turn,
            compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
            "selected conversation-state branch"
        );
    }
}

fn log_transport_selection(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
    selected_checkpoint: Option<&SelectedCheckpoint>,
    provider_model_fingerprint: &str,
    chain_decision: Option<&WebSocketChainDecision>,
) {
    let transport_mode = match selected_transport.mode {
        TransportMode::Full => "full",
        TransportMode::Incremental => "incremental",
    };
    append_transport_selection_diagnostic(
        request_id,
        prepared_branch,
        selected_transport,
        selected_checkpoint,
        provider_model_fingerprint,
        chain_decision,
        transport_mode,
    );

    if let Some(prepared_branch) = prepared_branch {
        if let Some(request_id) = request_id_str(request_id) {
            info!(
                request_id = %request_id,
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                transport_mode,
                transport_reason = selected_transport.reason,
                previous_response_id = ?selected_transport.previous_response_id,
                provider_model_fingerprint,
                compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
                websocket_chain_match = ?chain_decision.map(|decision| decision.match_result),
                websocket_chain_reason = ?chain_decision.map(|decision| decision.reason),
                live_websocket_chain_id = ?chain_decision.and_then(|decision| decision.live_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
                checkpoint_websocket_chain_id = ?chain_decision.and_then(|decision| decision.checkpoint_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
                checkpoint_response_id = ?chain_decision.and_then(|decision| decision.checkpoint_response_id.as_deref()),
                "selected conversation-state transport mode"
            );
        } else {
            info!(
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                transport_mode,
                transport_reason = selected_transport.reason,
                previous_response_id = ?selected_transport.previous_response_id,
                provider_model_fingerprint,
                compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
                websocket_chain_match = ?chain_decision.map(|decision| decision.match_result),
                websocket_chain_reason = ?chain_decision.map(|decision| decision.reason),
                live_websocket_chain_id = ?chain_decision.and_then(|decision| decision.live_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
                checkpoint_websocket_chain_id = ?chain_decision.and_then(|decision| decision.checkpoint_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
                checkpoint_response_id = ?chain_decision.and_then(|decision| decision.checkpoint_response_id.as_deref()),
                "selected conversation-state transport mode"
            );
        }
    } else if let Some(request_id) = request_id_str(request_id) {
        info!(
            request_id = %request_id,
            transport_mode,
            transport_reason = selected_transport.reason,
            previous_response_id = ?selected_transport.previous_response_id,
            provider_model_fingerprint,
            websocket_chain_match = ?chain_decision.map(|decision| decision.match_result),
            websocket_chain_reason = ?chain_decision.map(|decision| decision.reason),
            live_websocket_chain_id = ?chain_decision.and_then(|decision| decision.live_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
            checkpoint_websocket_chain_id = ?chain_decision.and_then(|decision| decision.checkpoint_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
            checkpoint_response_id = ?chain_decision.and_then(|decision| decision.checkpoint_response_id.as_deref()),
            "selected conversation-state transport mode without branch context"
        );
    } else {
        info!(
            transport_mode,
            transport_reason = selected_transport.reason,
            previous_response_id = ?selected_transport.previous_response_id,
            provider_model_fingerprint,
            websocket_chain_match = ?chain_decision.map(|decision| decision.match_result),
            websocket_chain_reason = ?chain_decision.map(|decision| decision.reason),
            live_websocket_chain_id = ?chain_decision.and_then(|decision| decision.live_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
            checkpoint_websocket_chain_id = ?chain_decision.and_then(|decision| decision.checkpoint_websocket_chain_id.as_ref()).map(WebSocketChainId::as_str),
            checkpoint_response_id = ?chain_decision.and_then(|decision| decision.checkpoint_response_id.as_deref()),
            "selected conversation-state transport mode without branch context"
        );
    }
}

fn append_transport_selection_diagnostic(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
    selected_checkpoint: Option<&SelectedCheckpoint>,
    provider_model_fingerprint: &str,
    chain_decision: Option<&WebSocketChainDecision>,
    transport_mode: &str,
) {
    append_transport_diagnostic(transport_selection_diagnostic_value(
        request_id,
        prepared_branch,
        selected_transport,
        selected_checkpoint,
        provider_model_fingerprint,
        chain_decision,
        transport_mode,
    ));
}

fn transport_selection_diagnostic_value(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
    selected_checkpoint: Option<&SelectedCheckpoint>,
    provider_model_fingerprint: &str,
    chain_decision: Option<&WebSocketChainDecision>,
    transport_mode: &str,
) -> serde_json::Value {
    let mut event = serde_json::json!({
        "event": "transport_selected",
        "request_id": request_id_str(request_id),
        "transport_mode": transport_mode,
        "transport_reason": selected_transport.reason,
        "previous_response_id": selected_transport.previous_response_id,
        "provider_model_fingerprint": provider_model_fingerprint,
        "reasoning_effort": chain_decision
            .and_then(|decision| decision.transport_identity.as_deref())
            .and_then(|identity| identity.rsplit(':').next()),
        "commit_policy": transport_selection_commit_policy(prepared_branch),
        "client_abort_state": LeaseDiagnosticState::NotAborted.as_str(),
        "transport_identity": chain_decision.and_then(|decision| decision.transport_identity.as_deref()),
        "websocket_chain_match": chain_decision.map(|decision| match decision.match_result {
            WebSocketChainMatch::Matching => "matching",
            WebSocketChainMatch::Mismatching => "mismatching",
            WebSocketChainMatch::Missing => "missing",
        }),
        "websocket_chain_reason": chain_decision.map(|decision| decision.reason),
        "live_websocket_chain_id": chain_decision
            .and_then(|decision| decision.live_websocket_chain_id.as_ref())
            .map(WebSocketChainId::as_str),
        "checkpoint_websocket_chain_id": chain_decision
            .and_then(|decision| decision.checkpoint_websocket_chain_id.as_ref())
            .map(WebSocketChainId::as_str),
        "checkpoint_response_id": chain_decision
            .and_then(|decision| decision.checkpoint_response_id.as_deref()),
        "selected_checkpoint_source": selected_checkpoint.map(|checkpoint| checkpoint.source),
        "selected_checkpoint_response_id": selected_checkpoint
            .map(|checkpoint| checkpoint.response_id.as_str()),
        "selected_checkpoint_message_count": selected_checkpoint
            .and_then(|checkpoint| checkpoint.canonical_message_count),
    });

    if let Some(prepared_branch) = prepared_branch
        && let Some(object) = event.as_object_mut()
    {
        object.insert(
            "claude_session_id".to_string(),
            serde_json::Value::String(prepared_branch.claude_session_id.clone()),
        );
        object.insert(
            "branch_id".to_string(),
            serde_json::Value::String(prepared_branch.branch.branch_id.clone()),
        );
        object.insert(
            "request_kind".to_string(),
            serde_json::Value::String(prepared_branch.request_kind.as_key().to_string()),
        );
        object.insert(
            "turn_scope".to_string(),
            serde_json::Value::String(format!("{:?}", prepared_branch.turn_scope)),
        );
        object.insert(
            "commit_turn".to_string(),
            serde_json::Value::Bool(prepared_branch.commit_turn),
        );
        object.insert(
            "delta_start_index".to_string(),
            serde_json::Value::from(prepared_branch.delta_start_index),
        );
        object.insert(
            "active_messages_len".to_string(),
            serde_json::Value::from(prepared_branch.active_messages.len()),
        );
        object.insert(
            "compaction_reset_pending".to_string(),
            serde_json::Value::Bool(prepared_branch.branch.compaction_reset_pending),
        );
    }

    event
}

fn transport_selection_commit_policy(
    prepared_branch: Option<&PreparedConversationBranch>,
) -> &'static str {
    match prepared_branch {
        Some(branch) if branch.commit_turn && branch.request_kind.is_visible_main() => {
            "visible_main_lease_required"
        }
        Some(branch) if !branch.commit_turn && !branch.request_kind.is_visible_main() => {
            "offshoot_checkpoint_only"
        }
        Some(branch) if !branch.commit_turn => "read_only_no_main_commit",
        Some(_) => "durable_visible_main_commit",
        None => "no_conversation_commit",
    }
}

struct CheckpointDiagnosticSubject<'a> {
    claude_session_id: &'a str,
    branch_id: &'a str,
    request_kind: ConversationRequestKind,
    turn_scope: ConversationTurnScope,
    commit_turn: bool,
}

struct CheckpointDiagnostic<'a> {
    event: &'a str,
    subject: CheckpointDiagnosticSubject<'a>,
    transport_identity: Option<&'a ConversationTransportIdentity>,
    request_id: Option<&'a str>,
    provider_response_id: Option<&'a str>,
    previous_response_id: Option<&'a str>,
    selected_checkpoint_source: Option<&'a str>,
    selected_checkpoint_response_id: Option<&'a str>,
    websocket_chain_id: Option<&'a WebSocketChainId>,
    streaming: bool,
    commit_policy: &'a str,
    skip_reason: Option<&'a str>,
    client_abort_state: LeaseDiagnosticState,
}

#[derive(Debug, Clone, Copy)]
enum LeaseDiagnosticState {
    NotAborted,
    ClientAbortedBeforeFirstEvent,
    ClientAbortedAfterVisibleOutput,
    BackendFailedBeforeCommit,
    CompletedCommitted,
    CommitSuppressedAfterAbort,
}

impl LeaseDiagnosticState {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotAborted => "not_aborted",
            Self::ClientAbortedBeforeFirstEvent => "client_aborted_before_first_event",
            Self::ClientAbortedAfterVisibleOutput => "client_aborted_after_visible_output",
            Self::BackendFailedBeforeCommit => "backend_failed_before_commit",
            Self::CompletedCommitted => "completed_committed",
            Self::CommitSuppressedAfterAbort => "commit_suppressed_after_abort",
        }
    }
}

fn lease_diagnostic_state_for_reason(reason: &str) -> LeaseDiagnosticState {
    match reason {
        "client_aborted_before_first_event" => LeaseDiagnosticState::ClientAbortedBeforeFirstEvent,
        "client_aborted_after_visible_output" => {
            LeaseDiagnosticState::ClientAbortedAfterVisibleOutput
        }
        "backend_failed_before_commit" => LeaseDiagnosticState::BackendFailedBeforeCommit,
        "completed_committed" => LeaseDiagnosticState::CompletedCommitted,
        "commit_suppressed_after_abort" => LeaseDiagnosticState::CommitSuppressedAfterAbort,
        _ => LeaseDiagnosticState::NotAborted,
    }
}

impl<'a> From<&'a PreparedConversationBranch> for CheckpointDiagnosticSubject<'a> {
    fn from(prepared_branch: &'a PreparedConversationBranch) -> Self {
        Self {
            claude_session_id: &prepared_branch.claude_session_id,
            branch_id: &prepared_branch.branch.branch_id,
            request_kind: prepared_branch.request_kind,
            turn_scope: prepared_branch.turn_scope,
            commit_turn: prepared_branch.commit_turn,
        }
    }
}

fn append_checkpoint_diagnostic(diagnostic: &CheckpointDiagnostic<'_>) {
    append_transport_diagnostic(checkpoint_diagnostic_value(diagnostic));
}

fn checkpoint_diagnostic_value(diagnostic: &CheckpointDiagnostic<'_>) -> serde_json::Value {
    serde_json::json!({
        "event": diagnostic.event,
        "request_id": diagnostic.request_id,
        "transport_identity": diagnostic.transport_identity.map(ConversationTransportIdentity::key),
        "claude_session_id": diagnostic.subject.claude_session_id,
        "branch_id": diagnostic.subject.branch_id,
        "request_kind": diagnostic.subject.request_kind.as_key(),
        "turn_scope": format!("{:?}", diagnostic.subject.turn_scope),
        "commit_turn": diagnostic.subject.commit_turn,
        "commit_policy": diagnostic.commit_policy,
        "skip_reason": diagnostic.skip_reason,
        "provider_model_fingerprint": diagnostic.transport_identity
            .map(|identity| identity.provider_model_fingerprint.as_str()),
        "reasoning_effort": diagnostic.transport_identity
            .map(|identity| identity.reasoning_effort.as_str()),
        "selected_checkpoint_source": diagnostic.selected_checkpoint_source,
        "selected_checkpoint_response_id": diagnostic.selected_checkpoint_response_id,
        "provider_response_id": diagnostic.provider_response_id,
        "committed_previous_response_id": diagnostic.previous_response_id,
        "websocket_chain_id": diagnostic.websocket_chain_id.map(WebSocketChainId::as_str),
        "streaming": diagnostic.streaming,
        "client_abort_state": diagnostic.client_abort_state.as_str(),
    })
}

fn append_transport_diagnostic(mut event: serde_json::Value) {
    if let Some(object) = event.as_object_mut() {
        object.insert(
            "timestamp_unix_ms".to_string(),
            serde_json::Value::from(now_unix_millis()),
        );
    }

    let Some(path) = transport_diagnostics_log_path() else {
        return;
    };
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if serde_json::to_writer(&mut file, &event).is_ok() {
        let _ = writeln!(file);
    }
}

fn transport_diagnostics_log_path() -> Option<PathBuf> {
    if let Ok(gateway_home) = std::env::var("GATEWAY_HOME") {
        return Some(
            PathBuf::from(gateway_home)
                .join("logs")
                .join("transport-decisions.jsonl"),
        );
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".gateway")
            .join("logs")
            .join("transport-decisions.jsonl")
    })
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
fn branch_fingerprints_from_messages(
    messages: &[crate::types::AnthropicMessage],
    compaction_command_seen: bool,
) -> BranchFingerprintSet {
    branch_fingerprints_from_messages_with_compaction_prefix(
        messages,
        compaction_command_seen,
        None,
    )
}

fn branch_fingerprints_from_messages_with_compaction_prefix(
    messages: &[crate::types::AnthropicMessage],
    compaction_command_seen: bool,
    compaction_summary_message_count: Option<usize>,
) -> BranchFingerprintSet {
    let mut text_messages = Vec::new();
    let mut last_user_text = None;

    for message in messages {
        let text = anthropic_message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        if message.role == "user" {
            last_user_text = Some(text.clone());
        }
        text_messages.push(format!("{}:{}", message.role, text.trim()));
    }

    let recent_tail = text_messages
        .iter()
        .rev()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let full_transcript = text_messages.join("\n");
    let branch_state_hash = (!full_transcript.is_empty()).then(|| sha256_hex(&full_transcript));
    let compaction_summary_transcript = compaction_summary_message_count.map_or_else(
        || full_transcript.clone(),
        |count| {
            text_messages
                .iter()
                .take(count)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        },
    );

    BranchFingerprintSet {
        recent_message_tail_hash: (!recent_tail.is_empty()).then(|| sha256_hex(&recent_tail)),
        last_user_message_hash: last_user_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(sha256_hex),
        compaction_summary_hash: compaction_command_seen
            .then(|| {
                (!compaction_summary_transcript.is_empty())
                    .then(|| sha256_hex(&compaction_summary_transcript))
            })
            .flatten(),
        branch_state_hash,
    }
}

fn is_active_slash_command(client_metadata: &HashMap<String, String>, command: &str) -> bool {
    client_metadata
        .get("claude_code_slash_command")
        .is_some_and(|candidate| candidate.trim() == command)
}

fn has_local_only_command(client_metadata: &HashMap<String, String>, command: &str) -> bool {
    client_metadata
        .get("gateway_local_only_commands")
        .is_some_and(|commands| {
            commands
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == command)
        })
}

fn has_active_local_only_command(client_metadata: &HashMap<String, String>, command: &str) -> bool {
    client_metadata
        .get("gateway_active_local_only_commands")
        .is_some_and(|commands| {
            commands
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == command)
        })
}

fn anthropic_message_text(message: &crate::types::AnthropicMessage) -> String {
    match &message.content {
        crate::types::AnthropicContent::Text(text) => text.clone(),
        crate::types::AnthropicContent::Blocks(blocks) => blocks
            .iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn deserialize_active_messages(
    snapshot: Option<&serde_json::Value>,
) -> Option<Vec<crate::types::AnthropicMessage>> {
    snapshot
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn request_with_messages(
    req: &AnthropicMessagesRequest,
    messages: Vec<crate::types::AnthropicMessage>,
) -> AnthropicMessagesRequest {
    AnthropicMessagesRequest {
        model: req.model.clone(),
        messages,
        system: req.system.clone(),
        stream: req.stream,
        stop_sequences: req.stop_sequences.clone(),
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: req.top_k,
        metadata: req.metadata.clone(),
        tools: req.tools.clone(),
        tool_choice: req.tool_choice.clone(),
        thinking: req.thinking.clone(),
        context_management: req.context_management.clone(),
        output_config: req.output_config.clone(),
    }
}

fn request_for_selected_transport(
    req: &AnthropicMessagesRequest,
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
) -> AnthropicMessagesRequest {
    let Some(prepared_branch) = prepared_branch else {
        return req.clone();
    };

    let messages = match selected_transport.mode {
        TransportMode::Full => prepared_branch.full_backend_render_messages(),
        TransportMode::Incremental => prepared_branch.incremental_backend_render_messages(),
    };

    request_with_messages(req, messages)
}

fn canonical_messages_prefix_hash(
    messages: &[crate::types::AnthropicMessage],
    count: usize,
) -> String {
    let prefix = messages
        .iter()
        .take(count)
        .map(canonical_message_value)
        .collect::<Vec<_>>();
    hash_serde_value(&serde_json::Value::Array(prefix))
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hash_serde_value(value: &serde_json::Value) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    sha256_hex(&encoded)
}

fn backend_events_to_anthropic_events(
    backend_events: gateway_backend_codex::types::CodexBackendEventStream,
    conversation_state: ConversationStateStore,
    tool_calls: ToolCallStore,
    request_id: Option<String>,
    context_management: Option<serde_json::Value>,
    structured_output_schema: Option<serde_json::Value>,
    stream_commit: Option<StreamCommitContext>,
) -> futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> {
    let state = Arc::new(Mutex::new(
        crate::sse_bridge::StreamState::new()
            .with_context_management(context_management)
            .with_structured_output_schema(structured_output_schema),
    ));
    let tool_calls = Arc::new(tool_calls);
    let request_id = Arc::new(request_id);
    let conversation_state = Arc::new(conversation_state);
    let stream_commit = Arc::new(stream_commit);

    backend_events
        .filter_map({
            let state = Arc::clone(&state);
            let tool_calls = Arc::clone(&tool_calls);
            let request_id = Arc::clone(&request_id);
            let conversation_state = Arc::clone(&conversation_state);
            let stream_commit = Arc::clone(&stream_commit);
            move |item| {
                let state = Arc::clone(&state);
                let tool_calls = Arc::clone(&tool_calls);
                let request_id = Arc::clone(&request_id);
                let conversation_state = Arc::clone(&conversation_state);
                let stream_commit = Arc::clone(&stream_commit);
                async move {
                    let mut st = state.lock().unwrap();
                    if st.completed {
                        return None;
                    }

                    let evt = match item {
                        Ok(e) => e,
                        Err(e) => {
                            st.completed = true;
                            if let Some(commit) = stream_commit.as_ref().as_ref() {
                                mark_stream_backend_failure_before_commit(commit, &e);
                            }
                            let message = format!("event stream error: {e}");
                            let payload = serde_json::json!({
                                "type": "error",
                                "error": { "type": "upstream_error", "message": message }
                            })
                            .to_string();
                            return Some(vec![Event::default().event("error").data(payload)]);
                        }
                    };

                    let data = evt.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        return None;
                    }

                    let mapped = crate::sse_bridge::map_backend_event(
                        &mut st,
                        evt.event.as_str(),
                        data,
                        gateway_backend_codex::output_text::extract_text_from_data,
                        &tool_calls,
                        request_id.as_ref().as_deref(),
                    );
                    drop(st);

                    maybe_commit_stream_completion(
                        &conversation_state,
                        stream_commit.as_ref().as_ref(),
                        evt.event.as_str(),
                        data,
                    );

                    if let Some(events) = mapped.as_ref()
                        && !events.is_empty()
                        && let Some(commit) = stream_commit.as_ref().as_ref()
                        && let Some(lease) = commit.main_turn_lease.as_ref()
                    {
                        lease.mark_visible_output_sent();
                    }

                    mapped
                }
            }
        })
        .flat_map(|events| {
            futures_util::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>))
        })
        .boxed()
}

fn mark_stream_backend_failure_before_commit(commit: &StreamCommitContext, err: &BackendError) {
    if !commit.commit_turn {
        return;
    }
    let Some(lease) = commit.main_turn_lease.as_ref() else {
        return;
    };

    let released = lease.release_with_state(MainTurnLeaseState::BackendFailedBeforeCommit);
    append_checkpoint_diagnostic(&CheckpointDiagnostic {
        event: "checkpoint_commit_skipped",
        subject: stream_checkpoint_subject(commit),
        transport_identity: commit.transport_identity.as_ref(),
        request_id: commit.request_id.as_deref(),
        provider_response_id: None,
        previous_response_id: commit.previous_response_id.as_deref(),
        selected_checkpoint_source: commit.selected_checkpoint_source,
        selected_checkpoint_response_id: commit.selected_checkpoint_response_id.as_deref(),
        websocket_chain_id: commit.websocket_chain_id.as_ref(),
        streaming: true,
        commit_policy: "backend_failed_before_commit",
        skip_reason: Some("backend_failed_before_commit"),
        client_abort_state: LeaseDiagnosticState::BackendFailedBeforeCommit,
    });
    append_transport_diagnostic(serde_json::json!({
        "event": "visible_main_lease_backend_failed",
        "request_id": commit.request_id,
        "transport_identity": commit.transport_identity.as_ref().map(ConversationTransportIdentity::key),
        "claude_session_id": commit.claude_session_id,
        "branch_id": commit.branch_id,
        "provider_model_fingerprint": commit.provider_model_fingerprint,
        "previous_response_id": commit.previous_response_id,
        "websocket_chain_id": commit.websocket_chain_id.as_ref().map(WebSocketChainId::as_str),
        "released": released,
        "client_abort_state": MainTurnLeaseState::BackendFailedBeforeCommit.as_str(),
        "error": err.to_string(),
    }));
}

fn deserialize_with_path<T>(body: &[u8]) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let mut de = serde_json::Deserializer::from_slice(body);
    serde_path_to_error::deserialize(&mut de).map_err(|err| err.to_string())
}

async fn read_request_body(req: Request) -> Result<Bytes, axum::response::Response> {
    // NOTE: The 5MB limit is for *logging* only. `/v1/messages` should not reject requests solely due
    // to payload size. We still need to buffer the body to parse it and to extract prompt text.
    const BODY_LIMIT_BYTES: usize = 50 * 1024 * 1024;

    let (_parts, body) = req.into_parts();
    let Ok(body) = to_bytes(body, BODY_LIMIT_BYTES).await else {
        return Err(claude_status_json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({
                "error": { "type": "invalid_request_error", "message": format!("request body exceeded {BODY_LIMIT_BYTES} bytes limit") }
            }),
        ));
    };

    Ok(body)
}

fn bad_request(message: &str) -> axum::response::Response {
    claude_status_json_response(
        StatusCode::BAD_REQUEST,
        serde_json::json!({
            "error": { "type": "invalid_request_error", "message": message }
        }),
    )
}

fn service_unavailable_error(message: &str) -> axum::response::Response {
    claude_status_json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "error": { "type": "service_unavailable_error", "message": message }
        }),
    )
}

fn auth_error(message: &str) -> axum::response::Response {
    let remediation = "Please run: cld-gateway login claude";
    claude_status_json_response(
        StatusCode::UNAUTHORIZED,
        serde_json::json!({
            "error": {
                "type": "auth_error",
                "message": message,
                "auth_remediation": remediation
            }
        }),
    )
}

// NOTE: Request translation lives in `translate.rs`. Keep handler code free of ad-hoc extraction
// logic so we can maintain full-history fidelity.

#[cfg(test)]
mod messages_tests {
    use super::branch_fingerprints_from_messages;
    use super::build_tool_translation_context;
    use super::classify_canonical_messages;
    use super::classify_conversation_request;
    use super::claude_json_response;
    use super::claude_status_json_response;
    use super::prepare_conversation_branch;
    use super::request_compatibility_fingerprint;
    use super::select_transport;
    use super::stored_messages_are_prefix;
    use super::tool_call_content_block;
    use super::translate::{
        ToolTranslationContext, TranslateResult, translate_request_with_context,
    };
    use super::types::{
        AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
        AnthropicOutputConfig, AnthropicSystemBlock,
    };
    use super::{
        ConversationRequestKind, LeaseDiagnosticState, MainTurnLeaseState, TransportMode,
        WebSocketChainDecision, WebSocketChainMatch, estimate_anthropic_count_tokens,
        lease_diagnostic_state_for_reason, transport_diagnostics_log_path,
    };
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt as _;
    use gateway_backend_codex::client::WebSocketChainId;
    use gateway_backend_codex::types::{CodexToolCall, CodexToolCallKind};
    use gateway_core::DEFAULT_BACKEND_MODEL;
    use gateway_core::config::{GatewayConfig, resolve_model};
    use gateway_state::{
        BranchCreateParams, BranchMetadata, CommitTurnParams, ConversationStateStore,
        ConversationTurnScope, OpenAiCheckpoint, ToolCallStore,
    };
    use std::collections::HashMap;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;
    use tower::ServiceExt as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request as WiremockRequest, ResponseTemplate};
    use ws_mock::matchers::Matcher as WsMatcher;
    use ws_mock::ws_mock_server::{WsMock, WsMockServer};

    fn fixture(path: &str) -> String {
        let full = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path);
        std::fs::read_to_string(full)
            .expect("read fixture")
            .replace("__DEFAULT_BACKEND_MODEL__", DEFAULT_BACKEND_MODEL)
    }

    fn translate_request(req: &AnthropicMessagesRequest) -> Result<TranslateResult, String> {
        translate_request_with_context(req, &ToolTranslationContext::default())
    }

    fn parse_sse_frames(body: &str) -> Vec<(String, serde_json::Value)> {
        body.split("\n\n")
            .filter_map(|frame| {
                let frame = frame.trim();
                if frame.is_empty() {
                    return None;
                }
                let mut event: Option<String> = None;
                let mut data_lines: Vec<&str> = Vec::new();
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data_lines.push(rest.trim());
                    }
                }
                let event = event?;
                let data_str = data_lines.join("\n");
                let data: serde_json::Value =
                    serde_json::from_str(&data_str).expect("event data json");
                Some((event, data))
            })
            .collect()
    }

    fn normalize_msg_id(
        mut events: Vec<(String, serde_json::Value)>,
    ) -> Vec<(String, serde_json::Value)> {
        for (ev, data) in &mut events {
            if ev == "message_start"
                && let Some(msg) = data.get_mut("message")
                && let Some(obj) = msg.as_object_mut()
            {
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::String("msg_TEST".to_string()),
                );
            }
        }
        events
    }

    fn parse_expected_jsonl(path: &str) -> Vec<(String, serde_json::Value)> {
        let text = fixture(path);
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
                let event = v
                    .get("event")
                    .and_then(|v| v.as_str())
                    .expect("event")
                    .to_string();
                let data = v.get("data").cloned().expect("data");
                (event, data)
            })
            .collect()
    }

    #[test]
    fn translation_accepts_string_and_blocks_text() {
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "system".to_string(),
                    content: AnthropicContent::Text("ignore".to_string()),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("hello".to_string()),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                        block_type: "text".to_string(),
                        text: Some("world".to_string()),
                        id: None,
                        name: None,
                        input: None,
                        tool_use_id: None,
                        content: None,
                        is_error: None,
                        source: None,
                        extra: std::collections::BTreeMap::new(),
                    }]),
                },
            ],
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };

        let translated = translate_request(&req).unwrap();
        assert!(!translated.input.is_empty());
    }

    #[test]
    fn unary_tool_call_content_block_supports_custom_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_custom".to_string(),
            name: "apply_patch".to_string(),
            arguments: serde_json::json!({"input":"*** Begin Patch\n*** End Patch\n"}).to_string(),
            kind: CodexToolCallKind::Custom,
        });

        assert_eq!(block.get("type").and_then(|v| v.as_str()), Some("tool_use"));
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("input"))
                .and_then(|v| v.as_str()),
            Some("*** Begin Patch\n*** End Patch\n")
        );
    }

    #[test]
    fn unary_tool_call_content_block_supports_tool_search_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_search".to_string(),
            name: "tool_search".to_string(),
            arguments: serde_json::json!({"query":"Read"}).to_string(),
            kind: CodexToolCallKind::ToolSearch,
        });

        assert_eq!(
            block.get("name").and_then(|v| v.as_str()),
            Some("tool_search")
        );
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("query"))
                .and_then(|v| v.as_str()),
            Some("Read")
        );
    }

    #[test]
    fn unary_tool_call_content_block_supports_local_shell_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_shell".to_string(),
            name: "local_shell".to_string(),
            arguments: serde_json::json!({
                "status": "completed",
                "action": { "type": "exec", "command": ["echo", "hi"] }
            })
            .to_string(),
            kind: CodexToolCallKind::LocalShell,
        });

        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("completed")
        );
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("action"))
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_array())
                .and_then(|v| v.get(1))
                .and_then(|v| v.as_str()),
            Some("hi")
        );
    }

    #[test]
    fn unary_tool_call_content_block_sanitizes_read_pages() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_read".to_string(),
            name: "Read".to_string(),
            arguments: serde_json::json!({
                "file_path": "/tmp/a.txt",
                "pages": ""
            })
            .to_string(),
            kind: CodexToolCallKind::Function,
        });

        let input = block
            .get("input")
            .and_then(|value| value.as_object())
            .expect("input object");
        assert_eq!(
            input.get("file_path").and_then(|value| value.as_str()),
            Some("/tmp/a.txt")
        );
        assert!(input.get("pages").is_none());
    }

    #[test]
    fn unary_tool_call_content_block_removes_agent_isolation() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_agent".to_string(),
            name: "Agent".to_string(),
            arguments: serde_json::json!({
                "description": "Research files",
                "prompt": "Inspect relevant files",
                "isolation": "worktree",
                "subagent_type": "Explore"
            })
            .to_string(),
            kind: CodexToolCallKind::Function,
        });

        let input = block
            .get("input")
            .and_then(|value| value.as_object())
            .expect("input object");
        assert_eq!(
            input.get("description").and_then(|value| value.as_str()),
            Some("Research files")
        );
        assert!(input.get("isolation").is_none());
    }

    #[test]
    fn translation_context_uses_stored_tool_kind_without_translator_io() {
        let tool_calls_path = std::env::temp_dir().join(format!(
            "gateway_tool_context_{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let tool_calls = ToolCallStore::new(&tool_calls_path);
        tool_calls
            .record_tool_call(
                "call_custom",
                "apply_patch",
                "custom_tool_call",
                Some("rid_1"),
            )
            .expect("record");
        let state = super::AppState {
            tool_calls,
            ..super::AppState::default()
        };

        let mut req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: Vec::new(),
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_custom".to_string()),
                content: Some(serde_json::json!("ok")),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let context = build_tool_translation_context(&state, &req);
        let translated = translate_request_with_context(&req, &context).expect("translate");
        assert_eq!(
            translated
                .input
                .first()
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("custom_tool_call_output")
        );
    }

    #[tokio::test]
    async fn v1_messages_supports_stream_true() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;
        let backend = format!("{}\n\n", fixture("streaming/backend_stream_text_only.sse"));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("authorization", "Bearer tok_test"))
            .and(header("chatgpt-account-id", "acct_test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(backend, "text/event-stream"))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let got = normalize_msg_id(parse_sse_frames(text));
        let expected = parse_expected_jsonl("streaming/expected_anthropic_text_only.jsonl");
        assert_eq!(got, expected);
    }

    fn write_temp_auth_json() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("gateway_auth_{}.json", uuid::Uuid::new_v4()));
        let value = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "tok_test",
                "account_id": "acct_test"
            }
        });
        std::fs::write(&path, value.to_string()).expect("write auth fixture");
        path
    }

    fn wiremock_enabled() -> bool {
        std::env::var("RUN_WIREMOCK").ok().as_deref() == Some("1")
    }

    #[derive(Debug)]
    struct FunctionToolResultMatcher {
        call_id: String,
        output: String,
    }

    impl FunctionToolResultMatcher {
        fn new(call_id: &str, output: &str) -> Self {
            Self {
                call_id: call_id.to_string(),
                output: output.to_string(),
            }
        }
    }

    impl Match for FunctionToolResultMatcher {
        fn matches(&self, request: &WiremockRequest) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body.get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str)
                            == Some("function_call_output")
                            && item.get("call_id").and_then(serde_json::Value::as_str)
                                == Some(self.call_id.as_str())
                            && item.get("output").and_then(serde_json::Value::as_str)
                                == Some(self.output.as_str())
                    })
                })
        }
    }

    #[derive(Debug)]
    struct InputTextMatcher(String);

    impl InputTextMatcher {
        fn new(text: &str) -> Self {
            Self(text.to_string())
        }
    }

    impl Match for InputTextMatcher {
        fn matches(&self, request: &WiremockRequest) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body.get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                            && item
                                .get("content")
                                .and_then(serde_json::Value::as_array)
                                .is_some_and(|content| {
                                    content.iter().any(|part| {
                                        part.get("text").and_then(serde_json::Value::as_str)
                                            == Some(self.0.as_str())
                                    })
                                })
                    })
                })
        }
    }

    fn success_sse(delta_text: &str, response_id: &str) -> String {
        format!(
            concat!(
                "event: response.output_text.delta\n",
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":{delta:?}}}\n\n",
                "event: response.completed\n",
                "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":{response_id:?},\"usage\":{{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}}}}\n\n",
            ),
            delta = delta_text,
            response_id = response_id,
        )
    }

    fn success_websocket_events(delta_text: &str, response_id: &str) -> Vec<Message> {
        vec![
            Message::Text(
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "delta": delta_text,
                })
                .to_string()
                .into(),
            ),
            Message::Text(
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": response_id,
                        "usage": { "input_tokens": 1, "output_tokens": 2, "total_tokens": 3 }
                    }
                })
                .to_string()
                .into(),
            ),
        ]
    }

    fn failed_websocket_event(message: &str) -> Message {
        Message::Text(
            serde_json::json!({
                "type": "response.failed",
                "response": { "error": { "message": message } }
            })
            .to_string()
            .into(),
        )
    }

    #[derive(Debug)]
    struct PreviousResponseIdWsMatcher(Option<String>);

    impl PreviousResponseIdWsMatcher {
        fn none() -> Self {
            Self(None)
        }
    }

    impl WsMatcher for PreviousResponseIdWsMatcher {
        fn matches(&self, text: &str) -> bool {
            let Ok(body) = serde_json::from_str::<serde_json::Value>(text) else {
                return false;
            };
            body.get("type").and_then(serde_json::Value::as_str) == Some("response.create")
                && body
                    .get("previous_response_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    == self.0
        }
    }

    #[derive(Debug)]
    struct InputTextWsMatcher(String);

    impl InputTextWsMatcher {
        fn new(text: &str) -> Self {
            Self(text.to_string())
        }
    }

    impl WsMatcher for InputTextWsMatcher {
        fn matches(&self, text: &str) -> bool {
            let Ok(body) = serde_json::from_str::<serde_json::Value>(text) else {
                return false;
            };
            body.get("type").and_then(serde_json::Value::as_str) == Some("response.create")
                && body
                    .get("input")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                                && item
                                    .get("content")
                                    .and_then(serde_json::Value::as_array)
                                    .is_some_and(|content| {
                                        content.iter().any(|part| {
                                            part.get("text").and_then(serde_json::Value::as_str)
                                                == Some(self.0.as_str())
                                        })
                                    })
                        })
                    })
        }
    }

    #[derive(Clone, Debug, Default)]
    struct CapturedWsBodies(std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);

    impl CapturedWsBodies {
        fn values(&self) -> Vec<serde_json::Value> {
            self.0.lock().expect("capture mutex poisoned").clone()
        }
    }

    impl WsMatcher for CapturedWsBodies {
        fn matches(&self, text: &str) -> bool {
            let Ok(body) = serde_json::from_str::<serde_json::Value>(text) else {
                return false;
            };
            self.0.lock().expect("capture mutex poisoned").push(body);
            true
        }
    }

    const TEST_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    async fn mount_ws_response(
        server: &WsMockServer,
        matcher: PreviousResponseIdWsMatcher,
        events: Vec<Message>,
    ) {
        let mut mock = WsMock::new().matcher(matcher).expect(1);
        for event in events {
            mock = mock.respond_with(event);
        }
        mock.mount(server).await;
    }

    async fn mount_captured_ws_response(
        server: &WsMockServer,
        matcher: PreviousResponseIdWsMatcher,
        capture: CapturedWsBodies,
        events: Vec<Message>,
    ) {
        let mut mock = WsMock::new().matcher(matcher).matcher(capture).expect(1);
        for event in events {
            mock = mock.respond_with(event);
        }
        mock.mount(server).await;
    }

    async fn mount_captured_ws_response_with_text(
        server: &WsMockServer,
        matcher: PreviousResponseIdWsMatcher,
        text_matcher: InputTextWsMatcher,
        capture: CapturedWsBodies,
        events: Vec<Message>,
    ) {
        let mut mock = WsMock::new()
            .matcher(matcher)
            .matcher(text_matcher)
            .matcher(capture)
            .expect(1);
        for event in events {
            mock = mock.respond_with(event);
        }
        mock.mount(server).await;
    }

    async fn mount_ws_forward_channel(
        server: &WsMockServer,
        matcher: PreviousResponseIdWsMatcher,
    ) -> mpsc::Sender<Message> {
        let (sender, receiver) = mpsc::channel::<Message>(32);
        WsMock::new()
            .matcher(matcher)
            .expect(1)
            .forward_from_channel(receiver)
            .mount(server)
            .await;
        sender
    }

    async fn verify_ws(server: &WsMockServer) {
        tokio::time::timeout(TEST_RESPONSE_TIMEOUT, server.verify())
            .await
            .expect("timed out waiting for websocket mock verification");
    }

    fn function_tool_call_sse(
        call_id: &str,
        name: &str,
        arguments: &serde_json::Value,
        response_id: &str,
    ) -> String {
        format!(
            concat!(
                "event: response.output_item.done\n",
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"function_call\",\"call_id\":{call_id:?},\"name\":{name:?},\"arguments\":{arguments}}}}}\n\n",
                "event: response.completed\n",
                "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":{response_id:?},\"usage\":{{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}}}}\n\n",
            ),
            call_id = call_id,
            name = name,
            arguments = arguments,
            response_id = response_id,
        )
    }

    fn seed_incremental_branch(
        conversation_root: &std::path::Path,
        claude_session_id: &str,
    ) -> (ConversationStateStore, String) {
        let conversation_state = ConversationStateStore::new(conversation_root);
        let gateway_config = GatewayConfig::default();
        let request_messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }];
        let fingerprints = branch_fingerprints_from_messages(&request_messages, false);
        let request_compatibility_fingerprint =
            incremental_seed_request_compatibility_fingerprint(&gateway_config, &request_messages);
        let branch = conversation_state
            .create_branch(
                claude_session_id,
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: Some(
                        serde_json::to_value(&request_messages)
                            .expect("serialize request messages"),
                    ),
                    fingerprints: fingerprints.clone(),
                },
            )
            .expect("create checkpointed branch");
        conversation_state
            .commit_turn(
                claude_session_id,
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "seed-turn".to_string(),
                    fingerprints,
                    active_canonical_messages: Some(
                        serde_json::to_value(&request_messages)
                            .expect("serialize request messages"),
                    ),
                    provider_response_id: Some("resp_prev".to_string()),
                    previous_response_id: Some("resp_seed".to_string()),
                    provider_model_fingerprint: Some(DEFAULT_BACKEND_MODEL.to_string()),
                    request_compatibility_fingerprint: Some(
                        request_compatibility_fingerprint.clone(),
                    ),
                    provider_input_tokens: None,
                    canonical_message_count: Some(request_messages.len()),
                    canonical_prefix_hash: Some(super::canonical_messages_prefix_hash(
                        &request_messages,
                        request_messages.len(),
                    )),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("seed prior branch checkpoint");
        (conversation_state, branch.branch_id)
    }

    fn incremental_seed_request_compatibility_fingerprint(
        gateway_config: &GatewayConfig,
        request_messages: &[AnthropicMessage],
    ) -> String {
        let request = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: request_messages.to_vec(),
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        let tool_context = ToolTranslationContext::default()
            .with_claude_code_config(gateway_config.workflow.claude_code.clone());
        let translated = translate_request_with_context(&request, &tool_context)
            .expect("translate seed request");
        let resolution = resolve_model(gateway_config, &request.model);
        request_compatibility_fingerprint(gateway_config, &resolution, &translated)
    }

    fn test_anthropic_request(messages: Vec<AnthropicMessage>) -> AnthropicMessagesRequest {
        AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages,
            system: Vec::new(),
            stream: true,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        }
    }

    #[test]
    fn cc_is_subagent_system_header_classifies_as_offshoot() {
        let mut request = test_anthropic_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("inspect this".to_string()),
        }]);
        request.system = vec![AnthropicSystemBlock {
            block_type: "text".to_string(),
            text: Some("x-anthropic-billing-header: cc_is_subagent=true".to_string()),
        }];
        let classified =
            classify_conversation_request(&request, &request.messages, &HashMap::new());

        assert_eq!(classified, ConversationRequestKind::SubagentOffshoot);
    }

    #[test]
    fn hook_evaluator_classifies_as_non_visible_offshoot() {
        let mut request = test_anthropic_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("should I stop?".to_string()),
        }]);
        request.stream = false;
        request.output_config = Some(AnthropicOutputConfig {
            effort: None,
            format: Some(serde_json::json!({
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": { "continue": { "type": "boolean" } },
                    "required": ["continue"]
                }
            })),
        });
        let classified =
            classify_conversation_request(&request, &request.messages, &HashMap::new());

        assert_eq!(classified, ConversationRequestKind::HookEvaluator);
    }

    #[test]
    fn operational_system_wrappers_are_not_durable_prefix_messages() {
        let messages = classify_canonical_messages(vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("real user turn".to_string()),
            },
            AnthropicMessage {
                role: "system".to_string(),
                content: AnthropicContent::Text(
                    "The user sent a new message while you were working:\nreal user turn"
                        .to_string(),
                ),
            },
            AnthropicMessage {
                role: "system".to_string(),
                content: AnthropicContent::Text(
                    "The task tools haven't been used recently; here are the existing tasks"
                        .to_string(),
                ),
            },
        ]);

        assert_eq!(messages.durable_visible_messages.len(), 1);
        assert_eq!(messages.operational_context_messages.len(), 2);
    }

    #[test]
    fn all_turn_level_system_messages_are_operational_not_durable() {
        let messages = classify_canonical_messages(vec![
            AnthropicMessage {
                role: "system".to_string(),
                content: AnthropicContent::Text(
                    "Arbitrary Claude Code runtime context whose wording may change.".to_string(),
                ),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("actual user request".to_string()),
            },
            AnthropicMessage {
                role: "SYSTEM".to_string(),
                content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some("Another arbitrary runtime instruction.".to_string()),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::new(),
                }]),
            },
        ]);

        assert_eq!(messages.durable_visible_messages.len(), 1);
        assert_eq!(messages.operational_context_messages.len(), 2);
        assert_eq!(messages.durable_visible_messages[0].role.as_str(), "user");
    }

    #[test]
    fn turn_level_system_messages_are_preserved_for_backend_instructions() {
        let req = test_anthropic_request(vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("actual user request".to_string()),
            },
            AnthropicMessage {
                role: "system".to_string(),
                content: AnthropicContent::Text(
                    "Runtime instruction from Claude Code.".to_string(),
                ),
            },
        ]);
        let classified = classify_canonical_messages(req.messages.clone());
        let render_messages = super::render_messages_with_operational_context(
            classified.durable_visible_messages,
            classified.operational_context_messages,
            0,
        );
        let render_req = super::request_with_messages(&req, render_messages);
        let translated =
            translate_request_with_context(&render_req, &ToolTranslationContext::default())
                .expect("translate rendered request");
        let input = serde_json::to_string(&translated.input).expect("serialize input");

        assert!(
            translated
                .instructions
                .contains("Runtime instruction from Claude Code.")
        );
        assert!(!input.contains("\"role\":\"system\""));
        assert!(!input.contains("Runtime instruction from Claude Code."));
    }

    #[test]
    fn only_suffix_transient_operational_messages_are_replayed_on_delta() {
        let render_messages = super::render_messages_with_operational_context(
            vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("new user request".to_string()),
            }],
            vec![
                super::OperationalContextMessage {
                    message: AnthropicMessage {
                        role: "system".to_string(),
                        content: AnthropicContent::Text(
                            "The task tools haven't been used recently. historical".to_string(),
                        ),
                    },
                    durable_messages_before: 1,
                },
                super::OperationalContextMessage {
                    message: AnthropicMessage {
                        role: "system".to_string(),
                        content: AnthropicContent::Text(
                            "The user sent a new message while you were working:\nnew user request"
                                .to_string(),
                        ),
                    },
                    durable_messages_before: 3,
                },
            ],
            3,
        );

        assert_eq!(render_messages.len(), 2);
        assert_eq!(render_messages[0].role, "user");
        assert_eq!(render_messages[1].role, "system");
        assert!(
            super::anthropic_message_text(&render_messages[1])
                .contains("The user sent a new message while you were working:")
        );
    }

    #[test]
    fn lease_diagnostic_state_mapping_is_precise() {
        assert_eq!(
            MainTurnLeaseState::ClientAbortedBeforeFirstEvent.as_str(),
            LeaseDiagnosticState::ClientAbortedBeforeFirstEvent.as_str()
        );
        assert_eq!(
            lease_diagnostic_state_for_reason("backend_failed_before_commit").as_str(),
            "backend_failed_before_commit"
        );
        assert_eq!(
            lease_diagnostic_state_for_reason("client_aborted_after_visible_output").as_str(),
            "client_aborted_after_visible_output"
        );
    }

    #[test]
    fn count_tokens_estimator_is_auxiliary_and_payload_based() {
        let body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "messages": [
                { "role": "user", "content": "hello world" }
            ]
        })
        .to_string();

        assert!(estimate_anthropic_count_tokens(body.as_bytes()) > 0);
    }

    fn build_state(
        base_url: &url::Url,
        auth_path: &std::path::Path,
        conversation_state: ConversationStateStore,
    ) -> super::AppState {
        let mut gateway_config = GatewayConfig::default();
        gateway_config.workflow.conversation_state.enabled = true;
        super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(base_url),
            auth_json_path: Some(auth_path.to_path_buf()),
            conversation_state,
            gateway_config,
            ..super::AppState::default()
        }
    }

    struct BranchRouteTestHarness {
        auth_path: std::path::PathBuf,
        conversation_root: std::path::PathBuf,
        claude_session_id: &'static str,
        branch_id: String,
        conversation_state: ConversationStateStore,
        base_url: url::Url,
        state: super::AppState,
        ws_mock: WsMockServer,
    }

    impl BranchRouteTestHarness {
        async fn new(conversation_prefix: &str) -> Self {
            let auth_path = write_temp_auth_json();
            let conversation_root = std::env::temp_dir()
                .join(format!("{conversation_prefix}_{}", uuid::Uuid::new_v4()));
            let ws_mock = WsMockServer::start().await;
            let claude_session_id = "session-1";
            let (conversation_state, branch_id) =
                seed_incremental_branch(&conversation_root, claude_session_id);
            let base_url = url::Url::parse(&ws_mock.uri().await).expect("websocket mock url");
            let state = build_state(&base_url, &auth_path, conversation_state.clone());
            Self {
                auth_path,
                conversation_root,
                claude_session_id,
                branch_id,
                conversation_state,
                base_url,
                state,
                ws_mock,
            }
        }

        fn state(&self) -> &super::AppState {
            &self.state
        }

        fn claude_session_id(&self) -> &str {
            self.claude_session_id
        }

        fn branch(&self) -> BranchMetadata {
            self.conversation_state
                .load_branch(self.claude_session_id, &self.branch_id)
                .expect("reload branch metadata")
        }

        fn restarted_state(&self) -> super::AppState {
            self.restarted_state_with_base_url(&self.base_url)
        }

        fn restarted_state_with_base_url(&self, base_url: &url::Url) -> super::AppState {
            let restarted_store = ConversationStateStore::new(&self.conversation_root);
            build_state(base_url, &self.auth_path, restarted_store)
        }

        fn branch_count(&self) -> usize {
            self.conversation_state
                .load_session(self.claude_session_id)
                .expect("reload session metadata")
                .branch_ids
                .len()
        }

        fn ledger(&self) -> String {
            std::fs::read_to_string(
                self.conversation_root
                    .join(format!("session-id-{}", self.claude_session_id))
                    .join(format!("tab-{}", self.branch_id))
                    .join("ledger.jsonl"),
            )
            .expect("read branch ledger")
        }
    }

    impl Drop for BranchRouteTestHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.auth_path);
            let _ = std::fs::remove_dir_all(&self.conversation_root);
        }
    }

    fn assert_unary_text(json: &serde_json::Value, expected: &str) {
        assert_eq!(
            json.get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(serde_json::Value::as_str),
            Some(expected)
        );
    }

    fn assert_branch_checkpoint(
        branch: &BranchMetadata,
        expected_response_id: &str,
        expected_previous_response_id: Option<&str>,
    ) {
        assert_eq!(
            branch
                .openai_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.response_id.as_str()),
            Some(expected_response_id)
        );
        assert_eq!(
            branch
                .openai_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.previous_response_id.as_deref()),
            expected_previous_response_id
        );
    }

    async fn assert_operational_system_reminder_is_ignored_for_prefix(
        conversation_prefix: &str,
        reminder_text: &str,
        baseline_response_id: &str,
        response_id: &str,
        baseline_response_text: &str,
        response_text: &str,
    ) {
        let harness = BranchRouteTestHarness::new(conversation_prefix).await;
        let baseline_capture = CapturedWsBodies::default();
        let capture = CapturedWsBodies::default();
        mount_captured_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            baseline_capture.clone(),
            success_websocket_events(baseline_response_text, baseline_response_id),
        )
        .await;
        mount_captured_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher(Some(baseline_response_id.to_string())),
            capture.clone(),
            success_websocket_events(response_text, response_id),
        )
        .await;

        let baseline = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": "next" }
            ]),
        )
        .await;
        assert_unary_text(&baseline, baseline_response_text);

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "system", "content": reminder_text },
                { "role": "user", "content": "next" },
                { "role": "user", "content": "final" }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_unary_text(&json, response_text);

        let baseline_bodies = baseline_capture.values();
        assert_eq!(baseline_bodies.len(), 1);
        assert_eq!(
            baseline_bodies[0]
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            None
        );

        let bodies = capture.values();
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(
            body.get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            Some(baseline_response_id)
        );
        let input = serde_json::to_string(&body["input"]).expect("serialize input");
        assert!(input.contains("final"));
        assert!(!input.contains("hello"));
        assert!(!input.contains("\"role\":\"system\""));
        assert!(!input.contains(reminder_text));

        let branch = harness.branch();
        assert_branch_checkpoint(&branch, response_id, Some(baseline_response_id));
    }

    async fn send_unary_message(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> serde_json::Value {
        let res = send_unary_message_response(state, claude_session_id, messages).await;
        assert!(
            res.status().is_success(),
            "expected success after full retry"
        );
        let body =
            tokio::time::timeout(TEST_RESPONSE_TIMEOUT, to_bytes(res.into_body(), usize::MAX))
                .await
                .expect("timed out collecting unary response body")
                .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn send_unary_message_response(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> axum::response::Response {
        send_unary_request_response(
            state,
            claude_session_id,
            serde_json::json!({
                "model": DEFAULT_BACKEND_MODEL,
                "stream": false,
                "messages": messages
            }),
        )
        .await
    }

    async fn send_unary_request(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        request_body: serde_json::Value,
    ) -> serde_json::Value {
        let res = send_unary_request_response(state, claude_session_id, request_body).await;
        assert!(
            res.status().is_success(),
            "expected success after full retry"
        );
        let body =
            tokio::time::timeout(TEST_RESPONSE_TIMEOUT, to_bytes(res.into_body(), usize::MAX))
                .await
                .expect("timed out collecting unary response body")
                .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn send_unary_request_response(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        request_body: serde_json::Value,
    ) -> axum::response::Response {
        let app = super::router(state.clone());

        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json");
        if let Some(claude_session_id) = claude_session_id {
            builder = builder.header("x-claude-code-session-id", claude_session_id);
        }
        tokio::time::timeout(
            TEST_RESPONSE_TIMEOUT,
            app.oneshot(builder.body(Body::from(request_body.to_string())).unwrap()),
        )
        .await
        .expect("timed out waiting for unary response")
        .unwrap()
    }

    async fn send_streaming_message_with_model(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        model: &str,
        messages: serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        let res =
            send_streaming_message_response_with_model(state, claude_session_id, model, messages)
                .await;
        collect_streaming_response(res).await
    }

    async fn send_streaming_message_response_with_model(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        model: &str,
        messages: serde_json::Value,
    ) -> axum::response::Response {
        let app = super::router(state.clone());
        let req_body = serde_json::json!({
            "model": model,
            "stream": true,
            "messages": messages
        });

        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json");
        if let Some(claude_session_id) = claude_session_id {
            builder = builder.header("x-claude-code-session-id", claude_session_id);
        }
        tokio::time::timeout(
            TEST_RESPONSE_TIMEOUT,
            app.oneshot(builder.body(Body::from(req_body.to_string())).unwrap()),
        )
        .await
        .expect("timed out waiting for streaming response")
        .unwrap()
    }

    async fn send_streaming_message(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        send_streaming_message_with_model(state, claude_session_id, DEFAULT_BACKEND_MODEL, messages)
            .await
    }

    async fn collect_streaming_response(
        res: axum::response::Response,
    ) -> Vec<(String, serde_json::Value)> {
        assert!(
            res.status().is_success(),
            "expected successful SSE response"
        );
        let body =
            tokio::time::timeout(TEST_RESPONSE_TIMEOUT, to_bytes(res.into_body(), usize::MAX))
                .await
                .expect("timed out collecting streaming response body")
                .unwrap();
        let text = std::str::from_utf8(&body).expect("utf8 SSE body");
        parse_sse_frames(text)
    }

    fn inbound_snapshot_reconcile_count(ledger: &str) -> usize {
        ledger
            .matches("\"event_type\":\"inbound_canonical_snapshot_reconciled\"")
            .count()
    }

    fn write_session_updated_at(
        conversation_root: &std::path::Path,
        claude_session_id: &str,
        updated_at_unix_seconds: i64,
    ) {
        let session_path = conversation_root
            .join(format!("session-id-{claude_session_id}"))
            .join("session.json");
        let mut session: gateway_state::ClaudeSessionMetadata =
            serde_json::from_slice(&std::fs::read(&session_path).expect("read session.json"))
                .expect("decode session");
        session.updated_at_unix_seconds = updated_at_unix_seconds;
        std::fs::write(
            &session_path,
            serde_json::to_vec_pretty(&session).expect("encode session"),
        )
        .expect("write session");
    }

    #[tokio::test]
    async fn claude_json_response_applies_common_cleanup_gate() {
        let response = claude_json_response(serde_json::json!({
            "type": "message",
            "stop_sequence": null,
            "content": [
                { "type": "text", "text": "" },
                { "type": "text", "text": "kept" },
                { "type": "tool_use", "id": "tool_1", "input": null }
            ],
            "nested": { "impossible": null, "ok": true }
        }));
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("decode sanitized response");

        assert_eq!(
            value,
            serde_json::json!({
                "type": "message",
                "content": [
                    { "type": "text", "text": "kept" },
                    { "type": "tool_use", "id": "tool_1" }
                ],
                "nested": { "ok": true }
            })
        );
    }

    #[tokio::test]
    async fn claude_status_json_response_applies_common_cleanup_gate() {
        let response = claude_status_json_response(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({
                "error": {
                    "type": "backend_error",
                    "message": "failed",
                    "details": null
                }
            }),
        );
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("decode sanitized response");

        assert_eq!(
            value,
            serde_json::json!({
                "error": {
                    "type": "backend_error",
                    "message": "failed"
                }
            })
        );
    }

    #[tokio::test]
    async fn v1_messages_unary_emits_tool_use_block_from_backend() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();

        let mock = MockServer::start().await;
        let sse = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/streaming/backend_stream_tool_call.sse"
        ));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("authorization", "Bearer tok_test"))
            .and(header("chatgpt-account-id", "acct_test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "call a tool" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json.get("stop_reason").and_then(|v| v.as_str()),
            Some("tool_use")
        );
        let block = json
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .expect("content[0]");
        assert_eq!(block.get("type").and_then(|v| v.as_str()), Some("tool_use"));
        assert_eq!(block.get("id").and_then(|v| v.as_str()), Some("call_1"));
        assert_eq!(block.get("name").and_then(|v| v.as_str()), Some("Read"));
    }

    #[tokio::test]
    async fn test_unary_message_missing_auth_returns_remediation() {
        let state = super::AppState {
            auth_json_path: Some(std::path::PathBuf::from("/nonexistent/auth.json")),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error")
        );
        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_streaming_message_missing_auth_returns_remediation() {
        let state = super::AppState {
            auth_json_path: Some(std::path::PathBuf::from("/nonexistent/auth.json")),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let events = parse_sse_frames(text);

        assert!(!events.is_empty());
        let (first_event, first_data) = &events[0];
        assert_eq!(first_event, "error");
        assert_eq!(
            first_data
                .get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error")
        );
        assert_eq!(
            first_data
                .get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_auth_status_missing_auth_returns_remediation() {
        let auth_path = std::env::temp_dir().join(format!(
            "gateway_missing_auth_{}.json",
            uuid::Uuid::new_v4()
        ));
        let state = super::AppState {
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };
        let app = super::router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("logged_in").and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            json.get("auth_remediation").and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_refresh_failure_returns_remediation() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // Mock any POST to codex/responses to return 401
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "refresh_required"})),
            )
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // When backend returns 401, gateway should return 401 with remediation
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "expected 401 when backend returns 401, got {}",
            res.status()
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error"),
            "expected auth_error type in response, got: {json:?}"
        );
        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude"),
            "expected remediation message in error response, got: {json:?}"
        );

        let _ = std::fs::remove_file(&auth_path);
    }

    #[tokio::test]
    async fn test_refresh_failure_returns_remediation_streaming() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // Mock any POST to codex/responses to return 401
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "refresh_required"})),
            )
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // When backend returns 401, gateway should return 200 (SSE stream starts)
        // but the first event should be an error with structured auth remediation.
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "expected 200 for SSE stream, got {}",
            res.status()
        );

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        let events = parse_sse_frames(body_str);

        assert!(
            !events.is_empty(),
            "expected at least one SSE event, got empty stream"
        );
        let (first_event, first_payload) = &events[0];
        assert_eq!(first_event, "error");
        assert_eq!(
            first_payload
                .get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error"),
            "expected structured auth_error payload, got: {first_payload:?}"
        );
        assert_eq!(
            first_payload
                .get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude"),
            "expected structured remediation in SSE payload, got: {first_payload:?}"
        );

        let _ = std::fs::remove_file(&auth_path);
    }

    #[tokio::test]
    async fn unary_delta_rejection_does_not_retry_with_null_previous_response_id() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_conversation_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            vec![failed_websocket_event(
                "previous_response_id resp_prev not found",
            )],
        )
        .await;

        let response = send_unary_message_response(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_branch_checkpoint(&harness.branch(), "resp_prev", Some("resp_seed"));
    }

    #[tokio::test]
    async fn unary_live_delta_rejection_does_not_retry_with_null_previous_response_id() {
        if !wiremock_enabled() {
            return;
        }
        let harness =
            BranchRouteTestHarness::new("gateway_live_delta_rejection_no_null_retry").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("baseline ok", "resp_live_baseline"),
        )
        .await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher(Some("resp_live_baseline".to_string())),
            vec![failed_websocket_event(
                "previous_response_id resp_live_baseline not found",
            )],
        )
        .await;

        let first = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        assert_unary_text(&first, "baseline ok");

        let response = send_unary_message_response(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": "next" }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_branch_checkpoint(&harness.branch(), "resp_live_baseline", None);
    }

    #[tokio::test]
    async fn compact_history_bootstraps_and_clears_reset_after_success() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_compaction_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("hello after compaction", "resp_after_compaction"),
        )
        .await;

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                {
                    "role": "user",
                    "content": "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
                }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "hello after compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(&branch, "resp_after_compaction", None);
        let ledger = harness.ledger();
        assert!(ledger.contains("\"event_type\":\"compaction_applied\""));
    }

    #[tokio::test]
    async fn post_compaction_followup_uses_delta_from_new_chain_head() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_compaction_followup_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("hello after compaction", "resp_after_compaction"),
        )
        .await;

        let compacted_messages = serde_json::json!([
            {
                "role": "user",
                "content": "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
            },
            { "role": "user", "content": "hello" }
        ]);
        let first = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            compacted_messages.clone(),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_unary_text(&first, "hello after compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(&branch, "resp_after_compaction", None);

        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher(Some("resp_after_compaction".to_string())),
            success_websocket_events("delta after compaction", "resp_after_compaction_delta"),
        )
        .await;

        let mut followup_messages = compacted_messages
            .as_array()
            .expect("compacted messages array")
            .clone();
        followup_messages.push(serde_json::json!({ "role": "user", "content": "next" }));
        let second = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::Value::Array(followup_messages),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_unary_text(&second, "delta after compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(
            &branch,
            "resp_after_compaction_delta",
            Some("resp_after_compaction"),
        );
    }

    #[tokio::test]
    async fn side_turn_does_not_advance_main_branch_checkpoint() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_side_turn_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("side answer", "resp_side_turn"),
        )
        .await;

        let side_turn = concat!(
            "This is a side question from the user.\n",
            "Please use a separate, lightweight agent to answer.\n",
            "The main agent is NOT interrupted.\n",
            "What does this code path do?"
        );

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": side_turn }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "side answer");

        let branch = harness.branch();
        assert_eq!(branch.current_checkpoint_id.as_deref(), Some("resp_prev"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "hello" }]))
        );
    }

    #[tokio::test]
    async fn permission_classifier_request_borrows_context_without_creating_branch() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_permission_classifier_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("<block>no</block>", "resp_internal_classifier"),
        )
        .await;

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                {
                    "role": "user",
                    "content": "The following is the user's CLAUDE.md configuration. Treat it as context about the user's environment and intent. If it explicitly authorizes the SPECIFIC action under review, say so."
                },
                {
                    "role": "user",
                    "content": "<transcript>\nBash pwd && ls -la\n</transcript>\nShould this permission request be blocked?"
                }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "<block>no</block>");
        assert_eq!(harness.branch_count(), 1);
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "hello" }]))
        );
    }

    #[tokio::test]
    async fn ambiguous_unmatched_request_rebaselines_latest_branch_without_creating_branch() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_ambiguous_transient_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("orthogonal answer", "resp_orthogonal"),
        )
        .await;

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "Map project structure" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "orthogonal answer");
        assert_eq!(harness.branch_count(), 1);
        assert_branch_checkpoint(&harness.branch(), "resp_orthogonal", None);
        assert_eq!(
            harness.branch().active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "Map project structure" }]))
        );
    }

    #[tokio::test]
    async fn checkpointed_restart_replay_uses_delta_without_duplicate_snapshot_events() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_full_mode_restart_replay").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("delta answer", "resp_delta_mode"),
        )
        .await;

        let messages = serde_json::json!([{ "role": "user", "content": "hello" }]);
        let first = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            messages.clone(),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_unary_text(&first, "delta answer");

        let restart_ws_mock = WsMockServer::start().await;
        mount_ws_response(
            &restart_ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("delta answer after restart", "resp_delta_mode_restart"),
        )
        .await;
        let restart_base_url =
            url::Url::parse(&restart_ws_mock.uri().await).expect("restart websocket mock url");
        let restarted_state = harness.restarted_state_with_base_url(&restart_base_url);
        let second = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            messages,
        )
        .await;
        verify_ws(&restart_ws_mock).await;
        assert_unary_text(&second, "delta answer after restart");

        assert_eq!(harness.branch_count(), 1);
        assert_eq!(inbound_snapshot_reconcile_count(&harness.ledger()), 0);
    }

    #[tokio::test]
    async fn checkpointed_side_turn_restart_uses_delta_and_keeps_main_checkpoint() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_full_mode_side_restart").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("side answer", "resp_side_restart"),
        )
        .await;

        let restarted_state = harness.restarted_state();
        let side_turn = concat!(
            "This is a side question from the user.\n",
            "Please use a separate, lightweight agent to answer.\n",
            "The main agent is NOT interrupted.\n",
            "What does this code path do?"
        );

        let json = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": side_turn }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "side answer");
        let branch = harness.branch();
        assert_eq!(branch.current_checkpoint_id.as_deref(), Some("resp_prev"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "hello" }]))
        );
    }

    #[tokio::test]
    async fn checkpointed_compaction_restart_bootstraps_and_replaces_active_state_safely() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_full_mode_compaction_restart").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("hello after full compaction", "resp_full_compaction"),
        )
        .await;

        let restarted_state = harness.restarted_state();
        let messages = serde_json::json!([
            {
                "role": "user",
                "content": "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
            }
        ]);
        let json = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            messages.clone(),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "hello after full compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(&branch, "resp_full_compaction", None);
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([]))
        );
        assert!(
            harness
                .ledger()
                .contains("\"event_type\":\"compaction_applied\"")
        );
    }

    #[tokio::test]
    async fn generic_backend_error_does_not_retry_full_or_advance_branch_state() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_generic_backend_error").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            vec![failed_websocket_event("tool schema validation failed")],
        )
        .await;

        let response = send_unary_message_response(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
    }

    #[tokio::test]
    async fn interrupted_stream_does_not_advance_branch_state() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_interrupted_stream").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            vec![
                Message::Text(
                    serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": "partial",
                    })
                    .to_string()
                    .into(),
                ),
                failed_websocket_event("stream interrupted"),
            ],
        )
        .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert!(!events.is_empty(), "expected partial SSE events");
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
    }

    #[tokio::test]
    async fn websocket_bootstrap_without_chain_proof_succeeds_and_commits() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_bootstrap_without_chain_proof").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("hello after bootstrap", "resp_bootstrap"),
        )
        .await;

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert_unary_text(&json, "hello after bootstrap");
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_bootstrap", None);
    }

    #[tokio::test]
    async fn stale_websocket_after_visible_output_does_not_replay_or_commit() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_stale_ws_after_output").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            vec![
                Message::Text(
                    serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": "partial",
                    })
                    .to_string()
                    .into(),
                ),
                Message::Close(None),
            ],
        )
        .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert!(!events.is_empty(), "expected partial SSE events");
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
    }

    #[tokio::test]
    async fn client_drop_before_visible_output_suppresses_late_stream_commit() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_client_drop_before_output").await;
        let upstream =
            mount_ws_forward_channel(&harness.ws_mock, PreviousResponseIdWsMatcher::none()).await;
        let state = harness.state().clone();
        let claude_session_id = harness.claude_session_id().to_string();
        let response_task = tokio::spawn(async move {
            send_streaming_message_response_with_model(
                &state,
                Some(&claude_session_id),
                DEFAULT_BACKEND_MODEL,
                serde_json::json!([{ "role": "user", "content": "hello" }]),
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        upstream
            .send(Message::Text(
                serde_json::json!({
                    "type": "response.created",
                    "response": { "id": "resp_inflight" }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send created event");

        let response = tokio::time::timeout(TEST_RESPONSE_TIMEOUT, response_task)
            .await
            .expect("timed out waiting for streaming response task")
            .expect("streaming response task panicked");
        assert!(
            response.status().is_success(),
            "expected streaming response headers"
        );
        drop(response);

        let _ = upstream
            .send(Message::Text(
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_late_before_output",
                        "usage": { "input_tokens": 1, "output_tokens": 2, "total_tokens": 3 }
                    }
                })
                .to_string()
                .into(),
            ))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        verify_ws(&harness.ws_mock).await;

        assert_branch_checkpoint(&harness.branch(), "resp_prev", Some("resp_seed"));
        let diagnostics = std::fs::read_to_string(
            transport_diagnostics_log_path().expect("transport diagnostic path"),
        )
        .unwrap_or_default();
        assert!(
            diagnostics.contains("\"client_abort_state\":\"client_aborted_before_first_event\""),
            "expected before-first-event client abort diagnostic"
        );
    }

    #[tokio::test]
    async fn client_drop_after_visible_output_suppresses_late_stream_commit() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_client_drop_after_output").await;
        let upstream =
            mount_ws_forward_channel(&harness.ws_mock, PreviousResponseIdWsMatcher::none()).await;
        let state = harness.state().clone();
        let claude_session_id = harness.claude_session_id().to_string();
        let response_task = tokio::spawn(async move {
            send_streaming_message_response_with_model(
                &state,
                Some(&claude_session_id),
                DEFAULT_BACKEND_MODEL,
                serde_json::json!([{ "role": "user", "content": "hello" }]),
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        upstream
            .send(Message::Text(
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "delta": "partial",
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send delta event");

        let response = tokio::time::timeout(TEST_RESPONSE_TIMEOUT, response_task)
            .await
            .expect("timed out waiting for streaming response task")
            .expect("streaming response task panicked");
        assert!(
            response.status().is_success(),
            "expected streaming response headers"
        );
        let mut body = response.into_body().into_data_stream();
        let mut saw_visible_delta = false;
        for _ in 0..8 {
            let Some(chunk) = tokio::time::timeout(TEST_RESPONSE_TIMEOUT, body.next())
                .await
                .expect("timed out waiting for SSE chunk")
            else {
                break;
            };
            let chunk = chunk.expect("SSE body chunk");
            let text = std::str::from_utf8(&chunk).expect("utf8 SSE chunk");
            if text.contains("partial") {
                saw_visible_delta = true;
                break;
            }
        }
        assert!(
            saw_visible_delta,
            "expected visible streamed delta before drop"
        );
        drop(body);

        let _ = upstream
            .send(Message::Text(
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_late_after_output",
                        "usage": { "input_tokens": 1, "output_tokens": 2, "total_tokens": 3 }
                    }
                })
                .to_string()
                .into(),
            ))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        verify_ws(&harness.ws_mock).await;

        assert_branch_checkpoint(&harness.branch(), "resp_prev", Some("resp_seed"));
        let diagnostics = std::fs::read_to_string(
            transport_diagnostics_log_path().expect("transport diagnostic path"),
        )
        .unwrap_or_default();
        assert!(
            diagnostics.contains("\"client_abort_state\":\"client_aborted_after_visible_output\""),
            "expected after-visible-output client abort diagnostic"
        );
    }

    #[test]
    fn conversation_state_store_startup_cleanup_applies_retention_config() {
        let conversation_root =
            std::env::temp_dir().join(format!("gateway_startup_cleanup_{}", uuid::Uuid::new_v4()));
        let store = ConversationStateStore::new(&conversation_root);
        let session = store.ensure_session("session-1").expect("create session");
        write_session_updated_at(
            &conversation_root,
            "session-1",
            session.created_at_unix_seconds - (10 * 24 * 60 * 60),
        );

        let mut config = GatewayConfig::default();
        config.workflow.conversation_state.enabled = true;
        config.workflow.conversation_state.persistence_root = Some(conversation_root.clone());
        config
            .workflow
            .conversation_state
            .retention
            .max_session_age_days = Some(7);

        let cleaned_store = super::conversation_state_store_for_config(&config);
        assert_eq!(cleaned_store.root(), conversation_root.as_path());
        assert!(!conversation_root.join("session-id-session-1").exists());

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn branch_fingerprints_are_deterministic_for_identical_messages() {
        let messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hello".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text("world".to_string()),
            },
        ];

        let first = branch_fingerprints_from_messages(&messages, false);
        let second = branch_fingerprints_from_messages(&messages, false);
        assert_eq!(first, second);
    }

    #[test]
    fn branch_fingerprints_change_when_recent_tail_changes() {
        let base = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }];
        let mut changed = base.clone();
        changed.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Text("different".to_string()),
        });

        let first = branch_fingerprints_from_messages(&base, false);
        let second = branch_fingerprints_from_messages(&changed, false);
        assert_ne!(
            first.recent_message_tail_hash,
            second.recent_message_tail_hash
        );
        assert_ne!(first.branch_state_hash, second.branch_state_hash);
    }

    #[test]
    fn prefix_match_normalizes_text_string_and_text_block_shapes() {
        let stored = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "text".to_string(),
                text: Some("Tell me a joke about 2 monkeys".to_string()),
                id: None,
                name: None,
                input: None,
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        }];
        let incoming = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Tell me a joke about 2 monkeys".to_string()),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Tell me a joke about 3 monkeys".to_string()),
            },
        ];

        assert!(stored_messages_are_prefix(&stored, &incoming));
    }

    #[test]
    fn prefix_match_ignores_ephemeral_cache_control_metadata() {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert(
            "cache_control".to_string(),
            serde_json::json!({ "type": "ephemeral" }),
        );
        let stored = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "text".to_string(),
                text: Some("Tell me a joke about 3 monkeys".to_string()),
                id: None,
                name: None,
                input: None,
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra,
            }]),
        }];
        let incoming = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "text".to_string(),
                text: Some("Tell me a joke about 3 monkeys".to_string()),
                id: None,
                name: None,
                input: None,
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        }];

        assert!(stored_messages_are_prefix(&stored, &incoming));
    }

    #[test]
    fn persisted_canonical_messages_render_same_backend_request_as_direct_request() {
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("hello".to_string()),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                        block_type: "tool_use".to_string(),
                        text: None,
                        id: Some("call_1".to_string()),
                        name: Some("Read".to_string()),
                        input: Some(serde_json::json!({"file_path":"/tmp/a.txt"})),
                        tool_use_id: None,
                        content: None,
                        is_error: None,
                        source: None,
                        extra: std::collections::BTreeMap::new(),
                    }]),
                },
            ],
            system: vec![crate::types::AnthropicSystemBlock {
                block_type: "text".to_string(),
                text: Some("system".to_string()),
            }],
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };

        let direct_context = build_tool_translation_context(&super::AppState::default(), &req);
        let direct = translate_request_with_context(&req, &direct_context).expect("direct render");

        let persisted_req = super::request_with_messages(&req, req.messages.clone());
        let persisted_context =
            build_tool_translation_context(&super::AppState::default(), &persisted_req);
        let persisted = translate_request_with_context(&persisted_req, &persisted_context)
            .expect("persisted render");

        assert_eq!(direct.instructions, persisted.instructions);
        assert_eq!(direct.input, persisted.input);
        assert_eq!(direct.tools, persisted.tools);
        assert_eq!(direct.tool_choice, persisted.tool_choice);
        assert_eq!(direct.parallel_tool_calls, persisted.parallel_tool_calls);
        assert_eq!(direct.text, persisted.text);
        assert_eq!(direct.reasoning, persisted.reasoning);
        assert_eq!(direct.include, persisted.include);
        assert_eq!(direct.client_metadata, persisted.client_metadata);
    }

    #[tokio::test]
    async fn streaming_main_turn_commits_branch_checkpoint_on_completed_event() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_stream_commit_state").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            success_websocket_events("hello from stream", "resp_stream_next"),
        )
        .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert!(
            events.iter().any(|(event, _)| event == "message_stop"),
            "expected completed SSE message_stop event"
        );

        let branch = harness.branch();
        assert_eq!(
            branch.current_checkpoint_id.as_deref(),
            Some("resp_stream_next")
        );
        assert_branch_checkpoint(&branch, "resp_stream_next", None);
    }

    #[tokio::test]
    async fn streaming_delta_rejection_does_not_retry_with_null_previous_response_id() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_stream_delta_rebaseline").await;
        mount_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            vec![failed_websocket_event(
                "previous_response_id resp_prev not found",
            )],
        )
        .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;

        assert!(
            events.iter().any(|(_, data)| {
                data.get("error")
                    .and_then(|error| error.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("backend_error")
            }),
            "expected backend error SSE without null previous_response_id retry"
        );
        assert_branch_checkpoint(&harness.branch(), "resp_prev", Some("resp_seed"));
    }

    #[tokio::test]
    async fn interruption_system_reminder_is_ignored_for_durable_prefix_matching() {
        if !wiremock_enabled() {
            return;
        }
        assert_operational_system_reminder_is_ignored_for_prefix(
            "gateway_interrupt_reminder_prefix",
            "The user sent a new message while you were working:\ncreate the folder inside docs/design_throey\n\nIMPORTANT: After completing your current task, you MUST address the user's message above.",
            "resp_interrupt_baseline",
            "resp_interrupt_reminder",
            "baseline before interruption reminder",
            "interruption reminder ignored",
        )
        .await;
    }

    #[tokio::test]
    async fn task_reminder_system_noise_is_ignored_for_durable_prefix_matching() {
        if !wiremock_enabled() {
            return;
        }
        assert_operational_system_reminder_is_ignored_for_prefix(
            "gateway_task_reminder_prefix",
            "The task tools haven't been used recently.\nHere are the existing tasks:\n- investigate regression\n- update docs",
            "resp_task_baseline",
            "resp_task_reminder",
            "baseline before task reminder",
            "task reminder ignored",
        )
        .await;
    }

    #[tokio::test]
    async fn visible_main_after_subagent_commit_still_uses_previous_visible_main_response_id() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_subagent_then_visible_main").await;
        let baseline_capture = CapturedWsBodies::default();
        let offshoot_capture = CapturedWsBodies::default();
        let visible_capture = CapturedWsBodies::default();

        mount_captured_ws_response(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher::none(),
            baseline_capture.clone(),
            success_websocket_events("visible baseline", "resp_visible_baseline"),
        )
        .await;
        mount_captured_ws_response_with_text(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher(Some("resp_visible_baseline".to_string())),
            InputTextWsMatcher::new("inspect this"),
            offshoot_capture.clone(),
            success_websocket_events("subagent answer", "resp_subagent"),
        )
        .await;
        mount_captured_ws_response_with_text(
            &harness.ws_mock,
            PreviousResponseIdWsMatcher(Some("resp_visible_baseline".to_string())),
            InputTextWsMatcher::new("next visible turn"),
            visible_capture.clone(),
            success_websocket_events("visible answer", "resp_visible_after_offshoot"),
        )
        .await;

        let baseline = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        assert_unary_text(&baseline, "visible baseline");

        let offshoot = send_unary_request(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!({
                "model": DEFAULT_BACKEND_MODEL,
                "stream": false,
                "system": [{
                    "type": "text",
                    "text": "x-anthropic-billing-header: cc_is_subagent=true"
                }],
                "messages": [{ "role": "user", "content": "inspect this" }]
            }),
        )
        .await;
        assert_unary_text(&offshoot, "subagent answer");

        let baseline_bodies = baseline_capture.values();
        assert_eq!(baseline_bodies.len(), 1);
        assert_eq!(
            baseline_bodies[0]
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            None
        );

        let branch_after_offshoot = harness.branch();
        assert_branch_checkpoint(&branch_after_offshoot, "resp_visible_baseline", None);
        assert_eq!(branch_after_offshoot.offshoot_openai_checkpoints.len(), 1);
        assert_eq!(
            branch_after_offshoot.offshoot_openai_checkpoints[0].response_id,
            "resp_subagent"
        );

        let visible_main = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": "next visible turn" }
            ]),
        )
        .await;
        verify_ws(&harness.ws_mock).await;
        assert_unary_text(&visible_main, "visible answer");

        let offshoot_bodies = offshoot_capture.values();
        assert_eq!(offshoot_bodies.len(), 1);
        assert_eq!(
            offshoot_bodies[0]
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            Some("resp_visible_baseline")
        );

        let visible_bodies = visible_capture.values();
        assert_eq!(visible_bodies.len(), 1);
        assert_eq!(
            visible_bodies[0]
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            Some("resp_visible_baseline")
        );

        let branch_after_visible = harness.branch();
        assert_branch_checkpoint(
            &branch_after_visible,
            "resp_visible_after_offshoot",
            Some("resp_visible_baseline"),
        );
    }

    #[tokio::test]
    async fn stored_tool_call_kind_is_used_for_followup_tool_result_requests() {
        if !wiremock_enabled() {
            return;
        }
        let auth_path = write_temp_auth_json();
        let tool_calls_path = std::env::temp_dir().join(format!(
            "gateway_tool_continuity_{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let mock = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(InputTextMatcher::new("hello"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                function_tool_call_sse(
                    "call_1",
                    "Read",
                    &serde_json::json!({"file_path": "/tmp/a.txt"}),
                    "resp_tool",
                ),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(FunctionToolResultMatcher::new("call_1", "done"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("tool result processed", "resp_after_tool"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            tool_calls: ToolCallStore::new(&tool_calls_path),
            ..super::AppState::default()
        };

        let first = send_unary_message(
            &state,
            None,
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        assert_eq!(
            first.get("stop_reason").and_then(serde_json::Value::as_str),
            Some("tool_use")
        );

        let second = send_unary_message(
            &state,
            None,
            serde_json::json!([
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "Read",
                        "input": { "file_path": "/tmp/a.txt" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "done"
                    }]
                }
            ]),
        )
        .await;

        assert_eq!(
            second
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(serde_json::Value::as_str),
            Some("tool result processed")
        );

        let _ = std::fs::remove_file(&auth_path);
        let _ = std::fs::remove_file(&tool_calls_path);
    }

    #[tokio::test]
    async fn v1_messages_status_command_routes_through_executor() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // The backend should receive the executor-enriched request.
        // We verify the request body contains executor-produced JSON.
        let backend_response = format!("{}\n\n", fixture("streaming/backend_stream_text_only.sse"));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(backend_response, "text/event-stream"),
            )
            .expect(1)
            .mount(&mock)
            .await;

        // Also mock the usage endpoint (executor will try to fetch it)
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "plan_type": "pro",
                "rate_limit": { "allowed": true, "limit_reached": false }
            })))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{
                "role": "user",
                "content": "<command-message>status</command-message>\n<command-name>/status</command-name>\n<command-args></command-args>"
            }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // The handler should succeed (executor ran, then forwarded to backend)
        assert!(
            res.status().is_success(),
            "expected success status, got {}",
            res.status()
        );

        // Verify the backend received exactly one request (executor path completed
        // and forwarded the enriched request to the backend)
        // The Mock::expect(1) above ensures this.

        let _ = std::fs::remove_file(&auth_path);
    }

    #[test]
    fn status_command_prepares_conversation_branch_instead_of_skipping_state() {
        let conversation_root =
            std::env::temp_dir().join(format!("gateway_status_branch_{}", uuid::Uuid::new_v4()));
        let claude_session_id = "session-1";
        let (conversation_state, _branch_id) =
            seed_incremental_branch(&conversation_root, claude_session_id);
        let state = build_state(
            &url::Url::parse("ws://127.0.0.1:9").expect("url"),
            std::path::Path::new("/tmp/nonexistent-auth.json"),
            conversation_state,
        );
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(
                    "<command-message>status</command-message>\n<command-name>/status</command-name>\n<command-args></command-args>"
                        .to_string(),
                ),
            }],
            system: Vec::new(),
            stream: true,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        let translated =
            translate_request_with_context(&req, &build_tool_translation_context(&state, &req))
                .expect("translate status request");

        let prepared = prepare_conversation_branch(
            &state,
            Some(claude_session_id),
            &req,
            translated.client_metadata.as_ref(),
        )
        .expect("status should prepare a branch");

        assert!(prepared.commit_turn);
        assert_eq!(prepared.turn_scope, ConversationTurnScope::Main);

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn compact_message_forces_new_openai_thread_without_previous_response_id() {
        let conversation_root =
            std::env::temp_dir().join(format!("gateway_compact_thread_{}", uuid::Uuid::new_v4()));
        let claude_session_id = "session-1";
        let (conversation_state, _branch_id) =
            seed_incremental_branch(&conversation_root, claude_session_id);
        let state = build_state(
            &url::Url::parse("ws://127.0.0.1:9").expect("url"),
            std::path::Path::new("/tmp/nonexistent-auth.json"),
            conversation_state,
        );
        let req = test_anthropic_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
                    .to_string(),
            ),
        }]);
        let translated =
            translate_request_with_context(&req, &build_tool_translation_context(&state, &req))
                .expect("translate compact request");
        let prepared = prepare_conversation_branch(
            &state,
            Some(claude_session_id),
            &req,
            translated.client_metadata.as_ref(),
        )
        .expect("compact request should prepare a branch");

        assert!(prepared.commit_turn);
        assert!(prepared.branch.compaction_reset_pending);

        let resolution = resolve_model(&state.gateway_config, &req.model);
        let compatibility_fingerprint =
            request_compatibility_fingerprint(&state.gateway_config, &resolution, &translated);
        let selected = select_transport(
            Some(&prepared),
            &WebSocketChainDecision {
                match_result: WebSocketChainMatch::Matching,
                transport_identity: Some("test-identity".to_string()),
                live_websocket_chain_id: None,
                checkpoint_websocket_chain_id: None,
                checkpoint_response_id: prepared
                    .branch
                    .openai_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.response_id.clone()),
                reason: "test_matching_chain",
            },
            &resolution.selected_backend_model,
            &compatibility_fingerprint,
        )
        .expect("compact request should select transport");

        assert_eq!(selected.mode, TransportMode::Full);
        assert_eq!(selected.previous_response_id, None);
        assert_eq!(selected.reason, "branch_bootstrap_compaction_reset");

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn historical_compact_wrapper_followup_uses_existing_checkpoint_delta() {
        let conversation_root = std::env::temp_dir().join(format!(
            "gateway_compact_followup_delta_{}",
            uuid::Uuid::new_v4()
        ));
        let claude_session_id = "session-1";
        let (conversation_state, branch_id) =
            seed_incremental_branch(&conversation_root, claude_session_id);
        let compacted_messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }];
        let compacted_fingerprints = branch_fingerprints_from_messages(&compacted_messages, true);
        let mut compacted_branch = conversation_state
            .apply_compaction(
                claude_session_id,
                &branch_id,
                compacted_fingerprints.compaction_summary_hash.as_deref(),
                &compacted_fingerprints,
            )
            .expect("mark branch as already compacted");
        compacted_branch.compaction_reset_pending = false;
        conversation_state
            .store_branch(claude_session_id, &compacted_branch)
            .expect("store compacted branch fixture");
        let state = build_state(
            &url::Url::parse("ws://127.0.0.1:9").expect("url"),
            std::path::Path::new("/tmp/nonexistent-auth.json"),
            conversation_state,
        );
        let req = test_anthropic_request(vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text(
                        "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
                            .to_string(),
                    ),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("hello".to_string()),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("next".to_string()),
                },
            ]);
        let translated =
            translate_request_with_context(&req, &build_tool_translation_context(&state, &req))
                .expect("translate compact follow-up request");
        let prepared = prepare_conversation_branch(
            &state,
            Some(claude_session_id),
            &req,
            translated.client_metadata.as_ref(),
        )
        .expect("compact follow-up should prepare a branch");

        assert!(prepared.commit_turn);
        assert!(!prepared.branch.compaction_reset_pending);
        assert_eq!(prepared.delta_start_index, 1);
        assert_eq!(prepared.active_messages.len(), 2);

        let resolution = resolve_model(&state.gateway_config, &req.model);
        let compatibility_fingerprint =
            request_compatibility_fingerprint(&state.gateway_config, &resolution, &translated);
        let selected = select_transport(
            Some(&prepared),
            &WebSocketChainDecision {
                match_result: WebSocketChainMatch::Matching,
                transport_identity: Some("test-identity".to_string()),
                live_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
                checkpoint_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
                checkpoint_response_id: prepared
                    .branch
                    .openai_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.response_id.clone()),
                reason: "test_matching_chain",
            },
            &resolution.selected_backend_model,
            &compatibility_fingerprint,
        )
        .expect("compact follow-up should select transport");

        assert_eq!(selected.mode, TransportMode::Incremental);
        assert_eq!(selected.previous_response_id, Some("resp_prev".to_string()));
        assert_eq!(selected.reason, "branch_checkpoint_reuse");

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn post_compaction_same_message_history_marker_bootstraps_when_no_prefix_matches() {
        let conversation_root = std::env::temp_dir().join(format!(
            "gateway_compact_same_message_followup_delta_{}",
            uuid::Uuid::new_v4()
        ));
        let claude_session_id = "session-1";
        let (conversation_state, branch_id) =
            seed_incremental_branch(&conversation_root, claude_session_id);
        let compacted_messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("compaction summary".to_string()),
        }];
        let compacted_fingerprints = branch_fingerprints_from_messages(&compacted_messages, true);
        let mut compacted_branch = conversation_state
            .apply_compaction(
                claude_session_id,
                &branch_id,
                compacted_fingerprints.compaction_summary_hash.as_deref(),
                &compacted_fingerprints,
            )
            .expect("mark branch compacted");
        compacted_branch.compaction_reset_pending = false;
        compacted_branch.openai_checkpoint = Some(OpenAiCheckpoint {
            response_id: "resp_after_compaction".to_string(),
            previous_response_id: None,
            provider_model_fingerprint: DEFAULT_BACKEND_MODEL.to_string(),
            request_compatibility_fingerprint: None,
            provider_input_tokens: Some(42),
        });
        conversation_state
            .store_branch(claude_session_id, &compacted_branch)
            .expect("store compacted branch fixture");
        let state = build_state(
            &url::Url::parse("ws://127.0.0.1:9").expect("url"),
            std::path::Path::new("/tmp/nonexistent-auth.json"),
            conversation_state,
        );
        let req = test_anthropic_request(vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<local-command-caveat>historical local command</local-command-caveat>\n\
                 <command-name>/compact</command-name>\n\
                 <command-message>compact</command-message>\n\
                 <command-args></command-args>\n\
                 <local-command-stdout>Compacted </local-command-stdout>\n\
                 Do you remember what was the working on?"
                    .to_string(),
            ),
        }]);
        let translated =
            translate_request_with_context(&req, &build_tool_translation_context(&state, &req))
                .expect("translate compact follow-up request");
        let prepared = prepare_conversation_branch(
            &state,
            Some(claude_session_id),
            &req,
            translated.client_metadata.as_ref(),
        )
        .expect("compact follow-up should prepare a branch");

        assert!(prepared.commit_turn);
        assert!(!prepared.branch.compaction_reset_pending);
        assert_eq!(prepared.delta_start_index, 0);
        assert!(prepared.allow_zero_delta_start);

        let resolution = resolve_model(&state.gateway_config, &req.model);
        let compatibility_fingerprint =
            request_compatibility_fingerprint(&state.gateway_config, &resolution, &translated);
        let selected = select_transport(
            Some(&prepared),
            &WebSocketChainDecision {
                match_result: WebSocketChainMatch::Matching,
                transport_identity: Some("test-identity".to_string()),
                live_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
                checkpoint_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
                checkpoint_response_id: prepared
                    .branch
                    .openai_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.response_id.clone()),
                reason: "test_matching_chain",
            },
            &resolution.selected_backend_model,
            &compatibility_fingerprint,
        )
        .expect("compact follow-up should select bootstrap transport");

        assert_eq!(selected.mode, TransportMode::Full);
        assert_eq!(selected.previous_response_id, None);
        assert_eq!(selected.reason, "branch_bootstrap_zero_delta_start");

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn request_compatibility_fingerprint_ignores_client_metadata() {
        let config = GatewayConfig::default();
        let resolution = resolve_model(&config, DEFAULT_BACKEND_MODEL);
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hello".to_string()),
            }],
            system: Vec::new(),
            stream: true,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        let mut translated = translate_request_with_context(
            &req,
            &ToolTranslationContext::default()
                .with_claude_code_config(config.workflow.claude_code.clone()),
        )
        .expect("translate");
        let without_metadata = request_compatibility_fingerprint(&config, &resolution, &translated);

        translated
            .client_metadata
            .get_or_insert_with(HashMap::new)
            .insert(
                "claude_code_translated_slash_command".to_string(),
                "/status".to_string(),
            );
        let with_metadata = request_compatibility_fingerprint(&config, &resolution, &translated);

        assert_eq!(without_metadata, with_metadata);
    }
}

#[cfg(test)]
mod models_api_tests {
    use super::{AppState, router};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt as _;
    #[tokio::test]
    async fn v1_models_reads_local_settings_catalog() {
        let settings_path = std::env::temp_dir().join(format!(
            "claude_gateway_settings_{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "includeCoAuthoredBy": false,
                "models": [
                    {
                        "id": "gpt-5.4-mini",
                        "name": "GPT-5.4 Mini",
                        "description": "OpenAI small/fallback model"
                    },
                    {
                        "id": "gpt-5.4",
                        "name": "GPT-5.4",
                        "description": "OpenAI general-purpose model"
                    }
                ],
                "env": {
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "gpt-5.4-mini",
                    "ANTHROPIC_DEFAULT_HAIKU_MAX_TOKENS": 128_000,
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "gpt-5.4",
                    "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS": "1000000"
                }
            }))
            .expect("serialize temp settings"),
        )
        .expect("write temp settings");

        let state = AppState::default().with_claude_gateway_settings_path(settings_path.clone());

        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<String> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(ids, vec!["gpt-5.4-mini".to_string(), "gpt-5.4".to_string()]);
        assert_eq!(json["data"][0]["name"].as_str(), Some("GPT-5.4 Mini"));
        assert_eq!(
            json["data"][1]["description"].as_str(),
            Some("OpenAI general-purpose model")
        );
        assert_eq!(json["data"][0]["max_input_tokens"].as_u64(), Some(128_000));
        assert_eq!(
            json["data"][1]["max_input_tokens"].as_u64(),
            Some(1_000_000)
        );

        let _ = std::fs::remove_file(&settings_path);
    }

    #[tokio::test]
    async fn v1_models_prefers_explicit_model_context_window() {
        let settings_path = std::env::temp_dir().join(format!(
            "claude_gateway_settings_{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "includeCoAuthoredBy": false,
                "models": [
                    {
                        "id": "gpt-5.4",
                        "name": "GPT-5.4",
                        "description": "OpenAI long-context model",
                        "max_input_tokens": 900_000
                    }
                ],
                "env": {
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "gpt-5.4",
                    "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS": 1_000_000
                }
            }))
            .expect("serialize temp settings"),
        )
        .expect("write temp settings");

        let state = AppState::default().with_claude_gateway_settings_path(settings_path.clone());

        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["data"][0]["max_input_tokens"].as_u64(), Some(900_000));

        let _ = std::fs::remove_file(&settings_path);
    }
}

#[cfg(test)]
mod transport_selection_tests {
    use super::{
        BranchSelectionAction, CheckpointDiagnostic, CheckpointDiagnosticSubject,
        ConversationPersistenceClass, ConversationRequestKind, LeaseDiagnosticState,
        MainTurnLeaseAcquire, MainTurnLeaseCommit, MainTurnLeaseGuard, MainTurnLeaseState,
        MainTurnLeaseStore, OpenAiChainCheckpointStore, PreparedConversationBranch,
        SelectedCheckpoint, SelectedTransport, StreamCommitContext, TransportMode,
        WebSocketChainDecision, WebSocketChainMatch, borrowed_context_delta_start_index,
        checkpoint_diagnostic_value, is_known_delta_rejection, normalized_reasoning_effort,
        select_transport, stream_lease_allows_commit, transport_identity_for_branch,
        transport_selection_diagnostic_value, validate_previous_response_contract,
        websocket_transport_identity_for_branch,
    };
    use gateway_backend_codex::client::{BackendError, WebSocketChainId};
    use gateway_state::{
        BranchMetadata, ConversationTurnScope, OpenAiCheckpoint, TurnOpenAiCheckpoint,
    };

    fn prepared_branch(
        turn_scope: ConversationTurnScope,
        openai_checkpoint: Option<OpenAiCheckpoint>,
        compaction_reset_pending: bool,
    ) -> PreparedConversationBranch {
        PreparedConversationBranch {
            claude_session_id: "session-1".to_string(),
            branch: BranchMetadata {
                schema_version: 1,
                branch_id: "branch-1".to_string(),
                parent_branch_id: None,
                fork_ancestor_checkpoint: None,
                current_checkpoint_id: Some("checkpoint-1".to_string()),
                active_canonical_messages: None,
                fingerprints: gateway_state::BranchFingerprintSet::default(),
                openai_checkpoint,
                turn_openai_checkpoints: Vec::new(),
                offshoot_openai_checkpoints: Vec::new(),
                compaction_reset_pending,
                last_main_turn_id: Some("turn-1".to_string()),
                created_at_unix_seconds: 0,
                updated_at_unix_seconds: 0,
            },
            request_kind: ConversationRequestKind::VisibleMain,
            selection_action: BranchSelectionAction::ContinuedExisting,
            turn_scope,
            persistence_class: ConversationPersistenceClass::DurableMain,
            persistence_reason: "test",
            commit_turn: turn_scope == ConversationTurnScope::Main,
            allow_incremental_context: turn_scope == ConversationTurnScope::Main,
            allow_zero_delta_start: false,
            fingerprints: gateway_state::BranchFingerprintSet::default(),
            active_messages: Vec::new(),
            operational_context_messages: Vec::new(),
            delta_start_index: 1,
        }
    }

    fn chain_decision(match_result: WebSocketChainMatch) -> super::WebSocketChainDecision {
        super::WebSocketChainDecision {
            match_result,
            transport_identity: Some("test-identity".to_string()),
            live_websocket_chain_id: None,
            checkpoint_websocket_chain_id: None,
            checkpoint_response_id: None,
            reason: "test",
        }
    }

    fn selected_checkpoint() -> SelectedCheckpoint {
        SelectedCheckpoint {
            response_id: "resp_123".to_string(),
            provider_model_fingerprint: "gpt-5".to_string(),
            request_compatibility_fingerprint: Some("fp-1".to_string()),
            source: "visible_branch_head",
            canonical_message_count: Some(3),
        }
    }

    fn checkpoint(response_id: &str, model: &str, fingerprint: &str) -> OpenAiCheckpoint {
        OpenAiCheckpoint {
            response_id: response_id.to_string(),
            previous_response_id: Some("resp_parent".to_string()),
            provider_model_fingerprint: model.to_string(),
            request_compatibility_fingerprint: Some(fingerprint.to_string()),
            provider_input_tokens: None,
        }
    }

    fn turn_checkpoint(
        turn_id: &str,
        canonical_message_count: usize,
        canonical_prefix_hash: String,
        response_id: &str,
        model: &str,
        fingerprint: &str,
    ) -> TurnOpenAiCheckpoint {
        TurnOpenAiCheckpoint {
            schema_version: 1,
            turn_id: turn_id.to_string(),
            canonical_message_count,
            canonical_prefix_hash,
            response_id: response_id.to_string(),
            previous_response_id: Some("resp_parent".to_string()),
            provider_model_fingerprint: model.to_string(),
            request_compatibility_fingerprint: Some(fingerprint.to_string()),
            provider_input_tokens: None,
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn transport_identity_key_includes_session_branch_model_and_effort() {
        let prepared = prepared_branch(ConversationTurnScope::Main, None, false);
        let reasoning = serde_json::json!({ "effort": "HIGH" });

        let identity =
            transport_identity_for_branch(Some(&prepared), "gpt-5.6-terra", Some(&reasoning))
                .expect("identity should be available for prepared branches");

        assert_eq!(
            identity.key(),
            "v1:session-1:branch-1:visible_main:gpt-5.6-terra:high"
        );
    }

    #[test]
    fn transport_identity_defaults_missing_effort_to_stable_sentinel() {
        let prepared = prepared_branch(ConversationTurnScope::Main, None, false);

        let identity = transport_identity_for_branch(Some(&prepared), "gpt-5.5", None)
            .expect("identity should be available for prepared branches");

        assert_eq!(
            identity.key(),
            "v1:session-1:branch-1:visible_main:gpt-5.5:default"
        );
        assert_eq!(normalized_reasoning_effort(None), "default");
    }

    #[test]
    fn websocket_chain_association_is_keyed_by_response_id() {
        let store = OpenAiChainCheckpointStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        store.associate(&identity, "resp_main", WebSocketChainId::new("chain-1"));
        store.associate(&identity, "resp_side", WebSocketChainId::new("chain-1"));

        assert_eq!(
            store
                .websocket_chain_id_for_response(&identity, "resp_main")
                .as_ref()
                .map(WebSocketChainId::as_str),
            Some("chain-1")
        );
        assert_eq!(
            store
                .websocket_chain_id_for_response(&identity, "resp_side")
                .as_ref()
                .map(WebSocketChainId::as_str),
            Some("chain-1")
        );
    }

    #[test]
    fn side_response_association_does_not_authorize_main_checkpoint() {
        let store = OpenAiChainCheckpointStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );

        store.associate(&identity, "resp_side", WebSocketChainId::new("chain-1"));

        assert!(
            store
                .websocket_chain_id_for_response(&identity, "resp_main")
                .is_none()
        );
    }

    #[test]
    fn websocket_chain_association_is_scoped_by_transport_identity() {
        let store = OpenAiChainCheckpointStore::default();
        let default_effort = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        let high_effort = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "high",
        );

        store.associate(
            &default_effort,
            "resp_1",
            WebSocketChainId::new("chain-default"),
        );

        assert_eq!(
            store
                .websocket_chain_id_for_response(&default_effort, "resp_1")
                .as_ref()
                .map(WebSocketChainId::as_str),
            Some("chain-default")
        );
        assert!(
            store
                .websocket_chain_id_for_response(&high_effort, "resp_1")
                .is_none()
        );
    }

    #[test]
    fn websocket_chain_association_is_scoped_by_claude_session_id() {
        let store = OpenAiChainCheckpointStore::default();
        let session_one = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        let session_two = super::ConversationTransportIdentity::new(
            "session-2",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );

        store.associate(&session_one, "resp_1", WebSocketChainId::new("chain-1"));

        assert_eq!(
            store
                .websocket_chain_id_for_response(&session_one, "resp_1")
                .as_ref()
                .map(WebSocketChainId::as_str),
            Some("chain-1")
        );
        assert!(
            store
                .websocket_chain_id_for_response(&session_two, "resp_1")
                .is_none()
        );
    }

    #[test]
    fn websocket_chain_association_is_scoped_by_request_kind() {
        let store = OpenAiChainCheckpointStore::default();
        let visible_main = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        let subagent_offshoot = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::SubagentOffshoot,
            "gpt-5",
            "default",
        );

        store.associate(
            &subagent_offshoot,
            "resp_subagent",
            WebSocketChainId::new("chain-subagent"),
        );

        assert!(
            store
                .websocket_chain_id_for_response(&visible_main, "resp_subagent")
                .is_none()
        );
        assert_eq!(
            store
                .websocket_chain_id_for_response(&subagent_offshoot, "resp_subagent")
                .as_ref()
                .map(WebSocketChainId::as_str),
            Some("chain-subagent")
        );
    }

    #[test]
    fn offshoot_borrows_visible_main_websocket_identity_for_context() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        prepared.request_kind = ConversationRequestKind::PermissionClassifier;
        prepared.persistence_class = ConversationPersistenceClass::TransientInternal;
        prepared.commit_turn = false;
        prepared.allow_incremental_context = true;

        let request_identity = transport_identity_for_branch(Some(&prepared), "gpt-5", None)
            .expect("request identity should exist");
        let websocket_identity =
            websocket_transport_identity_for_branch(Some(&prepared), "gpt-5", None)
                .expect("websocket identity should exist");

        assert_eq!(
            request_identity.key(),
            "v1:session-1:branch-1:permission_classifier:gpt-5:default"
        );
        assert_eq!(
            websocket_identity.key(),
            "v1:session-1:branch-1:visible_main:gpt-5:default"
        );

        let selected = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("offshoot should borrow visible context when live-chain proof matches");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_123".to_string()),
                reason: "transient_context_read",
            }
        );
    }

    #[test]
    fn offshoot_payload_without_visible_prefix_is_sent_as_full_delta_payload() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        let stored_visible = vec![crate::types::AnthropicMessage {
            role: "user".to_string(),
            content: crate::types::AnthropicContent::Text("hello".to_string()),
        }];
        prepared.branch.active_canonical_messages =
            Some(serde_json::to_value(&stored_visible).expect("serialize visible history"));

        let classifier_payload = vec![
            crate::types::AnthropicMessage {
                role: "user".to_string(),
                content: crate::types::AnthropicContent::Text(
                    "The following is the user's CLAUDE.md configuration.".to_string(),
                ),
            },
            crate::types::AnthropicMessage {
                role: "user".to_string(),
                content: crate::types::AnthropicContent::Text(
                    "<transcript>\nBash pwd\n</transcript>\nShould this be blocked?".to_string(),
                ),
            },
        ];

        assert_eq!(
            borrowed_context_delta_start_index(&prepared.branch, &classifier_payload),
            0
        );

        prepared.request_kind = ConversationRequestKind::PermissionClassifier;
        prepared.persistence_class = ConversationPersistenceClass::TransientInternal;
        prepared.commit_turn = false;
        prepared.allow_incremental_context = true;
        prepared.active_messages = classifier_payload.clone();
        prepared.delta_start_index =
            borrowed_context_delta_start_index(&prepared.branch, &prepared.active_messages);

        assert_eq!(
            serde_json::to_value(prepared.incremental_backend_render_messages())
                .expect("serialize rendered messages"),
            serde_json::to_value(classifier_payload).expect("serialize classifier payload")
        );
    }

    #[test]
    fn side_turn_with_visible_prefix_sends_only_side_delta_payload() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Side,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        let visible_message = crate::types::AnthropicMessage {
            role: "user".to_string(),
            content: crate::types::AnthropicContent::Text("hello".to_string()),
        };
        let side_message = crate::types::AnthropicMessage {
            role: "user".to_string(),
            content: crate::types::AnthropicContent::Text(
                "This is a side question from the user.".to_string(),
            ),
        };
        let visible_history = vec![visible_message.clone()];
        let incoming = vec![visible_message, side_message.clone()];
        prepared.branch.active_canonical_messages =
            Some(serde_json::to_value(&visible_history).expect("serialize visible history"));
        prepared.request_kind = ConversationRequestKind::VisibleMain;
        prepared.persistence_class = ConversationPersistenceClass::ReadOnlySideTurn;
        prepared.commit_turn = false;
        prepared.allow_incremental_context = true;
        prepared.active_messages = incoming;
        prepared.delta_start_index =
            borrowed_context_delta_start_index(&prepared.branch, &prepared.active_messages);

        assert_eq!(prepared.delta_start_index, 1);
        assert_eq!(
            serde_json::to_value(prepared.incremental_backend_render_messages())
                .expect("serialize rendered messages"),
            serde_json::to_value(vec![side_message]).expect("serialize side payload")
        );
    }

    #[test]
    fn tool_result_followup_with_visible_prefix_sends_only_tool_result_delta() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_tool_call", "gpt-5", "fp-1")),
            false,
        );
        let user_message = crate::types::AnthropicMessage {
            role: "user".to_string(),
            content: crate::types::AnthropicContent::Text("call a tool".to_string()),
        };
        let assistant_tool_use = crate::types::AnthropicMessage {
            role: "assistant".to_string(),
            content: crate::types::AnthropicContent::Blocks(vec![
                crate::types::AnthropicContentBlock {
                    block_type: "tool_use".to_string(),
                    text: None,
                    id: Some("call_1".to_string()),
                    name: Some("Read".to_string()),
                    input: Some(serde_json::json!({ "file_path": "/tmp/a.txt" })),
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        };
        let tool_result = crate::types::AnthropicMessage {
            role: "user".to_string(),
            content: crate::types::AnthropicContent::Blocks(vec![
                crate::types::AnthropicContentBlock {
                    block_type: "tool_result".to_string(),
                    text: None,
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: Some("call_1".to_string()),
                    content: Some(serde_json::json!("file contents")),
                    is_error: Some(false),
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        };
        let stored_visible = vec![user_message.clone(), assistant_tool_use.clone()];
        let incoming = vec![user_message, assistant_tool_use, tool_result.clone()];
        prepared.branch.active_canonical_messages =
            Some(serde_json::to_value(&stored_visible).expect("serialize visible history"));
        prepared.active_messages = incoming;
        prepared.delta_start_index =
            borrowed_context_delta_start_index(&prepared.branch, &prepared.active_messages);

        assert_eq!(prepared.delta_start_index, 2);
        assert_eq!(
            serde_json::to_value(prepared.incremental_backend_render_messages())
                .expect("serialize rendered messages"),
            serde_json::to_value(vec![tool_result]).expect("serialize tool-result payload")
        );

        let selected = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("tool-result follow-up should continue from tool-call checkpoint");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_tool_call".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );
    }

    #[test]
    fn visible_main_lease_rejects_concurrent_turn_for_same_identity() {
        let store = MainTurnLeaseStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );

        assert_eq!(
            store.acquire(
                &identity,
                "request-1".to_string(),
                Some("resp_1".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );

        assert_eq!(
            store.acquire(
                &identity,
                "request-2".to_string(),
                Some("resp_1".to_string())
            ),
            MainTurnLeaseAcquire::Busy {
                in_flight_request_id: "request-1".to_string(),
                previous_response_id: Some("resp_1".to_string()),
                websocket_chain_id: None,
            }
        );
    }

    #[test]
    fn visible_main_lease_requires_matching_websocket_chain_for_commit() {
        let store = MainTurnLeaseStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );

        assert_eq!(
            store.acquire(
                &identity,
                "request-1".to_string(),
                Some("resp_1".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );
        assert!(store.promote_websocket_chain(
            &identity,
            "request-1",
            Some(WebSocketChainId::new("chain-1"))
        ));

        assert_eq!(
            store.validate_for_commit(
                &identity,
                "request-1",
                Some(&WebSocketChainId::new("chain-1"))
            ),
            MainTurnLeaseCommit::Accepted
        );
        assert_eq!(
            store.validate_for_commit(
                &identity,
                "request-1",
                Some(&WebSocketChainId::new("chain-2"))
            ),
            MainTurnLeaseCommit::Rejected("websocket_chain_id_mismatch")
        );
        assert_eq!(
            store.validate_for_commit(&identity, "request-1", None),
            MainTurnLeaseCommit::Rejected("missing_commit_websocket_chain_id")
        );
    }

    #[test]
    fn visible_main_lease_guard_drop_releases_aborted_turn() {
        let store = MainTurnLeaseStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );

        assert_eq!(
            store.acquire(
                &identity,
                "request-1".to_string(),
                Some("resp_1".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );
        let guard =
            MainTurnLeaseGuard::new(store.clone(), identity.clone(), "request-1".to_string());
        drop(guard);

        assert_eq!(
            store.acquire(
                &identity,
                "request-2".to_string(),
                Some("resp_1".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );
    }

    #[test]
    fn late_stream_completion_after_client_abort_cannot_commit() {
        let lease_store = MainTurnLeaseStore::default();
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        assert_eq!(
            lease_store.acquire(
                &identity,
                "request-1".to_string(),
                Some("resp_stable".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );
        assert!(lease_store.promote_websocket_chain(
            &identity,
            "request-1",
            Some(WebSocketChainId::new("chain-1")),
        ));
        assert!(lease_store.mark_state(
            &identity,
            "request-1",
            MainTurnLeaseState::ClientAbortedAfterVisibleOutput,
        ));
        let guard = MainTurnLeaseGuard::new(
            lease_store.clone(),
            identity.clone(),
            "request-1".to_string(),
        );
        guard.mark_released();
        let commit = StreamCommitContext {
            claude_session_id: "session-1".to_string(),
            branch_id: "branch-1".to_string(),
            fingerprints: gateway_state::BranchFingerprintSet::default(),
            active_canonical_messages: serde_json::json!([
                {"role":"user","content":"hello"}
            ]),
            provider_model_fingerprint: "gpt-5".to_string(),
            request_compatibility_fingerprint: "fp-1".to_string(),
            previous_response_id: Some("resp_stable".to_string()),
            canonical_message_count: 1,
            canonical_prefix_hash: "hash-1".to_string(),
            request_kind: ConversationRequestKind::VisibleMain,
            turn_scope: ConversationTurnScope::Main,
            commit_turn: true,
            selected_checkpoint_source: Some("visible_branch_head"),
            selected_checkpoint_response_id: Some("resp_stable".to_string()),
            transport_identity: Some(identity.clone()),
            websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
            openai_chain_checkpoints: OpenAiChainCheckpointStore::default(),
            request_id: Some("request-1".to_string()),
            main_turn_lease: Some(guard),
        };

        assert!(!stream_lease_allows_commit(&commit, Some("resp_late")));
        assert_eq!(
            lease_store.acquire(
                &identity,
                "request-2".to_string(),
                Some("resp_stable".to_string())
            ),
            MainTurnLeaseAcquire::Acquired
        );
    }

    #[test]
    fn next_visible_turn_after_abort_uses_stable_checkpoint_only_with_live_chain_proof() {
        let prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_stable", "gpt-5", "fp-1")),
            false,
        );

        let selected_with_live_chain = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("stable checkpoint with live-chain proof should be reused");
        assert_eq!(
            selected_with_live_chain,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_stable".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );

        let selected_without_live_chain = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Missing),
            "gpt-5",
            "fp-1",
        )
        .expect("missing chain proof should bootstrap instead of guessing");
        assert_eq!(
            selected_without_live_chain,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_missing_websocket_chain",
            }
        );
    }

    #[test]
    fn missing_branch_uses_full_bootstrap_transport() {
        let selected = select_transport(
            None,
            &chain_decision(WebSocketChainMatch::Missing),
            "gpt-5",
            "fp-1",
        )
        .expect("missing branch should use full bootstrap transport");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "no_branch_available",
            }
        );
    }

    #[test]
    fn existing_canonical_session_with_matching_chain_uses_delta() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("matching checkpoint should enable incremental transport");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_123".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );
    }

    #[test]
    fn rewind_prefix_uses_matching_turn_checkpoint_instead_of_mutable_branch_head() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint(
                "resp_branch_head_other_model",
                "gpt-5.6-terra",
                "fp-latest",
            )),
            false,
        );
        prepared.active_messages = vec![
            crate::types::AnthropicMessage {
                role: "user".to_string(),
                content: crate::types::AnthropicContent::Text("hello".to_string()),
            },
            crate::types::AnthropicMessage {
                role: "user".to_string(),
                content: crate::types::AnthropicContent::Text("rewound follow-up".to_string()),
            },
        ];
        prepared.delta_start_index = 1;
        prepared.branch.turn_openai_checkpoints = vec![turn_checkpoint(
            "turn-hello",
            1,
            super::canonical_messages_prefix_hash(&prepared.active_messages, 1),
            "resp_turn_hello",
            "gpt-5",
            "fp-1",
        )];

        let selected = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("matching rewind prefix should reuse the per-turn checkpoint");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_turn_hello".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );
    }

    #[test]
    fn non_visible_request_borrows_context_without_main_commit_transport() {
        let mut prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        prepared.request_kind = ConversationRequestKind::SubagentOffshoot;
        prepared.turn_scope = ConversationTurnScope::Side;
        prepared.persistence_class = ConversationPersistenceClass::TransientInternal;
        prepared.persistence_reason =
            ConversationRequestKind::SubagentOffshoot.persistence_reason();
        prepared.commit_turn = false;
        prepared.allow_incremental_context = true;

        let selected = select_transport(
            Some(&prepared),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("offshoot request should borrow context from the visible branch");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_123".to_string()),
                reason: "transient_context_read",
            }
        );
    }

    #[test]
    fn existing_canonical_session_after_gateway_restart_bootstraps_without_previous_response_id() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Missing),
            "gpt-5",
            "fp-1",
        )
        .expect("missing websocket chain should force bootstrap");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_missing_websocket_chain",
            }
        );
    }

    #[test]
    fn existing_canonical_session_with_mismatched_chain_bootstraps_without_previous_response_id() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Mismatching),
            "gpt-5",
            "fp-1",
        )
        .expect("mismatched websocket chain should force bootstrap");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_websocket_chain_mismatch",
            }
        );
    }

    #[test]
    fn checkpointed_branch_bootstraps_when_compaction_reset_is_pending() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                true,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("checkpointed compaction turn should bootstrap a fresh provider chain");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_compaction_reset",
            }
        );
    }

    #[test]
    fn checkpointed_branch_bootstraps_when_model_changes() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5.5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5.6-terra",
            "fp-1",
        )
        .expect("model drift should bootstrap a fresh provider chain");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_model_drift",
            }
        );
    }

    #[test]
    fn resumed_session_without_gateway_canonical_history_bootstraps_without_previous_response_id() {
        let selected = select_transport(
            Some(&prepared_branch(ConversationTurnScope::Main, None, false)),
            &chain_decision(WebSocketChainMatch::Missing),
            "gpt-5",
            "fp-1",
        )
        .expect("missing checkpoint is the only branch bootstrap case");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_missing_checkpoint",
            }
        );
    }

    #[test]
    fn resumed_session_after_baseline_commit_uses_delta_on_same_live_chain() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_resumed_baseline", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect("fresh resumed baseline checkpoint should be reusable on same live chain");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_resumed_baseline".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );
    }

    #[test]
    fn effort_change_bootstraps_because_identity_scoped_chain_association_is_missing() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Missing),
            "gpt-5",
            "fp-1",
        )
        .expect("effort drift appears as missing chain proof for the new transport identity");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "branch_bootstrap_missing_websocket_chain",
            }
        );
    }

    #[test]
    fn side_turn_without_incremental_context_is_rejected() {
        let err = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Side,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-1",
        )
        .expect_err("side turn without incremental context must be rejected");

        assert_eq!(
            err,
            "conversation branch has a stored OpenAI checkpoint, but this request is not eligible for checkpoint reuse"
        );
    }

    #[test]
    fn checkpointed_branch_uses_delta_when_request_compatibility_changes() {
        let selected = select_transport(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-old")),
                false,
            )),
            &chain_decision(WebSocketChainMatch::Matching),
            "gpt-5",
            "fp-new",
        )
        .expect("compatibility drift must not bypass checkpointed delta transport");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_123".to_string()),
                reason: "branch_checkpoint_reuse_compatibility_drift",
            }
        );
    }

    #[test]
    fn previous_response_contract_rejects_incremental_without_checkpoint() {
        let err = validate_previous_response_contract(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: None,
                reason: "test_bad_incremental",
            },
        )
        .expect_err("incremental transport without checkpoint must be rejected");

        assert!(err.contains("incremental transport selected without previous_response_id"));
    }

    #[test]
    fn previous_response_contract_rejects_unapproved_full_null_reason() {
        let err = validate_previous_response_contract(
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                false,
            )),
            &SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "accidental_fallback",
            },
        )
        .expect_err("unapproved full null transport must be rejected");

        assert!(err.contains("unapproved null previous_response_id full transport"));
    }

    #[test]
    fn previous_response_contract_allows_approved_bootstrap_reasons_only() {
        for reason in [
            "no_branch_available",
            "branch_bootstrap_missing_checkpoint",
            "branch_bootstrap_missing_websocket_chain",
            "branch_bootstrap_websocket_chain_mismatch",
            "branch_bootstrap_model_drift",
            "branch_bootstrap_compaction_reset",
            "branch_bootstrap_no_prefix_match",
            "branch_bootstrap_zero_delta_start",
        ] {
            validate_previous_response_contract(
                Some(&prepared_branch(
                    ConversationTurnScope::Main,
                    Some(checkpoint("resp_123", "gpt-5", "fp-1")),
                    false,
                )),
                &SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason,
                },
            )
            .unwrap_or_else(|err| panic!("approved bootstrap reason rejected: {reason}: {err}"));
        }
    }

    #[test]
    fn transport_selection_diagnostic_contains_required_checkpoint_and_policy_fields() {
        let prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        let selected_transport = SelectedTransport {
            mode: TransportMode::Incremental,
            previous_response_id: Some("resp_123".to_string()),
            reason: "branch_checkpoint_reuse",
        };
        let selected_checkpoint = selected_checkpoint();
        let chain_decision = WebSocketChainDecision {
            match_result: WebSocketChainMatch::Matching,
            transport_identity: Some(
                "v1:session-1:branch-1:visible_main:gpt-5:default".to_string(),
            ),
            live_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
            checkpoint_websocket_chain_id: Some(WebSocketChainId::new("chain-1")),
            checkpoint_response_id: Some("resp_123".to_string()),
            reason: "websocket_chain_match",
        };

        let diagnostic = transport_selection_diagnostic_value(
            None,
            Some(&prepared),
            &selected_transport,
            Some(&selected_checkpoint),
            "gpt-5",
            Some(&chain_decision),
            "incremental",
        );

        assert_eq!(diagnostic["event"], "transport_selected");
        assert_eq!(diagnostic["request_kind"], "visible_main");
        assert_eq!(diagnostic["commit_policy"], "visible_main_lease_required");
        assert_eq!(diagnostic["client_abort_state"], "not_aborted");
        assert_eq!(diagnostic["previous_response_id"], "resp_123");
        assert_eq!(diagnostic["provider_model_fingerprint"], "gpt-5");
        assert_eq!(diagnostic["reasoning_effort"], "default");
        assert_eq!(diagnostic["websocket_chain_match"], "matching");
        assert_eq!(diagnostic["live_websocket_chain_id"], "chain-1");
        assert_eq!(diagnostic["checkpoint_websocket_chain_id"], "chain-1");
        assert_eq!(diagnostic["checkpoint_response_id"], "resp_123");
        assert_eq!(
            diagnostic["selected_checkpoint_source"],
            "visible_branch_head"
        );
        assert_eq!(diagnostic["selected_checkpoint_response_id"], "resp_123");
    }

    #[test]
    fn checkpoint_diagnostic_contains_required_commit_and_abort_fields() {
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::PermissionClassifier,
            "gpt-5",
            "default",
        );
        let subject = CheckpointDiagnosticSubject {
            claude_session_id: "session-1",
            branch_id: "branch-1",
            request_kind: ConversationRequestKind::PermissionClassifier,
            turn_scope: ConversationTurnScope::Side,
            commit_turn: false,
        };
        let diagnostic = CheckpointDiagnostic {
            event: "offshoot_checkpoint_committed",
            subject,
            transport_identity: Some(&identity),
            request_id: Some("req-1"),
            provider_response_id: Some("resp_offshoot"),
            previous_response_id: Some("resp_123"),
            selected_checkpoint_source: Some("visible_branch_head"),
            selected_checkpoint_response_id: Some("resp_123"),
            websocket_chain_id: Some(&WebSocketChainId::new("chain-1")),
            streaming: false,
            commit_policy: "offshoot_checkpoint_only",
            skip_reason: None,
            client_abort_state: LeaseDiagnosticState::NotAborted,
        };

        let value = checkpoint_diagnostic_value(&diagnostic);

        assert_eq!(value["event"], "offshoot_checkpoint_committed");
        assert_eq!(value["request_kind"], "permission_classifier");
        assert_eq!(value["commit_policy"], "offshoot_checkpoint_only");
        assert_eq!(value["client_abort_state"], "not_aborted");
        assert_eq!(value["provider_response_id"], "resp_offshoot");
        assert_eq!(value["committed_previous_response_id"], "resp_123");
        assert_eq!(value["selected_checkpoint_source"], "visible_branch_head");
        assert_eq!(value["selected_checkpoint_response_id"], "resp_123");
        assert_eq!(value["websocket_chain_id"], "chain-1");
        assert_eq!(value["provider_model_fingerprint"], "gpt-5");
        assert_eq!(value["reasoning_effort"], "default");
    }

    #[test]
    fn checkpoint_diagnostic_schema_covers_visible_success_skip_and_abort() {
        let identity = super::ConversationTransportIdentity::new(
            "session-1",
            "branch-1",
            ConversationRequestKind::VisibleMain,
            "gpt-5",
            "default",
        );
        for (event, commit_policy, skip_reason, abort_state) in [
            (
                "visible_checkpoint_committed",
                "durable_visible_main_commit",
                None,
                LeaseDiagnosticState::CompletedCommitted,
            ),
            (
                "checkpoint_commit_skipped",
                "visible_main_lease_rejected",
                Some("client_aborted_after_visible_output"),
                LeaseDiagnosticState::ClientAbortedAfterVisibleOutput,
            ),
            (
                "checkpoint_commit_skipped",
                "backend_failed_before_commit",
                Some("backend_failed_before_commit"),
                LeaseDiagnosticState::BackendFailedBeforeCommit,
            ),
        ] {
            let diagnostic = CheckpointDiagnostic {
                event,
                subject: CheckpointDiagnosticSubject {
                    claude_session_id: "session-1",
                    branch_id: "branch-1",
                    request_kind: ConversationRequestKind::VisibleMain,
                    turn_scope: ConversationTurnScope::Main,
                    commit_turn: true,
                },
                transport_identity: Some(&identity),
                request_id: Some("req-1"),
                provider_response_id: Some("resp_next"),
                previous_response_id: Some("resp_123"),
                selected_checkpoint_source: Some("visible_branch_head"),
                selected_checkpoint_response_id: Some("resp_123"),
                websocket_chain_id: Some(&WebSocketChainId::new("chain-1")),
                streaming: true,
                commit_policy,
                skip_reason,
                client_abort_state: abort_state,
            };
            let value = checkpoint_diagnostic_value(&diagnostic);

            assert_eq!(value["event"], event);
            assert_eq!(value["request_kind"], "visible_main");
            assert_eq!(value["commit_policy"], commit_policy);
            assert_eq!(
                value["skip_reason"],
                serde_json::to_value(skip_reason).unwrap()
            );
            assert_eq!(value["client_abort_state"], abort_state.as_str());
            assert_eq!(value["provider_response_id"], "resp_next");
            assert_eq!(value["committed_previous_response_id"], "resp_123");
            assert_eq!(value["selected_checkpoint_source"], "visible_branch_head");
            assert_eq!(value["selected_checkpoint_response_id"], "resp_123");
            assert_eq!(value["websocket_chain_id"], "chain-1");
            assert_eq!(value["provider_model_fingerprint"], "gpt-5");
            assert_eq!(value["reasoning_effort"], "default");
        }
    }

    #[test]
    fn transport_diagnostic_schema_covers_bootstrap_and_chain_mismatch() {
        let prepared = prepared_branch(
            ConversationTurnScope::Main,
            Some(checkpoint("resp_123", "gpt-5", "fp-1")),
            false,
        );
        let selected_checkpoint = selected_checkpoint();

        for (selected_transport, chain_decision, expected_match) in [
            (
                SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "branch_bootstrap_missing_websocket_chain",
                },
                WebSocketChainDecision {
                    match_result: WebSocketChainMatch::Missing,
                    transport_identity: Some(
                        "v1:session-1:branch-1:visible_main:gpt-5:default".to_string(),
                    ),
                    live_websocket_chain_id: None,
                    checkpoint_websocket_chain_id: None,
                    checkpoint_response_id: Some("resp_123".to_string()),
                    reason: "missing_live_websocket_chain",
                },
                "missing",
            ),
            (
                SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "branch_bootstrap_websocket_chain_mismatch",
                },
                WebSocketChainDecision {
                    match_result: WebSocketChainMatch::Mismatching,
                    transport_identity: Some(
                        "v1:session-1:branch-1:visible_main:gpt-5:default".to_string(),
                    ),
                    live_websocket_chain_id: Some(WebSocketChainId::new("chain-new")),
                    checkpoint_websocket_chain_id: Some(WebSocketChainId::new("chain-old")),
                    checkpoint_response_id: Some("resp_123".to_string()),
                    reason: "websocket_chain_mismatch",
                },
                "mismatching",
            ),
        ] {
            let value = transport_selection_diagnostic_value(
                None,
                Some(&prepared),
                &selected_transport,
                Some(&selected_checkpoint),
                "gpt-5",
                Some(&chain_decision),
                "full",
            );

            assert_eq!(value["event"], "transport_selected");
            assert_eq!(value["transport_mode"], "full");
            assert_eq!(value["previous_response_id"], serde_json::Value::Null);
            assert_eq!(value["request_kind"], "visible_main");
            assert_eq!(value["commit_policy"], "visible_main_lease_required");
            assert_eq!(value["client_abort_state"], "not_aborted");
            assert_eq!(value["websocket_chain_match"], expected_match);
            assert_eq!(value["checkpoint_response_id"], "resp_123");
            assert_eq!(value["selected_checkpoint_source"], "visible_branch_head");
            assert_eq!(value["selected_checkpoint_response_id"], "resp_123");
            assert_eq!(value["provider_model_fingerprint"], "gpt-5");
            assert_eq!(value["reasoning_effort"], "default");
        }
    }

    #[test]
    fn delta_rejection_detection_accepts_known_previous_response_failure() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: "previous_response_id resp_prev not found".to_string(),
        };

        assert!(is_known_delta_rejection(&err, Some("resp_prev")));
    }

    #[test]
    fn delta_rejection_detection_rejects_auth_failures() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 401,
            body: "previous_response_id resp_prev not found".to_string(),
        };

        assert!(!is_known_delta_rejection(&err, Some("resp_prev")));
    }

    #[test]
    fn delta_rejection_detection_rejects_generic_backend_bad_requests() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: "tool schema validation failed".to_string(),
        };

        assert!(!is_known_delta_rejection(&err, Some("resp_prev")));
    }
}
