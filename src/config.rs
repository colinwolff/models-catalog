use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::vendors::VendorPlugin;
use crate::vendors::cline::ClineVendor;

/// Default `models.dev` public API endpoint.
pub const DEFAULT_MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Default Cline curated & ClinePass recommended models endpoint.
pub const DEFAULT_CLINE_RECOMMENDED_URL: &str =
    "https://api.cline.bot/api/v1/ai/cline/recommended-models";

/// Upstream endpoint URLs used by `CatalogManager`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamUrls {
    pub models_dev_url: String,
    pub cline_recommended_url: String,
}

impl Default for UpstreamUrls {
    fn default() -> Self {
        Self {
            models_dev_url: DEFAULT_MODELS_DEV_URL.to_string(),
            cline_recommended_url: DEFAULT_CLINE_RECOMMENDED_URL.to_string(),
        }
    }
}

/// Rate-limiting and retry backoff configuration for outbound upstream HTTP requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Minimum cooldown duration between outbound HTTP requests per upstream source.
    pub min_refresh_cooldown: Duration,
    /// Maximum number of retries on HTTP `429 Too Many Requests` or `5xx` responses.
    pub max_retries: u32,
    /// Initial backoff delay before the first retry.
    pub initial_backoff: Duration,
    /// Maximum backoff delay cap.
    pub max_backoff: Duration,
    /// Per-request HTTP timeout.
    pub request_timeout: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            min_refresh_cooldown: Duration::from_secs(5 * 60),
            max_retries: 3,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Complete configuration for `CatalogManager` memory caching, disk spooling, rate limiting,
/// and pluggable vendor overlays.
#[derive(Debug, Clone)]
pub struct SpoolConfig {
    /// Duration an in-memory snapshot stays fresh before triggering revalidation (default: 15 mins).
    pub memory_ttl: Duration,
    /// Duration a disk-spooled snapshot stays fresh before triggering a network check (default: 24 hours).
    pub disk_ttl: Duration,
    /// When `true`, expired in-memory snapshots are returned immediately while a background task refreshes.
    pub stale_while_revalidate: bool,
    /// Directory where per-source snapshots, `spool_meta.json`, and `catalog.json` are stored.
    /// When `None`, disk spooling is disabled (in-memory only).
    pub spool_dir: Option<PathBuf>,
    /// Whether to fetch and merge the Cline recommended/ClinePass overlay feed.
    pub enable_cline_feed: bool,
    /// Whether to fall back to the compile-time `bundled` snapshot when network and disk caches are cold/unavailable.
    pub use_bundled_fallback: bool,
    /// Upstream URL configuration.
    pub urls: UpstreamUrls,
    /// Rate limit and retry configuration.
    pub rate_limit: RateLimitConfig,
    /// Additional registered vendor plugins layered after built-in vendor overlays.
    pub custom_vendors: Vec<Arc<dyn VendorPlugin>>,
}

impl Default for SpoolConfig {
    fn default() -> Self {
        Self {
            memory_ttl: Duration::from_secs(15 * 60),
            disk_ttl: Duration::from_secs(24 * 60 * 60),
            stale_while_revalidate: true,
            spool_dir: default_spool_dir(),
            enable_cline_feed: cfg!(feature = "cline"),
            use_bundled_fallback: cfg!(feature = "bundled"),
            urls: UpstreamUrls::default(),
            rate_limit: RateLimitConfig::default(),
            custom_vendors: Vec::new(),
        }
    }
}

impl SpoolConfig {
    /// Returns the ordered list of active [`VendorPlugin`] overlays for this configuration.
    pub fn effective_vendors(&self) -> Vec<Arc<dyn VendorPlugin>> {
        let mut list: Vec<Arc<dyn VendorPlugin>> = Vec::new();
        if self.enable_cline_feed {
            list.push(Arc::new(ClineVendor::with_feed_url(
                self.urls.cline_recommended_url.clone(),
            )));
        }
        list.extend(self.custom_vendors.iter().cloned());
        list
    }
}

/// Resolves the platform-appropriate default cache directory (`~/.cache/models-catalog` on Linux/XDG,
/// `~/Library/Caches/dev.models.models-catalog` on macOS, etc.).
pub fn default_spool_dir() -> Option<PathBuf> {
    #[cfg(feature = "disk-spool")]
    {
        if let Some(proj_dirs) = directories::ProjectDirs::from("dev", "models", "models-catalog") {
            return Some(proj_dirs.cache_dir().to_path_buf());
        }
        if let Some(base_dirs) = directories::BaseDirs::new() {
            return Some(base_dirs.cache_dir().join("models-catalog"));
        }
        None
    }
    #[cfg(not(feature = "disk-spool"))]
    {
        None
    }
}
