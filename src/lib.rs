//! # `models-catalog`
//!
//! Production-grade LLM model and provider catalog normalizer, disk spooler,
//! and runtime token/reasoning calculator with `@cline/llms` parity.

pub mod builtins;
#[cfg(feature = "bundled")]
pub mod bundled;
pub mod config;
pub mod error;
pub mod local_settings;
pub mod lookup;
pub mod normalize;
pub mod reasoning;
#[cfg(any(feature = "reqwest-client", feature = "disk-spool"))]
pub mod spool;
pub mod types;

pub use builtins::{
    BuiltinProviderOverride, builtin_spec_overrides, get_builtin_override, infer_provider_protocol,
};
#[cfg(feature = "bundled")]
pub use bundled::{BUNDLED_CATALOG_GZ, decode_bundled_snapshot, encode_bundled_snapshot};
pub use config::{
    DEFAULT_CLINE_RECOMMENDED_URL, DEFAULT_MODELS_DEV_URL, RateLimitConfig, SpoolConfig,
    UpstreamUrls, default_spool_dir,
};
pub use error::{CatalogError, ContextOverflowError};
pub use local_settings::{
    ConfigValidationFinding, LocalProviderEntry, LocalProvidersConfig,
    default_cline_providers_path, parse_local_providers_str, read_local_providers_file,
    validate_local_config,
};
pub use lookup::merge_overrides;
pub use normalize::{
    build_catalog_snapshot,
    limits::{
        DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS, DEFAULT_MAX_INPUT_TOKENS, DEFAULT_MAX_TOKENS,
        GATEWAY_OUTPUT_RESERVE_TOKENS, resolve_context_window, resolve_max_input_tokens,
        resolve_max_output_tokens,
    },
};
pub use reasoning::{
    MIN_THINKING_BUDGET_TOKENS, effort_to_thinking_budget, normalize_reasoning_effort,
    to_wire_reasoning_params,
};
#[cfg(any(feature = "reqwest-client", feature = "disk-spool"))]
pub use spool::{CatalogManager, CatalogManagerBuilder};
pub use types::{
    BillingTier, CATALOG_SCHEMA_VERSION, CatalogSnapshot, Cost, DataOrigin, Limit, Modalities,
    Modality, ModelInfo, ModelOverrides, ProviderProtocol, ProviderSpec, ReasoningEffort,
    SourceStatus, WireReasoningParams,
};
