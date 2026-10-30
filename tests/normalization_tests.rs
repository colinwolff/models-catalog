use models_catalog::{
    BillingTier, CatalogSnapshot, ContextOverflowError, DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_INPUT_TOKENS, DEFAULT_MAX_TOKENS, GATEWAY_OUTPUT_RESERVE_TOKENS, Limit, Modality,
    ModelOverrides, ProviderProtocol, ReasoningEffort, WireReasoningParams, build_catalog_snapshot,
    merge_overrides, parse_local_providers_str, resolve_max_input_tokens,
    resolve_max_output_tokens, validate_local_config,
};

fn sample_catalog() -> CatalogSnapshot {
    let models_dev_json = include_str!("fixtures/models-dev.sample.json");
    let cline_json = include_str!("fixtures/cline-recommended.sample.json");
    build_catalog_snapshot(models_dev_json, Some(cline_json), true)
        .expect("sample fixtures should normalize cleanly")
}

#[test]
fn token_limit_resolution_matches_cline_llms_semantics() {
    // 1. Both context and input present -> min(context, input)
    let both = Limit {
        context: Some(200_000),
        input: Some(180_000),
        output: Some(64_000),
    };
    assert_eq!(resolve_max_input_tokens(&both), 180_000);
    assert_eq!(resolve_max_output_tokens(&both), 64_000);

    // 2. Context smaller than input -> min(context, input)
    let ctx_smaller = Limit {
        context: Some(100_000),
        input: Some(128_000),
        output: None,
    };
    assert_eq!(resolve_max_input_tokens(&ctx_smaller), 100_000);
    assert_eq!(resolve_max_output_tokens(&ctx_smaller), DEFAULT_MAX_TOKENS);

    // 3. Only context present -> context
    let only_ctx = Limit {
        context: Some(202_800),
        input: None,
        output: Some(202_800),
    };
    assert_eq!(resolve_max_input_tokens(&only_ctx), 202_800);
    assert_eq!(resolve_max_output_tokens(&only_ctx), 202_800);

    // 4. Neither context nor input present -> DEFAULT_MAX_INPUT_TOKENS (128_000)
    let empty = Limit::default();
    assert_eq!(resolve_max_input_tokens(&empty), DEFAULT_MAX_INPUT_TOKENS);
    assert_eq!(resolve_max_output_tokens(&empty), DEFAULT_MAX_TOKENS);
}

#[test]
fn gateway_max_output_tokens_caps_at_32k_and_lifts_for_reasoning_budget() {
    let catalog = sample_catalog();
    let minimax = catalog
        .get_model("openrouter", "minimax/minimax-m2.5")
        .expect("minimax-m2.5 exists");

    assert_eq!(minimax.context_window, Some(202_800));
    assert_eq!(minimax.max_tokens, 202_800);

    // Default cap is 32_000 when no requested_max_tokens or reasoning budget is passed
    let default_cap = minimax
        .resolve_gateway_max_output_tokens(10_000, None, None)
        .expect("should resolve");
    assert_eq!(default_cap, DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS);

    // Explicit reasoning budget > 32_000 lifts the floor to budget + GATEWAY_OUTPUT_RESERVE_TOKENS (1024)
    let lifted = minimax
        .resolve_gateway_max_output_tokens(10_000, None, Some(40_000))
        .expect("should resolve with lifted floor");
    assert_eq!(lifted, 40_000 + GATEWAY_OUTPUT_RESERVE_TOKENS);

    // Context headroom clamps output when estimated_input_tokens is close to context_window
    let tight = minimax
        .resolve_gateway_max_output_tokens(200_000, None, None)
        .expect("should clamp to remaining headroom");
    assert_eq!(tight, 202_800 - 200_000 - GATEWAY_OUTPUT_RESERVE_TOKENS);

    // Overflow error when estimated_input_tokens + reserve >= context_window
    let overflow = minimax.resolve_gateway_max_output_tokens(202_000, None, None);
    assert_eq!(
        overflow,
        Err(ContextOverflowError {
            context_window: 202_800,
            estimated_input_tokens: 202_000,
            reserve_tokens: GATEWAY_OUTPUT_RESERVE_TOKENS,
        })
    );
}

