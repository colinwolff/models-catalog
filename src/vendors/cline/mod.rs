pub mod builtins;
pub mod feed;
pub(crate) mod merge;
pub mod settings;

use std::collections::BTreeMap;

use crate::config::DEFAULT_CLINE_RECOMMENDED_URL;
use crate::error::CatalogError;
use crate::types::ProviderSpec;
use crate::vendors::{VendorFeedSpec, VendorPlugin};

pub use builtins::{
    BuiltinProviderOverride, builtin_spec_overrides, get_builtin_override, infer_provider_protocol,
};
pub use feed::{
    RawClineCollection, RawClineItem, RawClineModelEntry, RawClineRecommendedFeed,
    merge_cline_feed, parse_and_merge_cline_feed,
};
pub use settings::{
    ConfigValidationFinding, LocalProviderEntry, LocalProvidersConfig,
    default_cline_providers_path, parse_local_providers_str, read_local_providers_file,
    validate_local_config,
};

/// Default source cache key for Cline's recommended/ClinePass feed.
pub const CLINE_FEED_SOURCE_NAME: &str = "cline_recommended";

/// Pluggable vendor implementation for Cline (`@cline/llms` parity, `BUILTIN_SPEC_OVERRIDES`,
/// and `api.cline.bot` recommended/ClinePass model enrichment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClineVendor {
    /// Optional HTTP URL for `recommended-models`. When `None`, only static built-in provider
    /// overrides are applied without fetching the live curated feed.
    pub feed_url: Option<String>,
}

impl Default for ClineVendor {
    fn default() -> Self {
        Self {
            feed_url: Some(DEFAULT_CLINE_RECOMMENDED_URL.to_string()),
        }
    }
}

impl ClineVendor {
    /// Creates a `ClineVendor` with a custom recommended-models endpoint URL.
    pub fn with_feed_url(url: impl Into<String>) -> Self {
        Self {
            feed_url: Some(url.into()),
        }
    }

    /// Creates a `ClineVendor` that applies static `BUILTIN_SPEC_OVERRIDES` without fetching
    /// the live `recommended-models` HTTP feed.
    pub fn without_live_feed() -> Self {
        Self { feed_url: None }
    }
}

impl VendorPlugin for ClineVendor {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn builtin_overrides(&self) -> &'static BTreeMap<String, BuiltinProviderOverride> {
        builtin_spec_overrides()
    }

    fn upstream_feed(&self) -> Option<VendorFeedSpec> {
        self.feed_url.as_ref().map(|url| VendorFeedSpec {
            source_name: CLINE_FEED_SOURCE_NAME.to_string(),
            url: url.clone(),
        })
    }

    fn apply_overlay(
        &self,
        raw_feed_json: Option<&str>,
        providers: &mut BTreeMap<String, ProviderSpec>,
    ) -> Result<(), CatalogError> {
        if let Some(json_str) = raw_feed_json.filter(|s| !s.trim().is_empty()) {
            parse_and_merge_cline_feed(json_str, providers)?;
        }
        Ok(())
    }
}
