use std::collections::BTreeMap;

use crate::normalize::limits::{
    DEFAULT_MAX_INPUT_TOKENS, DEFAULT_MAX_TOKENS, resolve_context_window, resolve_max_input_tokens,
    resolve_max_output_tokens,
};
use crate::normalize::modalities::sanitize_modalities;
use crate::normalize::resolution::{
    resolve_provider_default_model, sort_model_ids_by_release_date,
};
use crate::types::{
    BillingTier, Cost, Limit, ModelInfo, ProviderProtocol, ProviderSpec, ReasoningEffort,
};
use crate::vendors::cline::builtins::get_builtin_override;
use crate::vendors::cline::feed::{RawClineModelEntry, RawClineRecommendedFeed};

pub(crate) fn merge_cline_feed_impl(
    feed: RawClineRecommendedFeed,
    providers: &mut BTreeMap<String, ProviderSpec>,
) {
    ensure_target_provider(providers, "cline-pass");
    ensure_target_provider(providers, "cline");
    ensure_target_provider(providers, "openrouter");

    for entry in feed.recommended.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let raw_id = raw_id.to_string();
        upsert_curated_model(
            providers,
            &raw_id,
            &entry,
            BillingTier::Credits,
            true,
            false,
        );
    }

    for entry in feed.free.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let raw_id = raw_id.to_string();
        upsert_curated_model(providers, &raw_id, &entry, BillingTier::Free, false, false);
    }

    let mut active_cline_pass_ids = std::collections::BTreeSet::new();
    for entry in feed.cline_pass.into_entries() {
        let Some(raw_id) = entry.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let wire_id = if raw_id.starts_with("cline-pass/") {
            raw_id.to_string()
        } else {
            format!("cline-pass/{raw_id}")
        };
        active_cline_pass_ids.insert(wire_id.clone());
        upsert_curated_model(
            providers,
            &wire_id,
            &entry,
            BillingTier::Subscription,
            false,
            true,
        );
    }

    if !active_cline_pass_ids.is_empty() {
        for provider_id in ["cline-pass", "cline"] {
            if let Some(spec) = providers.get_mut(provider_id) {
                spec.models.retain(|k, _| {
                    !k.starts_with("cline-pass/") || active_cline_pass_ids.contains(k)
                });
            }
        }
    }

    for provider_id in ["cline-pass", "cline", "openrouter"] {
        if let Some(spec) = providers.get_mut(provider_id) {
            spec.sorted_model_ids = sort_model_ids_by_release_date(&spec.models);
            spec.default_model_id = resolve_provider_default_model(spec, None, true);
        }
    }
}

fn ensure_target_provider(providers: &mut BTreeMap<String, ProviderSpec>, provider_id: &str) {
    if providers.contains_key(provider_id) {
        if provider_id == "cline-pass" || provider_id == "cline" {
            if let Some(spec) = providers.get_mut(provider_id) {
                spec.requires_streaming = true;
                spec.protocol = ProviderProtocol::ClinePass;
            }
        }
        return;
    }

    let builtin = get_builtin_override(provider_id);
    let requires_streaming = builtin
        .map(|b| b.requires_streaming)
        .unwrap_or(provider_id == "cline-pass" || provider_id == "cline");
    let protocol = builtin.map(|b| b.protocol).unwrap_or_else(|| {
        if provider_id == "cline-pass" || provider_id == "cline" {
            ProviderProtocol::ClinePass
        } else {
            ProviderProtocol::OpenAiCompatible
        }
    });

    providers.insert(
        provider_id.to_string(),
        ProviderSpec {
            id: provider_id.to_string(),
            name: builtin
                .map(|b| b.name.clone())
                .unwrap_or_else(|| provider_id.to_string()),
            protocol,
            base_url: builtin.and_then(|b| b.base_url.clone()),
            env_vars: builtin.map(|b| b.env_vars.clone()).unwrap_or_default(),
            doc_url: None,
            default_model_id: builtin.and_then(|b| b.default_model_id.clone()),
            requires_streaming,
            sorted_model_ids: Vec::new(),
            models: BTreeMap::new(),
        },
    );
}