#[test]
fn directional_modalities_strip_text_from_gpt_image_and_prune_incomplete() {
    let catalog = sample_catalog();

    // gpt-image-1 has Text stripped from output modalities and is NOT a chat model,
    // but remains in the catalog without tool calling.
    let gpt_image = catalog
        .get_model("openai", "gpt-image-1")
        .expect("gpt-image-1 stays in catalog");
    assert!(!gpt_image.supports_tool_call);
    assert!(!gpt_image.is_chat_model());
    let modalities = gpt_image.modalities.as_ref().expect("modalities present");
    assert_eq!(modalities.input, vec![Modality::Text, Modality::Image]);
    assert_eq!(modalities.output, vec![Modality::Image]);

    // gpt-legacy-no-limits had an incomplete modality declaration (`output: []`),
    // which is discarded (`None`) so it remains classified as a chat model.
    let legacy = catalog
        .get_model("openai", "gpt-legacy-no-limits")
        .expect("legacy model exists");
    assert!(legacy.modalities.is_none());
    assert!(legacy.is_chat_model());
    assert_eq!(legacy.max_input_tokens, DEFAULT_MAX_INPUT_TOKENS);
    assert_eq!(legacy.max_tokens, DEFAULT_MAX_TOKENS);
}

#[test]
fn three_layer_default_model_resolution_and_release_date_sorting() {
    let catalog = sample_catalog();

    // Layer 1: user_settings_model override takes top priority
    assert_eq!(
        catalog.resolve_default_model("anthropic", Some("claude-sonnet-4-5")),
        Some("claude-sonnet-4-5".to_string())
    );

    // Layer 2: BUILTIN_SPEC_OVERRIDES pins default models for known providers
    assert_eq!(
        catalog.resolve_default_model("anthropic", None),
        Some("claude-sonnet-5".to_string())
    );
    assert_eq!(
        catalog.resolve_default_model("openrouter", None),
        Some("anthropic/claude-sonnet-5".to_string())
    );
    assert_eq!(
        catalog.resolve_default_model("bedrock", None),
        Some("minimax.minimax-m2.5".to_string())
    );
    assert_eq!(
        catalog.resolve_default_model("openai-native", None),
        Some("gpt-5.4".to_string())
    );
    assert_eq!(
        catalog.resolve_default_model("zai", None),
        Some("zai-org/glm-5.3".to_string())
    );

    // Layer 3: Newest chat model by release_date descending, tie-broken lexicographically by ID.
    // In `custom-research`:
    // - `aa-image-only-newest` (2026-06-01) is newest overall, but is NOT a chat model (output: [image]).
    // - `alpha-newest-chat` and `beta-newest-chat` are both on 2026-05-01; lexicographical tie-break picks `alpha-newest-chat`.
    let custom_prov = catalog
        .get_provider("custom-research")
        .expect("custom-research exists");
    assert_eq!(
        custom_prov.sorted_model_ids,
        vec![
            "aa-image-only-newest",
            "alpha-newest-chat",
            "beta-newest-chat",
            "zeta-old-chat"
        ]
    );
    assert_eq!(
        catalog.resolve_default_model("custom-research", None),
        Some("alpha-newest-chat".to_string())
    );
}

#[test]
fn cline_pass_and_recommended_enrichment_cross_references_models_dev() {
    let catalog = sample_catalog();

    let cline_pass_prov = catalog
        .get_provider("cline-pass")
        .expect("cline-pass provider exists");
    assert!(cline_pass_prov.requires_streaming);
    assert_eq!(cline_pass_prov.protocol, ProviderProtocol::ClinePass);

    // Check `cline-pass/minimax/minimax-m2.5` inherited 202_800 limits and cost from models.dev
    let cp_minimax = catalog
        .get_model("cline-pass", "cline-pass/minimax/minimax-m2.5")
        .expect("cline-pass minimax exists");
    assert_eq!(cp_minimax.context_window, Some(202_800));
    assert_eq!(cp_minimax.max_tokens, 202_800);
    assert_eq!(cp_minimax.billing_tier, Some(BillingTier::Subscription));
    assert!(cp_minimax.requires_streaming);
    assert_eq!(
        cp_minimax.reasoning_options,
        vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max
        ]
    );

    // Check `is_cline_pass_model` works with both prefixed and unprefixed slugs
    assert!(catalog.is_cline_pass_model("cline-pass/minimax/minimax-m2.5"));
    assert!(catalog.is_cline_pass_model("minimax/minimax-m2.5"));
    assert!(!catalog.is_cline_pass_model("gpt-image-1"));

    // Check free tier model in cline-pass & openrouter
    let free_glm = catalog
        .get_model("openrouter", "zai-org/glm-5.3:free")
        .expect("free glm exists");
    assert_eq!(free_glm.billing_tier, Some(BillingTier::Free));

    // Check recommended flag on anthropic/claude-sonnet-5
    let rec_sonnet = catalog
        .get_model("openrouter", "anthropic/claude-sonnet-5")
        .expect("openrouter sonnet exists");
    assert!(rec_sonnet.is_recommended);
}

