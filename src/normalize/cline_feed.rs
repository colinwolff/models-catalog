use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::builtins::get_builtin_override;
use crate::error::CatalogError;
use crate::normalize::limits::{
    DEFAULT_MAX_INPUT_TOKENS, DEFAULT_MAX_TOKENS, resolve_context_window, resolve_max_input_tokens,
    resolve_max_output_tokens,
};
use crate::normalize::modalities::{RawModalities, sanitize_modalities};
use crate::normalize::models_dev::{RawCost, RawLimit};
use crate::normalize::resolution::{
    resolve_provider_default_model, sort_model_ids_by_release_date,
};
use crate::types::{
    BillingTier, Cost, Limit, ModelInfo, ProviderProtocol, ProviderSpec, ReasoningEffort,
};

/// Top-level payload from `https://api.cline.bot/api/v1/ai/cline/recommended-models`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawClineRecommendedFeed {
    #[serde(default)]
    pub recommended: RawClineCollection,
    #[serde(default)]
    pub free: RawClineCollection,
    #[serde(default, alias = "cline_pass", alias = "clinePass")]
    pub cline_pass: RawClineCollection,
}

/// Supports arrays of strings/objects or maps keyed by model ID.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RawClineCollection {
    List(Vec<RawClineItem>),
    Map(BTreeMap<String, RawClineModelEntry>),
}