fn upsert_curated_model(
    providers: &mut BTreeMap<String, ProviderSpec>,
    raw_id: &str,
    entry: &RawClineModelEntry,
    tier: BillingTier,
    mark_recommended: bool,
    is_cline_pass: bool,
) {
    let base_template = find_upstream_reference_model(providers, raw_id);
    let slug_without_prefix = raw_id.strip_prefix("cline-pass/").unwrap_or(raw_id);

    for cline_prov in ["cline-pass", "cline"] {
        let target_id = if is_cline_pass {
            if raw_id.starts_with("cline-pass/") {
                raw_id.to_string()
            } else {
                format!("cline-pass/{raw_id}")
            }
        } else {
            raw_id.to_string()
        };

        let existing = providers
            .get(cline_prov)
            .and_then(|p| p.models.get(&target_id))
            .cloned();

        let merged = build_enriched_model(
            cline_prov,
            &target_id,
            existing.as_ref().or(base_template.as_ref()),
            entry,
            tier,
            mark_recommended,
            true,
        );

        if let Some(spec) = providers.get_mut(cline_prov) {
            spec.models.insert(target_id, merged);
        }
    }

    let openrouter_id = if is_cline_pass {
        raw_id.to_string()
    } else {
        slug_without_prefix.to_string()
    };

    let existing_or = providers
        .get("openrouter")
        .and_then(|p| p.models.get(&openrouter_id))
        .cloned();

    let merged_or = build_enriched_model(
        "openrouter",
        &openrouter_id,
        existing_or.as_ref().or(base_template.as_ref()),
        entry,
        tier,
        mark_recommended,
        is_cline_pass,
    );

    if let Some(or_spec) = providers.get_mut("openrouter") {
        or_spec.models.insert(openrouter_id.clone(), merged_or);

        if is_cline_pass && slug_without_prefix != openrouter_id {
            if let Some(underlying) = or_spec.models.get_mut(slug_without_prefix) {
                if mark_recommended {
                    underlying.is_recommended = true;
                }
                if underlying.billing_tier.is_none() {
                    underlying.billing_tier = Some(tier);
                }
            }
        }
    }
}

fn find_upstream_reference_model(
    providers: &BTreeMap<String, ProviderSpec>,
    raw_id: &str,
) -> Option<ModelInfo> {
    let stripped_pass = raw_id.strip_prefix("cline-pass/").unwrap_or(raw_id);
    let stripped_free = stripped_pass.strip_suffix(":free").unwrap_or(stripped_pass);

    let candidates = [raw_id, stripped_pass, stripped_free];

    for prov_id in ["cline-pass", "openrouter"] {
        if let Some(prov) = providers.get(prov_id) {
            for cand in candidates {
                if let Some(m) = prov.models.get(cand) {
                    return Some(m.clone());
                }
            }
        }
    }

    for cand in [stripped_pass, stripped_free] {
        if let Some((vendor, model_slug)) = cand.split_once('/') {
            let vendor_candidates = match vendor {
                "z-ai" | "zai-org" | "zhipu" => vec![vendor, "zai", "zhipuai"],
                "x-ai" => vec![vendor, "xai"],
                "google" => vec!["google", "vertex", "google-vertex"],
                "openai" => vec!["openai", "openai-native"],
                "anthropic" => vec!["anthropic"],
                _ => vec![vendor],
            };
            for v in vendor_candidates {
                if let Some(prov) = providers.get(v) {
                    if let Some(m) = prov
                        .models
                        .get(model_slug)
                        .or_else(|| prov.models.get(cand))
                    {
                        return Some(m.clone());
                    }
                }
            }
        }
    }

    let basename = stripped_free.rsplit('/').next().unwrap_or(stripped_free);
    let suffix_match = format!("/{basename}");

    for prov in providers.values() {
        for cand in [stripped_pass, stripped_free, basename] {
            if let Some(m) = prov.models.get(cand) {
                return Some(m.clone());
            }
        }
        if let Some((_, m)) = prov.models.iter().find(|(k, _)| k.ends_with(&suffix_match)) {
            return Some(m.clone());
        }
    }

    // Fallback: if `raw_id` has a trailing 4-digit date suffix (e.g. `cline-pass/deepseek-v4.1-flash-1017`),
    // check for the base sibling inside `cline-pass` or `openrouter`.
    if let Some((prefix, tail)) = raw_id.rsplit_once('-') {
        if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit()) {
            for prov_id in ["cline-pass", "openrouter"] {
                if let Some(prov) = providers.get(prov_id) {
                    if let Some(m) = prov.models.get(prefix) {
                        return Some(m.clone());
                    }
                }
            }
        }
    }

    None
}

