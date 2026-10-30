use crate::normalize::limits::GATEWAY_OUTPUT_RESERVE_TOKENS;
use crate::types::{ModelInfo, ProviderProtocol, ReasoningEffort, WireReasoningParams};

/// Minimum valid thinking token budget for Anthropic/Google token-budgeted reasoning.
pub const MIN_THINKING_BUDGET_TOKENS: u64 = 1_024;

/// Clamps `requested` reasoning effort to the closest level supported by `model.reasoning_options`.
///
/// - Returns `None` if `!model.supports_reasoning` and `model.reasoning_options.is_empty()`.
/// - If `model.reasoning_options` is empty but `model.supports_reasoning` is `true`, assumes
///   `[Low, Medium, High]`.
/// - On ordinal distance ties, prefers the lower effort level.
pub fn normalize_reasoning_effort(
    model: &ModelInfo,
    requested: ReasoningEffort,
) -> Option<ReasoningEffort> {
    if !model.supports_reasoning && model.reasoning_options.is_empty() {
        return None;
    }

    let default_options = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];

    let supported: &[ReasoningEffort] = if !model.reasoning_options.is_empty() {
        &model.reasoning_options
    } else {
        &default_options
    };

    if supported.contains(&requested) {
        return Some(requested);
    }

    let target_rank = i16::from(requested.rank());
    supported.iter().copied().min_by(|a, b| {
        let dist_a = (i16::from(a.rank()) - target_rank).abs();
        let dist_b = (i16::from(b.rank()) - target_rank).abs();
        dist_a.cmp(&dist_b).then_with(|| a.rank().cmp(&b.rank()))
    })
}

/// Maps a normalized `ReasoningEffort` to a token budget for providers that use explicit token budgets
/// (such as Anthropic `thinking.budget_tokens` and Google `thinkingConfig.thinkingBudget`).
pub fn effort_to_thinking_budget(effort: ReasoningEffort, max_output_tokens: u64) -> Option<u64> {
    let available = max_output_tokens.saturating_sub(GATEWAY_OUTPUT_RESERVE_TOKENS);
    if available < MIN_THINKING_BUDGET_TOKENS {
        return None;
    }

    let raw_target = match effort {
        ReasoningEffort::None => return None,
        ReasoningEffort::Low => 4_096,
        ReasoningEffort::Medium => 10_240,
        ReasoningEffort::High => 16_384,
        ReasoningEffort::XHigh => 32_768,
        ReasoningEffort::Max => available,
    };

    Some(std::cmp::max(
        MIN_THINKING_BUDGET_TOKENS,
        std::cmp::min(raw_target, available),
    ))
}

fn is_thinking_toggle_model(model: &ModelInfo) -> bool {
    let id_lower = model.id.to_ascii_lowercase();
    let fam_lower = model
        .family
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    id_lower.contains("glm-")
        || id_lower.contains("/glm")
        || fam_lower.contains("glm")
        || id_lower.contains("deepseek-v3")
        || id_lower.contains("deepseek-r1")
        || fam_lower.contains("deepseek")
}

fn is_anthropic_claude_model(model: &ModelInfo) -> bool {
    let id_lower = model.id.to_ascii_lowercase();
    let fam_lower = model
        .family
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    id_lower.contains("claude-") || fam_lower.contains("claude")
}

/// Translates a requested `ReasoningEffort` and `max_output_tokens` into provider-specific wire parameters.
///
/// Covers:
/// - Anthropic `thinking: { type: "enabled", budget_tokens }`
/// - OpenAI `reasoning_effort`
/// - GLM / DeepSeek `thinking: { type: "enabled" | "disabled" }` toggles
/// - Google / Vertex `thinkingConfig: { thinkingBudget }`
pub fn to_wire_reasoning_params(
    provider_protocol: ProviderProtocol,
    model: &ModelInfo,
    effort: ReasoningEffort,
    max_output_tokens: u64,
) -> WireReasoningParams {
    let Some(normalized) = normalize_reasoning_effort(model, effort) else {
        return WireReasoningParams::Disabled;
    };

    // Check GLM / DeepSeek thinking toggle models first
    if is_thinking_toggle_model(model) {
        return WireReasoningParams::ThinkingToggle {
            enabled: normalized != ReasoningEffort::None,
        };
    }

    if normalized == ReasoningEffort::None {
        return WireReasoningParams::Disabled;
    }

    match provider_protocol {
        ProviderProtocol::Anthropic => {
            match effort_to_thinking_budget(normalized, max_output_tokens) {
                Some(budget_tokens) => WireReasoningParams::AnthropicThinking { budget_tokens },
                None => WireReasoningParams::Disabled,
            }
        }
        ProviderProtocol::Bedrock => {
            if is_anthropic_claude_model(model) {
                match effort_to_thinking_budget(normalized, max_output_tokens) {
                    Some(budget_tokens) => WireReasoningParams::AnthropicThinking { budget_tokens },
                    None => WireReasoningParams::Disabled,
                }
            } else {
                WireReasoningParams::OpenAiEffort {
                    reasoning_effort: normalized.as_str().to_string(),
                }
            }
        }
        ProviderProtocol::Google | ProviderProtocol::Vertex => {
            if is_anthropic_claude_model(model) {
                match effort_to_thinking_budget(normalized, max_output_tokens) {
                    Some(budget_tokens) => WireReasoningParams::AnthropicThinking { budget_tokens },
                    None => WireReasoningParams::Disabled,
                }
            } else {
                match effort_to_thinking_budget(normalized, max_output_tokens) {
                    Some(thinking_budget) => {
                        WireReasoningParams::GoogleThinking { thinking_budget }
                    }
                    None => WireReasoningParams::Disabled,
                }
            }
        }
        ProviderProtocol::ClinePass => {
            if is_anthropic_claude_model(model) {
                match effort_to_thinking_budget(normalized, max_output_tokens) {
                    Some(budget_tokens) => WireReasoningParams::AnthropicThinking { budget_tokens },
                    None => WireReasoningParams::Disabled,
                }
            } else {
                WireReasoningParams::OpenAiEffort {
                    reasoning_effort: normalized.as_str().to_string(),
                }
            }
        }
        ProviderProtocol::OpenAiCompatible
        | ProviderProtocol::OpenAiResponses
        | ProviderProtocol::Ollama => WireReasoningParams::OpenAiEffort {
            reasoning_effort: normalized.as_str().to_string(),
        },
    }
}

impl ModelInfo {
    /// Clamps `requested` reasoning effort to the closest level supported by `self.reasoning_options`.
    pub fn normalize_reasoning_effort(
        &self,
        requested: ReasoningEffort,
    ) -> Option<ReasoningEffort> {
        normalize_reasoning_effort(self, requested)
    }

    /// Generates provider-specific wire reasoning parameters for `self`.
    pub fn to_wire_reasoning_params(
        &self,
        provider_protocol: ProviderProtocol,
        effort: ReasoningEffort,
        max_output_tokens: u64,
    ) -> WireReasoningParams {
        to_wire_reasoning_params(provider_protocol, self, effort, max_output_tokens)
    }
}
