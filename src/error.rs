use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Error returned when the estimated input tokens plus required output reserve
/// exceed a model's total context window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[error(
    "context overflow: estimated input ({estimated_input_tokens}) + reserve ({reserve_tokens}) exceeds context window ({context_window})"
)]
pub struct ContextOverflowError {
    pub context_window: u64,
    pub estimated_input_tokens: u64,
    pub reserve_tokens: u64,
}

/// Top-level error hierarchy for `models-catalog`.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    ContextOverflow(#[from] ContextOverflowError),

    #[error("rate limited: upstream `{source_name}` is within refresh cooldown until {retry_after}")]
    RateLimited {
        source_name: String,
        retry_after: DateTime<Utc>,
    },

    #[error("failed to parse catalog payload for `{source_name}`: {source}")]
    Parse {
        source_name: String,
        #[source]
        source: serde_json::Error,
    },

    #[cfg(feature = "reqwest-client")]
    #[error("http error fetching `{source_name}`: {source}")]
    Http {
        source_name: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("http status {status} fetching `{source_name}`")]
    HttpStatus {
        source_name: String,
        status: u16,
    },

    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no catalog data available from network, disk spool, or bundled fallback")]
    NoCatalogAvailable,

    #[error("invalid local provider configuration: {0}")]
    InvalidLocalConfig(String),
}