#[test]
fn reasoning_effort_clamping_and_wire_translation() {
    let catalog = sample_catalog();

    // MiniMax M2.5 supports [None, Low, High, Max] (no Medium or XHigh)
    let minimax = catalog
        .get_model("cline-pass", "cline-pass/minimax/minimax-m2.5")
        .expect("minimax exists");
    assert_eq!(
        minimax.normalize_reasoning_effort(ReasoningEffort::Medium),
        Some(ReasoningEffort::Low)
    );
    assert_eq!(
        minimax.normalize_reasoning_effort(ReasoningEffort::XHigh),
        Some(ReasoningEffort::High)
    );
    assert_eq!(
        minimax.normalize_reasoning_effort(ReasoningEffort::None),
        Some(ReasoningEffort::None)
    );

    // Anthropic Claude Sonnet 5 -> AnthropicThinking budget_tokens
    let sonnet = catalog
        .get_model("anthropic", "claude-sonnet-5")
        .expect("sonnet exists");
    let wire_high =
        sonnet.to_wire_reasoning_params(ProviderProtocol::Anthropic, ReasoningEffort::High, 32_000);
    assert_eq!(
        wire_high,
        WireReasoningParams::AnthropicThinking {
            budget_tokens: 16_384
        }
    );

    let wire_max =
        sonnet.to_wire_reasoning_params(ProviderProtocol::Anthropic, ReasoningEffort::Max, 32_000);
    assert_eq!(
        wire_max,
        WireReasoningParams::AnthropicThinking {
            budget_tokens: 32_000 - GATEWAY_OUTPUT_RESERVE_TOKENS
        }
    );

    // OpenAI GPT-5.4 -> OpenAiEffort
    let gpt = catalog.get_model("openai", "gpt-5.4").expect("gpt-5.4");
    let wire_oai = gpt.to_wire_reasoning_params(
        ProviderProtocol::OpenAiResponses,
        ReasoningEffort::High,
        32_000,
    );
    assert_eq!(
        wire_oai,
        WireReasoningParams::OpenAiEffort {
            reasoning_effort: "high".to_string()
        }
    );

    // Z.AI GLM 5.3 -> ThinkingToggle
    let glm = catalog
        .get_model("zai", "zai-org/glm-5.3")
        .expect("glm-5.3");
    assert_eq!(
        glm.to_wire_reasoning_params(
            ProviderProtocol::OpenAiCompatible,
            ReasoningEffort::High,
            16_384
        ),
        WireReasoningParams::ThinkingToggle { enabled: true }
    );
    assert_eq!(
        glm.to_wire_reasoning_params(
            ProviderProtocol::OpenAiCompatible,
            ReasoningEffort::None,
            16_384
        ),
        WireReasoningParams::ThinkingToggle { enabled: false }
    );
}

