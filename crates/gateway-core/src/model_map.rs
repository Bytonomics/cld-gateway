#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct ModelMap {
    #[serde(default = "default_backend_model")]
    pub default_backend_model: String,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

fn default_backend_model() -> String {
    "gpt-5.2".to_string()
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
pub fn resolve_model(map: &ModelMap, requested: &str) -> ModelResolution {
    let requested_string = requested.to_string();
    let allowed = map.allowed_backend_models();

    if allowed.contains(requested) {
        return ModelResolution {
            requested: requested_string,
            selected_backend_model: requested.to_string(),
            selection_reason: "passthrough",
        };
    }

    let normalized = normalize_model_id(requested);
    if let Some(selected) = map.aliases.get(&normalized) {
        return ModelResolution {
            requested: requested_string,
            selected_backend_model: selected.clone(),
            selection_reason: "alias_match",
        };
    }

    ModelResolution {
        requested: requested_string,
        selected_backend_model: map.default_backend_model.clone(),
        selection_reason: "fallback_default",
    }
}

fn normalize_model_id(input: &str) -> String {
    let mut out = String::new();
    let mut prev_ws = false;
    for c in input.trim().chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
            continue;
        }
        prev_ws = false;
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ModelMap, resolve_model};
    use std::collections::BTreeMap;

    #[test]
    fn passthrough_when_requested_is_allowed_backend_model() {
        let map = ModelMap {
            default_backend_model: "gpt-5.2".to_string(),
            aliases: BTreeMap::from([("sonnet".to_string(), "gpt-5.2-codex".to_string())]),
        };

        let res = resolve_model(&map, "gpt-5.2-codex");
        assert_eq!(res.selected_backend_model, "gpt-5.2-codex");
        assert_eq!(res.selection_reason, "passthrough");
    }

    #[test]
    fn alias_match_when_requested_is_alias() {
        let map = ModelMap {
            default_backend_model: "gpt-5.2".to_string(),
            aliases: BTreeMap::from([("sonnet".to_string(), "gpt-5.2-codex".to_string())]),
        };

        let res = resolve_model(&map, "Sonnet");
        assert_eq!(res.selected_backend_model, "gpt-5.2-codex");
        assert_eq!(res.selection_reason, "alias_match");
    }

    #[test]
    fn fallback_default_when_unknown() {
        let map = ModelMap {
            default_backend_model: "gpt-5.2".to_string(),
            aliases: BTreeMap::new(),
        };

        let res = resolve_model(&map, "unknown-model");
        assert_eq!(res.selected_backend_model, "gpt-5.2");
        assert_eq!(res.selection_reason, "fallback_default");
    }
}
