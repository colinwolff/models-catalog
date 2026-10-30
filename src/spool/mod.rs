pub mod disk;
pub mod limiter;
pub mod memory;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

use crate::config::{RateLimitConfig, SpoolConfig, UpstreamUrls};
use crate::error::CatalogError;
use crate::normalize::{cline_feed, models_dev};
use crate::spool::disk::{DiskSpool, SourceSpoolMeta, SpoolMeta};
use crate::spool::limiter::SourceFetchOutcome;
use crate::spool::memory::MemoryCache;
use crate::types::{CATALOG_SCHEMA_VERSION, CatalogSnapshot, DataOrigin, SourceStatus};

const SOURCE_MODELS_DEV: &str = "models_dev";
const SOURCE_CLINE_RECOMMENDED: &str = "cline_recommended";

#[derive(Debug, Clone)]
enum SharedFlightError {
    RateLimited {
        source_name: String,
        retry_after: DateTime<Utc>,
    },
    HttpStatus {
        source_name: String,
        status: u16,
    },
    NoCatalogAvailable,
}

impl SharedFlightError {
    fn from_catalog_error(err: &CatalogError) -> Self {
        match err {
            CatalogError::RateLimited {
                source_name,
                retry_after,
            } => Self::RateLimited {
                source_name: source_name.clone(),
                retry_after: *retry_after,
            },
            CatalogError::HttpStatus {
                source_name,
                status,
            } => Self::HttpStatus {
                source_name: source_name.clone(),
                status: *status,
            },
            _ => Self::NoCatalogAvailable,
        }
    }

    fn into_catalog_error(self) -> CatalogError {
        match self {
            Self::RateLimited {
                source_name,
                retry_after,
            } => CatalogError::RateLimited {
                source_name,
                retry_after,
            },
            Self::HttpStatus {
                source_name,
                status,
            } => CatalogError::HttpStatus {
                source_name,
                status,
            },
            Self::NoCatalogAvailable => CatalogError::NoCatalogAvailable,
        }
    }
}

#[derive(Debug, Default)]
struct FlightState {
    result: Mutex<Option<Result<Arc<CatalogSnapshot>, SharedFlightError>>>,
    notify: Notify,
}

#[derive(Debug, Default)]
struct InMemorySourceCache {
    meta: SpoolMeta,
    sources: BTreeMap<String, String>,
    catalog: Option<CatalogSnapshot>,
}

struct CatalogManagerInner {
    config: SpoolConfig,
    memory: MemoryCache,
    disk: Option<DiskSpool>,
    in_memory_spool: Mutex<InMemorySourceCache>,
    active_flight: Mutex<Option<Arc<FlightState>>>,
    #[cfg(feature = "reqwest-client")]
    client: reqwest::Client,
}

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

impl CatalogManagerInner {
    async fn coalesced_refresh(
        self: &Arc<Self>,
        force: bool,
    ) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let existing_flight = {
            let mut guard = self.active_flight.lock().await;
            if let Some(flight) = guard.as_ref() {
                debug!("joining existing singleflight catalog refresh");
                Some(Arc::clone(flight))
            } else {
                // Double-check memory cache if not forcing
                if !force {
                    if let Some((snapshot, is_fresh)) =
                        self.memory.get(self.config.memory_ttl).await
                    {
                        if is_fresh {
                            return Ok(snapshot);
                        }
                    }
                }
                let new_flight = Arc::new(FlightState::default());
                *guard = Some(Arc::clone(&new_flight));
                None
            }
        };

        if let Some(flight) = existing_flight {
            let notified = flight.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let res_guard = flight.result.lock().await;
                if let Some(res) = res_guard.as_ref() {
                    return res.clone().map_err(SharedFlightError::into_catalog_error);
                }
            }