fn build_enriched_model(
    provider_id: &str,
    model_id: &str,
    base: Option<&ModelInfo>,
    entry: &RawClineModelEntry,
    tier: BillingTier,
    mark_recommended: bool,
    requires_streaming: bool,
) -> ModelInfo {
    let family = entry
        .family
        .clone()
        .or_else(|| base.and_then(|b| b.family.clone()));

    let raw_limit = if let Some(l) = &entry.limit {
        Limit {
            context: l
                .context
                .or(entry.context_window)
                .or_else(|| base.and_then(|b| b.raw_limit.context)),
            input: l
                .input
                .or(entry.max_input_tokens)
                .or_else(|| base.and_then(|b| b.raw_limit.input)),
            output: l
                .output
                .or(entry.max_tokens)
                .or_else(|| base.and_then(|b| b.raw_limit.output)),
        }
    } else {
        Limit {
            context: entry
                .context_window
                .or_else(|| base.and_then(|b| b.context_window)),
            input: entry
                .max_input_tokens
                .or_else(|| base.and_then(|b| b.raw_limit.input)),
            output: entry
                .max_tokens
                .or_else(|| base.and_then(|b| b.raw_limit.output)),
        }
    };

    let context_window =
        resolve_context_window(&raw_limit).or_else(|| base.and_then(|b| b.context_window));
    let max_input_tokens = if raw_limit.context.is_some() || raw_limit.input.is_some() {
        resolve_max_input_tokens(&raw_limit)
    } else {
        base.map(|b| b.max_input_tokens)
            .unwrap_or(DEFAULT_MAX_INPUT_TOKENS)
    };
    let max_tokens = if raw_limit.output.is_some() {
        resolve_max_output_tokens(&raw_limit)
    } else {
        base.map(|b| b.max_tokens).unwrap_or(DEFAULT_MAX_TOKENS)
    };

    let modalities = if entry.modalities.is_some() {
        sanitize_modalities(entry.modalities.as_ref(), model_id, family.as_deref())
    } else {
        base.and_then(|b| b.modalities.clone())
    };

    let cost = if let Some(c) = &entry.cost {
        Some(Cost {
            input: c.input,
            output: c.output,
            reasoning: c.reasoning,
            cache_read: c.cache_read,
            cache_write: c.cache_write,
        })
    } else {
        base.and_then(|b| b.cost.clone())
    };

    let supports_reasoning = entry
        .reasoning
        .or_else(|| entry.reasoning_options.as_ref().map(|o| !o.is_empty()))
        .or_else(|| base.map(|b| b.supports_reasoning))
        .unwrap_or(false);

    let reasoning_options = entry
        .reasoning_options
        .clone()
        .filter(|o| !o.is_empty())
        .or_else(|| {
            base.map(|b| b.reasoning_options.clone())
                .filter(|o| !o.is_empty())
        })
        .unwrap_or_else(|| {
            if supports_reasoning {
                vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ]
            } else {
                Vec::new()
            }
        });

    let default_reasoning_effort = entry
        .default_reasoning_effort
        .or_else(|| base.and_then(|b| b.default_reasoning_effort));

    let supports_tool_call = entry
        .tool_call
        .or_else(|| base.map(|b| b.supports_tool_call))
        .unwrap_or(false);

    let supports_prompt_cache = entry
        .prompt_cache
        .or_else(|| base.map(|b| b.supports_prompt_cache))
        .unwrap_or_else(|| {
            cost.as_ref()
                .map(|c| c.cache_read.is_some() || c.cache_write.is_some())
                .unwrap_or(false)
        });

    let release_date = entry
        .release_date
        .clone()
        .or_else(|| base.and_then(|b| b.release_date.clone()));

    let stripped_model_id = model_id.strip_prefix("cline-pass/").unwrap_or(model_id);
    let basename_id = stripped_model_id
        .rsplit('/')
        .next()
        .unwrap_or(stripped_model_id);
    let entry_name = entry.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let entry_name_is_slug = entry_name.is_some_and(|n| {
        n == model_id
            || n == stripped_model_id
            || n == basename_id
            || entry.id.as_deref() == Some(n)
    });

    let name = if let Some(en) = entry_name.filter(|_| !entry_name_is_slug) {
        en.to_string()
    } else if let Some(base_name) = base.map(|b| b.name.as_str()).filter(|bn| {
        !bn.is_empty() && Some(*bn) != base.map(|b| b.id.as_str())
    }) {
        base_name.to_string()
    } else {
        entry_name
            .map(ToOwned::to_owned)
            .or_else(|| base.map(|b| b.name.clone()))
            .unwrap_or_else(|| model_id.to_string())
    };

    let effective_tier = match (base.and_then(|b| b.billing_tier), tier) {
        (Some(BillingTier::Subscription), _) | (_, BillingTier::Subscription) => {
            BillingTier::Subscription
        }
        (Some(BillingTier::Free), _) | (_, BillingTier::Free) => BillingTier::Free,
        _ => BillingTier::Credits,
    };

    let is_recommended = mark_recommended || base.map(|b| b.is_recommended).unwrap_or(false);
    let requires_streaming =
        requires_streaming || base.map(|b| b.requires_streaming).unwrap_or(false);

    ModelInfo {
        id: model_id.to_string(),
        name,
        provider_id: provider_id.to_string(),
        family,
        context_window,
        max_input_tokens,
        max_tokens,
        raw_limit,
        modalities,
        cost,
        supports_reasoning,
        reasoning_options,
        default_reasoning_effort,
        supports_tool_call,
        supports_prompt_cache,
        release_date,
        billing_tier: Some(effective_tier),
        is_recommended,
        requires_streaming,
    }
}
