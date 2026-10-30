use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current schema version for serialized `CatalogSnapshot` documents.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// Provenance of a catalog source payload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(rename_all = "snake_case")]
pub enum DataOrigin {
    Live,
    NotModified304,
    DiskSpool,
    BundledFallback,
    CustomInput,
}

/// Freshness and provenance metadata for an individual upstream feed (e.g. `models_dev`, `cline_recommended`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct SourceStatus {
    pub origin: DataOrigin,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub etag: Option<String>,
    pub warning: Option<String>,
}

/// Wire protocol family used by an LLM provider.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    Anthropic,
    #[default]
    OpenAiCompatible,
    OpenAiResponses,
    Google,
    Bedrock,
    Vertex,
    Ollama,
    ClinePass,
}

/// Discrete reasoning effort tiers ordered from lowest to highest intensity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    /// Numeric ordinal rank (`0..=5`) used for distance/clamping calculations.
    pub const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::XHigh => 4,
            Self::Max => 5,
        }
    }

    /// Canonical wire string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Provider-specific wire reasoning parameter representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireReasoningParams {
    /// Reasoning disabled or omitted from wire payload.
    Disabled,
    /// Anthropic / Bedrock Claude extended thinking (`thinking: { type: "enabled", budget_tokens }`).
    AnthropicThinking { budget_tokens: u64 },
    /// OpenAI Chat Completions / Responses (`reasoning_effort: "low" | "medium" | "high" | ...`).
    OpenAiEffort { reasoning_effort: String },
    /// Zhipu GLM / DeepSeek thinking toggle (`thinking: { type: "enabled" | "disabled" }`).
    ThinkingToggle { enabled: bool },
    /// Google Gemini thinking budget (`thinkingConfig: { thinkingBudget }`).
    GoogleThinking { thinking_budget: u64 },
}

/// Raw token limit declarations from upstream catalog feeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct Limit {
    pub context: Option<u64>,
    pub input: Option<u64>,
    pub output: Option<u64>,
}

/// Known model input/output modalities.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Pdf,
}

impl Modality {
    /// Parse a case-insensitive modality string into a known `Modality`.
    pub fn from_raw(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// Directional input and output modality sets for a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct Modalities {
    pub input: Vec<Modality>,
    pub output: Vec<Modality>,
}

/// Token pricing in USD per 1M tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct Cost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub reasoning: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// Billing tier classification for Cline/OpenRouter curated models.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[serde(rename_all = "snake_case")]
pub enum BillingTier {
    Subscription,
    Free,
    Credits,
}

/// Normalized model metadata, limits, modalities, capabilities, and pricing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub family: Option<String>,
    /// Total context window (`limit.context`).
    pub context_window: Option<u64>,
    /// Resolved maximum input tokens (`min(context, input)` or fallback `128_000`).
    pub max_input_tokens: u64,
    /// Resolved maximum output tokens (`limit.output` or fallback `4_096`).
    pub max_tokens: u64,
    /// Raw upstream limits preserved for inspection.
    pub raw_limit: Limit,
    /// Sanitized directional modalities (`None` if unspecified or pruned as incomplete).
    pub modalities: Option<Modalities>,
    /// Token pricing in USD per 1M tokens.
    pub cost: Option<Cost>,
    /// True if model supports reasoning/thinking tokens.
    pub supports_reasoning: bool,
    /// Supported discrete reasoning effort levels.
    pub reasoning_options: Vec<ReasoningEffort>,
    /// Default reasoning effort if recommended by upstream/overlay.
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// True if model supports structured tool/function calling.
    pub supports_tool_call: bool,
    /// True if model supports prompt caching.
    pub supports_prompt_cache: bool,
    /// ISO-8601 release date string (`YYYY-MM-DD`) if reported.
    pub release_date: Option<String>,
    /// Billing tier annotation (`Subscription`, `Free`, `Credits`).
    pub billing_tier: Option<BillingTier>,
    /// True if curated in the recommended models feed.
    pub is_recommended: bool,
    /// True if underlying provider requires streaming for this model.
    pub requires_streaming: bool,
}

/// Normalized provider specification and its catalog of models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: Option<String>,
    pub env_vars: Vec<String>,
    pub doc_url: Option<String>,
    /// Resolved default model ID for this provider.
    pub default_model_id: Option<String>,
    /// True when the provider endpoint rejects non-streaming requests (e.g., `api.cline.bot` / `cline-pass`).
    pub requires_streaming: bool,
    /// Model IDs sorted by `release_date` descending (newest first), tie-broken lexicographically by ID.
    pub sorted_model_ids: Vec<String>,
    /// Models keyed by model ID.
    pub models: BTreeMap<String, ModelInfo>,
}

/// Complete, deterministically sorted snapshot of all normalized providers and models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct CatalogSnapshot {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub sources: BTreeMap<String, SourceStatus>,
    pub degraded: bool,
    pub providers: BTreeMap<String, ProviderSpec>,
}

/// Caller or user-config overrides that layer on top of catalog values (`override > catalog > default`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
pub struct ModelOverrides {
    pub context_window: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_options: Option<Vec<ReasoningEffort>>,
    pub requires_streaming: Option<bool>,
}