            notified.await;
            let res_guard = flight.result.lock().await;
            return match res_guard.as_ref() {
                Some(Ok(snap)) => Ok(Arc::clone(snap)),
                Some(Err(err)) => Err(err.clone().into_catalog_error()),
                None => Err(CatalogError::NoCatalogAvailable),
            };
        }

        // We are the singleflight leader
        let refresh_res = self.perform_refresh(force).await;

        let flight_to_notify = {
            let mut guard = self.active_flight.lock().await;
            guard.take()
        };

        if let Some(flight) = flight_to_notify {
            let shared_res = match &refresh_res {
                Ok(snap) => Ok(Arc::clone(snap)),
                Err(err) => Err(SharedFlightError::from_catalog_error(err)),
            };
            {
                let mut res_guard = flight.result.lock().await;
                *res_guard = Some(shared_res);
            }
            flight.notify.notify_waiters();
        }

        refresh_res
    }

    async fn perform_refresh(&self, force: bool) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let mut meta = self.load_meta().await;
        let cached_models_dev = self.load_source(SOURCE_MODELS_DEV).await;
        let cached_cline = if self.config.enable_cline_feed {
            self.load_source(SOURCE_CLINE_RECOMMENDED).await
        } else {
            None
        };
        let cached_catalog = self.load_cached_catalog().await;

        let mut models_dev_changed = false;
        let mut cline_changed = false;
        let mut degraded = false;

        let mut sources_status = BTreeMap::new();

        // 1. Resolve `models.dev`
        let prev_md_meta = meta
            .sources
            .get(SOURCE_MODELS_DEV)
            .cloned()
            .unwrap_or_default();

        let md_outcome = self
            .resolve_single_source(
                SOURCE_MODELS_DEV,
                &self.config.urls.models_dev_url,
                &prev_md_meta,
                cached_models_dev.is_some() || cached_catalog.is_some(),
                force,
            )
            .await;

        let effective_models_dev_body = match md_outcome {
            Ok(SourceFetchOutcome::Live {
                body,
                meta: new_meta,
            }) => {
                models_dev_changed = true;
                self.save_source(SOURCE_MODELS_DEV, &body).await?;
                sources_status.insert(
                    SOURCE_MODELS_DEV.to_string(),
                    SourceStatus {
                        origin: DataOrigin::Live,
                        last_fetched_at: new_meta.last_fetched_at,
                        last_verified_at: new_meta.last_verified_at,
                        etag: new_meta.etag.clone(),
                        warning: None,
                    },
                );
                meta.sources.insert(SOURCE_MODELS_DEV.to_string(), new_meta);
                Some(body)
            }
            Ok(SourceFetchOutcome::NotModified304 { meta: new_meta }) => {
                sources_status.insert(
                    SOURCE_MODELS_DEV.to_string(),
                    SourceStatus {
                        origin: DataOrigin::NotModified304,
                        last_fetched_at: new_meta.last_fetched_at,
                        last_verified_at: new_meta.last_verified_at,
                        etag: new_meta.etag.clone(),
                        warning: None,
                    },
                );
                meta.sources.insert(SOURCE_MODELS_DEV.to_string(), new_meta);
                cached_models_dev
            }
            Ok(SourceFetchOutcome::CooldownOrFresh { meta: cur_meta }) => {
                sources_status.insert(
                    SOURCE_MODELS_DEV.to_string(),
                    SourceStatus {
                        origin: DataOrigin::DiskSpool,
                        last_fetched_at: cur_meta.last_fetched_at,
                        last_verified_at: cur_meta.last_verified_at,
                        etag: cur_meta.etag.clone(),
                        warning: None,
                    },
                );
                cached_models_dev
            }
            Err(err) => {
                warn!(error = %err, "failed to fetch models.dev upstream");
                let mut err_meta = prev_md_meta.clone();
                err_meta.last_attempt_at = Some(Utc::now());
                meta.sources.insert(SOURCE_MODELS_DEV.to_string(), err_meta);
                degraded = true;
                if cached_models_dev.is_some() || cached_catalog.is_some() {
                    sources_status.insert(
                        SOURCE_MODELS_DEV.to_string(),
                        SourceStatus {
                            origin: DataOrigin::DiskSpool,
                            last_fetched_at: prev_md_meta.last_fetched_at,
                            last_verified_at: prev_md_meta.last_verified_at,
                            etag: prev_md_meta.etag,
                            warning: Some(err.to_string()),
                        },
                    );
                    cached_models_dev
                } else {
                    None
                }
            }
        };

        // 2. Resolve `cline_recommended` (if enabled)
        let effective_cline_body = if self.config.enable_cline_feed {
            let prev_cf_meta = meta
                .sources
                .get(SOURCE_CLINE_RECOMMENDED)
                .cloned()
                .unwrap_or_default();

            let cf_outcome = self
                .resolve_single_source(
                    SOURCE_CLINE_RECOMMENDED,
                    &self.config.urls.cline_recommended_url,
                    &prev_cf_meta,
                    cached_cline.is_some() || cached_catalog.is_some(),
                    force,
                )
                .await;

            match cf_outcome {
                Ok(SourceFetchOutcome::Live {
                    body,
                    meta: new_meta,
                }) => {
                    cline_changed = true;
                    self.save_source(SOURCE_CLINE_RECOMMENDED, &body).await?;
                    sources_status.insert(
                        SOURCE_CLINE_RECOMMENDED.to_string(),
                        SourceStatus {
                            origin: DataOrigin::Live,
                            last_fetched_at: new_meta.last_fetched_at,
                            last_verified_at: new_meta.last_verified_at,
                            etag: new_meta.etag.clone(),
                            warning: None,
                        },
                    );
                    meta.sources
                        .insert(SOURCE_CLINE_RECOMMENDED.to_string(), new_meta);
                    Some(body)
                }
                Ok(SourceFetchOutcome::NotModified304 { meta: new_meta }) => {
                    sources_status.insert(
                        SOURCE_CLINE_RECOMMENDED.to_string(),
                        SourceStatus {
                            origin: DataOrigin::NotModified304,
                            last_fetched_at: new_meta.last_fetched_at,
                            last_verified_at: new_meta.last_verified_at,
                            etag: new_meta.etag.clone(),
                            warning: None,
                        },
                    );
                    meta.sources
                        .insert(SOURCE_CLINE_RECOMMENDED.to_string(), new_meta);
                    cached_cline
                }
                Ok(SourceFetchOutcome::CooldownOrFresh { meta: cur_meta }) => {
                    sources_status.insert(
                        SOURCE_CLINE_RECOMMENDED.to_string(),
                        SourceStatus {
                            origin: DataOrigin::DiskSpool,
                            last_fetched_at: cur_meta.last_fetched_at,
                            last_verified_at: cur_meta.last_verified_at,
                            etag: cur_meta.etag.clone(),
                            warning: None,
                        },
                    );
                    cached_cline
                }
                Err(err) => {
                    warn!(error = %err, "failed to fetch cline_recommended upstream; degrading gracefully");
                    let mut err_meta = prev_cf_meta.clone();
                    err_meta.last_attempt_at = Some(Utc::now());
                    meta.sources
                        .insert(SOURCE_CLINE_RECOMMENDED.to_string(), err_meta);
                    degraded = true;
                    sources_status.insert(
                        SOURCE_CLINE_RECOMMENDED.to_string(),
                        SourceStatus {
                            origin: if cached_cline.is_some() {
                                DataOrigin::DiskSpool
                            } else {
                                DataOrigin::BundledFallback
                            },
                            last_fetched_at: prev_cf_meta.last_fetched_at,
                            last_verified_at: prev_cf_meta.last_verified_at,
                            etag: prev_cf_meta.etag,
                            warning: Some(err.to_string()),
                        },
                    );
                    cached_cline
                }
            }
        } else {
            None
        };

        self.save_meta(&meta).await?;

        // Fast path: if neither upstream changed (e.g. 304 Not Modified or Cooldown/DiskFresh)
        // and we already have a normalized `CatalogSnapshot`, update its metadata without re-parsing!
        if !models_dev_changed && !cline_changed {
            if let Some(mut existing_snap) = cached_catalog {
                info!("using existing catalog snapshot without re-parsing JSON payload");
                for (k, v) in sources_status {
                    existing_snap.sources.insert(k, v);
                }
                existing_snap.degraded = degraded;
                let arc_snap = Arc::new(existing_snap);
                self.memory.touch_with(Arc::clone(&arc_snap)).await;
                return Ok(arc_snap);
            }
        }

        // If we have `models.dev` payload, normalize & merge!
        if let Some(md_json) = effective_models_dev_body {
            let mut providers = models_dev::parse_and_normalize_models_dev(
                &md_json,
                cfg!(feature = "cline") || self.config.enable_cline_feed,
            )?;

            if let Some(cf_json) = effective_cline_body {
                if let Err(err) = cline_feed::parse_and_merge_cline_feed(&cf_json, &mut providers) {
                    warn!(error = %err, "failed to parse cline_recommended feed; continuing with base catalog");
                    degraded = true;
                    if let Some(st) = sources_status.get_mut(SOURCE_CLINE_RECOMMENDED) {
                        st.warning = Some(err.to_string());
                    }
                }
            }

            let snapshot = CatalogSnapshot {
                schema_version: CATALOG_SCHEMA_VERSION,
                generated_at: Utc::now(),
                sources: sources_status,
                degraded,
                providers,
            };

            self.save_catalog(&snapshot).await?;
            let arc_snap = Arc::new(snapshot);
            self.memory.set(Arc::clone(&arc_snap)).await;
            return Ok(arc_snap);
        }

        // Fallback to existing cached catalog if available
        if let Some(mut existing_snap) = cached_catalog {
            existing_snap.degraded = true;
            let arc_snap = Arc::new(existing_snap);
            self.memory.set(Arc::clone(&arc_snap)).await;
            return Ok(arc_snap);
        }

        // Fallback to compile-time bundled snapshot if enabled
        #[cfg(feature = "bundled")]
        if self.config.use_bundled_fallback {
            info!("falling back to compile-time bundled catalog snapshot");
            let mut bundled_snap = CatalogSnapshot::bundled()?;
            bundled_snap.degraded = degraded;
            let arc_snap = Arc::new(bundled_snap);
            self.memory.set(Arc::clone(&arc_snap)).await;
            return Ok(arc_snap);
        }

        Err(CatalogError::NoCatalogAvailable)
    }

    async fn resolve_single_source(
        &self,
        source_name: &str,
        url: &str,
        prev_meta: &SourceSpoolMeta,
        has_cached_payload: bool,
        force_refresh: bool,
    ) -> Result<SourceFetchOutcome, CatalogError> {
        #[cfg(feature = "reqwest-client")]
        {
            limiter::fetch_upstream_with_policy(
                &self.client,
                source_name,
                url,
                prev_meta,
                has_cached_payload,
                &self.config,
                force_refresh,
            )
            .await
        }
        #[cfg(not(feature = "reqwest-client"))]
        {
            let _ = (source_name, url, force_refresh);
            if has_cached_payload {
                Ok(SourceFetchOutcome::CooldownOrFresh {
                    meta: prev_meta.clone(),
                })
            } else {
                Err(CatalogError::NoCatalogAvailable)
            }
        }
    }

    async fn load_meta(&self) -> SpoolMeta {
        if let Some(disk) = &self.disk {
            if let Ok(m) = disk.read_meta().await {
                if !m.sources.is_empty() {
                    return m;
                }
            }
        }
        self.in_memory_spool.lock().await.meta.clone()
    }

    async fn save_meta(&self, meta: &SpoolMeta) -> Result<(), CatalogError> {
        self.in_memory_spool.lock().await.meta = meta.clone();
        if let Some(disk) = &self.disk {
            disk.write_meta(meta).await?;
        }
        Ok(())
    }

    async fn load_source(&self, source_name: &str) -> Option<String> {
        if let Some(disk) = &self.disk {
            if let Ok(Some(s)) = disk.read_source(source_name).await {
                return Some(s);
            }
        }
        self.in_memory_spool
            .lock()
            .await
            .sources
            .get(source_name)
            .cloned()
    }

    async fn save_source(&self, source_name: &str, payload: &str) -> Result<(), CatalogError> {
        self.in_memory_spool
            .lock()
            .await
            .sources
            .insert(source_name.to_string(), payload.to_string());
        if let Some(disk) = &self.disk {
            disk.write_source(source_name, payload).await?;
        }
        Ok(())
    }

    async fn load_cached_catalog(&self) -> Option<CatalogSnapshot> {
        if let Some(mem_snap) = self.memory.try_get() {
            return Some((*mem_snap).clone());
        }
        if let Some(disk) = &self.disk {
            if let Ok(Some(snap)) = disk.read_catalog().await {
                return Some(snap);
            }
        }
        self.in_memory_spool.lock().await.catalog.clone()
    }

    async fn save_catalog(&self, snapshot: &CatalogSnapshot) -> Result<(), CatalogError> {
        self.in_memory_spool.lock().await.catalog = Some(snapshot.clone());
        if let Some(disk) = &self.disk {
            disk.write_catalog(snapshot).await?;
        }
        Ok(())
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
