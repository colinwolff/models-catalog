use crate::builtins::{builtin_spec_overrides, infer_provider_protocol};
use crate::normalize::limits::{
    DEFAULT_MAX_INPUT_TOKENS, DEFAULT_MAX_TOKENS, resolve_context_window, resolve_max_input_tokens,
    resolve_max_output_tokens,
};
use crate::types::{
    BillingTier, CatalogSnapshot, Limit, ModelInfo, ModelOverrides, ProviderProtocol, ProviderSpec,
};

impl CatalogSnapshot {
    /// Returns a reference to a provider specification by ID.
    pub fn get_provider(&self, provider_id: &str) -> Option<&ProviderSpec> {
        self.providers.get(provider_id)
    }

    /// Returns the wire `ProviderProtocol` for `provider_id`.
    pub fn provider_protocol(&self, provider_id: &str) -> ProviderProtocol {
        if let Some(prov) = self.providers.get(provider_id) {
            return prov.protocol;
        }
        infer_provider_protocol(provider_id, None)
    }

    /// Looks up a model by exact `(provider_id, model_id)`.
    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<&ModelInfo> {
        self.providers.get(provider_id)?.models.get(model_id)
    }

    /// Resolves a model across providers from a bare or qualified identifier:
    /// 1. Direct `cline-pass/*` lookup in `cline-pass` / `cline`.
    /// 2. `provider/model` split lookup in `provider`.
    /// 3. `openrouter` qualified ID lookup.
    /// 4. Suffix-stripped (`:free`, `:thinking`) fallback across steps 1–3.
    /// 5. Priority scan across built-in providers and remaining providers.
    pub fn find_model(&self, query: &str) -> Option<&ModelInfo> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }

        if let Some(m) = self.find_model_exact_or_qualified(query) {
            return Some(m);
        }

        // Try stripping `:free` or `:thinking` suffixes
        for suffix in [":free", ":thinking"] {
            if let Some(stripped) = query.strip_suffix(suffix) {
                if let Some(m) = self.find_model_exact_or_qualified(stripped) {
                    return Some(m);
                }
            }
        }

        // Try stripping `cline-pass/` prefix if not found in `cline-pass` directly
        if let Some(stripped) = query.strip_prefix("cline-pass/") {
            if let Some(m) = self.find_model_exact_or_qualified(stripped) {
                return Some(m);
            }
            if let Some(stripped_free) = stripped.strip_suffix(":free") {
                if let Some(m) = self.find_model_exact_or_qualified(stripped_free) {
                    return Some(m);
                }
            }
        }

        None
    }

    fn find_model_exact_or_qualified(&self, query: &str) -> Option<&ModelInfo> {
        // 1. Direct `cline-pass/*` check
        if query.starts_with("cline-pass/") {
            for prov in ["cline-pass", "cline", "openrouter"] {
                if let Some(m) = self.get_model(prov, query) {
                    return Some(m);
                }
            }
        }

        // 2. `<provider>/<model>` split check
        if let Some((prefix, rest)) = query.split_once('/') {
            if let Some(m) = self.get_model(prefix, rest) {
                return Some(m);
            }
            if let Some(m) = self.get_model(prefix, query) {
                return Some(m);
            }
        }

        // 3. OpenRouter and ClinePass qualified check
        for prov in ["openrouter", "cline-pass", "cline"] {
            if let Some(m) = self.get_model(prov, query) {
                return Some(m);
            }
        }

        // 4. Built-in override providers first
        for builtin_id in builtin_spec_overrides().keys() {
            if let Some(m) = self.get_model(builtin_id, query) {
                return Some(m);
            }
        }

        // 5. Remaining providers in deterministic BTreeMap order
        for prov in self.providers.values() {
            if let Some(m) = prov.models.get(query) {
                return Some(m);
            }
        }

        None
    }

    /// Returns `true` if `model_id` (either `cline-pass/<slug>` or `<slug>`) is a known
    /// ClinePass subscription model.
    pub fn is_cline_pass_model(&self, model_id: &str) -> bool {
        let trimmed = model_id.trim();
        let qualified = if trimmed.starts_with("cline-pass/") {
            trimmed.to_string()
        } else {
            format!("cline-pass/{trimmed}")
        };

        for prov_id in ["cline-pass", "cline", "openrouter"] {
            if let Some(prov) = self.providers.get(prov_id) {
                if let Some(m) = prov
                    .models
                    .get(&qualified)
                    .or_else(|| prov.models.get(trimmed))
                {
                    if m.billing_tier == Some(BillingTier::Subscription)
                        || m.id.starts_with("cline-pass/")
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Returns all curated recommended models across the catalog.
    pub fn recommended_models(&self, provider_id: Option<&str>) -> Vec<&ModelInfo> {
        match provider_id {
            Some(pid) => self
                .providers
                .get(pid)
                .map(|p| p.models.values().filter(|m| m.is_recommended).collect())
                .unwrap_or_default(),
            None => self
                .providers
                .values()
                .flat_map(|p| p.models.values())
                .filter(|m| m.is_recommended)
                .collect(),
        }
    }
}

impl ModelInfo {
    /// Applies caller or user-config overrides (`caller_override > local_settings > catalog > default`)
    /// and recomputes derived token limits accordingly.
    pub fn with_overrides(&self, overrides: &ModelOverrides) -> Self {
        let mut updated = self.clone();

        let new_limit = Limit {
            context: overrides.context_window.or(self.raw_limit.context),
            input: overrides.max_input_tokens.or(self.raw_limit.input),
            output: overrides.max_output_tokens.or(self.raw_limit.output),
        };

        if overrides.context_window.is_some() {
            updated.context_window = resolve_context_window(&new_limit);
        }

        if overrides.context_window.is_some() || overrides.max_input_tokens.is_some() {
            updated.max_input_tokens = match (overrides.context_window, overrides.max_input_tokens)
            {
                (Some(c), Some(i)) if c > 0 && i > 0 => std::cmp::min(c, i),
                (None, Some(i)) if i > 0 => {
                    if let Some(c) = updated.context_window {
                        std::cmp::min(c, i)
                    } else {
                        i
                    }
                }
                (Some(c), None) if c > 0 => {
                    if let Some(raw_i) = self.raw_limit.input.filter(|&n| n > 0) {
                        std::cmp::min(c, raw_i)
                    } else {
                        c
                    }
                }
                _ => resolve_max_input_tokens(&new_limit)
                    .max(DEFAULT_MAX_INPUT_TOKENS.min(updated.max_input_tokens)),
            };
        }

        if let Some(max_out) = overrides.max_output_tokens.filter(|&n| n > 0) {
            updated.max_tokens = max_out;
        } else if new_limit.output.is_some() {
            updated.max_tokens = resolve_max_output_tokens(&new_limit)
                .max(DEFAULT_MAX_TOKENS.min(updated.max_tokens));
        }

        updated.raw_limit = new_limit;

        if let Some(effort) = overrides.default_reasoning_effort {
            updated.default_reasoning_effort = Some(effort);
            updated.supports_reasoning = true;
        }

        if let Some(opts) = &overrides.reasoning_options {
            updated.reasoning_options = opts.clone();
            if !opts.is_empty() {
                updated.supports_reasoning = true;
            }
        }

        if let Some(streaming) = overrides.requires_streaming {
            updated.requires_streaming = streaming;
        }

        updated
    }
}

/// Merges two `ModelOverrides` instances where `primary` takes precedence over `fallback`
/// (`caller_override > local_settings`).
pub fn merge_overrides(primary: &ModelOverrides, fallback: &ModelOverrides) -> ModelOverrides {
    ModelOverrides {
        context_window: primary.context_window.or(fallback.context_window),
        max_input_tokens: primary.max_input_tokens.or(fallback.max_input_tokens),
        max_output_tokens: primary.max_output_tokens.or(fallback.max_output_tokens),
        default_reasoning_effort: primary
            .default_reasoning_effort
            .or(fallback.default_reasoning_effort),
        reasoning_options: primary
            .reasoning_options
            .clone()
            .or_else(|| fallback.reasoning_options.clone()),
        requires_streaming: primary.requires_streaming.or(fallback.requires_streaming),
    }
}
