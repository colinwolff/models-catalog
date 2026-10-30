# Normalization Rules, Business Logic & `@cline/llms` Parity

This document specifies the exact normalization semantics, runtime token/reasoning calculations, lookup rules, and automated parity verification against Cline's `@cline/llms` package (`sdk/packages/llms`).

---

## 1. Token Limit Resolution (`src/normalize/limits.rs`)

### Constants
```rust
pub const DEFAULT_MAX_INPUT_TOKENS: u64 = 128_000;
pub const DEFAULT_MAX_TOKENS: u64 = 4_096;
pub const DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS: u64 = 32_000;
pub const GATEWAY_OUTPUT_RESERVE_TOKENS: u64 = 1_024;
```

### Static Catalog Limit Normalization
For each model parsed from `models.dev` (`limit: { context, input, output }`):
1. **`context_window: Option<u64>`**:
   - Directly set to `limit.context` (filtering `Some(0)` as `None`).
2. **`max_input_tokens: u64`**:
   - If both `limit.context` and `limit.input` are `Some(n)` (with `n > 0`), return `std::cmp::min(context, input)`.
   - Otherwise return `limit.input.or(limit.context).unwrap_or(DEFAULT_MAX_INPUT_TOKENS)`.
3. **`max_tokens: u64`** (reported maximum output budget):
   - Return `limit.output.filter(|&n| n > 0).unwrap_or(DEFAULT_MAX_TOKENS)`.

### Runtime Gateway Output Token Calculation
`ModelInfo::resolve_gateway_max_output_tokens` implements `resolveGatewayRequestMaxTokens` from `sdk/packages/llms/src/providers/gateway.ts`:

```rust
pub fn resolve_gateway_max_output_tokens(
    &self,
    estimated_input_tokens: u64,
    requested_max_tokens: Option<u64>,
    explicit_reasoning_budget: Option<u64>,
) -> Result<u64, ContextOverflowError>
```

**Algorithm:**
1. **Reasoning floor:**
   \[
   \text{reasoning\_floor} = \begin{cases}
   \text{budget} + \text{GATEWAY\_OUTPUT\_RESERVE\_TOKENS} & \text{if } \text{explicit\_reasoning\_budget} = \text{Some}(\text{budget}) \\
   0 & \text{otherwise}
   \end{cases}
   \]
2. **Desired output target (`base_cap`):**
   - If `requested_max_tokens` is `Some(req)`, use `std::cmp::max(req, reasoning_floor)`.
   - Otherwise use `std::cmp::max(DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS, reasoning_floor)`.
3. **Model output limit clamp:**
   - Clamp `base_cap` to `self.max_tokens` (`let mut resolved = std::cmp::min(base_cap, self.max_tokens)`).
4. **Context window headroom clamp:**
   - If `self.context_window` is `Some(ctx)`:
     - Compute required input + reserve: `used = estimated_input_tokens.saturating_add(GATEWAY_OUTPUT_RESERVE_TOKENS)`.
     - If `used >= ctx`, return `Err(ContextOverflowError { context_window: ctx, estimated_input_tokens, reserve_tokens: GATEWAY_OUTPUT_RESERVE_TOKENS })`.
     - Compute `available_context = ctx - estimated_input_tokens` (or `ctx - used` depending on whether provider counts reserve against output; in `@cline/llms`, available output headroom is `ctx - estimated_input_tokens - GATEWAY_OUTPUT_RESERVE_TOKENS`, minimum `1` when `estimated_input_tokens + GATEWAY_OUTPUT_RESERVE_TOKENS < ctx`).
     - Set `resolved = std::cmp::min(resolved, available_context)`.
5. Return `Ok(resolved)`.

---

## 2. Directional Modalities & Model Filtering (`src/normalize/modalities.rs`)

1. **Known Modality Set:**
   - Filter raw `modalities.input` and `modalities.output` strings strictly to `{ "text", "image", "audio", "video", "pdf" }` (`Modality::{Text, Image, Audio, Video, Pdf}`), preserving order and deduplicating entries.
2. **Incomplete Modality Pruning:**
   - If a model declares a `modalities` object where either the filtered `input` vec or filtered `output` vec is empty **before** special-case stripping, discard the incomplete `modalities` declaration (`modalities = None`) so chat models with malformed upstream declarations are not broken.
3. **`gpt-image` Family Workaround:**
   - Dedicated OpenAI image generation models matching `id.starts_with("gpt-image")` or `family.as_deref() == Some("gpt-image")` erroneously declare `text` in `modalities.output` in `models.dev`.
   - Strip `Modality::Text` from `modalities.output` for these models so they remain in the catalog as image-output models without being misclassified as text chat models.
4. **Chat Model Classification (`ModelInfo::is_chat_model(&self) -> bool`):**
   - Returns `true` if `self.modalities` is `None` **OR** (`modalities.input.contains(&Modality::Text)` **AND** `modalities.output.contains(&Modality::Text)`).

