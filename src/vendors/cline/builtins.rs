use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::types::ProviderProtocol;

const BUILTIN_SPECS_JSON: &str = include_str!("builtin_specs.json");

/// Built-in provider specification override (e.g., synced from `@cline/llms` `BUILTIN_SPEC_OVERRIDES`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct BuiltinProviderOverride {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: Option<String>,
    pub env_vars: Vec<String>,
    pub default_model_id: Option<String>,
    pub requires_streaming: bool,
}

static BUILTIN_SPEC_OVERRIDES: LazyLock<BTreeMap<String, BuiltinProviderOverride>> =
    LazyLock::new(|| serde_json::from_str(BUILTIN_SPECS_JSON).unwrap_or_default());

/// Returns the full map of Cline built-in provider specification overrides.
pub fn builtin_spec_overrides() -> &'static BTreeMap<String, BuiltinProviderOverride> {
    &BUILTIN_SPEC_OVERRIDES
}

/// Looks up a built-in provider override by provider ID.
pub fn get_builtin_override(provider_id: &str) -> Option<&'static BuiltinProviderOverride> {
    BUILTIN_SPEC_OVERRIDES.get(provider_id)
}

/// Infers the wire `ProviderProtocol` for a provider ID, consulting `BUILTIN_SPEC_OVERRIDES`
/// first and falling back to well-known provider ID conventions or `OpenAiCompatible`.
pub fn infer_provider_protocol(provider_id: &str, npm_package: Option<&str>) -> ProviderProtocol {
    if let Some(spec) = get_builtin_override(provider_id) {
        return spec.protocol;
    }

    match provider_id {
        "anthropic" => ProviderProtocol::Anthropic,
        "openai" | "openai-native" => ProviderProtocol::OpenAiResponses,
        "google" | "gemini" => ProviderProtocol::Google,
        "bedrock" | "amazon-bedrock" => ProviderProtocol::Bedrock,
        "vertex" | "google-vertex" | "google-vertex-anthropic" => ProviderProtocol::Vertex,
        "ollama" => ProviderProtocol::Ollama,
        "cline" | "cline-pass" => ProviderProtocol::ClinePass,
        _ => match npm_package.unwrap_or_default() {
            "@ai-sdk/anthropic" => ProviderProtocol::Anthropic,
            "@ai-sdk/google" => ProviderProtocol::Google,
            "@ai-sdk/google-vertex" => ProviderProtocol::Vertex,
            "@ai-sdk/amazon-bedrock" => ProviderProtocol::Bedrock,
            _ => ProviderProtocol::OpenAiCompatible,
        },
    }
}

