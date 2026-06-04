#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::{DEFAULT_BACKEND_MODEL, UNSUPPORTED_BACKEND_MODELS};

#[derive(Debug, Clone, Deserialize)]
pub struct ModelMap {
    #[serde(default = "default_backend_model")]
    pub default_backend_model: String,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

fn default_backend_model() -> String {
    DEFAULT_BACKEND_MODEL.to_string()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelResolution {
    pub requested: String,
    pub selected_backend_model: String,
    pub selection_reason: &'static str, // "passthrough" | "alias_match" | "fallback_default"
}

#[derive(Debug, thiserror::Error)]
pub enum ModelMapError {
    #[error("failed to read model_map.json")]
    Io(#[from] std::io::Error),
    #[error("failed to parse model_map.json")]
    Json(#[from] serde_json::Error),
}

#[must_use]
pub fn default_model_map_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".gateway").join("model_map.json")
}

/// Loads the model map from disk.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed as JSON.
pub fn load_model_map(path: &Path) -> Result<ModelMap, ModelMapError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<ModelMap>(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModelMap {
            default_backend_model: default_backend_model(),
            aliases: BTreeMap::new(),
        }),
        Err(e) => Err(ModelMapError::Io(e)),
    }
}

impl ModelMap {
    #[must_use]
    pub fn allowed_backend_models(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        out.insert(self.default_backend_model.clone());
        out.extend(self.aliases.values().cloned());
        out
    }

    #[must_use]
    pub fn supported_model_ids(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        out.extend(self.aliases.keys().cloned());
        out
    }
}

#[must_use]
pub fn resolve_model(requested: &str) -> ModelResolution {
    if UNSUPPORTED_BACKEND_MODELS.contains(&requested) {
        return ModelResolution {
            requested: requested.to_string(),
            selected_backend_model: DEFAULT_BACKEND_MODEL.to_string(),
            selection_reason: "unsupported_model_compat_override",
        };
    }

    ModelResolution {
        requested: requested.to_string(),
        selected_backend_model: requested.to_string(),
        selection_reason: "passthrough",
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BACKEND_MODEL, UNSUPPORTED_BACKEND_MODELS, resolve_model};

    #[test]
    fn always_passes_through_requested_model() {
        let res = resolve_model(DEFAULT_BACKEND_MODEL);
        assert_eq!(res.requested, DEFAULT_BACKEND_MODEL);
        assert_eq!(res.selected_backend_model, DEFAULT_BACKEND_MODEL);
        assert_eq!(res.selection_reason, "passthrough");
    }

    #[test]
    fn overrides_unsupported_gpt_5_2() {
        let unsupported_model = UNSUPPORTED_BACKEND_MODELS[0];
        let res = resolve_model(unsupported_model);
        assert_eq!(res.requested, unsupported_model);
        assert_eq!(res.selected_backend_model, DEFAULT_BACKEND_MODEL);
        assert_eq!(res.selection_reason, "unsupported_model_compat_override");
    }
}
