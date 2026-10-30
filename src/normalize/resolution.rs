use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::builtins::get_builtin_override;
use crate::types::{CatalogSnapshot, ModelInfo, ProviderSpec};

/// Sorts a provider's model IDs by `release_date` descending (newest first),
/// with deterministic lexicographical tie-breaking by model ID ascending.
/// Models with `Some(release_date)` always sort before models with `None`.
pub fn sort_model_ids_by_release_date(models: &BTreeMap<String, ModelInfo>) -> Vec<String> {
    let mut entries: Vec<&ModelInfo> = models.values().collect();
    entries.sort_by(|a, b| compare_models_by_release_date(a, b));
    entries.into_iter().map(|m| m.id.clone()).collect()
}

/// Comparator ordering `ModelInfo` by `release_date` descending, then `id` ascending.
pub fn compare_models_by_release_date(a: &ModelInfo, b: &ModelInfo) -> Ordering {
    let a_date = a.release_date.as_deref().filter(|s| !s.trim().is_empty());
    let b_date = b.release_date.as_deref().filter(|s| !s.trim().is_empty());

    match (a_date, b_date) {
        (Some(da), Some(db)) => db.cmp(da).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    }
}

/// Resolves the default model ID for a single `ProviderSpec` using the 3-layer rule:
/// 1. Layer 1: `user_settings_model` (if non-empty).
/// 2. Layer 2: `BUILTIN_SPEC_OVERRIDES` pinned `default_model_id` for `provider.id` (when `cline` feature is active or override exists).
/// 3. Layer 3: First model in `provider.sorted_model_ids` where `model.is_chat_model() == true`,
///    falling back to the first model key.
pub fn resolve_provider_default_model(
    provider: &ProviderSpec,
    user_settings_model: Option<&str>,
    apply_builtin_overrides: bool,
) -> Option<String> {
    if let Some(user_model) = user_settings_model.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(user_model.to_string());
    }

    if apply_builtin_overrides {
        if let Some(builtin) = get_builtin_override(&provider.id) {
            if let Some(pinned) = builtin
                .default_model_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(pinned.to_string());
            }
        }
    }

    let ordered_ids: Vec<&String> = if !provider.sorted_model_ids.is_empty() {
        provider.sorted_model_ids.iter().collect()
    } else {
        provider.models.keys().collect()
    };

    for model_id in &ordered_ids {
        if let Some(model) = provider.models.get(*model_id) {
            if model.is_chat_model() {
                return Some((*model_id).clone());
            }
        }
    }

    ordered_ids.first().map(|id| (*id).clone())
}

impl CatalogSnapshot {
    /// Resolves the default model for `provider_id` following the 3-layer resolution rule:
    /// 1. `user_settings_model` (if non-empty)
    /// 2. `BUILTIN_SPEC_OVERRIDES` pinned `default_model_id` for `provider_id`
    /// 3. First chat model in the release-date-sorted provider catalog, falling back to the first model key.
    pub fn resolve_default_model(
        &self,
        provider_id: &str,
        user_settings_model: Option<&str>,
    ) -> Option<String> {
        if let Some(user_model) = user_settings_model.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(user_model.to_string());
        }

        if let Some(provider) = self.providers.get(provider_id) {
            if let Some(pinned) = provider
                .default_model_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(pinned.to_string());
            }
            return resolve_provider_default_model(
                provider,
                user_settings_model,
                cfg!(feature = "cline"),
            );
        }

        #[cfg(feature = "cline")]
        if let Some(builtin) = get_builtin_override(provider_id) {
            if let Some(pinned) = &builtin.default_model_id {
                return Some(pinned.clone());
            }
        }

        None
    }
}
