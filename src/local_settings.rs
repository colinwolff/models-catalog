//! Re-exports local provider configuration helpers from [`crate::vendors::cline::settings`].

pub use crate::vendors::cline::settings::{
    ConfigValidationFinding, LocalProviderEntry, LocalProvidersConfig,
    default_cline_providers_path, parse_local_providers_str, read_local_providers_file,
    validate_local_config,
};
