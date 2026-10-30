use std::collections::BTreeMap;

use models_catalog::{BillingTier, Modalities, build_catalog_snapshot};
use serde::{Deserialize, Serialize};
use similar_asserts::assert_eq;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OracleExpected {
    default_models: BTreeMap<String, String>,
    models: Vec<OracleModelExpectation>,
    gateway_calculations: Vec<OracleGatewayCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OracleModelExpectation {
    provider_id: String,
    model_id: String,
    context_window: Option<u64>,
    max_input_tokens: u64,
    max_tokens: u64,
    modalities: Option<Modalities>,
    billing_tier: Option<BillingTier>,
    is_recommended: bool,
    is_chat_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OracleGatewayCase {
    provider_id: String,
    model_id: String,
    estimated_input_tokens: u64,
    requested_max_tokens: Option<u64>,
    explicit_reasoning_budget: Option<u64>,
    expected_ok: Option<u64>,
    expected_overflow: bool,
}

#[test]
fn differential_parity_matches_cline_llms_oracle() {
    let models_dev_json = include_str!("fixtures/models-dev.sample.json");
    let cline_json = include_str!("fixtures/cline-recommended.sample.json");
    let oracle_json = include_str!("fixtures/oracle-expected.json");

    let expected: OracleExpected =
        serde_json::from_str(oracle_json).expect("valid oracle-expected.json");
    let catalog =
        build_catalog_snapshot(models_dev_json, Some(cline_json), true).expect("build catalog");

    // 1. Compare resolved default_model_id for every provider in oracle
    let mut actual_defaults = BTreeMap::new();
    for provider_id in expected.default_models.keys() {
        if let Some(resolved) = catalog.resolve_default_model(provider_id, None) {
            actual_defaults.insert(provider_id.clone(), resolved);
        }
    }
    assert_eq!(actual_defaults, expected.default_models);

    // 2. Compare normalized model limits, directional modalities, billing_tier, and chat classification
    let mut actual_models = Vec::new();
    for item in &expected.models {
        let m = catalog
            .get_model(&item.provider_id, &item.model_id)
            .unwrap_or_else(|| {
                panic!(
                    "expected model {}/{} to exist in normalized catalog",
                    item.provider_id, item.model_id
                )
            });

        actual_models.push(OracleModelExpectation {
            provider_id: item.provider_id.clone(),
            model_id: item.model_id.clone(),
            context_window: m.context_window,
            max_input_tokens: m.max_input_tokens,
            max_tokens: m.max_tokens,
            modalities: m.modalities.clone(),
            billing_tier: m.billing_tier,
            is_recommended: m.is_recommended,
            is_chat_model: m.is_chat_model(),
        });
    }
    assert_eq!(actual_models, expected.models);

    // 3. Compare resolve_gateway_max_output_tokens across matrix inputs
    let mut actual_gateway = Vec::new();
    for case in &expected.gateway_calculations {
        let m = catalog
            .get_model(&case.provider_id, &case.model_id)
            .expect("gateway test model exists");
        let res = m.resolve_gateway_max_output_tokens(
            case.estimated_input_tokens,
            case.requested_max_tokens,
            case.explicit_reasoning_budget,
        );
        actual_gateway.push(OracleGatewayCase {
            provider_id: case.provider_id.clone(),
            model_id: case.model_id.clone(),
            estimated_input_tokens: case.estimated_input_tokens,
            requested_max_tokens: case.requested_max_tokens,
            explicit_reasoning_budget: case.explicit_reasoning_budget,
            expected_ok: res.as_ref().ok().copied(),
            expected_overflow: res.is_err(),
        });
    }
    assert_eq!(actual_gateway, expected.gateway_calculations);
}
