use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::types::{CatalogSnapshot, ModelOverrides, ReasoningEffort};

/// Represents a user's local `~/.cline/data/settings/providers.json` configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalProvidersConfig {
    /// Active or default provider ID if configured at root level.
    #[serde(default, alias = "defaultProvider", alias = "activeProvider")]
    pub active_provider: Option<String>,
    /// Per-provider user settings keyed by provider ID.
    #[serde(default)]
    pub providers: BTreeMap<String, LocalProviderEntry>,
}

/// Per-provider entry in `providers.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalProviderEntry {
    #[serde(default, alias = "modelId", alias = "defaultModelId", alias = "model")]
    pub model_id: Option<String>,
    #[serde(default, alias = "baseUrl", alias = "apiUrl")]
    pub base_url: Option<String>,
    #[serde(default, alias = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(default, alias = "maxInputTokens")]
    pub max_input_tokens: Option<u64>,
    #[serde(default, alias = "maxTokens", alias = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, alias = "reasoningEffort", alias = "defaultReasoningEffort")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, alias = "requiresStreaming")]
    pub requires_streaming: Option<bool>,
}

impl LocalProviderEntry {
    /// Converts this local provider entry's model limit settings into `ModelOverrides`.
    pub fn to_model_overrides(&self) -> ModelOverrides {
        ModelOverrides {
            context_window: self.context_window,
            max_input_tokens: self.max_input_tokens,
            max_output_tokens: self.max_output_tokens,
            default_reasoning_effort: self.reasoning_effort,
            reasoning_options: None,
            requires_streaming: self.requires_streaming,
        }
    }
}

/// Diagnostic item emitted when validating a `LocalProvidersConfig` against a `CatalogSnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigValidationFinding {
    pub provider_id: String,
    pub model_id: Option<String>,
    pub is_valid: bool,
    pub message: String,
}

/// Returns the default path to `~/.cline/data/settings/providers.json` if a home directory exists.
pub fn default_cline_providers_path() -> Option<PathBuf> {
    #[cfg(feature = "disk-spool")]
    {
        if let Some(base_dirs) = directories::BaseDirs::new() {
            return Some(
                base_dirs
                    .home_dir()
                    .join(".cline")
                    .join("data")
                    .join("settings")
                    .join("providers.json"),
            );
        }
        None
    }
    #[cfg(not(feature = "disk-spool"))]
    {
        std::env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join(".cline")
                .join("data")
                .join("settings")
                .join("providers.json")
        })
    }
}

/// Parses a `LocalProvidersConfig` from a JSON string.
/// Supports both `{ "providers": { ... } }` and flat `{ "<provider_id>": { ... } }` layouts.
pub fn parse_local_providers_str(json_str: &str) -> Result<LocalProvidersConfig, CatalogError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawLayout {
        Structured {
            #[serde(default, alias = "defaultProvider", alias = "activeProvider")]
            active_provider: Option<String>,
            providers: BTreeMap<String, LocalProviderEntry>,
        },
        Flat(BTreeMap<String, LocalProviderEntry>),
    }

    let parsed: RawLayout =
        serde_json::from_str(json_str).map_err(|source| CatalogError::Parse {
            source_name: "providers.json".to_string(),
            source,
        })?;

    match parsed {
        RawLayout::Structured {
            active_provider,
            providers,
        } => Ok(LocalProvidersConfig {
            active_provider,
            providers,
        }),
        RawLayout::Flat(providers) => Ok(LocalProvidersConfig {
            active_provider: None,
            providers,
        }),
    }
}

/// Reads and parses a local `providers.json` file from `path`.
pub fn read_local_providers_file(path: &Path) -> Result<LocalProvidersConfig, CatalogError> {
    let content = std::fs::read_to_string(path).map_err(|source| CatalogError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_local_providers_str(&content)
}

/// Validates a `LocalProvidersConfig` against a `CatalogSnapshot`, checking whether each configured
/// provider and model ID exist in the catalog and resolving default models.
pub fn validate_local_config(
    config: &LocalProvidersConfig,
    catalog: &CatalogSnapshot,
) -> Vec<ConfigValidationFinding> {
    let mut findings = Vec::new();

    for (provider_id, entry) in &config.providers {
        let Some(provider_spec) = catalog.get_provider(provider_id) else {
            findings.push(ConfigValidationFinding {
                provider_id: provider_id.clone(),
                model_id: entry.model_id.clone(),
                is_valid: false,
                message: format!("Provider `{provider_id}` is not present in the catalog"),
            });
            continue;
        };

        if let Some(model_id) = entry
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if provider_spec.models.contains_key(model_id) || catalog.find_model(model_id).is_some()
            {
                findings.push(ConfigValidationFinding {
                    provider_id: provider_id.clone(),
                    model_id: Some(model_id.to_string()),
                    is_valid: true,
                    message: format!(
                        "Configured model `{model_id}` resolved for provider `{provider_id}`"
                    ),
                });
            } else {
                findings.push(ConfigValidationFinding {
                    provider_id: provider_id.clone(),
                    model_id: Some(model_id.to_string()),
                    is_valid: false,
                    message: format!(
                        "Configured model `{model_id}` was not found under provider `{provider_id}`"
                    ),
                });
            }
        } else {
            let resolved = catalog.resolve_default_model(provider_id, None);
            findings.push(ConfigValidationFinding {
                provider_id: provider_id.clone(),
                model_id: resolved.clone(),
                is_valid: resolved.is_some(),
                message: match resolved {
                    Some(def) => {
                        format!("No explicit model set; resolves to default `{def}`")
                    }
                    None => format!(
                        "No explicit model set and no fallback default available for `{provider_id}`"
                    ),
                },
            });
        }
    }

    findings
}

