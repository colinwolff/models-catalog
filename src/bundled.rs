use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::error::CatalogError;
use crate::types::{CatalogSnapshot, DataOrigin};

/// Raw gzipped bytes of the compile-time fallback `CatalogSnapshot`.
pub const BUNDLED_CATALOG_GZ: &[u8] = include_bytes!("../assets/bundled_catalog.json.gz");

/// Compresses a `CatalogSnapshot` into deterministic gzipped JSON bytes.
pub fn encode_bundled_snapshot(snapshot: &CatalogSnapshot) -> Result<Vec<u8>, CatalogError> {
    let json = serde_json::to_vec(snapshot).map_err(|source| CatalogError::Parse {
        source_name: "bundled-encode".to_string(),
        source,
    })?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(&json)
        .map_err(|source| CatalogError::Io {
            path: std::path::PathBuf::from("bundled_catalog.json.gz"),
            source,
        })?;
    encoder.finish().map_err(|source| CatalogError::Io {
        path: std::path::PathBuf::from("bundled_catalog.json.gz"),
        source,
    })
}

/// Decompresses and parses a `CatalogSnapshot` from gzipped JSON bytes.
pub fn decode_bundled_snapshot(gz_bytes: &[u8]) -> Result<CatalogSnapshot, CatalogError> {
    let mut decoder = GzDecoder::new(gz_bytes);
    let mut json_str = String::new();
    decoder
        .read_to_string(&mut json_str)
        .map_err(|source| CatalogError::Io {
            path: std::path::PathBuf::from("bundled_catalog.json.gz"),
            source,
        })?;

    let mut snapshot: CatalogSnapshot =
        serde_json::from_str(&json_str).map_err(|source| CatalogError::Parse {
            source_name: "bundled_catalog.json.gz".to_string(),
            source,
        })?;

    for status in snapshot.sources.values_mut() {
        status.origin = DataOrigin::BundledFallback;
    }

    Ok(snapshot)
}

impl CatalogSnapshot {
    /// Loads the embedded compile-time fallback `CatalogSnapshot` (`feature = "bundled"`).
    pub fn bundled() -> Result<Self, CatalogError> {
        decode_bundled_snapshot(BUNDLED_CATALOG_GZ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::build_catalog_snapshot;

    #[test]
    fn bundled_snapshot_roundtrips_and_loads() {
        let md = include_str!("../tests/fixtures/models-dev.sample.json");
        let cf = include_str!("../tests/fixtures/cline-recommended.sample.json");
        let snap = build_catalog_snapshot(md, Some(cf), true).expect("build snapshot");
        let encoded = encode_bundled_snapshot(&snap).expect("encode bundled");
        let decoded = decode_bundled_snapshot(&encoded).expect("decode bundled");
        assert_eq!(decoded.providers.len(), snap.providers.len());

        // Ensure assets/bundled_catalog.json.gz is populated if it is currently a tiny placeholder
        let asset_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bundled_catalog.json.gz");
        if asset_path.metadata().map(|m| m.len() < 100).unwrap_or(true) {
            std::fs::write(&asset_path, &encoded).expect("write bundled asset");
        }

        let loaded = CatalogSnapshot::bundled().expect("load embedded bundled snapshot");
        assert!(loaded.get_model("anthropic", "claude-sonnet-5").is_some());
    }
}
