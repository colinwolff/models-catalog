use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use models_catalog::{
    BillingTier, CatalogError, CatalogManager, ModelOverrides, ReasoningEffort,
    default_cline_providers_path, read_local_providers_file, validate_local_config,
};

#[derive(Debug, Parser)]
#[command(
    name = "models-catalog",
    version,
    about = "LLM model/provider catalog sync, inspection, and verification CLI"
)]
struct Cli {
    /// Custom disk spool directory (defaults to platform XDG cache directory).
    #[arg(long, global = true)]
    spool_dir: Option<PathBuf>,

    /// Disable querying the Cline recommended/ClinePass overlay feed.
    #[arg(long, global = true)]
    no_cline_feed: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Fetch and synchronize upstream catalogs into the local disk spool.
    Sync {
        /// Bypass TTL and cooldown checks to force an upstream conditional HTTP check.
        #[arg(long, short)]
        force: bool,
    },
    /// Inspect a model's normalized limits, modalities, pricing, and gateway token/reasoning calculations.
    Inspect {
        /// Model ID or qualified query (e.g. `anthropic/claude-sonnet-5` or `cline-pass/minimax/minimax-m2.5`).
        query: String,
        /// Optional explicit provider ID to scope lookup.
        #[arg(long, short)]
        provider: Option<String>,
        /// Estimated input tokens for gateway max output calculation.
        #[arg(long, default_value_t = 16_000)]
        input_tokens: u64,
        /// Optional explicit requested max output tokens.
        #[arg(long)]
        requested_max_tokens: Option<u64>,
        /// Optional explicit reasoning token budget for gateway floor calculation.
        #[arg(long)]
        reasoning_budget: Option<u64>,
        /// Optional reasoning effort to translate into provider wire parameters.
        #[arg(long)]
        effort: Option<CliEffort>,
        /// Optional context window override (`caller_override > catalog`).
        #[arg(long)]
        override_context_window: Option<u64>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List models matching optional provider, recommendation, billing tier, or modality filters.
    List {
        /// Filter by provider ID.
        #[arg(long, short)]
        provider: Option<String>,
        /// Only show curated recommended models.
        #[arg(long)]
        recommended: bool,
        /// Filter by billing tier (`subscription`, `free`, `credits`).
        #[arg(long)]
        tier: Option<CliTier>,
        /// Only include chat models (`is_chat_model() == true`).
        #[arg(long)]
        chat_only: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export the deterministic merged `CatalogSnapshot` as JSON.
    ExportJson {
        /// Output file path (writes to stdout if omitted).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Pretty-print JSON output.
        #[arg(long, default_value_t = true)]
        pretty: bool,
    },
    /// Export TypeScript `.d.ts` definitions for public catalog types.
    ExportTs {
        /// Output directory for `.d.ts` files.
        #[arg(long, short, default_value = "./bindings")]
        output_dir: PathBuf,
    },
    /// Validate a local `~/.cline/data/settings/providers.json` configuration file against the catalog.
    CheckLocalConfig {
        /// Path to `providers.json` (defaults to `~/.cline/data/settings/providers.json`).
        #[arg(long, short)]
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliEffort {
    None,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<CliEffort> for ReasoningEffort {
    fn from(val: CliEffort) -> Self {
        match val {
            CliEffort::None => Self::None,
            CliEffort::Low => Self::Low,
            CliEffort::Medium => Self::Medium,
            CliEffort::High => Self::High,
            CliEffort::Xhigh => Self::XHigh,
            CliEffort::Max => Self::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliTier {
    Subscription,
    Free,
    Credits,
}

impl From<CliTier> for BillingTier {
    fn from(val: CliTier) -> Self {
        match val {
            CliTier::Subscription => Self::Subscription,
            CliTier::Free => Self::Free,
            CliTier::Credits => Self::Credits,
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), CatalogError> {
    let cli = Cli::parse();

    let mut builder = CatalogManager::builder();
    if let Some(dir) = cli.spool_dir {
        builder = builder.with_spool_dir(Some(dir));
    }
    if cli.no_cline_feed {
        builder = builder.with_cline_feed(false);
    }
    let manager = builder.build()?;

    match cli.command {
        Commands::Sync { force } => {
            let catalog = manager.refresh(force).await?;
            let total_models: usize = catalog.providers.values().map(|p| p.models.len()).sum();
            println!(
                "Synced catalog (schema v{}, degraded: {}): {} providers, {} models",
                catalog.schema_version,
                catalog.degraded,
                catalog.providers.len(),
                total_models
            );
            for (name, status) in &catalog.sources {
                println!(
                    "  - {name}: origin={:?}, etag={:?}, warning={:?}",
                    status.origin, status.etag, status.warning
                );
            }
        }
        Commands::Inspect {
            query,
            provider,
            input_tokens,
            requested_max_tokens,
            reasoning_budget,
            effort,
            override_context_window,
            json,
        } => {
            let catalog = manager.get_catalog().await?;
            let found = match provider.as_deref() {
                Some(pid) => catalog.get_model(pid, &query),
                None => catalog.find_model(&query),
            };

            let Some(base_model) = found else {
                eprintln!("Model `{query}` not found in catalog.");
                std::process::exit(2);
            };

            let model = if override_context_window.is_some() {
                base_model.with_overrides(&ModelOverrides {
                    context_window: override_context_window,
                    ..Default::default()
                })
            } else {
                base_model.clone()
            };

            let gateway_budget = model.resolve_gateway_max_output_tokens(
                input_tokens,
                requested_max_tokens,
                reasoning_budget,
            );

            let protocol = catalog.provider_protocol(&model.provider_id);
            let req_effort = effort
                .map(ReasoningEffort::from)
                .or(model.default_reasoning_effort)
                .unwrap_or(ReasoningEffort::Medium);

            let normalized_effort = model.normalize_reasoning_effort(req_effort);
            let wire_params = gateway_budget
                .as_ref()
                .ok()
                .map(|&max_out| model.to_wire_reasoning_params(protocol, req_effort, max_out));

            if json {
                let output = serde_json::json!({
                    "model": model,
                    "provider_protocol": protocol,
                    "is_chat_model": model.is_chat_model(),
                    "gateway_max_output_tokens": gateway_budget.as_ref().ok(),
                    "gateway_error": gateway_budget.as_ref().err().map(ToString::to_string),
                    "normalized_reasoning_effort": normalized_effort,
                    "wire_reasoning_params": wire_params,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).map_err(|source| {
                        CatalogError::Parse {
                            source_name: "inspect-json".to_string(),
                            source,
                        }
                    })?
                );
            } else {
                println!("Model: {} ({})", model.name, model.id);
                println!("Provider: {} ({:?})", model.provider_id, protocol);
                println!("Chat Model: {}", model.is_chat_model());
                println!(
                    "Limits: context={:?}, max_input={}, max_output={}",
                    model.context_window, model.max_input_tokens, model.max_tokens
                );
                println!("Modalities: {:?}", model.modalities);
                println!(
                    "Reasoning: supported={}, options={:?}, normalized_effort={:?}",
                    model.supports_reasoning, model.reasoning_options, normalized_effort
                );
                println!(
                    "Billing Tier: {:?}, Recommended: {}, Requires Streaming: {}",
                    model.billing_tier, model.is_recommended, model.requires_streaming
                );
                match gateway_budget {
                    Ok(max_out) => {
                        println!("Gateway Max Output (at {input_tokens} input tokens): {max_out}")
                    }
                    Err(err) => println!("Gateway Max Output Error: {err}"),
                }
                if let Some(wp) = wire_params {
                    println!("Wire Reasoning Params: {wp:?}");
                }
            }
        }
        Commands::List {
            provider,
            recommended,
            tier,
            chat_only,
            json,
        } => {
            let catalog = manager.get_catalog().await?;
            let target_tier = tier.map(BillingTier::from);

            let mut matches = Vec::new();
            for (pid, pspec) in &catalog.providers {
                if let Some(filter_pid) = provider.as_deref() {
                    if pid != filter_pid {
                        continue;
                    }
                }
                for model_id in &pspec.sorted_model_ids {
                    if let Some(m) = pspec.models.get(model_id) {
                        if recommended && !m.is_recommended {
                            continue;
                        }
                        if let Some(t) = target_tier {
                            if m.billing_tier != Some(t) {
                                continue;
                            }
                        }
                        if chat_only && !m.is_chat_model() {
                            continue;
                        }
                        matches.push(m);
                    }
                }
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&matches).map_err(|source| {
                        CatalogError::Parse {
                            source_name: "list-json".to_string(),
                            source,
                        }
                    })?
                );
            } else {
                for m in matches {
                    println!(
                        "{}/{} (ctx={:?}, out={}, chat={}, tier={:?}, rec={})",
                        m.provider_id,
                        m.id,
                        m.context_window,
                        m.max_tokens,
                        m.is_chat_model(),
                        m.billing_tier,
                        m.is_recommended
                    );
                }
            }
        }
        Commands::ExportJson { output, pretty } => {
            let catalog = manager.get_catalog().await?;
            let serialized = if pretty {
                serde_json::to_string_pretty(&*catalog)
            } else {
                serde_json::to_string(&*catalog)
            }
            .map_err(|source| CatalogError::Parse {
                source_name: "export-json".to_string(),
                source,
            })?;

            if let Some(path) = output {
                std::fs::write(&path, serialized)
                    .map_err(|source| CatalogError::Io { path, source })?;
            } else {
                println!("{serialized}");
            }
        }
        Commands::ExportTs { output_dir } => {
            export_typescript_definitions(&output_dir)?;
            println!(
                "Exported TypeScript definitions to {}",
                output_dir.display()
            );
        }
        Commands::CheckLocalConfig { path } => {
            let config_path = path.or_else(default_cline_providers_path).ok_or_else(|| {
                CatalogError::InvalidLocalConfig(
                    "Could not resolve path to providers.json".to_string(),
                )
            })?;

            let local_cfg = read_local_providers_file(&config_path)?;
            let catalog = manager.get_catalog().await?;
            let findings = validate_local_config(&local_cfg, &catalog);

            let mut all_valid = true;
            for f in &findings {
                let status = if f.is_valid { "OK" } else { "INVALID" };
                if !f.is_valid {
                    all_valid = false;
                }
                println!("[{status}] {}: {}", f.provider_id, f.message);
            }

            if !all_valid {
                std::process::exit(3);
            }
        }
    }

    Ok(())
}

fn export_typescript_definitions(output_dir: &std::path::Path) -> Result<(), CatalogError> {
    std::fs::create_dir_all(output_dir).map_err(|source| CatalogError::Io {
        path: output_dir.to_path_buf(),
        source,
    })?;

    #[cfg(feature = "ts-bindings")]
    {
        use ts_rs::TS;
        let cfg = ts_rs::Config::from_env().with_out_dir(output_dir);
        models_catalog::CatalogSnapshot::export_all(&cfg)
            .map_err(|e| CatalogError::InvalidLocalConfig(format!("ts-rs export failed: {e}")))?;
        models_catalog::ModelOverrides::export_all(&cfg)
            .map_err(|e| CatalogError::InvalidLocalConfig(format!("ts-rs export failed: {e}")))?;
        models_catalog::WireReasoningParams::export_all(&cfg)
            .map_err(|e| CatalogError::InvalidLocalConfig(format!("ts-rs export failed: {e}")))?;
        models_catalog::ContextOverflowError::export_all(&cfg)
            .map_err(|e| CatalogError::InvalidLocalConfig(format!("ts-rs export failed: {e}")))?;
    }

    #[cfg(not(feature = "ts-bindings"))]
    {
        let index_dts = output_dir.join("index.d.ts");
        let content = r#"// Generated by models-catalog
export type DataOrigin = "live" | "not_modified304" | "disk_spool" | "bundled_fallback" | "custom_input";
export type ProviderProtocol = "anthropic" | "open_ai_compatible" | "open_ai_responses" | "google" | "bedrock" | "vertex" | "ollama" | "cline_pass";
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type Modality = "text" | "image" | "audio" | "video" | "pdf";
export type BillingTier = "subscription" | "free" | "credits";
"#;
        std::fs::write(&index_dts, content).map_err(|source| CatalogError::Io {
            path: index_dts,
            source,
        })?;
    }

    Ok(())
}
