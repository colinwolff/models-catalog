# Contributing to `models-catalog`

Thank you for your interest in contributing to `models-catalog`! This guide explains the repository layout, how to add or update **Vendor Plugins** (`src/vendors/<vendor>/`), and our verification requirements.

---

## 1. Architecture Overview

`models-catalog` separates its catalog pipeline into two layers:
1. **Base Registry (`src/normalize/models_dev.rs`):** Ingests and normalizes the public `models.dev` catalog (`200+` providers, `6,000+` models), resolving token limits, directional modalities, and release-date sorting.
2. **Pluggable Vendor Overlays (`src/vendors/`):** Encapsulates vendor-specific overrides, curated/recommended model feeds, subscription/free billing tiers, local settings validators, and parity scripts.

```text
src/vendors/
├── mod.rs                  # VendorPlugin trait, VendorFeedSpec, & default_vendors()
└── cline/                  # Reference vendor implementation (@cline/llms parity)
    ├── mod.rs              # ClineVendor implementing VendorPlugin
    ├── builtins.rs         # Static provider overrides & builtin_specs.json loader
    ├── builtin_specs.json  # Generated/pinned provider specs
    ├── feed.rs             # Live recommended-models feed parser
    ├── merge.rs            # Cross-reference metadata inheritance & merger
    └── settings.rs         # Local ~/.cline/data/settings/providers.json validator
```

---

## 2. How to Add a New Vendor Plugin

Whether you are adding support for **RooCode**, **KiloCode**, **Cursor**, **LiteLLM**, **OpenRouter Curated**, or another AI coding tool/gateway, every vendor is encapsulated inside its own folder under `src/vendors/<vendor_id>/`.

### Step 1: Create `src/vendors/<vendor_id>/`
Create a directory for your vendor (or a single `src/vendors/<vendor_id>.rs` file if it is under ~250 lines):
- `mod.rs` — Implements [`VendorPlugin`](src/vendors/mod.rs)
- `builtins.rs` *(optional)* — Static provider protocol/default-model overrides
- `feed.rs` & `merge.rs` *(optional)* — Deserializer and merger if the vendor publishes a live curated JSON endpoint
- `settings.rs` *(optional)* — Local user config reader/validator if the tool has a local config file

### Step 2: Implement the `VendorPlugin` Trait
```rust
use std::collections::BTreeMap;
use crate::{
    CatalogError, ProviderSpec,
    vendors::{BuiltinProviderOverride, VendorFeedSpec, VendorPlugin},
};

#[derive(Debug, Clone, Default)]
pub struct MyVendor {
    pub feed_url: Option<String>,
}

impl VendorPlugin for MyVendor {
    fn id(&self) -> &'static str {
        "my-vendor"
    }

    /// Optional: Return static provider overrides (default models, base URLs, protocols)
    fn builtin_overrides(&self) -> &'static BTreeMap<String, BuiltinProviderOverride> {
        todo!("return static BTreeMap or use default empty implementation")
    }

    /// Optional: Declare a live HTTP feed.
    /// `CatalogManager` will automatically cache `sources/<source_name>.json` on disk,
    /// track its ETag / Last-Modified headers (`304 Not Modified`), enforce rate-limit
    /// cooldowns, and degrade gracefully if the endpoint is unreachable!
    fn upstream_feed(&self) -> Option<VendorFeedSpec> {
        self.feed_url.as_ref().map(|url| VendorFeedSpec {
            source_name: "my_vendor_curated".to_string(),
            url: url.clone(),
        })
    }

    /// Merge your vendor's overrides and optional live feed JSON onto the provider map.
    fn apply_overlay(
        &self,
        raw_feed_json: Option<&str>,
        providers: &mut BTreeMap<String, ProviderSpec>,
    ) -> Result<(), CatalogError> {
        if let Some(json) = raw_feed_json {
            // Parse `json` and enrich `providers`...
        }
        Ok(())
    }
}
```

### Step 3: Register the Feature Flag & Export
1. Add an optional Cargo feature in `Cargo.toml` (e.g., `my-vendor = []`).
2. Expose `pub mod my_vendor;` in `src/vendors/mod.rs`.
3. If the vendor has an upstream TypeScript/Python SDK you want to track for differential parity, add an oracle generator script under `parity-harness/` and a fixture test in `tests/`.

### Using a Custom `VendorPlugin` Without Forking
Downstream library consumers can also inject custom `VendorPlugin` implementations at runtime without modifying `models-catalog`:
```rust
use std::sync::Arc;
use models_catalog::CatalogManager;

let manager = CatalogManager::builder()
    .with_vendor(Arc::new(MyCustomVendor::default()))
    .build()?;
```

---

## 3. Code Quality & Verification Gates

Before opening a Pull Request, please ensure all checks pass locally:

1. **File Size Limit (< 500 lines):**
   We enforce modular files (< 500 lines per `.rs` file). Split large normalizers into `feed.rs` (serde types) and `merge.rs` (business logic).
2. **Formatting & Lints:**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   ```
3. **Hermetic Test Suite (No Network I/O in Tests):**
   All unit, `wiremock`, and parity tests must run offline against deterministic fixtures in `tests/fixtures/`:
   ```bash
   cargo test --no-default-features
   cargo test --all-features
   ```
4. **Documentation Build:**
   ```bash
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
   ```

---

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in `models-catalog` by you shall be dual-licensed under the `MIT OR Apache-2.0` license, without any additional terms or conditions.

