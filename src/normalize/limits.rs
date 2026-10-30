use crate::error::ContextOverflowError;
use crate::types::{Limit, ModelInfo};

/// Fallback `max_input_tokens` when neither `limit.context` nor `limit.input` is present.
pub const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;

/// Fallback `max_tokens` (output budget) when `limit.output` is absent.
pub const DEFAULT_MAX_TOKENS: u64 = 4_096;

/// Default gateway output token cap when caller does not specify `requested_max_tokens`.
pub const DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS: u64 = 32_000;

/// Token reserve buffer subtracted from remaining context window (and added above explicit reasoning budget).
pub const GATEWAY_OUTPUT_RESERVE_TOKENS: u64 = 1_024;

/// Resolves normalized `context_window` (`limit.context`, treating `0` as `None`).
pub fn resolve_context_window(limit: &Limit) -> Option<u64> {
    limit.context.filter(|&n| n > 0)
}

/// Resolves `max_input_tokens`:
/// - If both `limit.context` and `limit.input` are present (`> 0`), returns `min(context, input)`.
/// - Otherwise returns `limit.input.or(limit.context).unwrap_or(DEFAULT_MAX_INPUT_TOKENS)`.
pub fn resolve_max_input_tokens(limit: &Limit) -> u64 {
    let context = limit.context.filter(|&n| n > 0);
    let input = limit.input.filter(|&n| n > 0);

    match (context, input) {
        (Some(c), Some(i)) => std::cmp::min(c, i),
        (None, Some(i)) => i,
        (Some(c), None) => c,
        (None, None) => DEFAULT_MAX_INPUT_TOKENS,
    }
}

/// Resolves `max_tokens` (reported output budget):
/// - Returns `limit.output` (`> 0`) or falls back to `DEFAULT_MAX_TOKENS` (`4_096`).
pub fn resolve_max_output_tokens(limit: &Limit) -> u64 {
    limit
        .output
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
}

impl ModelInfo {
    /// Calculates the safe gateway `max_tokens` for an outbound request (`resolveGatewayRequestMaxTokens` parity).
    ///
    /// - Computes `reasoning_floor = explicit_reasoning_budget.map(|b| b + GATEWAY_OUTPUT_RESERVE_TOKENS).unwrap_or(0)`.
    /// - Computes `base_cap` from `requested_max_tokens` (or `DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS`), lifted by `reasoning_floor`.
    /// - Clamps to `self.max_tokens` and `context_window - estimated_input_tokens - GATEWAY_OUTPUT_RESERVE_TOKENS`.
    /// - Returns `Err(ContextOverflowError)` if remaining context after reserve is `<= 0`.
    pub fn resolve_gateway_max_output_tokens(
        &self,
        estimated_input_tokens: u64,
        requested_max_tokens: Option<u64>,
        explicit_reasoning_budget: Option<u64>,
    ) -> Result<u64, ContextOverflowError> {
        let reasoning_floor = explicit_reasoning_budget
            .filter(|&b| b > 0)
            .map(|b| b.saturating_add(GATEWAY_OUTPUT_RESERVE_TOKENS))
            .unwrap_or(0);

        let base_cap = match requested_max_tokens.filter(|&r| r > 0) {
            Some(req) => std::cmp::max(req, reasoning_floor),
            None => std::cmp::max(DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS, reasoning_floor),
        };

        let mut resolved = std::cmp::min(base_cap, self.max_tokens);

        if let Some(ctx) = self.context_window.filter(|&c| c > 0) {
            let used_with_reserve =
                estimated_input_tokens.saturating_add(GATEWAY_OUTPUT_RESERVE_TOKENS);
            if used_with_reserve >= ctx {
                return Err(ContextOverflowError {
                    context_window: ctx,
                    estimated_input_tokens,
                    reserve_tokens: GATEWAY_OUTPUT_RESERVE_TOKENS,
                });
            }
            let remaining_context = ctx - used_with_reserve;
            resolved = std::cmp::min(resolved, remaining_context);
        }

        Ok(resolved)
    }
}
