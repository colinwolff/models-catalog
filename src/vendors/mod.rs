pub mod cline;

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

use crate::error::CatalogError;
use crate::types::ProviderSpec;
pub use crate::vendors::cline::BuiltinProviderOverride;

/// Descriptor for an optional live HTTP feed owned by a [`VendorPlugin`].
///
/// The disk spooler automatically caches `sources/<source_name>.json`, tracks its `ETag`
/// and `Last-Modified` in `spool_meta.json`, enforces `min_refresh_cooldown`, and degrades
/// gracefully if the endpoint is unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorFeedSpec {
    /// Unique cache key for this feed (e.g. `"cline_recommended"`).
    pub source_name: String,
    /// HTTP(S) endpoint URL to fetch the feed from.
    pub url: String,
}

/// Pluggable vendor overlay trait.
///
/// Implementing `VendorPlugin` allows any vendor or gateway ecosystem (such as Cline,
/// RooCode, KiloCode, Cursor, LiteLLM, or a custom enterprise catalog) to layer static
/// provider overrides, custom default models, billing tiers, and live curated model feeds
/// on top of the base `models.dev` catalog.
pub trait VendorPlugin: Debug + Send + Sync {
    /// Canonical vendor identifier (e.g., `"cline"`).
    fn id(&self) -> &'static str;

    /// Returns built-in static provider overrides for this vendor.
    fn builtin_overrides(&self) -> &'static BTreeMap<String, BuiltinProviderOverride> {
        static EMPTY: LazyLock<BTreeMap<String, BuiltinProviderOverride>> =
            LazyLock::new(BTreeMap::new);
        &EMPTY
    }

    /// Optional live upstream HTTP feed to fetch and spool alongside `models.dev`.
    fn upstream_feed(&self) -> Option<VendorFeedSpec> {
        None
    }

    /// Applies the vendor's static overrides and optional live feed JSON onto the normalized
    /// provider catalog map.
    fn apply_overlay(
        &self,
        raw_feed_json: Option<&str>,
        providers: &mut BTreeMap<String, ProviderSpec>,
    ) -> Result<(), CatalogError>;
}

/// Returns the default registered vendor plugins based on enabled Cargo features.
pub fn default_vendors() -> Vec<Arc<dyn VendorPlugin>> {
    #[cfg(feature = "cline")]
    {
        vec![Arc::new(cline::ClineVendor::default())]
    }
    #[cfg(not(feature = "cline"))]
    {
        Vec::new()
    }
}

