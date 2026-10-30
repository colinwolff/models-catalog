pub mod disk;
mod engine;
pub mod limiter;
pub mod memory;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::debug;

use crate::config::{RateLimitConfig, SpoolConfig, UpstreamUrls};
use crate::error::CatalogError;
use crate::spool::disk::DiskSpool;
use crate::spool::engine::{CatalogManagerInner, InMemorySourceCache};
use crate::spool::memory::MemoryCache;
use crate::types::CatalogSnapshot;

/// High-throughput catalog manager with in-memory SWR caching, singleflight request coalescing,
/// per-source atomic disk spooling, conditional HTTP (`ETag` / `304`), and rate limiting.
#[derive(Clone)]
pub struct CatalogManager {
    inner: Arc<CatalogManagerInner>,
}

impl std::fmt::Debug for CatalogManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogManager")
            .field("config", &self.inner.config)
            .finish()
    }
}

impl CatalogManager {
    /// Creates a new `CatalogManager` with the given `SpoolConfig`.
    pub fn new(config: SpoolConfig) -> Result<Self, CatalogError> {
        #[cfg(feature = "reqwest-client")]
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|source| CatalogError::Http {
                source_name: "reqwest-client-init".to_string(),
                source,
            })?;

        let disk = config.spool_dir.clone().map(DiskSpool::new);

        Ok(Self {
            inner: Arc::new(CatalogManagerInner {
                config,
                memory: MemoryCache::new(),
                disk,
                in_memory_spool: Mutex::new(InMemorySourceCache::default()),
                active_flight: Mutex::new(None),
                #[cfg(feature = "reqwest-client")]
                client,
            }),
        })
    }

    /// Creates a fluent `CatalogManagerBuilder`.
    pub fn builder() -> CatalogManagerBuilder {
        CatalogManagerBuilder::default()
    }

    /// Returns a reference to the manager's active `SpoolConfig`.
    pub fn config(&self) -> &SpoolConfig {
        &self.inner.config
    }

    /// Non-blocking check returning the currently cached in-memory snapshot if present.
    pub fn cached_snapshot(&self) -> Option<Arc<CatalogSnapshot>> {
        self.inner.memory.try_get()
    }

    /// Retrieves the normalized `CatalogSnapshot`.
    ///
    /// - If the in-memory snapshot is fresh (`< memory_ttl`), returns it immediately.
    /// - If the in-memory snapshot is expired and `stale_while_revalidate == true`, returns the
    ///   stale snapshot immediately and spawns a singleflight background refresh task.
    /// - Otherwise awaits a singleflight refresh across disk spool, network, and bundled fallback.
    pub async fn get_catalog(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        if let Some((snapshot, is_fresh)) =
            self.inner.memory.get(self.inner.config.memory_ttl).await
        {
            if is_fresh {
                debug!("returning fresh in-memory catalog snapshot");
                return Ok(snapshot);
            }

            if self.inner.config.stale_while_revalidate {
                debug!(
                    "in-memory catalog stale; returning stale snapshot and spawning SWR refresh"
                );
                let inner_clone = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    let _ = inner_clone.coalesced_refresh(false).await;
                });
                return Ok(snapshot);
            }
        }

        self.inner.coalesced_refresh(false).await
    }

    /// Explicitly refreshes the catalog.
    ///
    /// When `force == true`, bypasses `memory_ttl`, `disk_ttl`, and `min_refresh_cooldown`
    /// while still coalescing concurrent in-flight requests and sending `If-None-Match` if cached.
    pub async fn refresh(&self, force: bool) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        self.inner.coalesced_refresh(force).await
    }
}

/// Fluent builder for `CatalogManager`.
#[derive(Debug, Clone, Default)]
pub struct CatalogManagerBuilder {
    config: SpoolConfig,
}

impl CatalogManagerBuilder {
    pub fn with_config(mut self, config: SpoolConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_spool_dir(mut self, spool_dir: impl Into<Option<PathBuf>>) -> Self {
        self.config.spool_dir = spool_dir.into();
        self
    }

    pub fn without_disk_spool(mut self) -> Self {
        self.config.spool_dir = None;
        self
    }

    pub fn with_memory_ttl(mut self, ttl: Duration) -> Self {
        self.config.memory_ttl = ttl;
        self
    }

    pub fn with_disk_ttl(mut self, ttl: Duration) -> Self {
        self.config.disk_ttl = ttl;
        self
    }

    pub fn with_min_refresh_cooldown(mut self, cooldown: Duration) -> Self {
        self.config.rate_limit.min_refresh_cooldown = cooldown;
        self
    }

    pub fn with_stale_while_revalidate(mut self, swr: bool) -> Self {
        self.config.stale_while_revalidate = swr;
        self
    }

    pub fn with_cline_feed(mut self, enabled: bool) -> Self {
        self.config.enable_cline_feed = enabled;
        self
    }

    pub fn with_bundled_fallback(mut self, enabled: bool) -> Self {
        self.config.use_bundled_fallback = enabled;
        self
    }

    pub fn with_urls(mut self, urls: UpstreamUrls) -> Self {
        self.config.urls = urls;
        self
    }

    pub fn with_models_dev_url(mut self, url: impl Into<String>) -> Self {
        self.config.urls.models_dev_url = url.into();
        self
    }

    pub fn with_cline_recommended_url(mut self, url: impl Into<String>) -> Self {
        self.config.urls.cline_recommended_url = url.into();
        self
    }

    pub fn with_rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.config.rate_limit = rate_limit;
        self
    }

    pub fn build(self) -> Result<CatalogManager, CatalogError> {
        CatalogManager::new(self.config)
    }
}
