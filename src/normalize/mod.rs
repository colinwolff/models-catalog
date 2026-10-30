pub mod cline_feed;
pub mod limits;
pub mod modalities;
pub mod models_dev;
pub mod resolution;

use std::collections::BTreeMap;

use chrono::Utc;

use crate::error::CatalogError;
use crate::types::{CATALOG_SCHEMA_VERSION, CatalogSnapshot, DataOrigin, SourceStatus};
use crate::vendors::VendorPlugin;

/// Builds a normalized `CatalogSnapshot` from raw `models.dev` JSON and an optional
/// `api.cline.bot/api/v1/ai/cline/recommended-models` JSON payload.
pub fn build_catalog_snapshot(
    models_dev_json: &str,
    cline_recommended_json: Option<&str>,
    apply_builtin_overrides: bool,
) -> Result<CatalogSnapshot, CatalogError> {
    let mut providers =
        models_dev::parse_and_normalize_models_dev(models_dev_json, apply_builtin_overrides)?;

    let now = Utc::now();
    let mut sources = BTreeMap::new();
    sources.insert(
        "models_dev".to_string(),
        SourceStatus {
            origin: DataOrigin::CustomInput,
            last_fetched_at: Some(now),
            last_verified_at: Some(now),
            etag: None,
            warning: None,
        },
    );

    if let Some(cline_json) = cline_recommended_json.filter(|s| !s.trim().is_empty()) {
        cline_feed::parse_and_merge_cline_feed(cline_json, &mut providers)?;
        sources.insert(
            "cline_recommended".to_string(),
            SourceStatus {
                origin: DataOrigin::CustomInput,
                last_fetched_at: Some(now),
                last_verified_at: Some(now),
                etag: None,
                warning: None,
            },
        );
    }

    Ok(CatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        generated_at: now,
        sources,
        degraded: false,
        providers,
    })
}

/// Builds a normalized `CatalogSnapshot` from raw `models.dev` JSON and a slice of
/// `(VendorPlugin, Option<raw_feed_json>)` overlays.
pub fn build_catalog_with_vendors(
    models_dev_json: &str,
    vendor_inputs: &[(&dyn VendorPlugin, Option<&str>)],
    apply_builtin_overrides: bool,
) -> Result<CatalogSnapshot, CatalogError> {
    let mut providers =
        models_dev::parse_and_normalize_models_dev(models_dev_json, apply_builtin_overrides)?;

    let now = Utc::now();
    let mut sources = BTreeMap::new();
    sources.insert(
        "models_dev".to_string(),
        SourceStatus {
            origin: DataOrigin::CustomInput,
            last_fetched_at: Some(now),
            last_verified_at: Some(now),
            etag: None,
            warning: None,
        },
    );

    for (vendor, raw_feed) in vendor_inputs {
        vendor.apply_overlay(*raw_feed, &mut providers)?;
        if let Some(feed_spec) = vendor.upstream_feed() {
            if raw_feed.is_some() {
                sources.insert(
                    feed_spec.source_name,
                    SourceStatus {
                        origin: DataOrigin::CustomInput,
                        last_fetched_at: Some(now),
                        last_verified_at: Some(now),
                        etag: None,
                        warning: None,
                    },
                );
            }
        }
    }

    Ok(CatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        generated_at: now,
        sources,
        degraded: false,
        providers,
    })
}
