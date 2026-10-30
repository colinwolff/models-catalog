use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::builtins::{get_builtin_override, infer_provider_protocol};
use crate::error::CatalogError;
use crate::normalize::limits::{
    resolve_context_window, resolve_max_input_tokens, resolve_max_output_tokens,
};
use crate::normalize::modalities::{RawModalities, sanitize_modalities};
use crate::normalize::resolution::{
    resolve_provider_default_model, sort_model_ids_by_release_date,
};
use crate::types::{BillingTier, Cost, Limit, ModelInfo, ProviderSpec, ReasoningEffort};

/// Raw provider entry from `https://models.dev/api.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawModelsDevProvider {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, RawModelsDevModel>,
}

/// Polymorphic entry inside `models.dev`'s `reasoning_options` array.
///
/// Upstream `models.dev/api.json` represents `reasoning_options` either as direct effort strings
/// (`["low", "medium", "high"]`) or as capability descriptor objects (`[{"type": "effort", "values": [...]}]`,
/// `[{"type": "toggle"}]`, `[{"type": "budget_tokens", "min": 0, "max": 32768}]`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RawReasoningOption {
    Effort(ReasoningEffort),
    Object {
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        values: Vec<String>,
    },
    Other(serde_json::Value),
}

fn parse_effort_token(s: &str) -> Option<ReasoningEffort> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "off" | "disabled" => Some(ReasoningEffort::None),
        "minimal" | "low" => Some(ReasoningEffort::Low),
        "medium" | "med" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" | "x-high" | "extra_high" => Some(ReasoningEffort::XHigh),
        "max" | "maximum" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

pub(crate) fn extract_reasoning_efforts(
    raw_opts: Option<&[RawReasoningOption]>,
) -> (bool, Vec<ReasoningEffort>) {
    let Some(opts) = raw_opts else {
        return (false, Vec::new());
    };
    let mut had_any_descriptor = false;
    let mut efforts = Vec::new();
    let mut push_unique = |e: ReasoningEffort| {
        if !efforts.contains(&e) {
            efforts.push(e);
        }
    };

    for item in opts {
        match item {
            RawReasoningOption::Effort(e) => {
                had_any_descriptor = true;
                push_unique(*e);
            }
            RawReasoningOption::Object { kind, values } => {
                had_any_descriptor = true;
                for v in values {
                    if let Some(e) = parse_effort_token(v) {
                        push_unique(e);
                    }
                }
                if values.is_empty() && kind.as_deref() == Some("toggle") {
                    push_unique(ReasoningEffort::None);
                    push_unique(ReasoningEffort::High);
                }
            }
            RawReasoningOption::Other(val) => {
                if let Some(s) = val.as_str() {
                    if let Some(e) = parse_effort_token(s) {
                        had_any_descriptor = true;
                        push_unique(e);
                    }
                }
            }
        }
    }

    (had_any_descriptor, efforts)
}

/// Raw model entry from `https://models.dev/api.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawModelsDevModel {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default, alias = "reasoning_efforts", alias = "efforts")]
    pub reasoning_options: Option<Vec<RawReasoningOption>>,
    #[serde(default)]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub tool_call: Option<bool>,
    #[serde(default)]
    pub prompt_cache: Option<bool>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub modalities: Option<RawModalities>,
    #[serde(default)]
    pub cost: Option<RawCost>,
    #[serde(default)]
    pub limit: Option<RawLimit>,
}

/// Raw cost object from `models.dev`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

/// Raw limit object from `models.dev`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

/// Parses and normalizes a raw `models.dev/api.json` JSON string into a `BTreeMap<String, ProviderSpec>`.
pub fn parse_and_normalize_models_dev(
    json_str: &str,
    apply_builtin_overrides: bool,
) -> Result<BTreeMap<String, ProviderSpec>, CatalogError> {
    let raw_map: BTreeMap<String, RawModelsDevProvider> =
        serde_json::from_str(json_str).map_err(|source| CatalogError::Parse {
            source_name: "models.dev".to_string(),
            source,
        })?;

    Ok(normalize_models_dev_map(raw_map, apply_builtin_overrides))
}

/// Normalizes a deserialized map of `models.dev` providers.
pub fn normalize_models_dev_map(
    raw_map: BTreeMap<String, RawModelsDevProvider>,
    apply_builtin_overrides: bool,
) -> BTreeMap<String, ProviderSpec> {
    let mut providers = BTreeMap::new();

    for (provider_key, raw_provider) in raw_map {
        let provider_id = raw_provider
            .id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&provider_key)
            .to_string();

        let builtin = if apply_builtin_overrides {
            get_builtin_override(&provider_id)
        } else {
            None
        };

        let protocol = if let Some(b) = builtin {
            b.protocol
        } else {
            infer_provider_protocol(&provider_id, raw_provider.npm.as_deref())
        };

        let requires_streaming = builtin.map(|b| b.requires_streaming).unwrap_or(false);

        let name = builtin
            .map(|b| b.name.clone())
            .or(raw_provider.name)
            .unwrap_or_else(|| provider_id.clone());

        let base_url = builtin
            .and_then(|b| b.base_url.clone())
            .or(raw_provider.api);

        let env_vars = if let Some(b) = builtin {
            if !b.env_vars.is_empty() {
                b.env_vars.clone()
            } else {
                raw_provider.env
            }
        } else {
            raw_provider.env
        };

        let mut models = BTreeMap::new();
        for (model_key, raw_model) in raw_provider.models {
            if (provider_id == "cline-pass" || provider_id == "cline")
                && raw_model.status.as_deref() == Some("deprecated")
            {
                continue;
            }
            let model_info =
                normalize_single_model(&provider_id, &model_key, raw_model, requires_streaming);
            models.insert(model_info.id.clone(), model_info);
        }

        let sorted_model_ids = sort_model_ids_by_release_date(&models);

        let mut spec = ProviderSpec {
            id: provider_id.clone(),
            name,
            protocol,
            base_url,
            env_vars,
            doc_url: raw_provider.doc,
            default_model_id: None,
            requires_streaming,
            sorted_model_ids,
            models,
        };

        spec.default_model_id =
            resolve_provider_default_model(&spec, None, apply_builtin_overrides);

        providers.insert(provider_id, spec);
    }

    // Ensure any builtin providers not present in models.dev (e.g., `cline-pass` or aliases like `openai-native`)
    // exist in the provider map when `apply_builtin_overrides` is enabled.
    if apply_builtin_overrides {
        ensure_builtin_provider_aliases(&mut providers);
    }

    providers
}

