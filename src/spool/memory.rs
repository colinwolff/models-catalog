use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use crate::types::CatalogSnapshot;

/// Entry stored in the in-memory `CatalogManager` cache.
#[derive(Debug, Clone)]
pub struct CachedCatalog {
    pub snapshot: Arc<CatalogSnapshot>,
    pub cached_at: Instant,
}

/// Thread-safe in-memory cache holding the latest `Arc<CatalogSnapshot>`.
#[derive(Debug, Default)]
pub struct MemoryCache {
    inner: RwLock<Option<CachedCatalog>>,
}

impl MemoryCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Reads the current snapshot and whether it is still within `memory_ttl`.
    pub async fn get(&self, memory_ttl: Duration) -> Option<(Arc<CatalogSnapshot>, bool)> {
        let guard = self.inner.read().await;
        let entry = guard.as_ref()?;
        let is_fresh = entry.cached_at.elapsed() < memory_ttl;
        Some((Arc::clone(&entry.snapshot), is_fresh))
    }

    /// Synchronously checks if a snapshot is currently cached in memory (non-blocking try_read).
    pub fn try_get(&self) -> Option<Arc<CatalogSnapshot>> {
        self.inner
            .try_read()
            .ok()
            .and_then(|g| g.as_ref().map(|e| Arc::clone(&e.snapshot)))
    }

    /// Updates the in-memory cached snapshot and resets `cached_at` to `Instant::now()`.
    pub async fn set(&self, snapshot: Arc<CatalogSnapshot>) {
        let mut guard = self.inner.write().await;
        *guard = Some(CachedCatalog {
            snapshot,
            cached_at: Instant::now(),
        });
    }

    /// Bumps `cached_at` on the existing in-memory snapshot (e.g. after a `304 Not Modified`).
    pub async fn touch_with(&self, updated: Arc<CatalogSnapshot>) {
        self.set(updated).await;
    }
}