---

## 3. Built-in Provider Overrides & 3-Layer Default Model Resolution (`src/builtins.rs`, `src/normalize/resolution.rs`)

### Built-in Provider Overrides (`BUILTIN_SPEC_OVERRIDES`)
Loaded from `src/generated/builtin_specs.json` (synced from `@cline/llms` `sdk/packages/llms/src/providers/builtins.ts`), defining:
- Pinned `default_model_id` overrides:
  - `anthropic` → `"claude-sonnet-5"`
  - `openrouter` → `"anthropic/claude-sonnet-5"`
  - `bedrock` → `"minimax.minimax-m2.5"`
  - `openai-native` / `openai` → `"gpt-5.4"`
  - `zai` / `zhipuai` → `"zai-org/glm-5.3"`
  - Plus `aihubmix`, `kilo`, `opencode`, `vercel-ai-gateway`, `cline-pass`, `google`, `vertex`, `ollama`, etc.
- Provider `protocol: ProviderProtocol` (`Anthropic`, `OpenAiCompatible`, `OpenAiResponses`, `Google`, `Bedrock`, `Vertex`, `Ollama`, `ClinePass`).
- Provider `requires_streaming: bool` (`true` for `cline-pass` / `api.cline.bot`).

### Release-Date Model Sorting
Within every `ProviderSpec`, `sorted_model_ids` orders models by:
1. `release_date` descending (newest ISO date `YYYY-MM-DD` first; models with `Some(release_date)` precede `None`).
2. Deterministic lexicographical tie-breaking by `model.id` ascending (`a.id.cmp(&b.id)`).

### 3-Layer Default Model Resolution
```rust
pub fn resolve_default_model(
    &self,
    provider_id: &str,
    user_settings_model: Option<&str>,
) -> Option<String>
```
1. **Layer 1 (Caller / User Settings Override):** If `user_settings_model` is `Some(s)` and `!s.trim().is_empty()`, return `Some(s.trim().to_string())`.
2. **Layer 2 (`BUILTIN_SPEC_OVERRIDES`):** If the provider has a pinned `default_model_id` in `BUILTIN_SPEC_OVERRIDES`, return it.
3. **Layer 3 (Catalog Release-Date Discovery):**
   - Find the first model in `provider.sorted_model_ids` where `model.is_chat_model() == true`.
   - If no model satisfies `is_chat_model()`, fall back to the first key in `provider.sorted_model_ids`.

---

## 4. Cline Recommended, Free & ClinePass Enrichment (`src/normalize/cline_feed.rs`)

When the `cline` feature is enabled, `models-catalog` ingests `https://api.cline.bot/api/v1/ai/cline/recommended-models`:

1. **Feed Categories & Billing Tiers:**
   - `recommended`: Sets `is_recommended = true` and `billing_tier = Some(BillingTier::Credits)` (unless overridden by `free` or `clinePass`).
   - `free`: Sets `billing_tier = Some(BillingTier::Free)`.
   - `clinePass`: Sets `billing_tier = Some(BillingTier::Subscription)` and retains the `cline-pass/<slug>` wire ID.
2. **Cross-Reference Metadata Inheritance:**
   - Many entries in `recommended-models` (especially `cline-pass/<provider>/<model>` or `cline-pass/<slug>`) carry partial metadata or rely on upstream `models.dev` definitions.
   - The normalizer resolves the underlying model in `models.dev` (checking the `cline-pass` provider entry in `models.dev`, `openrouter`, and the native upstream provider by stripping `cline-pass/` and `:free` suffixes) so the enriched `ModelInfo` inherits full `context_window`, `max_input_tokens`, `max_tokens`, `modalities`, `cost`, `supports_reasoning`, `reasoning_options`, and `release_date`.
   - Explicit overrides present in the Cline feed take precedence over base `models.dev` fields.
3. **Dual Catalog Population:**
   - Populates a dedicated `cline-pass` (and alias `cline`) provider spec with `protocol = ProviderProtocol::ClinePass` and `requires_streaming = true`.
   - Enriches matching models in the `openrouter` provider spec with `is_recommended` and `billing_tier`.

---

## 5. Reasoning Effort Clamping & Wire Translation (`src/reasoning.rs`)

### `ReasoningEffort` Enum
Ordered from lowest to highest intensity:
```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}
```

### Effort Clamping (`normalize_reasoning_effort`)
```rust
pub fn normalize_reasoning_effort(
    model: &ModelInfo,
    requested: ReasoningEffort,
) -> Option<ReasoningEffort>
```
- If `!model.supports_reasoning` and `model.reasoning_options.is_empty()`, returns `None`.
- If `model.reasoning_options` is empty but `model.supports_reasoning` is `true`, defaults supported options to `[Low, Medium, High]`.
- If `requested` is in `supported`, returns `Some(requested)`.
- Otherwise clamps `requested` to the closest supported level by ordinal rank (preferring the nearest lower supported level on ties, or the minimum/maximum supported level if out of bounds).