/// Normalizes a single model entry from `models.dev`.
pub fn normalize_single_model(
    provider_id: &str,
    model_key: &str,
    raw: RawModelsDevModel,
    provider_requires_streaming: bool,
) -> ModelInfo {
    let model_id = raw
        .id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(model_key)
        .to_string();

    let name = raw
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| model_id.clone());

    let raw_limit = Limit {
        context: raw.limit.as_ref().and_then(|l| l.context),
        input: raw.limit.as_ref().and_then(|l| l.input),
        output: raw.limit.as_ref().and_then(|l| l.output),
    };

    let context_window = resolve_context_window(&raw_limit);
    let max_input_tokens = resolve_max_input_tokens(&raw_limit);
    let max_tokens = resolve_max_output_tokens(&raw_limit);

    let modalities = sanitize_modalities(raw.modalities.as_ref(), &model_id, raw.family.as_deref());

    let cost = raw.cost.map(|c| Cost {
        input: c.input,
        output: c.output,
        reasoning: c.reasoning,
        cache_read: c.cache_read,
        cache_write: c.cache_write,
    });

    let supports_prompt_cache = raw.prompt_cache.unwrap_or_else(|| {
        cost.as_ref()
            .map(|c| c.cache_read.is_some() || c.cache_write.is_some())
            .unwrap_or(false)
    });

    let (had_reasoning_descriptor, parsed_efforts) =
        extract_reasoning_efforts(raw.reasoning_options.as_deref());

    let supports_reasoning = raw.reasoning.unwrap_or(had_reasoning_descriptor);

    let reasoning_options = if !parsed_efforts.is_empty() {
        parsed_efforts
    } else if supports_reasoning {
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ]
    } else {
        Vec::new()
    };

    let billing_tier = if provider_id == "cline-pass" {
        Some(BillingTier::Subscription)
    } else {
        None
    };

    ModelInfo {
        id: model_id,
        name,
        provider_id: provider_id.to_string(),
        family: raw.family,
        context_window,
        max_input_tokens,
        max_tokens,
        raw_limit,
        modalities,
        cost,
        supports_reasoning,
        reasoning_options,
        default_reasoning_effort: raw.default_reasoning_effort,
        supports_tool_call: raw.tool_call.unwrap_or(false),
        supports_prompt_cache,
        release_date: raw.release_date,
        billing_tier,
        is_recommended: false,
        requires_streaming: provider_requires_streaming,
    }
}

/// Ensures built-in provider aliases (such as `openai-native` mirroring `openai`,
/// `bedrock` mirroring `amazon-bedrock`, `vertex` mirroring `google-vertex`, and `cline-pass`)
/// are populated in the provider map.
fn ensure_builtin_provider_aliases(providers: &mut BTreeMap<String, ProviderSpec>) {
    let alias_pairs = [
        ("openai-native", "openai"),
        ("bedrock", "amazon-bedrock"),
        ("vertex", "google-vertex"),
    ];

    for (alias_id, source_id) in alias_pairs {
        if !providers.contains_key(alias_id) {
            if let Some(source_spec) = providers.get(source_id).cloned() {
                if let Some(builtin) = get_builtin_override(alias_id) {
                    let mut aliased_models = BTreeMap::new();
                    for (k, mut m) in source_spec.models {
                        m.provider_id = alias_id.to_string();
                        m.requires_streaming = builtin.requires_streaming;
                        aliased_models.insert(k, m);
                    }
                    let sorted_model_ids = sort_model_ids_by_release_date(&aliased_models);
                    let mut spec = ProviderSpec {
                        id: alias_id.to_string(),
                        name: builtin.name.clone(),
                        protocol: builtin.protocol,
                        base_url: builtin.base_url.clone().or(source_spec.base_url),
                        env_vars: if !builtin.env_vars.is_empty() {
                            builtin.env_vars.clone()
                        } else {
                            source_spec.env_vars
                        },
                        doc_url: source_spec.doc_url,
                        default_model_id: None,
                        requires_streaming: builtin.requires_streaming,
                        sorted_model_ids,
                        models: aliased_models,
                    };
                    spec.default_model_id = resolve_provider_default_model(&spec, None, true);
                    providers.insert(alias_id.to_string(), spec);
                }
            }
        }
    }

    for builtin_id in ["cline-pass", "cline"] {
        if !providers.contains_key(builtin_id) {
            if let Some(builtin) = get_builtin_override(builtin_id) {
                providers.insert(
                    builtin_id.to_string(),
                    ProviderSpec {
                        id: builtin_id.to_string(),
                        name: builtin.name.clone(),
                        protocol: builtin.protocol,
                        base_url: builtin.base_url.clone(),
                        env_vars: builtin.env_vars.clone(),
                        doc_url: None,
                        default_model_id: builtin.default_model_id.clone(),
                        requires_streaming: builtin.requires_streaming,
                        sorted_model_ids: Vec::new(),
                        models: BTreeMap::new(),
                    },
                );
            }
        }
    }
}