impl Default for RawClineCollection {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl RawClineCollection {
    pub fn into_entries(self) -> Vec<RawClineModelEntry> {
        match self {
            Self::List(items) => items
                .into_iter()
                .map(|item| match item {
                    RawClineItem::Id(id) => RawClineModelEntry {
                        id: Some(id),
                        ..Default::default()
                    },
                    RawClineItem::Object(obj) => obj,
                })
                .collect(),
            Self::Map(map) => map
                .into_iter()
                .map(|(k, mut v)| {
                    if v.id
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default()
                        .is_empty()
                    {
                        v.id = Some(k);
                    }
                    v
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RawClineItem {
    Id(String),
    Object(RawClineModelEntry),
}

/// Optional metadata override entry inside the Cline recommended/free/clinePass feed.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawClineModelEntry {
    #[serde(default, alias = "modelId", alias = "model_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default, alias = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(default, alias = "maxInputTokens")]
    pub max_input_tokens: Option<u64>,
    #[serde(default, alias = "maxTokens", alias = "maxOutputTokens")]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub limit: Option<RawLimit>,
    #[serde(default)]
    pub modalities: Option<RawModalities>,
    #[serde(default)]
    pub cost: Option<RawCost>,
    #[serde(default, alias = "supportsReasoning")]
    pub reasoning: Option<bool>,
    #[serde(default, alias = "reasoningEfforts", alias = "efforts")]
    pub reasoning_options: Option<Vec<ReasoningEffort>>,
    #[serde(default, alias = "defaultReasoningEffort")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, alias = "supportsToolCall", alias = "toolCall")]
    pub tool_call: Option<bool>,
    #[serde(default, alias = "supportsPromptCache", alias = "promptCache")]
    pub prompt_cache: Option<bool>,
    #[serde(default, alias = "releaseDate")]
    pub release_date: Option<String>,
}

/// Parses `api.cline.bot/api/v1/ai/cline/recommended-models` and merges it into `providers`.
pub fn parse_and_merge_cline_feed(
    json_str: &str,
    providers: &mut BTreeMap<String, ProviderSpec>,
) -> Result<(), CatalogError> {
    let raw_feed: RawClineRecommendedFeed =
        serde_json::from_str(json_str).map_err(|source| CatalogError::Parse {
            source_name: "cline-recommended".to_string(),
            source,
        })?;

    merge_cline_feed(raw_feed, providers);
    Ok(())
}

/// Merges a deserialized `RawClineRecommendedFeed` into the provider catalog map.
pub fn merge_cline_feed(
    feed: RawClineRecommendedFeed,
    providers: &mut BTreeMap<String, ProviderSpec>,
) {
    ensure_target_provider(providers, "cline-pass");
    ensure_target_provider(providers, "cline");
    ensure_target_provider(providers, "openrouter");

    // 1. Process `recommended` entries
    for entry in feed.recommended.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let raw_id = raw_id.to_string();
        upsert_curated_model(
            providers,
            &raw_id,
            &entry,
            BillingTier::Credits,
            true,
            false,
        );
    }

    // 2. Process `free` entries
    for entry in feed.free.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let raw_id = raw_id.to_string();
        upsert_curated_model(providers, &raw_id, &entry, BillingTier::Free, false, false);
    }

    // 3. Process `clinePass` entries (always retain `cline-pass/<slug>` wire ID)
    for entry in feed.cline_pass.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let wire_id = if raw_id.starts_with("cline-pass/") {
            raw_id.to_string()
        } else {
            format!("cline-pass/{raw_id}")
        };
        upsert_curated_model(
            providers,
            &wire_id,
            &entry,
            BillingTier::Subscription,
            false,
            true,
        );
    }

    // Re-sort and re-resolve default models on updated providers
    for provider_id in ["cline-pass", "cline", "openrouter"] {
        if let Some(spec) = providers.get_mut(provider_id) {
            spec.sorted_model_ids = sort_model_ids_by_release_date(&spec.models);
            spec.default_model_id = resolve_provider_default_model(spec, None, true);
        }
    }
}

fn ensure_target_provider(providers: &mut BTreeMap<String, ProviderSpec>, provider_id: &str) {
    if providers.contains_key(provider_id) {
        if provider_id == "cline-pass" || provider_id == "cline" {
            if let Some(spec) = providers.get_mut(provider_id) {
                spec.requires_streaming = true;
                spec.protocol = ProviderProtocol::ClinePass;
            }
        }
        return;
    }

    let builtin = get_builtin_override(provider_id);
    let requires_streaming = builtin
        .map(|b| b.requires_streaming)
        .unwrap_or(provider_id == "cline-pass" || provider_id == "cline");
    let protocol = builtin.map(|b| b.protocol).unwrap_or_else(|| {
        if provider_id == "cline-pass" || provider_id == "cline" {
            ProviderProtocol::ClinePass
        } else {
            ProviderProtocol::OpenAiCompatible
        }
    });

    providers.insert(
        provider_id.to_string(),
        ProviderSpec {
            id: provider_id.to_string(),
            name: builtin
                .map(|b| b.name.clone())
                .unwrap_or_else(|| provider_id.to_string()),
            protocol,
            base_url: builtin.and_then(|b| b.base_url.clone()),
            env_vars: builtin.map(|b| b.env_vars.clone()).unwrap_or_default(),
            doc_url: None,
            default_model_id: builtin.and_then(|b| b.default_model_id.clone()),
            requires_streaming,
            sorted_model_ids: Vec::new(),
            models: BTreeMap::new(),
        },
    );
}

fn upsert_curated_model(
    providers: &mut BTreeMap<String, ProviderSpec>,
    raw_id: &str,
    entry: &RawClineModelEntry,
    tier: BillingTier,
    mark_recommended: bool,
    is_cline_pass: bool,
) {
    let base_template = find_upstream_reference_model(providers, raw_id);

    let slug_without_prefix = raw_id.strip_prefix("cline-pass/").unwrap_or(raw_id);

    // 1. Insert/update into `cline-pass` and `cline` catalogs
    for cline_prov in ["cline-pass", "cline"] {
        let target_id = if is_cline_pass {
            if raw_id.starts_with("cline-pass/") {
                raw_id.to_string()
            } else {
                format!("cline-pass/{raw_id}")
            }
        } else {
            raw_id.to_string()
        };

        let existing = providers
            .get(cline_prov)
            .and_then(|p| p.models.get(&target_id))
            .cloned();

        let merged = build_enriched_model(
            cline_prov,
            &target_id,
            existing.as_ref().or(base_template.as_ref()),
            entry,
            tier,
            mark_recommended,
            true,
        );

        if let Some(spec) = providers.get_mut(cline_prov) {
            spec.models.insert(target_id, merged);
        }
    }

    // 2. Insert/update into `openrouter` catalog
    let openrouter_id = if is_cline_pass {
        raw_id.to_string()
    } else {
        slug_without_prefix.to_string()
    };

    let existing_or = providers
        .get("openrouter")
        .and_then(|p| p.models.get(&openrouter_id))
        .cloned();

    let merged_or = build_enriched_model(
        "openrouter",
        &openrouter_id,
        existing_or.as_ref().or(base_template.as_ref()),
        entry,
        tier,
        mark_recommended,
        is_cline_pass,
    );

    if let Some(or_spec) = providers.get_mut("openrouter") {
        or_spec.models.insert(openrouter_id.clone(), merged_or);

        // If this is a cline-pass model whose underlying slug also exists in openrouter,
        // annotate the underlying openrouter model if it doesn't have a tier yet.
        if is_cline_pass && slug_without_prefix != openrouter_id {
            if let Some(underlying) = or_spec.models.get_mut(slug_without_prefix) {
                if mark_recommended {
                    underlying.is_recommended = true;
                }
                if underlying.billing_tier.is_none() {
                    underlying.billing_tier = Some(tier);
                }
            }
        }
    }
}

/// Cross-references `models.dev` providers to find rich metadata for a curated/ClinePass model ID.
fn find_upstream_reference_model(
    providers: &BTreeMap<String, ProviderSpec>,
    raw_id: &str,
) -> Option<ModelInfo> {
    let stripped_pass = raw_id.strip_prefix("cline-pass/").unwrap_or(raw_id);
    let stripped_free = stripped_pass.strip_suffix(":free").unwrap_or(stripped_pass);

    let candidates = [raw_id, stripped_pass, stripped_free];

    // 1. Check `cline-pass` and `openrouter` first
    for prov_id in ["cline-pass", "openrouter"] {
        if let Some(prov) = providers.get(prov_id) {
            for cand in candidates {
                if let Some(m) = prov.models.get(cand) {
                    return Some(m.clone());
                }
            }
        }
    }

    // 2. If candidate has `<vendor>/<model_slug>`, check native `<vendor>` provider
    for cand in [stripped_pass, stripped_free] {
        if let Some((vendor, model_slug)) = cand.split_once('/') {
            let vendor_candidates = match vendor {
                "z-ai" | "zai-org" | "zhipu" => vec![vendor, "zai", "zhipuai"],
                "x-ai" => vec![vendor, "xai"],
                "google" => vec!["google", "vertex", "google-vertex"],
                "openai" => vec!["openai", "openai-native"],
                "anthropic" => vec!["anthropic"],
                _ => vec![vendor],
            };
            for v in vendor_candidates {
                if let Some(prov) = providers.get(v) {
                    if let Some(m) = prov
                        .models
                        .get(model_slug)
                        .or_else(|| prov.models.get(cand))
                    {
                        return Some(m.clone());
                    }
                }
            }
        }
    }

    // 3. Scan all providers for exact `model_id` or suffix match
    let basename = stripped_free.rsplit('/').next().unwrap_or(stripped_free);
    for prov in providers.values() {
        for cand in [stripped_pass, stripped_free, basename] {
            if let Some(m) = prov.models.get(cand) {
                return Some(m.clone());
            }
        }
    }

    None
}

fn build_enriched_model(
    provider_id: &str,
    model_id: &str,
    base: Option<&ModelInfo>,
    entry: &RawClineModelEntry,
    tier: BillingTier,
    mark_recommended: bool,
    requires_streaming: bool,
) -> ModelInfo {
    let family = entry
        .family
        .clone()
        .or_else(|| base.and_then(|b| b.family.clone()));

    let raw_limit = if let Some(l) = &entry.limit {
        Limit {
            context: l
                .context
                .or(entry.context_window)
                .or_else(|| base.and_then(|b| b.raw_limit.context)),
            input: l
                .input
                .or(entry.max_input_tokens)
                .or_else(|| base.and_then(|b| b.raw_limit.input)),
            output: l
                .output
                .or(entry.max_tokens)
                .or_else(|| base.and_then(|b| b.raw_limit.output)),
        }
    } else {
        Limit {
            context: entry
                .context_window
                .or_else(|| base.and_then(|b| b.context_window)),
            input: entry
                .max_input_tokens
                .or_else(|| base.and_then(|b| b.raw_limit.input)),
            output: entry
                .max_tokens
                .or_else(|| base.and_then(|b| b.raw_limit.output)),
        }
    };

    let context_window =
        resolve_context_window(&raw_limit).or_else(|| base.and_then(|b| b.context_window));
    let max_input_tokens = if raw_limit.context.is_some() || raw_limit.input.is_some() {
        resolve_max_input_tokens(&raw_limit)
    } else {
        base.map(|b| b.max_input_tokens)
            .unwrap_or(DEFAULT_MAX_INPUT_TOKENS)
    };
    let max_tokens = if raw_limit.output.is_some() {
        resolve_max_output_tokens(&raw_limit)
    } else {
        base.map(|b| b.max_tokens).unwrap_or(DEFAULT_MAX_TOKENS)
    };

    let modalities = if entry.modalities.is_some() {
        sanitize_modalities(entry.modalities.as_ref(), model_id, family.as_deref())
    } else {
        base.and_then(|b| b.modalities.clone())
    };

    let cost = if let Some(c) = &entry.cost {
        Some(Cost {
            input: c.input,
            output: c.output,
            reasoning: c.reasoning,
            cache_read: c.cache_read,
            cache_write: c.cache_write,
        })
    } else {
        base.and_then(|b| b.cost.clone())
    };

    let supports_reasoning = entry
        .reasoning
        .or_else(|| entry.reasoning_options.as_ref().map(|o| !o.is_empty()))
        .or_else(|| base.map(|b| b.supports_reasoning))
        .unwrap_or(false);

    let reasoning_options = entry
        .reasoning_options
        .clone()
        .filter(|o| !o.is_empty())
        .or_else(|| {
            base.map(|b| b.reasoning_options.clone())
                .filter(|o| !o.is_empty())
        })
        .unwrap_or_else(|| {
            if supports_reasoning {
                vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ]
            } else {
                Vec::new()
            }
        });