### Wire Parameter Translation (`to_wire_reasoning_params`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireReasoningParams {
    /// No reasoning parameters should be sent on the wire.
    Disabled,
    /// Anthropic / Bedrock Claude extended thinking (`thinking: { type: "enabled", budget_tokens }`).
    AnthropicThinking { budget_tokens: u64 },
    /// OpenAI Chat Completions / Responses (`reasoning_effort: "low" | "medium" | "high"`).
    OpenAiEffort { reasoning_effort: String },
    /// Zhipu GLM / DeepSeek thinking toggle (`thinking: { type: "enabled" | "disabled" }`).
    ThinkingToggle { enabled: bool },
    /// Google Gemini thinking budget (`thinkingConfig: { thinkingBudget }`).
    GoogleThinking { thinking_budget: u64 },
}
```

**Budget Calculation for Token-Budgeted Thinking (`AnthropicThinking` / `GoogleThinking`):**
- Default target budgets by effort:
  - `None` → `WireReasoningParams::Disabled`
  - `Low` → `4_096` (or `1_024` minimum if `max_output_tokens` is small)
  - `Medium` → `10_240`
  - `High` → `16_384`
  - `XHigh` → `32_768`
  - `Max` → `max_output_tokens.saturating_sub(GATEWAY_OUTPUT_RESERVE_TOKENS)`
- Clamped so `budget_tokens <= max_output_tokens.saturating_sub(GATEWAY_OUTPUT_RESERVE_TOKENS)` and `budget_tokens >= 1_024` (if max available thinking room is `< 1_024`, returns `WireReasoningParams::Disabled`).

---

## 6. Flexible Lookup & Override Precedence (`src/lookup.rs`)

### Lookup Methods on `CatalogSnapshot`
1. `get_model(provider_id: &str, model_id: &str) -> Option<&ModelInfo>`:
   - Exact lookup in `providers.get(provider_id)?.models.get(model_id)`.
2. `find_model(query: &str) -> Option<&ModelInfo>`:
   - **Step 1 (Direct `cline-pass/*`):** If `query.starts_with("cline-pass/")`, check `providers.get("cline-pass")` first.
   - **Step 2 (`provider/model` split):** If `query` contains `/`, split on first `/` into `(prefix, rest)` and check `get_model(prefix, rest)` and `get_model(prefix, query)`.
   - **Step 3 (`openrouter` qualified ID):** Check `get_model("openrouter", query)`.
   - **Step 4 (Suffix-stripped fallback):** Strip `:free` or `:thinking` suffix and repeat Steps 1–3.
   - **Step 5 (Unqualified scan):** Search providers in priority order (`BUILTIN_SPEC_OVERRIDES` providers first, then alphabetical) for an exact `model_id == query`.

### Override Layering (`ModelInfo::with_overrides`)
Applies `ModelOverrides` using the strict precedence rule:
`caller_override > local_settings > catalog > default`
When `overrides.context_window` or `overrides.max_input_tokens` or `overrides.max_output_tokens` is set, `with_overrides` returns an updated `ModelInfo` whose `resolve_gateway_max_output_tokens` and `to_wire_reasoning_params` automatically respect the overridden limits.

---

## 7. Automated Upstream Parity Harness (`parity-harness/`)

To guarantee continuous 1:1 parity with `@cline/llms`:

1. **`parity-harness/dump-oracle.ts` (Bun + `@cline/llms`):**
   - Exports `src/generated/builtin_specs.json` directly from `@cline/llms/providers`.
   - Runs deterministic sample fixtures (`tests/fixtures/models-dev.sample.json` and `tests/fixtures/cline-recommended.sample.json`) through `@cline/llms` normalizers and gateway calculators to produce `tests/fixtures/oracle-expected.json`.
2. **`tests/oracle_parity.rs` (`similar-asserts`):**
   - Runs `models-catalog`'s normalizer and gateway calculator on the exact same fixtures and asserts 1:1 equality against `oracle-expected.json` across:
     - Resolved `default_model_id` per provider
     - `context_window`, `max_input_tokens`, and `max_tokens`
     - Directional `modalities.input` and `modalities.output`
     - `cline-pass/*` model merging and `billing_tier`
     - `resolve_gateway_max_output_tokens()` across overflow, 32k default cap, and reasoning floor cases
3. **`.github/workflows/upstream-parity.yml`:**
   - Scheduled daily (`0 6 * * *`) + manual `workflow_dispatch`.
   - Updates `@cline/llms` in `parity-harness/`, regenerates `src/generated/builtin_specs.json` and `oracle-expected.json`, and runs `cargo test --all-features`.
   - Opens an automated PR if only generated specs changed, or files an `upstream-drift` issue if logic diverged.

