use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::CatalogError;
use crate::types::CatalogSnapshot;

static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Per-source HTTP caching and rate-limiting metadata persisted in `spool_meta.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSpoolMeta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
}

/// Top-level `spool_meta.json` structure persisted inside `spool_dir`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpoolMeta {
    #[serde(default)]
    pub sources: BTreeMap<String, SourceSpoolMeta>,
}

/// Atomic disk spooler for raw upstream feeds (`sources/*.json`), ETag metadata (`spool_meta.json`),
/// and the normalized merged catalog (`catalog.json`).
#[derive(Debug, Clone)]
pub struct DiskSpool {
    root_dir: PathBuf,
}

impl DiskSpool {
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn meta_path(&self) -> PathBuf {
        self.root_dir.join("spool_meta.json")
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.root_dir.join("catalog.json")
    }

    pub fn source_path(&self, source_name: &str) -> PathBuf {
        self.root_dir
            .join("sources")
            .join(format!("{source_name}.json"))
    }

    /// Reads `spool_meta.json`, returning `SpoolMeta::default()` if it does not yet exist.
    pub async fn read_meta(&self) -> Result<SpoolMeta, CatalogError> {
        let path = self.meta_path();
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).map_err(|source| CatalogError::Parse {
                source_name: "spool_meta.json".to_string(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SpoolMeta::default()),
            Err(source) => Err(CatalogError::Io { path, source }),
        }
    }

    /// Atomically writes `spool_meta.json`.
    pub async fn write_meta(&self, meta: &SpoolMeta) -> Result<(), CatalogError> {
        let bytes = serde_json::to_vec_pretty(meta).map_err(|source| CatalogError::Parse {
            source_name: "spool_meta.json".to_string(),
            source,
        })?;
        atomic_write_bytes(&self.meta_path(), &bytes).await
    }

    /// Reads a cached raw upstream payload (`sources/<source_name>.json`) if present.
    pub async fn read_source(&self, source_name: &str) -> Result<Option<String>, CatalogError> {
        let path = self.source_path(source_name);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CatalogError::Io { path, source }),
        }
    }

    /// Atomically writes a raw upstream payload (`sources/<source_name>.json`).
    pub async fn write_source(&self, source_name: &str, payload: &str) -> Result<(), CatalogError> {
        atomic_write_bytes(&self.source_path(source_name), payload.as_bytes()).await
    }

    /// Reads the merged `catalog.json` snapshot if present.
    pub async fn read_catalog(&self) -> Result<Option<CatalogSnapshot>, CatalogError> {
        let path = self.catalog_path();
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let snap =
                    serde_json::from_str(&content).map_err(|source| CatalogError::Parse {
                        source_name: "catalog.json".to_string(),
                        source,
                    })?;
                Ok(Some(snap))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CatalogError::Io { path, source }),
        }
    }

    /// Atomically writes the merged `catalog.json` snapshot with deterministic `BTreeMap` key ordering.
    pub async fn write_catalog(&self, snapshot: &CatalogSnapshot) -> Result<(), CatalogError> {
        let bytes = serde_json::to_vec_pretty(snapshot).map_err(|source| CatalogError::Parse {
            source_name: "catalog.json".to_string(),
            source,
        })?;
        atomic_write_bytes(&self.catalog_path(), &bytes).await
    }
}

/// Writes `bytes` to `<target>.tmp.<pid>.<seq>`, flushes to disk, and atomically renames over `target`.
pub async fn atomic_write_bytes(target: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| CatalogError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }

    let pid = std::process::id();
    let seq = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("catalog.json");
    let tmp_path = target.with_file_name(format!("{file_name}.tmp.{pid}.{seq}"));

    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|source| CatalogError::Io {
            path: tmp_path.clone(),
            source,
        })?;

    file.write_all(bytes)
        .await
        .map_err(|source| CatalogError::Io {
            path: tmp_path.clone(),
            source,
        })?;

    file.sync_all().await.map_err(|source| CatalogError::Io {
        path: tmp_path.clone(),
        source,
    })?;

    drop(file);

    tokio::fs::rename(&tmp_path, target)
        .await
        .map_err(|source| CatalogError::Io {
            path: target.to_path_buf(),
            source,
        })?;

    Ok(())
}
