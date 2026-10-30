use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::CatalogError;
use crate::normalize::modalities::RawModalities;
use crate::normalize::models_dev::{RawCost, RawLimit};
use crate::types::{ProviderSpec, ReasoningEffort};
use crate::vendors::cline::merge::merge_cline_feed_impl;

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
                    RawClineItem::Object(obj) => *obj,
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
    Object(Box<RawClineModelEntry>),
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
    merge_cline_feed_impl(feed, providers);
}