#[test]
fn flexible_lookup_overrides_and_local_config_validation() {
    let catalog = sample_catalog();

    // Flexible lookup across qualified, cline-pass, and :free suffixed IDs
    assert!(catalog.find_model("anthropic/claude-sonnet-5").is_some());
    assert!(
        catalog
            .find_model("cline-pass/minimax/minimax-m2.5")
            .is_some()
    );
    assert!(catalog.find_model("claude-sonnet-4-5:free").is_some());

    // Override precedence: caller_override > local_settings > catalog > default
    let local_settings_override = ModelOverrides {
        context_window: Some(500_000),
        max_output_tokens: Some(16_000),
        ..Default::default()
    };
    let caller_override = ModelOverrides {
        context_window: Some(256_000),
        ..Default::default()
    };
    let merged = merge_overrides(&caller_override, &local_settings_override);

    let sonnet = catalog
        .get_model("anthropic", "claude-sonnet-5")
        .expect("sonnet")
        .with_overrides(&merged);
    assert_eq!(sonnet.context_window, Some(256_000));
    assert_eq!(sonnet.max_input_tokens, 256_000);
    assert_eq!(sonnet.max_tokens, 16_000);

    // Local providers.json validation
    let raw_local = r#"{
        "activeProvider": "anthropic",
        "providers": {
            "anthropic": { "modelId": "claude-sonnet-5", "contextWindow": 500000 },
            "openai": {},
            "unknown-provider": { "modelId": "foo" }
        }
    }"#;
    let parsed_cfg = parse_local_providers_str(raw_local).expect("parse local config");
    let findings = validate_local_config(&parsed_cfg, &catalog);
    assert_eq!(findings.len(), 3);
    assert!(
        findings
            .iter()
            .any(|f| f.provider_id == "anthropic" && f.is_valid)
    );
    assert!(
        findings
            .iter()
            .any(|f| f.provider_id == "openai" && f.is_valid)
    );
    assert!(
        findings
            .iter()
            .any(|f| f.provider_id == "unknown-provider" && !f.is_valid)
    );
}

#[test]
fn custom_vendor_plugin_enriches_catalog_via_trait() {
    use models_catalog::{
        CatalogError, ProviderSpec, VendorFeedSpec, VendorPlugin, build_catalog_with_vendors,
    };
    use std::collections::BTreeMap;

    #[derive(Debug)]
    struct CustomEnterpriseVendor;

    impl VendorPlugin for CustomEnterpriseVendor {
        fn id(&self) -> &'static str {
            "enterprise"
        }

        fn upstream_feed(&self) -> Option<VendorFeedSpec> {
            Some(VendorFeedSpec {
                source_name: "enterprise_curated".to_string(),
                url: "https://internal.example/models.json".to_string(),
            })
        }

        fn apply_overlay(
            &self,
            raw_feed_json: Option<&str>,
            providers: &mut BTreeMap<String, ProviderSpec>,
        ) -> Result<(), CatalogError> {
            if let Some(model_id) = raw_feed_json {
                if let Some(anthropic) = providers.get_mut("anthropic") {
                    if let Some(m) = anthropic.models.get_mut(model_id.trim()) {
                        m.is_recommended = true;
                        m.billing_tier = Some(BillingTier::Subscription);
                    }
                }
            }
            Ok(())
        }
    }

    let models_dev_json = include_str!("fixtures/models-dev.sample.json");
    let vendor = CustomEnterpriseVendor;
    let snap = build_catalog_with_vendors(
        models_dev_json,
        &[(&vendor, Some("claude-sonnet-4-5"))],
        true,
    )
    .expect("build with custom vendor");

    assert!(snap.sources.contains_key("enterprise_curated"));
    let m = snap
        .get_model("anthropic", "claude-sonnet-4-5")
        .expect("sonnet 4.5 exists");
    assert!(m.is_recommended);
    assert_eq!(m.billing_tier, Some(BillingTier::Subscription));
}

#[cfg(feature = "ts-bindings")]
#[test]
fn export_typescript_bindings_to_bindings_directory() {
    use ts_rs::TS;
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
    std::fs::create_dir_all(&out_dir).expect("create bindings dir");
    let cfg = ts_rs::Config::from_env().with_out_dir(&out_dir);

    CatalogSnapshot::export_all(&cfg).expect("export CatalogSnapshot TS bindings");
    ModelOverrides::export_all(&cfg).expect("export ModelOverrides TS bindings");
    WireReasoningParams::export_all(&cfg).expect("export WireReasoningParams TS bindings");
    ContextOverflowError::export_all(&cfg).expect("export ContextOverflowError TS bindings");

    assert!(out_dir.join("CatalogSnapshot.ts").exists());
    assert!(out_dir.join("ProviderSpec.ts").exists());
    assert!(out_dir.join("ModelInfo.ts").exists());
    assert!(out_dir.join("ReasoningEffort.ts").exists());
}
