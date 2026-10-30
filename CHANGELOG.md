# Changelog

All notable changes to `models-catalog` will be documented in this file.

## [0.1.0] - 2026-10-30

### Added
- Multi-layer catalog normalization combining `models.dev` (`https://models.dev/api.json`) with pluggable [`VendorPlugin`](src/vendors/mod.rs) overlays (`src/vendors/cline/`).
- Exact `@cline/llms` token limit normalization (`resolve_max_input_tokens`, `resolve_max_output_tokens`) and runtime gateway output calculator (`ModelInfo::resolve_gateway_max_output_tokens`).
- Directional modality sanitization (`{Text, Image, Audio, Video, Pdf}`), incomplete modality pruning, `gpt-image*` output `Text` stripping, and `ModelInfo::is_chat_model()`.
- 3-layer default model resolution (`user_settings` -> `BUILTIN_SPEC_OVERRIDES` -> newest chat model sorted by `release_date` descending with lexicographical tie-breaking).
- Reasoning effort clamping (`normalize_reasoning_effort`) and provider wire parameter translation (`to_wire_reasoning_params`) for Anthropic, OpenAI, Google/Vertex, Bedrock, Ollama, Z.AI/DeepSeek thinking toggles, and ClinePass.
- Flexible model lookup (`get_model`, `find_model`, `is_cline_pass_model`, `recommended_models`) and `ModelOverrides` layering (`caller_override > local_settings > catalog > default`).
- High-throughput `CatalogManager` with in-memory SWR cache, singleflight request coalescing, per-source atomic disk spooling, conditional HTTP (`ETag` / `304 Not Modified`), rate-limiting cooldowns, and compile-time `bundled` snapshot fallback.
- Optional `models-catalog` CLI (`--features cli`) and `ts-rs` TypeScript bindings (`--features ts-bindings`).