    let default_reasoning_effort = entry
        .default_reasoning_effort
        .or_else(|| base.and_then(|b| b.default_reasoning_effort));

    let supports_tool_call = entry
        .tool_call
        .or_else(|| base.map(|b| b.supports_tool_call))
        .unwrap_or(false);

    let supports_prompt_cache = entry
        .prompt_cache
        .or_else(|| base.map(|b| b.supports_prompt_cache))
        .unwrap_or_else(|| {
            cost.as_ref()
                .map(|c| c.cache_read.is_some() || c.cache_write.is_some())
                .unwrap_or(false)
        });

    let release_date = entry
        .release_date
        .clone()
        .or_else(|| base.and_then(|b| b.release_date.clone()));

    let name = entry
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| base.map(|b| b.name.clone()))
        .unwrap_or_else(|| model_id.to_string());

    let effective_tier = match (base.and_then(|b| b.billing_tier), tier) {
        (Some(BillingTier::Subscription), _) | (_, BillingTier::Subscription) => {
            BillingTier::Subscription
        }
        (Some(BillingTier::Free), _) | (_, BillingTier::Free) => BillingTier::Free,
        _ => BillingTier::Credits,
    };

    let is_recommended = mark_recommended || base.map(|b| b.is_recommended).unwrap_or(false);
    let requires_streaming =
        requires_streaming || base.map(|b| b.requires_streaming).unwrap_or(false);

    ModelInfo {
        id: model_id.to_string(),
        name,
        provider_id: provider_id.to_string(),
        family,
        context_window,
        max_input_tokens,
        max_tokens,
        raw_limit,
        modalities,
        cost,
        supports_reasoning,
        reasoning_options,
        default_reasoning_effort,
        supports_tool_call,
        supports_prompt_cache,
        release_date,
        billing_tier: Some(effective_tier),
        is_recommended,
        requires_streaming,
    }
}
