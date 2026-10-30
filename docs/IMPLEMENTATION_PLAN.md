# Implementation Plan (`models-catalog`)

## Phase 1: Workspace Scaffolding, Core Types & Error Hierarchy
1. **Configure `Cargo.toml`:**
   - Crate name: `models-catalog`, Edition `2024` (`rust-version = "1.85"`+).
   - Feature flags:
     - `default = ["reqwest-client", "disk-spool", "cline", "bundled"]`
     - `reqwest-client`, `disk-spool`, `cline`, `bundled`, `ts-bindings`, `cli`
   - Dependencies: `serde`, `serde_json`, `thiserror`, `tracing`, `chrono`, `flate2`, `reqwest`, `tokio`, `directories`, `rand`, `ts-rs`, `clap`.
   - Dev-dependencies: `wiremock`, `tempfile`, `similar-asserts`, `tokio-test`.
2. **Implement Core Modules:**
   - `src/error.rs`: `CatalogError` and `ContextOverflowError`.
   - `src/config.rs`: `SpoolConfig`, `RateLimitConfig`, `UpstreamUrls`, builder ergonomics.
   - `src/types.rs`: `CatalogSnapshot`, `ProviderSpec`, `ProviderProtocol`, `ModelInfo`, `Limit`, `Modalities`, `Modality`, `Cost`, `BillingTier`, `ModelOverrides`, `SourceStatus`, `DataOrigin` with optional `ts-rs` derives.

## Phase 2: Built-ins, Normalization Pipeline & Runtime Calculators (Pure Core)
1. **Built-in Provider Specs (`src/builtins.rs` & `src/generated/builtin_specs.json`):**
   - Create `src/generated/builtin_specs.json` with Cline's `BUILTIN_SPEC_OVERRIDES` (`anthropic`, `openrouter`, `bedrock`, `openai-native`, `zai`, `aihubmix`, `kilo`, `opencode`, `vercel-ai-gateway`, `cline-pass`, `google`, `vertex`, `ollama`) and load it at compile time in `src/builtins.rs`.
2. **Token Limit Normalizer & Gateway Calculator (`src/normalize/limits.rs`):**
   - Implement `resolve_max_input_tokens`, `resolve_max_output_tokens`, and `ModelInfo::resolve_gateway_max_output_tokens`.
3. **Directional Modalities & Chat Filter (`src/normalize/modalities.rs`):**
   - Filter to `{Text, Image, Audio, Video, Pdf}`, prune incomplete declarations, strip `Text` from `modalities.output` on `gpt-image*` models, and implement `ModelInfo::is_chat_model()`.
4. **Release-Date Sorting & 3-Layer Default Model Resolution (`src/normalize/resolution.rs`):**
   - Sort provider models by `release_date` descending with lexicographical ID tie-breaking.
   - Implement `CatalogSnapshot::resolve_default_model(provider_id, user_settings_model)`.
5. **`models.dev` & Cline Curated Normalizers (`src/normalize/models_dev.rs`, `src/normalize/cline_feed.rs`, `src/normalize/mod.rs`):**
   - Parse raw `models.dev/api.json`.
   - Parse `api.cline.bot/api/v1/ai/cline/recommended-models`, cross-reference `models.dev` for `cline-pass/*` models, populate `cline-pass` and `openrouter` catalogs, set `BillingTier`, `is_recommended`, and `requires_streaming: true`.
6. **Reasoning Clamping & Wire Translation (`src/reasoning.rs`):**
   - Implement `ReasoningEffort`, `normalize_reasoning_effort`, and `to_wire_reasoning_params`.
7. **Flexible Lookup, Overrides & Local Settings (`src/lookup.rs`, `src/local_settings.rs`):**
   - Implement `get_model`, `find_model`, `ModelInfo::with_overrides`, and `~/.cline/data/settings/providers.json` validation.

## Phase 3: Per-Source Disk Spool, SWR Cache, Singleflight & Bundled Snapshot
1. **Per-Source Atomic Disk Spool (`src/spool/disk.rs`):**
   - Store `sources/models_dev.json`, `sources/cline_recommended.json`, `spool_meta.json`, and `catalog.json` via atomic temp-file writes (`<name>.tmp.<pid>.<counter>`).
2. **Singleflight Coalescer, Cooldown Rate Limiter & Conditional HTTP (`src/spool/limiter.rs`):**
   - Coalesce concurrent refreshes per `CatalogManager` instance.
   - Enforce `min_refresh_cooldown` per upstream source.
   - Send `If-None-Match` (`ETag`) / `If-Modified-Since` headers; handle `304 Not Modified` without re-downloading.
   - Implement exponential backoff with jitter on `429` / `5xx`, plus graceful per-source fallback when an upstream fails.
3. **In-Memory SWR Cache & `CatalogManager` (`src/spool/memory.rs`, `src/spool/mod.rs`):**
   - Implement `Arc<RwLock<...>>` snapshot caching with background SWR task spawning.
4. **Bundled Compile-Time Snapshot (`src/bundled.rs` & `assets/bundled_catalog.json.gz`):**
   - Generate and compress a baseline `CatalogSnapshot` into `assets/bundled_catalog.json.gz` and expose `CatalogSnapshot::bundled()`.

## Phase 4: CLI Binary (`src/bin/main.rs`) & TypeScript Bindings
1. **Build `models-catalog` CLI (`--features cli`):**
   - Subcommands: `sync`, `inspect`, `list`, `export-json`, `export-ts`, `check-local-config`.
2. **Verify TypeScript Bindings (`--features ts-bindings`):**
   - Ensure `ts-rs` exports clean `.d.ts` definitions into `./bindings/`.

## Phase 5: Parity Harness, Comprehensive Test Suite & CI Workflow
1. **Fixtures & Parity Harness (`parity-harness/`, `tests/fixtures/`):**
   - Create `tests/fixtures/models-dev.sample.json`, `tests/fixtures/cline-recommended.sample.json`, and `tests/fixtures/oracle-expected.json`.
   - Write `parity-harness/package.json` and `parity-harness/dump-oracle.ts`.
   - Write `.github/workflows/upstream-parity.yml`.
2. **Integration & Verification Tests:**
   - `tests/normalization_tests.rs`: Unit/integration coverage for limits, modalities, `gpt-image*`, 3-layer defaults, reasoning, lookup, and overrides.
   - `tests/spool_tests.rs`: `wiremock` tests for 20-caller singleflight coalescing, cooldown enforcement, `304 Not Modified`, `429` retry backoff, and partial upstream failure degradation.
   - `tests/oracle_parity.rs`: Differential parity test against `oracle-expected.json` using `similar-asserts`.
   - Verify `cargo test --no-default-features`, `cargo test --all-features`, and CLI smoke tests.

