# `models-catalog`

A production-grade Rust library crate and optional CLI for fetching, normalizing, merging, caching, and querying LLM provider and model catalogs.

`models-catalog` combines the comprehensive public model registry from [`models.dev`](https://models.dev) (200+ providers, 6,000+ models) with optional vendor overlays—starting with first-class parity for Cline's `@cline/llms` (`api.cline.bot` recommended/free/ClinePass feeds and `BUILTIN_SPEC_OVERRIDES`).

It is designed as a standalone, zero-compromise crate that works just as well inside high-throughput backend orchestrators and CLI tools as it does powering rich desktop/web model pickers and TypeScript frontends.

---

## Key Capabilities

- **Multi-Layer Catalog Merging:**
  1. **Base Layer (`models.dev`):** Raw limits (`context`, `input`, `output`), directional modalities (`input`, `output`), token pricing (`cost`), tool calling, reasoning support, and release dates.
  2. **Built-in Provider Specs (`cline` feature):** Protocol classification (`Anthropic`, `OpenAiCompatible`, `OpenAiResponses`, `Google`, `Bedrock`, `Vertex`, `Ollama`, `ClinePass`), pinned default models (`BUILTIN_SPEC_OVERRIDES`), and auth/env metadata.
  3. **Live Curated Overlay (`cline` feature):** Merges `https://api.cline.bot/api/v1/ai/cline/recommended-models` (`recommended`, `free`, and `cline-pass/*` models), annotating `BillingTier` (`Subscription`, `Free`, `Credits`), cross-referencing `models.dev` metadata, and flagging `requires_streaming: true` where required.
- **Exact `@cline/llms` Normalization & Runtime Calculators:**
  - Token limit resolution (`resolve_max_input_tokens`) and gateway output budget calculation (`resolve_gateway_max_output_tokens`) with context overflow detection and reasoning floor adjustments.
  - Directional modality sanitization (including the `gpt-image*` family output `Text` strip workaround) and `is_chat_model()` classification.
  - 3-layer default model resolution (`user_override` → `BUILTIN_SPEC_OVERRIDES` → newest chat model by `release_date` descending with deterministic tie-breaking).
  - Reasoning effort normalization (`None`, `Low`, `Medium`, `High`, `XHigh`, `Max`) and provider-specific wire parameter translation (`to_wire_reasoning_params`).
- **Flexible Lookup & Override Precedence:**
  - Query by `(provider_id, model_id)` or qualified strings (`anthropic/claude-sonnet-5`, `cline-pass/minimax-m2.5`, `:free` suffix fallback).
  - Apply caller overrides cleanly with `ModelOverrides` following `caller_override > local_settings > catalog > default`.
- **High-Throughput Caching, Per-Source Spooling & Rate Limiting:**
  - **In-memory cache** with configurable TTL and Stale-While-Revalidate (SWR) background refresh.
  - **Singleflight coalescing** so thousands of concurrent `.get_catalog().await` calls trigger at most one outbound HTTP request per upstream.
  - **Per-source atomic disk spool** (`models_dev.json`, `cline_recommended.json`, `spool_meta.json`, and merged `catalog.json`) with `ETag` / `If-None-Match` (`304 Not Modified`) handling, cooldown rate-limiting, and graceful degradation when an upstream endpoint is unreachable.
  - **Injectable paths & hermetic testing:** No forced global singletons or ambient environment reads; pass any cache directory or run completely offline with `--no-default-features` or the `bundled` snapshot.
- **TypeScript Bindings (`ts-bindings` feature):**
  - Export `.d.ts` definitions via `ts-rs` for seamless sharing with TypeScript UIs.

---

## Cargo Feature Flags

```toml
[features]
default = ["reqwest-client", "disk-spool", "cline", "bundled"]

# Async HTTP fetching via reqwest + tokio, singleflight coalescing, and rate limiting
reqwest-client = ["dep:reqwest", "dep:tokio", "dep:rand"]

# Atomic disk caching, ETag persistence, and XDG default directory resolution
disk-spool = ["dep:tokio", "dep:directories"]

# Cline-specific enrichments: BUILTIN_SPEC_OVERRIDES, api.cline.bot recommended/ClinePass
# feed normalizer, and ~/.cline/data/settings/providers.json config reader
cline = []

# Embeds a compressed compile-time fallback catalog snapshot via include_bytes!
# so offline/cold-start environments always have a complete catalog immediately available
bundled = ["dep:flate2"]

# Derives ts-rs TypeScript interface bindings on public catalog types
ts-bindings = ["dep:ts-rs"]

# Builds the `models-catalog` CLI binary
cli = ["reqwest-client", "disk-spool", "cline", "bundled", "dep:clap"]
```

> **Hermetic / Pure-Core Usage:** Compiling with `default-features = false` (or `features = ["cline", "bundled"]`) strips all network and disk I/O dependencies, providing a pure synchronous crate for parsing, lookup, override resolution, and token/reasoning calculations.

---

## Quickstart

### 1. Async Catalog Manager (with SWR, Disk Spool & Rate Limiting)

```rust
use models_catalog::{CatalogManager, ModelOverrides, ReasoningEffort};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), models_catalog::CatalogError> {
    // Uses sensible XDG defaults (~/.cache/models-catalog), or inject a custom path:
    let manager = CatalogManager::builder()
        .with_spool_dir(PathBuf::from("/tmp/my-app-catalog-cache"))
        .build()?;

    let catalog = manager.get_catalog().await?;

    // Resolve default model for a provider (User override -> Builtin override -> Newest chat model)
    let default_id = catalog.resolve_default_model("anthropic", None);
    println!("Default Anthropic model: {:?}", default_id);

    // Lookup model by provider + ID or qualified wire ID
    if let Some(model) = catalog.find_model("cline-pass/minimax-m2.5") {
        // Calculate safe gateway max output tokens with a 16k reasoning budget
        let max_out = model.resolve_gateway_max_output_tokens(
            24_000,      // estimated input tokens
            None,        // requested max tokens (defaults to 32k cap or reasoning floor)
            Some(16_000) // explicit reasoning budget
        )?;

        // Clamp reasoning effort and generate wire parameters
        if let Some(effort) = model.normalize_reasoning_effort(ReasoningEffort::High) {
            let wire_params = model.to_wire_reasoning_params(
                catalog.provider_protocol("cline-pass"),
                effort,
                max_out,
            );
            println!("Max output: {max_out}, Wire params: {wire_params:?}");
        }
    }

    Ok(())
}
```

### 2. Offline / Synchronous Lookup & Caller Overrides

```rust
use models_catalog::{CatalogSnapshot, ModelOverrides};

fn resolve_limits() -> Result<(), models_catalog::CatalogError> {
    // Load the embedded compile-time snapshot (no async runtime or network needed)
    let catalog = CatalogSnapshot::bundled()?;

    let model = catalog
        .get_model("anthropic", "claude-sonnet-5")
        .expect("model exists in snapshot");

    // Apply caller config overrides (`caller_override > local_settings > catalog > default`)
    let overrides = ModelOverrides {
        context_window: Some(500_000),
        max_output_tokens: None,
        ..Default::default()
    };
    let effective = model.with_overrides(&overrides);

    assert_eq!(effective.context_window, Some(500_000));
    Ok(())
}
```

---

## CLI Usage (`--features cli`)

```bash
# Sync upstream catalogs to the local disk spool
cargo run --features cli -- sync

# Inspect a specific model's limits, modalities, pricing, and gateway budget
cargo run --features cli -- inspect anthropic/claude-sonnet-5

# List recommended or ClinePass subscription models
cargo run --features cli -- list --provider cline-pass --tier subscription

# Export deterministic merged catalog JSON
cargo run --features cli -- export-json --output ./catalog.json --pretty

# Export TypeScript definitions (.d.ts)
cargo run --features cli -- export-ts --output-dir ./bindings

# Validate local ~/.cline/data/settings/providers.json against the catalog
cargo run --features cli -- check-local-config
```

---

## Documentation Map

- [`CONTRIBUTING.md`](./CONTRIBUTING.md) — Guide to adding new **Vendor Plugins** (`src/vendors/<vendor>/`), implementing `VendorPlugin`, and running verification checks.
- [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) — System architecture, module layout, core data structures, per-source disk spooling, SWR + singleflight state machine, and graceful degradation.
- [`docs/NORMALIZATION_AND_PARITY.md`](./docs/NORMALIZATION_AND_PARITY.md) — Exact `@cline/llms` normalization rules, token/reasoning algorithms, 3-layer default resolution, lookup aliasing, and the automated TypeScript oracle parity harness.
- [`docs/IMPLEMENTATION_PLAN.md`](./docs/IMPLEMENTATION_PLAN.md) — Step-by-step build sequence, verification gates, and test suite specification.
- [`CHANGELOG.md`](./CHANGELOG.md) — Release history.

