#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::{DEFAULT_BACKEND_MODEL, UNSUPPORTED_BACKEND_MODELS};

pub const FAST_SERVICE_TIER: &str = "priority";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GatewayConfig {
    #[serde(default = "default_config_version")]
    pub version: u32,
    pub workflow: WorkflowConfig,
    pub providers: ProviderConfigs,
    pub network: NetworkConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct WorkflowConfig {
    pub fast_mode: bool,
    pub context_management: ContextManagementConfig,
    pub claude_code: ClaudeCodeWorkflowConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ContextManagementConfig {
    pub enabled: bool,
    pub mode: ContextManagementMode,
    pub default_edits: Vec<serde_json::Value>,
    pub override_edits: Option<Vec<serde_json::Value>>,
    pub hard_limits: ContextManagementHardLimits,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ClaudeCodeWorkflowConfig {
    pub slash_commands: ClaudeCodeSlashCommandConfig,
    pub skills: ClaudeCodeSkillConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClaudeCodeSlashCommandConfig {
    pub enabled: bool,
    pub mode: ClaudeCodeSlashCommandMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCodeSlashCommandMode {
    #[default]
    PromoteLatest,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClaudeCodeSkillConfig {
    pub enabled: bool,
    pub mode: ClaudeCodeSkillMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCodeSkillMode {
    #[default]
    PromoteActive,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextManagementMode {
    FollowRequest,
    OverrideRequest,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ContextManagementHardLimits {
    pub max_tool_result_chars: Option<usize>,
    pub max_tool_uses_to_keep: Option<usize>,
    pub max_thinking_turns_to_keep: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ProviderConfigs {
    pub openai: OpenAiProviderConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct NetworkConfig {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: SocketAddr,
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OpenAiProviderConfig {
    #[serde(default = "default_openai_model")]
    pub default_model: String,
    #[serde(default = "default_unsupported_models")]
    pub unsupported_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelResolution {
    pub requested: String,
    pub selected_backend_model: String,
    pub selection_reason: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayConfigError {
    #[error("failed to read gateway config")]
    Io(#[from] std::io::Error),
    #[error("failed to parse gateway config")]
    Yaml(#[from] serde_yaml::Error),
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            workflow: WorkflowConfig::default(),
            providers: ProviderConfigs::default(),
            network: NetworkConfig::default(),
        }
    }
}

impl Default for ContextManagementConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: ContextManagementMode::FollowRequest,
            default_edits: Vec::new(),
            override_edits: None,
            hard_limits: ContextManagementHardLimits::default(),
        }
    }
}

impl Default for ClaudeCodeSlashCommandConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: ClaudeCodeSlashCommandMode::PromoteLatest,
        }
    }
}

impl Default for ClaudeCodeSkillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: ClaudeCodeSkillMode::PromoteActive,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            allowed_hosts: Vec::default(),
        }
    }
}

impl Default for OpenAiProviderConfig {
    fn default() -> Self {
        Self {
            default_model: default_openai_model(),
            unsupported_models: default_unsupported_models(),
        }
    }
}

#[must_use]
fn default_config_version() -> u32 {
    1
}

#[must_use]
fn default_listen_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8080))
}

#[must_use]
fn default_openai_model() -> String {
    DEFAULT_BACKEND_MODEL.to_string()
}

#[must_use]
fn default_unsupported_models() -> Vec<String> {
    UNSUPPORTED_BACKEND_MODELS
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn gateway_config_path_from_sources(
    config_path: Option<String>,
    gateway_home: Option<String>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = config_path {
        return PathBuf::from(path);
    }

    if let Some(gateway_home) = gateway_home {
        return PathBuf::from(gateway_home).join("config.yml");
    }

    let home = home_dir.unwrap_or_else(|| PathBuf::from("."));
    home.join(".gateway").join("config-dev.yml")
}

#[must_use]
pub fn default_gateway_config_path() -> PathBuf {
    gateway_config_path_from_sources(
        std::env::var("GATEWAY_CONFIG_PATH").ok(),
        std::env::var("GATEWAY_HOME").ok(),
        dirs::home_dir(),
    )
}

/// Loads gateway runtime configuration from disk.
///
/// # Errors
///
/// Returns an error if the config file exists but cannot be read or parsed as YAML.
pub fn load_gateway_config(path: &Path) -> Result<GatewayConfig, GatewayConfigError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_yaml::from_slice::<GatewayConfig>(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(GatewayConfig::default()),
        Err(error) => Err(GatewayConfigError::Io(error)),
    }
}

/// Loads gateway runtime configuration from the configured default path.
///
/// # Errors
///
/// Returns an error if the config file exists but cannot be read or parsed as YAML.
pub fn load_gateway_config_default_path() -> Result<GatewayConfig, GatewayConfigError> {
    load_gateway_config(&default_gateway_config_path())
}

#[must_use]
pub fn resolve_model(config: &GatewayConfig, requested: &str) -> ModelResolution {
    let openai = &config.providers.openai;
    if openai
        .unsupported_models
        .iter()
        .any(|model| model == requested)
    {
        return ModelResolution {
            requested: requested.to_string(),
            selected_backend_model: openai.default_model.clone(),
            selection_reason: "unsupported_model_compat_override",
        };
    }

    ModelResolution {
        requested: requested.to_string(),
        selected_backend_model: requested.to_string(),
        selection_reason: "passthrough",
    }
}

#[must_use]
pub fn service_tier_for_config(config: &GatewayConfig) -> Option<String> {
    config
        .workflow
        .fast_mode
        .then(|| FAST_SERVICE_TIER.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gateway_config_{name}_{nanos}.yaml"))
    }

    fn write_yaml<T: Serialize>(path: &Path, value: &T) {
        let text = serde_yaml::to_string(value).expect("serialize config yaml");
        std::fs::write(path, text).expect("write config");
    }

    #[test]
    fn default_gateway_config_path_prefers_explicit_env_path() {
        let path = gateway_config_path_from_sources(
            Some("/tmp/explicit-config.yml".to_string()),
            Some("/tmp/gateway-home".to_string()),
            Some(PathBuf::from("/tmp/home")),
        );

        assert_eq!(path, PathBuf::from("/tmp/explicit-config.yml"));
    }

    #[test]
    fn default_gateway_config_path_uses_gateway_home_when_explicit_path_missing() {
        let path = gateway_config_path_from_sources(
            None,
            Some("/tmp/gateway-home".to_string()),
            Some(PathBuf::from("/tmp/home")),
        );

        assert_eq!(path, PathBuf::from("/tmp/gateway-home").join("config.yml"));
    }

    #[test]
    fn default_gateway_config_path_falls_back_to_dev_config_in_gateway_dir() {
        let path = gateway_config_path_from_sources(None, None, Some(PathBuf::from("/tmp/home")));

        assert_eq!(path, PathBuf::from("/tmp/home/.gateway/config-dev.yml"));
    }

    #[test]
    fn missing_config_uses_defaults() {
        let path = temp_config_path("missing");
        let config = load_gateway_config(&path).expect("load missing config");
        assert_eq!(config.version, 1);
        assert!(!config.workflow.fast_mode);
        assert!(config.workflow.context_management.enabled);
        assert!(config.workflow.claude_code.slash_commands.enabled);
        assert_eq!(
            config.workflow.claude_code.slash_commands.mode,
            ClaudeCodeSlashCommandMode::PromoteLatest
        );
        assert!(config.workflow.claude_code.skills.enabled);
        assert_eq!(
            config.workflow.claude_code.skills.mode,
            ClaudeCodeSkillMode::PromoteActive
        );
        assert_eq!(
            config.workflow.context_management.mode,
            ContextManagementMode::FollowRequest
        );
        assert!(config.workflow.context_management.default_edits.is_empty());
        assert!(config.workflow.context_management.override_edits.is_none());
        assert_eq!(config.network.listen_addr, default_listen_addr());
        assert!(config.network.allowed_hosts.is_empty());
        assert_eq!(config.providers.openai.default_model, DEFAULT_BACKEND_MODEL);
        assert_eq!(
            config.providers.openai.unsupported_models,
            vec!["gpt-5.2".to_string()]
        );
    }

    #[test]
    fn valid_config_overrides_defaults() {
        let path = temp_config_path("valid");
        let config = GatewayConfig {
            workflow: WorkflowConfig {
                fast_mode: true,
                ..WorkflowConfig::default()
            },
            providers: ProviderConfigs {
                openai: OpenAiProviderConfig {
                    default_model: "gpt-test-default".to_string(),
                    unsupported_models: vec!["gpt-test-old".to_string()],
                },
            },
            ..GatewayConfig::default()
        };
        write_yaml(&path, &config);

        let config = load_gateway_config(&path).expect("load config");
        assert!(config.workflow.fast_mode);
        assert_eq!(config.network.listen_addr, default_listen_addr());
        assert_eq!(config.providers.openai.default_model, "gpt-test-default");
        assert_eq!(
            config.providers.openai.unsupported_models,
            vec!["gpt-test-old".to_string()]
        );
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn partial_config_preserves_nested_defaults() {
        let path = temp_config_path("partial");
        write_yaml(
            &path,
            &serde_json::json!({
                "workflow": { "fast_mode": true },
                "providers": { "openai": { "default_model": "gpt-test-default" } }
            }),
        );

        let config = load_gateway_config(&path).expect("load config");
        assert_eq!(config.version, 1);
        assert!(config.workflow.fast_mode);
        assert_eq!(config.network.listen_addr, default_listen_addr());
        assert_eq!(config.providers.openai.default_model, "gpt-test-default");
        assert_eq!(
            config.providers.openai.unsupported_models,
            vec!["gpt-5.2".to_string()]
        );
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn explicit_listen_addr_parses_from_yaml() {
        let path = temp_config_path("listen_addr");
        write_yaml(
            &path,
            &serde_json::json!({
                "network": {
                    "listen_addr": "0.0.0.0:9090",
                    "allowed_hosts": ["example.com"]
                }
            }),
        );

        let config = load_gateway_config(&path).expect("load config");
        assert_eq!(
            config.network.listen_addr,
            "0.0.0.0:9090"
                .parse::<SocketAddr>()
                .expect("parse socket addr")
        );
        assert_eq!(
            config.network.allowed_hosts,
            vec!["example.com".to_string()]
        );
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn context_management_config_parses_from_yaml() {
        let path = temp_config_path("context_management");
        let config = GatewayConfig {
            workflow: WorkflowConfig {
                context_management: ContextManagementConfig {
                    mode: ContextManagementMode::OverrideRequest,
                    default_edits: vec![serde_json::json!({
                        "type": "clear_tool_uses_20250919"
                    })],
                    override_edits: Some(vec![serde_json::json!({
                        "type": "clear_thinking_20251015",
                        "keep": {
                            "type": "thinking_turns",
                            "value": 2
                        }
                    })]),
                    hard_limits: ContextManagementHardLimits {
                        max_tool_result_chars: Some(1000),
                        max_tool_uses_to_keep: Some(10),
                        max_thinking_turns_to_keep: Some(3),
                    },
                    ..ContextManagementConfig::default()
                },
                ..WorkflowConfig::default()
            },
            ..GatewayConfig::default()
        };
        write_yaml(&path, &config);

        let config = load_gateway_config(&path).expect("load config");
        assert_eq!(
            config.workflow.context_management.mode,
            ContextManagementMode::OverrideRequest
        );
        assert_eq!(config.workflow.context_management.default_edits.len(), 1);
        assert_eq!(
            config
                .workflow
                .context_management
                .override_edits
                .as_ref()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            config
                .workflow
                .context_management
                .hard_limits
                .max_tool_result_chars,
            Some(1000)
        );
        assert_eq!(
            config
                .workflow
                .context_management
                .hard_limits
                .max_tool_uses_to_keep,
            Some(10)
        );
        assert_eq!(
            config
                .workflow
                .context_management
                .hard_limits
                .max_thinking_turns_to_keep,
            Some(3)
        );
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn claude_code_config_parses_from_yaml() {
        let path = temp_config_path("claude_code");
        write_yaml(
            &path,
            &serde_json::json!({
                "workflow": {
                    "claude_code": {
                        "slash_commands": {
                            "enabled": false,
                            "mode": "promote_latest"
                        },
                        "skills": {
                            "enabled": false,
                            "mode": "promote_active"
                        }
                    }
                }
            }),
        );

        let config = load_gateway_config(&path).expect("load config");
        assert!(!config.workflow.claude_code.slash_commands.enabled);
        assert_eq!(
            config.workflow.claude_code.slash_commands.mode,
            ClaudeCodeSlashCommandMode::PromoteLatest
        );
        assert!(!config.workflow.claude_code.skills.enabled);
        assert_eq!(
            config.workflow.claude_code.skills.mode,
            ClaudeCodeSkillMode::PromoteActive
        );
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn invalid_yaml_errors_clearly() {
        let path = temp_config_path("invalid");
        std::fs::write(&path, [0x80]).expect("write config");
        let error = load_gateway_config(&path).expect_err("invalid config should error");
        assert!(matches!(error, GatewayConfigError::Yaml(_)));
        std::fs::remove_file(path).expect("remove config");
    }

    #[test]
    fn unsupported_model_uses_configured_default() {
        let config = GatewayConfig {
            providers: ProviderConfigs {
                openai: OpenAiProviderConfig {
                    default_model: "gpt-test-default".to_string(),
                    unsupported_models: vec!["gpt-test-old".to_string()],
                },
            },
            ..GatewayConfig::default()
        };

        let resolution = resolve_model(&config, "gpt-test-old");
        assert_eq!(resolution.requested, "gpt-test-old");
        assert_eq!(resolution.selected_backend_model, "gpt-test-default");
        assert_eq!(
            resolution.selection_reason,
            "unsupported_model_compat_override"
        );
    }

    #[test]
    fn supported_model_passes_through() {
        let resolution = resolve_model(&GatewayConfig::default(), DEFAULT_BACKEND_MODEL);
        assert_eq!(resolution.requested, DEFAULT_BACKEND_MODEL);
        assert_eq!(resolution.selected_backend_model, DEFAULT_BACKEND_MODEL);
        assert_eq!(resolution.selection_reason, "passthrough");
    }

    #[test]
    fn fast_mode_uses_priority_service_tier() {
        let config = GatewayConfig {
            workflow: WorkflowConfig {
                fast_mode: true,
                ..WorkflowConfig::default()
            },
            ..GatewayConfig::default()
        };

        assert_eq!(
            service_tier_for_config(&config),
            Some(FAST_SERVICE_TIER.to_string())
        );
        assert_eq!(service_tier_for_config(&GatewayConfig::default()), None);
    }
}
