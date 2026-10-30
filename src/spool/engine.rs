use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

use crate::config::SpoolConfig;
use crate::error::CatalogError;
use crate::normalize::models_dev;
#[cfg(feature = "reqwest-client")]
use crate::spool::limiter;
use crate::spool::{
    disk::{DiskSpool, SourceSpoolMeta, SpoolMeta},
    limiter::SourceFetchOutcome,
    memory::MemoryCache,
};
use crate::types::{CATALOG_SCHEMA_VERSION, CatalogSnapshot, DataOrigin, SourceStatus};

const SOURCE_MODELS_DEV: &str = "models_dev";

#[derive(Debug, Clone)]
pub(crate) enum SharedFlightError {
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
    pub(crate) fn from_catalog_error(err: &CatalogError) -> Self {
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

    pub(crate) fn into_catalog_error(self) -> CatalogError {
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
pub(crate) struct FlightState {
    pub(crate) result: Mutex<Option<Result<Arc<CatalogSnapshot>, SharedFlightError>>>,
    pub(crate) notify: Notify,
}

#[derive(Debug, Default)]
pub(crate) struct InMemorySourceCache {
    pub(crate) meta: SpoolMeta,
    pub(crate) sources: BTreeMap<String, String>,
    pub(crate) catalog: Option<CatalogSnapshot>,
}

pub(crate) struct CatalogManagerInner {
    pub(crate) config: SpoolConfig,
    pub(crate) memory: MemoryCache,
    pub(crate) disk: Option<DiskSpool>,
    pub(crate) in_memory_spool: Mutex<InMemorySourceCache>,
    pub(crate) active_flight: Mutex<Option<Arc<FlightState>>>,
    #[cfg(feature = "reqwest-client")]
    pub(crate) client: reqwest::Client,
}

impl CatalogManagerInner {
    pub(crate) async fn coalesced_refresh(
        self: &Arc<Self>,
        force: bool,
    ) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let existing_flight = {
            let mut guard = self.active_flight.lock().await;
            if let Some(flight) = guard.as_ref() {
                debug!("joining existing singleflight catalog refresh");
                Some(Arc::clone(flight))
            } else {
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
        let cached_catalog = self.load_cached_catalog().await;
        let active_vendors = self.config.effective_vendors();

        let mut any_source_changed = false;
        let mut degraded = false;
        let mut sources_status = BTreeMap::new();

        // 1. Refresh base `models.dev` feed
        let (effective_models_dev_body, md_changed, md_degraded) = self
            .refresh_source_step(
                SOURCE_MODELS_DEV,
                &self.config.urls.models_dev_url,
                &mut meta,
                cached_models_dev,
                cached_catalog.is_some(),
                force,
                &mut sources_status,
            )
            .await?;
        any_source_changed |= md_changed;
        degraded |= md_degraded;

        // 2. Refresh each registered VendorPlugin's upstream feed (if any)
        let mut vendor_payloads: Vec<(
            Arc<dyn crate::vendors::VendorPlugin>,
            Option<String>,
            Option<String>,
        )> = Vec::with_capacity(active_vendors.len());

        for vendor in active_vendors {
            if let Some(feed_spec) = vendor.upstream_feed() {
                let cached_vendor_body = self.load_source(&feed_spec.source_name).await;
                let (body, v_changed, v_degraded) = self
                    .refresh_source_step(
                        &feed_spec.source_name,
                        &feed_spec.url,
                        &mut meta,
                        cached_vendor_body,
                        cached_catalog.is_some(),
                        force,
                        &mut sources_status,
                    )
                    .await?;
                any_source_changed |= v_changed;
                degraded |= v_degraded;
                vendor_payloads.push((vendor, Some(feed_spec.source_name), body));
            } else {
                vendor_payloads.push((vendor, None, None));
            }
        }

        self.save_meta(&meta).await?;

        // Fast path: if no source changed (e.g., 304 Not Modified or Cooldown/DiskFresh)
        // and we have a cached snapshot, return it without re-parsing!
        if !any_source_changed {
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

        if let Some(md_json) = effective_models_dev_body {
            let apply_builtins = cfg!(feature = "cline") || !vendor_payloads.is_empty();
            let mut providers =
                models_dev::parse_and_normalize_models_dev(&md_json, apply_builtins)?;

            for (vendor, source_key, body) in vendor_payloads {
                if let Err(err) = vendor.apply_overlay(body.as_deref(), &mut providers) {
                    warn!(
                        vendor = vendor.id(),
                        error = %err,
                        "failed to apply vendor overlay; continuing with base catalog"
                    );
                    degraded = true;
                    if let Some(key) = source_key {
                        if let Some(st) = sources_status.get_mut(&key) {
                            st.warning = Some(err.to_string());
                        }
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

        if let Some(mut existing_snap) = cached_catalog {
            existing_snap.degraded = true;
            let arc_snap = Arc::new(existing_snap);
            self.memory.set(Arc::clone(&arc_snap)).await;
            return Ok(arc_snap);
        }

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

    #[allow(clippy::too_many_arguments)]
    async fn refresh_source_step(
        &self,
        source_name: &str,
        url: &str,
        meta: &mut SpoolMeta,
        cached_body: Option<String>,
        has_cached_catalog: bool,
        force: bool,
        sources_status: &mut BTreeMap<String, SourceStatus>,
    ) -> Result<(Option<String>, bool, bool), CatalogError> {
        let prev_meta = meta.sources.get(source_name).cloned().unwrap_or_default();
        let has_cache = cached_body.is_some() || has_cached_catalog;

        let outcome = self
            .resolve_single_source(source_name, url, &prev_meta, has_cache, force)
            .await;

        match outcome {
            Ok(SourceFetchOutcome::Live {
                body,
                meta: new_meta,
            }) => {
                self.save_source(source_name, &body).await?;
                sources_status.insert(
                    source_name.to_string(),
                    SourceStatus {
                        origin: DataOrigin::Live,
                        last_fetched_at: new_meta.last_fetched_at,
                        last_verified_at: new_meta.last_verified_at,
                        etag: new_meta.etag.clone(),
                        warning: None,
                    },
                );
                meta.sources.insert(source_name.to_string(), new_meta);
                Ok((Some(body), true, false))
            }
            Ok(SourceFetchOutcome::NotModified304 { meta: new_meta }) => {
                sources_status.insert(
                    source_name.to_string(),
                    SourceStatus {
                        origin: DataOrigin::NotModified304,
                        last_fetched_at: new_meta.last_fetched_at,
                        last_verified_at: new_meta.last_verified_at,
                        etag: new_meta.etag.clone(),
                        warning: None,
                    },
                );
                meta.sources.insert(source_name.to_string(), new_meta);
                Ok((cached_body, false, false))
            }
            Ok(SourceFetchOutcome::CooldownOrFresh { meta: cur_meta }) => {
                sources_status.insert(
                    source_name.to_string(),
                    SourceStatus {
                        origin: DataOrigin::DiskSpool,
                        last_fetched_at: cur_meta.last_fetched_at,
                        last_verified_at: cur_meta.last_verified_at,
                        etag: cur_meta.etag.clone(),
                        warning: None,
                    },
                );
                Ok((cached_body, false, false))
            }
            Err(err) => {
                warn!(source = source_name, error = %err, "failed to fetch upstream source");
                let mut err_meta = prev_meta.clone();
                err_meta.last_attempt_at = Some(Utc::now());
                meta.sources.insert(source_name.to_string(), err_meta);
                sources_status.insert(
                    source_name.to_string(),
                    SourceStatus {
                        origin: if has_cache {
                            DataOrigin::DiskSpool
                        } else {
                            DataOrigin::BundledFallback
                        },
                        last_fetched_at: prev_meta.last_fetched_at,
                        last_verified_at: prev_meta.last_verified_at,
                        etag: prev_meta.etag,
                        warning: Some(err.to_string()),
                    },
                );
                Ok((cached_body, false, true))
            }
        }
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
