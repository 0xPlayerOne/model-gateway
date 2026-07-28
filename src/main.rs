use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Input, Password, Select};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use model_gateway::benchmarks::{BenchmarkImport, parse_artificial_analysis};
use model_gateway::config::{
    BillingMode, Config, ConfigError, Exposure, ModelConfig, QuotaBoundary, QuotaKind, QuotaLimit,
    TargetConfig,
};
use model_gateway::gateway::{
    ModelMatchKind, is_exact_model_identity, reconcile_model_matches, run_server,
};
use model_gateway::identity::fetch_identity_sources;
use model_gateway::pricing::{ManualPriceImport, PriceSourceKind, fetch_models_dev};
use model_gateway::providers::{
    BuiltinProvider, ConnectionCheck, fetch_account_limit, fetch_catalog,
};
use model_gateway::routing::{
    CatalogRecord, RoutingStore, classify_access, provider_limit_reference,
};
use model_gateway::secrets::SecretResolver;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "model-gateway",
    version,
    about = "Local OpenAI-compatible model gateway"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Setup(SetupArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Credentials {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    Benchmarks {
        #[command(subcommand)]
        command: BenchmarkCommand,
    },
    Pricing {
        #[command(subcommand)]
        command: PricingCommand,
    },
    Matching {
        #[command(subcommand)]
        command: MatchingCommand,
    },
    Refresh,
    Healthcheck {
        #[arg(
            long,
            default_value = "http://127.0.0.1:8008",
            help = "Gateway base URL to probe"
        )]
        endpoint: String,
    },
    Serve,
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(long, help = "Skip network model discovery and validation")]
    offline: bool,
    #[arg(long, help = "Generate config for the local Docker container mode")]
    docker: bool,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Check {
        #[arg(long, help = "Explicitly contact configured providers")]
        online: bool,
    },
    Show,
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    Set { name: String },
    Remove { name: String },
    List,
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    Refresh {
        #[arg(long, help = "Refresh only one configured provider")]
        provider: Option<String>,
    },
    Status,
}

#[derive(Debug, Subcommand)]
enum BenchmarkCommand {
    Refresh,
    Import {
        #[arg(long, help = "Path to a validated benchmark JSON export")]
        file: PathBuf,
    },
    Status,
    Delete {
        source: String,
    },
}

#[derive(Debug, Subcommand)]
enum PricingCommand {
    Refresh,
    Import {
        #[arg(long, help = "Path to JSONL provider-scoped pricing overrides")]
        file: PathBuf,
    },
    Status,
    Explain {
        provider: String,
        model: String,
    },
}

#[derive(Debug, Subcommand)]
enum MatchingCommand {
    Refresh,
    Status,
    Reconcile {
        #[arg(long, help = "Reconcile only one configured provider")]
        provider: Option<String>,
        #[arg(long, help = "Emit machine-readable JSON")]
        json: bool,
        #[arg(long, help = "Fail when mappings drift or identities become ambiguous")]
        check: bool,
    },
    Approve {
        provider: String,
        catalog_model: String,
        benchmark_model: String,
    },
    ApproveEntity {
        entity_id: String,
        benchmark_model: String,
    },
    LinkAlias {
        provider_key: String,
        provider_model_id: String,
        entity_id: String,
    },
    Remove {
        provider: String,
        catalog_model: String,
    },
    RemoveEntity {
        entity_id: String,
        benchmark_model: String,
    },
    UnlinkAlias {
        provider_key: String,
        provider_model_id: String,
    },
    Explain {
        provider: String,
        catalog_model: String,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    init_logging()?;
    let cli = Cli::parse();
    match cli.command {
        Command::Setup(args) => setup(args)?,
        Command::Config {
            command: ConfigCommand::Check { online },
        } => config_check(online)?,
        Command::Config {
            command: ConfigCommand::Show,
        } => config_show()?,
        Command::Credentials { command } => credentials(command)?,
        Command::Catalog { command } => catalog(command)?,
        Command::Benchmarks { command } => benchmarks(command)?,
        Command::Pricing { command } => pricing(command)?,
        Command::Matching { command } => matching(command)?,
        Command::Refresh => refresh_all()?,
        Command::Healthcheck { endpoint } => healthcheck(&endpoint)?,
        Command::Serve => tokio::runtime::Runtime::new()?.block_on(serve())?,
    }
    Ok(())
}

fn benchmarks(command: BenchmarkCommand) -> Result<(), Box<dyn Error>> {
    const SOURCE: &str = "artificial-analysis";
    const ATTRIBUTION: &str = "Artificial Analysis (https://artificialanalysis.ai/)";
    const ENDPOINT: &str = "https://artificialanalysis.ai/api/v2/language/models/free";

    let resolver = SecretResolver::default();
    let config = Config::load(Config::default_path(), &resolver)?;
    let store = RoutingStore::open(config.server.state_path.as_deref())?;
    match command {
        BenchmarkCommand::Refresh => {
            let api_key = resolver
                .get("ARTIFICIAL_ANALYSIS_API_KEY")?
                .ok_or("ARTIFICIAL_ANALYSIS_API_KEY is unavailable")?;
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
                .build()?;
            let mut all_models = Vec::new();
            let mut page = 1u64;
            loop {
                let body: serde_json::Value = client
                    .get(format!("{ENDPOINT}?page={page}"))
                    .header("x-api-key", &api_key)
                    .send()?
                    .error_for_status()?
                    .json()?;
                let models = parse_artificial_analysis(&body)?;
                all_models.extend(models);
                let has_more = body
                    .pointer("/pagination/has_more")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !has_more {
                    break;
                }
                page += 1;
            }
            let import = BenchmarkImport {
                source: SOURCE.to_owned(),
                attribution: ATTRIBUTION.to_owned(),
                models: all_models,
            }
            .normalize()?;
            let snapshot =
                store.replace_benchmarks(&import.source, &import.attribution, &import.models)?;
            println!(
                "Refreshed {}: {} models, snapshot={snapshot}",
                import.source,
                import.models.len()
            );
            println!("Attribution: {}", import.attribution);
        }
        BenchmarkCommand::Import { file } => {
            let import: BenchmarkImport = serde_json::from_slice(&std::fs::read(file)?)?;
            let import = import.normalize()?;
            let snapshot =
                store.replace_benchmarks(&import.source, &import.attribution, &import.models)?;
            println!(
                "Imported {}: {} models, snapshot={snapshot}",
                import.source,
                import.models.len()
            );
            println!("Attribution: {}", import.attribution);
        }
        BenchmarkCommand::Status => {
            let status = store.benchmark_status()?;
            if status.is_empty() {
                println!("No active benchmark snapshots");
            }
            for (source, fetched_at, models, attribution) in status {
                println!(
                    "{source}: {models} models, fetched_at={fetched_at}, attribution={attribution}"
                );
            }
        }
        BenchmarkCommand::Delete { source } => {
            store.remove_benchmark_source(&source)?;
            println!("Deleted benchmark snapshot '{source}'");
        }
    }
    Ok(())
}

fn pricing(command: PricingCommand) -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    let config = Config::load(Config::default_path(), &resolver)?;
    let store = RoutingStore::open(config.server.state_path.as_deref())?;
    match command {
        PricingCommand::Refresh => {
            let observations = fetch_models_dev()?;
            let snapshot = store.replace_pricing(
                "models.dev",
                PriceSourceKind::ModelsDev,
                "Models.dev (https://models.dev/)",
                &observations,
            )?;
            println!(
                "Refreshed models.dev: {} observations, snapshot={snapshot}",
                observations.len()
            );
        }
        PricingCommand::Import { file } => {
            let fetched_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let fetched_at = i64::try_from(fetched_at)?;
            let observations = std::fs::read_to_string(&file)?
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
                .map(|(line_number, line)| {
                    serde_json::from_str::<ManualPriceImport>(line)
                        .map(|record| record.observation(fetched_at))
                        .map_err(|error| {
                            format!(
                                "{}:{}: invalid pricing override: {error}",
                                file.display(),
                                line_number + 1
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let snapshot = store.replace_pricing(
                "manual-overrides",
                PriceSourceKind::Manual,
                "Explicit provider-scoped pricing overrides",
                &observations,
            )?;
            println!(
                "Imported manual-overrides: {} observations, snapshot={snapshot}",
                observations.len()
            );
        }
        PricingCommand::Status => {
            let status = store.pricing_status()?;
            if status.is_empty() {
                println!("No active pricing snapshots");
            }
            for (source, kind, fetched_at, count, attribution) in status {
                println!(
                    "{source}: kind={kind}, {count} observations, fetched_at={fetched_at}, attribution={attribution}"
                );
            }
        }
        PricingCommand::Explain { provider, model } => {
            let provider_config = config
                .providers
                .get(&provider)
                .ok_or_else(|| format!("unknown provider '{provider}'"))?;
            let profile_key = provider_config.pricing_profile.as_deref().or_else(|| {
                provider_config
                    .profile
                    .and_then(BuiltinProvider::models_dev_key)
            });
            let canonical = provider_config
                .model_mappings
                .get(&model)
                .map(String::as_str);
            let price = store.effective_price(
                &provider,
                profile_key,
                &model,
                canonical,
                config.server.pricing_max_age_seconds,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&price.map(|price| {
                    serde_json::json!({
                        "provider": provider,
                        "model": model,
                        "input_price_per_million": price.input_price_per_million,
                        "output_price_per_million": price.output_price_per_million,
                        "source": price.source,
                        "scope": price.scope.as_str(),
                        "estimated": price.estimated,
                        "fetched_at": price.fetched_at,
                    })
                }))?
            );
        }
    }
    Ok(())
}

fn matching(command: MatchingCommand) -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    let config = Config::load(Config::default_path(), &resolver)?;
    let store = RoutingStore::open(config.server.state_path.as_deref())?;
    match command {
        MatchingCommand::Refresh => {
            for import in fetch_identity_sources()? {
                let snapshot = store.replace_identity_source(&import)?;
                println!(
                    "Refreshed {}: {} entities, {} aliases, snapshot={snapshot}",
                    import.source,
                    import.entities.len(),
                    import.aliases.len()
                );
            }
        }
        MatchingCommand::Status => {
            let status = store.identity_status()?;
            if status.is_empty() {
                println!("No active identity snapshots");
            }
            for (source, fetched_at, aliases, attribution) in status {
                println!(
                    "{source}: {aliases} aliases, fetched_at={fetched_at}, attribution={attribution}"
                );
            }
        }
        MatchingCommand::Reconcile {
            provider,
            json,
            check,
        } => {
            if provider
                .as_ref()
                .is_some_and(|name| !config.providers.contains_key(name))
            {
                return Err(format!("unknown provider '{}'", provider.unwrap()).into());
            }
            let report = reconcile_model_matches(&config, &store, provider.as_deref())?;
            let drift = report.iter().any(|entry| {
                entry.status == ModelMatchKind::Ambiguous
                    || (entry.status == ModelMatchKind::Unmatched
                        && matches!(
                            entry.source.as_deref(),
                            Some("config" | "registry" | "canonical_entity")
                        ))
            });
            let mut summary = BTreeMap::<&str, usize>::new();
            for entry in &report {
                *summary.entry(entry.status.as_str()).or_default() += 1;
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "summary": summary,
                        "models": report,
                    }))?
                );
            } else {
                for entry in &report {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        entry.status.as_str(),
                        entry.provider,
                        entry.catalog_model,
                        entry.benchmark_model.as_deref().unwrap_or("-"),
                        entry.alternatives.join(",")
                    );
                }
                println!("Summary:");
                for (status, count) in summary {
                    println!("  {status}: {count}");
                }
            }
            if check && drift {
                return Err(
                    "model identity reconciliation detected mapping drift or ambiguity".into(),
                );
            }
        }
        MatchingCommand::Approve {
            provider,
            catalog_model,
            benchmark_model,
        } => {
            let provider_config = config
                .providers
                .get(&provider)
                .ok_or_else(|| format!("unknown provider '{provider}'"))?;
            if provider_config.model_mappings.contains_key(&catalog_model) {
                return Err(format!(
                    "'{provider}/{catalog_model}' already has a configured model mapping"
                )
                .into());
            }
            let offering_exists = store
                .all_candidates(config.server.catalog_max_age_seconds)?
                .into_iter()
                .any(|offering| offering.provider == provider && offering.model == catalog_model);
            if !offering_exists {
                return Err(format!(
                    "fresh catalog offering '{provider}/{catalog_model}' does not exist"
                )
                .into());
            }
            let benchmark_model = store
                .benchmark_models(config.server.benchmark_max_age_seconds)?
                .into_iter()
                .find(|benchmark| is_exact_model_identity(&benchmark.id, &benchmark_model))
                .map(|benchmark| benchmark.id)
                .ok_or_else(|| {
                    format!("active benchmark model '{benchmark_model}' does not exist")
                })?;
            store.approve_model_mapping(&provider, &catalog_model, &benchmark_model)?;
            println!("Approved {provider}/{catalog_model} -> {benchmark_model}");
        }
        MatchingCommand::ApproveEntity {
            entity_id,
            benchmark_model,
        } => {
            let entity_exists = store
                .active_identity_aliases()?
                .into_iter()
                .any(|alias| alias.entity_id == entity_id);
            if !entity_exists {
                return Err(format!("active identity entity '{entity_id}' does not exist").into());
            }
            let benchmark_model = store
                .benchmark_models(config.server.benchmark_max_age_seconds)?
                .into_iter()
                .find(|benchmark| is_exact_model_identity(&benchmark.id, &benchmark_model))
                .map(|benchmark| benchmark.id)
                .ok_or_else(|| {
                    format!("active benchmark model '{benchmark_model}' does not exist")
                })?;
            store.approve_benchmark_identity_link(
                &entity_id,
                &benchmark_model,
                "operator-approved canonical entity",
            )?;
            println!("Approved entity {entity_id} -> {benchmark_model}");
        }
        MatchingCommand::LinkAlias {
            provider_key,
            provider_model_id,
            entity_id,
        } => {
            let aliases = store.active_identity_aliases()?;
            let source_alias = aliases
                .iter()
                .find(|alias| {
                    alias.source != "operator"
                        && alias.provider_key == provider_key
                        && is_exact_model_identity(&alias.provider_model_id, &provider_model_id)
                })
                .ok_or_else(|| {
                    format!(
                        "source-backed alias '{provider_key}/{provider_model_id}' does not exist"
                    )
                })?;
            if !aliases.iter().any(|alias| alias.entity_id == entity_id) {
                return Err(format!("active identity entity '{entity_id}' does not exist").into());
            }
            store.approve_entity_alias(
                &provider_key,
                &source_alias.provider_model_id,
                &entity_id,
                "operator-approved provider alias",
            )?;
            println!(
                "Linked {provider_key}/{} -> {entity_id}",
                source_alias.provider_model_id
            );
        }
        MatchingCommand::Remove {
            provider,
            catalog_model,
        } => {
            if store.remove_model_mapping(&provider, &catalog_model)? {
                println!("Removed approved mapping for {provider}/{catalog_model}");
            } else {
                println!("No approved mapping for {provider}/{catalog_model}");
            }
        }
        MatchingCommand::RemoveEntity {
            entity_id,
            benchmark_model,
        } => {
            if store.remove_benchmark_identity_link(&entity_id, &benchmark_model)? {
                println!("Removed entity mapping {entity_id} -> {benchmark_model}");
            } else {
                println!("No entity mapping {entity_id} -> {benchmark_model}");
            }
        }
        MatchingCommand::UnlinkAlias {
            provider_key,
            provider_model_id,
        } => {
            if store.remove_entity_alias(&provider_key, &provider_model_id)? {
                println!("Unlinked {provider_key}/{provider_model_id}");
            } else {
                println!("No approved alias {provider_key}/{provider_model_id}");
            }
        }
        MatchingCommand::Explain {
            provider,
            catalog_model,
        } => {
            let report = reconcile_model_matches(&config, &store, Some(&provider))?;
            let entry = report
                .into_iter()
                .find(|entry| entry.catalog_model == catalog_model)
                .ok_or_else(|| {
                    format!("fresh catalog offering '{provider}/{catalog_model}' does not exist")
                })?;
            println!("{}", serde_json::to_string_pretty(&entry)?);
        }
    }
    Ok(())
}

fn refresh_all() -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    Config::load(Config::default_path(), &resolver)?;
    let mut failures = Vec::new();
    if let Err(error) = pricing(PricingCommand::Refresh) {
        println!("Pricing refresh failed: {error}");
        failures.push(format!("pricing: {error}"));
    }
    if let Err(error) = matching(MatchingCommand::Refresh) {
        println!("Identity refresh failed: {error}");
        failures.push(format!("identity: {error}"));
    }
    if let Err(error) = catalog(CatalogCommand::Refresh { provider: None }) {
        println!("Catalog refresh failed: {error}");
        failures.push(format!("catalog: {error}"));
    }
    if resolver.get("ARTIFICIAL_ANALYSIS_API_KEY")?.is_some() {
        if let Err(error) = benchmarks(BenchmarkCommand::Refresh) {
            println!("Benchmark refresh failed: {error}");
            failures.push(format!("benchmarks: {error}"));
        }
    } else {
        println!("Skipped benchmark refresh: ARTIFICIAL_ANALYSIS_API_KEY is unavailable");
    }
    if failures.is_empty() {
        println!("Unified refresh completed");
        Ok(())
    } else {
        Err(format!("unified refresh had failures: {}", failures.join("; ")).into())
    }
}

fn catalog(command: CatalogCommand) -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    let config = Config::load(Config::default_path(), &resolver)?;
    let store = RoutingStore::open(config.server.state_path.as_deref())?;
    match command {
        CatalogCommand::Refresh { provider } => {
            let mut refreshed = 0usize;
            let mut failures = Vec::new();
            for (name, provider_config) in &config.providers {
                if provider.as_deref().is_some_and(|selected| selected != name) {
                    continue;
                }
                if provider_config.profile.is_some_and(|profile| {
                    profile.definition().connection_check == ConnectionCheck::ConfigurationOnly
                }) {
                    println!("Skipped {name}: provider has no documented model catalog");
                    continue;
                }
                let api_key = match provider_config.api_key_secret.as_deref() {
                    Some(secret) => match resolver.get(secret)? {
                        Some(api_key) => Some(api_key),
                        None => {
                            println!("Skipped {name}: credential is unavailable");
                            continue;
                        }
                    },
                    None => None,
                };
                let models = match fetch_catalog(provider_config, api_key.as_deref()) {
                    Ok(models) => models,
                    Err(error) => {
                        println!("Failed {name}: {error}");
                        failures.push(name.clone());
                        continue;
                    }
                };
                let models = models
                    .into_iter()
                    .map(|model| {
                        let access_kind =
                            classify_access(provider_config, &model.id, model.zero_priced);
                        CatalogRecord {
                            model: model.id,
                            access_kind,
                            context_length: model.context_length,
                            supports_tools: model.supports_tools,
                            supports_vision: model.supports_vision,
                            supports_structured_output: model.supports_structured_output,
                            input_price_per_million: model.input_price_per_million,
                            output_price_per_million: model.output_price_per_million,
                        }
                    })
                    .collect::<Vec<_>>();
                store.replace_catalog(name, &models)?;
                if let Some(account) = fetch_account_limit(provider_config, api_key.as_deref())? {
                    store.record_account_limit(name, &account)?;
                    println!(
                        "{name}: account limit remaining={:?}, usage={:?}",
                        account.remaining, account.usage
                    );
                }
                println!("Refreshed {name}: {} models", models.len());
                refreshed += 1;
            }
            if provider.is_some() && refreshed == 0 {
                return Err("selected provider was not refreshed".into());
            }
            if !failures.is_empty() {
                return Err(format!(
                    "{} provider catalog refresh(es) failed: {}",
                    failures.len(),
                    failures.join(", ")
                )
                .into());
            }
        }
        CatalogCommand::Status => {
            let summary = store.catalog_summary()?;
            if summary.is_empty() {
                println!("No cached provider catalogs");
            }
            for (provider, models, refreshed_at) in summary {
                println!("{provider}: {models} models, refreshed_at={refreshed_at}");
            }
            for (name, provider) in &config.providers {
                if let Some(reference) = provider.profile.and_then(provider_limit_reference) {
                    println!(
                        "{name}: quota_status={}, source={}",
                        reference.status, reference.source_url
                    );
                }
            }
            for (provider, fetched_at, limit, usage, remaining, free_tier) in
                store.account_limit_status()?
            {
                println!(
                    "{provider}: account_limit fetched_at={fetched_at}, limit={limit:?}, usage={usage:?}, remaining={remaining:?}, free_tier={free_tier:?}"
                );
            }
        }
    }
    Ok(())
}

fn healthcheck(endpoint: &str) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/health/ready", endpoint.trim_end_matches('/'));
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?
        .get(url)
        .send()?;
    if !response.status().is_success() {
        return Err("gateway health check failed".into());
    }
    Ok(())
}

fn init_logging() -> Result<(), Box<dyn Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    if std::env::var("MODEL_GATEWAY_LOG_FORMAT").as_deref() == Ok("json") {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()?;
    } else {
        registry.with(tracing_subscriber::fmt::layer()).try_init()?;
    }
    Ok(())
}

async fn serve() -> Result<(), Box<dyn Error>> {
    let path = Config::default_path();
    let resolver = SecretResolver::default();
    let config = Config::load(&path, &resolver)?;
    println!("Serving model gateway on {}", config.server.bind);
    run_server(config, &resolver).await?;
    Ok(())
}

fn setup(args: SetupArgs) -> Result<(), Box<dyn Error>> {
    let config_path = Config::default_path();
    let resolver = SecretResolver::default();
    let original = if config_path.exists() {
        println!("Editing {}", config_path.display());
        Some(Config::read(&config_path)?)
    } else {
        None
    };
    let mut config = original.clone().unwrap_or_default();
    let mut pending_secrets = BTreeMap::new();
    config.server.exposure = if args.docker {
        Exposure::LocalContainer
    } else {
        Exposure::Loopback
    };
    config.server.bind = if args.docker {
        "0.0.0.0:8008".to_owned()
    } else {
        "127.0.0.1:8008".to_owned()
    };
    config.server.local_base_url = if args.docker {
        "http://host.docker.internal:8000/v1".to_owned()
    } else {
        "http://127.0.0.1:8000/v1".to_owned()
    };

    if original.is_some() {
        let actions = [
            "Add provider or fallback target",
            "Remove provider",
            "Remove model alias",
            "Cancel",
        ];
        match Select::new()
            .with_prompt("Existing configuration action")
            .items(actions)
            .default(0)
            .interact()?
        {
            1 => {
                let name: String = Input::new()
                    .with_prompt("Provider name to remove")
                    .interact_text()?;
                if config
                    .models
                    .values()
                    .flat_map(|model| model.targets.iter())
                    .any(|target| target.provider == name)
                {
                    return Err(format!(
                        "provider '{name}' is still referenced by a model alias; remove its targets first"
                    )
                    .into());
                }
                config.providers.remove(&name);
                config.validate_structure()?;
                apply_pending_secrets(&resolver, &config_path, &config, pending_secrets)?;
                println!("Removed provider '{name}'");
                return Ok(());
            }
            2 => {
                let alias: String = Input::new()
                    .with_prompt("Model alias to remove")
                    .interact_text()?;
                if config.models.remove(&alias).is_none() {
                    return Err(format!("model alias '{alias}' does not exist").into());
                }
                config.validate_structure()?;
                apply_pending_secrets(&resolver, &config_path, &config, pending_secrets)?;
                println!("Removed model alias '{alias}'");
                return Ok(());
            }
            3 => return Err("configuration was not changed".into()),
            _ => {}
        }
    }

    loop {
        let profiles: Vec<_> = BuiltinProvider::all().collect();
        let choices: Vec<&str> = profiles
            .iter()
            .map(|provider| provider.display_name())
            .collect();
        let selection = Select::new()
            .with_prompt("Provider")
            .items(&choices)
            .default(0)
            .interact()?;
        let profile = profiles[selection];
        let default_name = profile.config_key();
        let name: String = Input::new()
            .with_prompt("Provider name")
            .default(default_name.to_owned())
            .interact_text()?;
        let base_url: String = Input::new()
            .with_prompt("Base URL")
            .default(profile.default_base_url(args.docker).to_owned())
            .interact_text()?;
        let needs_api_key = profile.needs_api_key()
            || (matches!(profile, BuiltinProvider::Custom | BuiltinProvider::LmStudio)
                && Confirm::new()
                    .with_prompt("Does this provider require an API key?")
                    .default(false)
                    .interact()?);
        let secret_name = if needs_api_key {
            let secret_name: String = Input::new()
                .with_prompt("API key secret name")
                .default(
                    profile
                        .definition()
                        .default_secret_name
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            format!("{}_API_KEY", name.to_ascii_uppercase().replace('-', "_"))
                        }),
                )
                .interact_text()?;
            let value = Password::new()
                .with_prompt("API key (leave empty to keep an available stored value)")
                .allow_empty_password(true)
                .interact()?
                .trim()
                .to_owned();
            if value.is_empty() {
                if resolver.get(&secret_name)?.is_none() {
                    return Err("an API key is required for this provider".into());
                }
            } else {
                pending_secrets.insert(secret_name.clone(), value);
            }
            Some(secret_name)
        } else {
            None
        };
        let mut provider = profile.config(base_url, secret_name);
        let billing_modes = ["Free only", "Paid usage", "Subscription"];
        provider.billing_mode = match Select::new()
            .with_prompt("Authorized billing mode")
            .items(billing_modes)
            .default(0)
            .interact()?
        {
            1 => BillingMode::Paid,
            2 => BillingMode::Subscription,
            _ => BillingMode::Free,
        };
        if provider.billing_mode != BillingMode::Free {
            for (label, boundary, window_seconds) in [
                ("daily", QuotaBoundary::UtcDay, 86_400),
                ("weekly", QuotaBoundary::UtcWeek, 604_800),
                ("monthly (30-day)", QuotaBoundary::UtcMonth, 2_592_000),
            ] {
                let cap: String = Input::new()
                    .with_prompt(format!(
                        "Optional {label} spend cap in currency (blank for none)"
                    ))
                    .allow_empty(true)
                    .interact_text()?;
                if cap.trim().is_empty() {
                    continue;
                }
                let currency = cap.trim().parse::<f64>().map_err(|_| {
                    format!("{label} spend cap must be a non-negative decimal currency amount")
                })?;
                if !currency.is_finite() || currency <= 0.0 {
                    return Err(format!("{label} spend cap must be greater than zero").into());
                }
                let microusd = currency
                    .mul_add(1_000_000.0, 0.0)
                    .ceil()
                    .clamp(1.0, u64::MAX as f64) as u64;
                provider.quotas.push(QuotaLimit {
                    kind: QuotaKind::CostMicrousd,
                    limit: microusd,
                    window_seconds,
                    boundary,
                });
            }
        }
        let mut discovered_models = Vec::new();
        if !args.offline {
            let key = provider.api_key_secret.as_deref().and_then(|name| {
                pending_secrets
                    .get(name)
                    .cloned()
                    .or_else(|| resolver.get(name).ok().flatten())
            });
            match profile.validate_and_fetch_models(&provider, key.as_deref()) {
                Ok(Some(models)) if !models.is_empty() => {
                    println!("Discovered {} model(s)", models.len());
                    discovered_models = models;
                }
                Ok(Some(_)) => println!("Provider returned no models; enter one manually."),
                Ok(None) => println!(
                    "Provider has no documented zero-credit connection endpoint; enter a model manually."
                ),
                Err(error) => {
                    eprintln!("Provider validation failed: {error}");
                    if !Confirm::new()
                        .with_prompt("Save this provider explicitly in offline mode?")
                        .default(false)
                        .interact()?
                    {
                        return Err("provider validation failed".into());
                    }
                }
            }
        }
        config.providers.insert(name.clone(), provider);
        let model: String = if discovered_models.is_empty() {
            Input::new()
                .with_prompt("Upstream model ID")
                .default(profile.suggested_model().to_owned())
                .interact_text()?
        } else {
            let mut choices = discovered_models.clone();
            choices.push("Enter model ID manually".to_owned());
            let selection = Select::new()
                .with_prompt("Upstream model ID")
                .items(&choices)
                .default(0)
                .interact()?;
            if selection == discovered_models.len() {
                Input::new()
                    .with_prompt("Upstream model ID")
                    .interact_text()?
            } else {
                discovered_models[selection].clone()
            }
        };
        let alias: String = Input::new()
            .with_prompt("Public model alias")
            .default(name.clone())
            .interact_text()?;
        let mut targets = config
            .models
            .remove(&alias)
            .map(|model| model.targets)
            .unwrap_or_default();
        targets.push(TargetConfig {
            provider: name,
            model,
        });
        config.models.insert(alias, ModelConfig { targets });
        if !Confirm::new()
            .with_prompt("Add another provider or fallback target?")
            .default(false)
            .interact()?
        {
            break;
        }
    }

    config.validate_structure()?;
    println!("Proposed non-secret configuration diff:");
    println!("{}", config_diff(original.as_ref(), &config)?);
    if !Confirm::new()
        .with_prompt("Apply the proposed configuration and credential changes?")
        .default(false)
        .interact()?
    {
        return Err("configuration was not changed".into());
    }
    apply_pending_secrets(&resolver, &config_path, &config, pending_secrets)?;
    println!("Saved {}", config_path.display());
    let mut routes = vec!["local"];
    if config.server.auto_free_enabled {
        routes.push("auto-free");
    }
    if config.server.auto_efficient_enabled {
        routes.push("auto-efficient");
    }
    if config.server.auto_balanced_enabled {
        routes.push("auto-balanced");
    }
    if config.server.auto_frontier_enabled {
        routes.push("auto-frontier");
    }
    routes.extend(config.models.keys().map(String::as_str));
    println!("Models: {}", routes.join(", "));
    let endpoint = "http://127.0.0.1:8008/v1";
    println!("Hermes custom-endpoint YAML:");
    println!("model:");
    println!("  provider: custom");
    println!("  base_url: {endpoint}");
    println!("  default: local");
    println!("curl http://127.0.0.1:8008/health/live");
    println!("curl http://127.0.0.1:8008/v1/models");
    Ok(())
}

fn apply_pending_secrets(
    resolver: &SecretResolver,
    config_path: &std::path::Path,
    config: &Config,
    pending: BTreeMap<String, String>,
) -> Result<(), Box<dyn Error>> {
    let previous = pending
        .keys()
        .map(|name| Ok((name.clone(), resolver.get(name)?)))
        .collect::<Result<BTreeMap<_, _>, model_gateway::secrets::SecretError>>()?;
    let mut applied = Vec::new();
    for (name, value) in &pending {
        if let Err(error) = resolver.set_preferred(name, value) {
            let rollback_error = rollback_secrets(resolver, &previous, &applied).err();
            return Err(match rollback_error {
                Some(rollback) => {
                    format!("credential update failed; rollback also failed: {error}; {rollback}")
                        .into()
                }
                None => error.into(),
            });
        }
        applied.push(name.clone());
    }

    if let Err(error) = config
        .validate(resolver)
        .and_then(|_| config.save_atomic(config_path))
    {
        let rollback_error = rollback_secrets(resolver, &previous, &applied).err();
        return Err(match rollback_error {
            Some(rollback) => format!(
                "configuration update failed; credential rollback also failed: {error}; {rollback}"
            )
            .into(),
            None => error.into(),
        });
    }
    Ok(())
}

fn rollback_secrets(
    resolver: &SecretResolver,
    previous: &BTreeMap<String, Option<String>>,
    applied: &[String],
) -> Result<(), model_gateway::secrets::SecretError> {
    for name in applied {
        match previous.get(name).and_then(Option::as_deref) {
            Some(value) => {
                resolver.set_preferred(name, value)?;
            }
            None => {
                resolver.remove(name)?;
            }
        }
    }
    Ok(())
}

fn config_diff(before: Option<&Config>, after: &Config) -> Result<String, ConfigError> {
    let before = before.map(Config::to_toml).transpose()?.unwrap_or_default();
    let after = after.to_toml()?;
    let before_lines = before.lines().collect::<BTreeSet<_>>();
    let after_lines = after.lines().collect::<BTreeSet<_>>();
    let mut output = Vec::new();
    output.extend(
        before
            .lines()
            .filter(|line| !after_lines.contains(line))
            .map(|line| format!("- {line}")),
    );
    output.extend(
        after
            .lines()
            .filter(|line| !before_lines.contains(line))
            .map(|line| format!("+ {line}")),
    );
    if output.is_empty() {
        Ok("  (no configuration changes)".to_owned())
    } else {
        Ok(output.join("\n"))
    }
}

fn config_check(online: bool) -> Result<(), Box<dyn Error>> {
    let path = Config::default_path();
    let resolver = SecretResolver::default();
    let config = Config::load(&path, &resolver)?;
    println!("Configuration is valid: {}", path.display());
    println!("Providers: {}", config.providers.len());
    println!("Aliases: {}", config.models.len());
    if online {
        let store = RoutingStore::open(config.server.state_path.as_deref())?;
        let mut failures = Vec::new();
        for (name, provider) in &config.providers {
            let profile = BuiltinProvider::from_profile_id(provider.profile);
            let key = provider
                .api_key_secret
                .as_deref()
                .and_then(|secret| resolver.get(secret).ok().flatten());
            match profile.definition().connection_check {
                ConnectionCheck::OpenAiModels | ConnectionCheck::OpenRouter => {
                    match fetch_catalog(provider, key.as_deref()) {
                        Ok(models) => {
                            let records = models
                                .into_iter()
                                .map(|model| CatalogRecord {
                                    access_kind: classify_access(
                                        provider,
                                        &model.id,
                                        model.zero_priced,
                                    ),
                                    model: model.id,
                                    context_length: model.context_length,
                                    supports_tools: model.supports_tools,
                                    supports_vision: model.supports_vision,
                                    supports_structured_output: model.supports_structured_output,
                                    input_price_per_million: model.input_price_per_million,
                                    output_price_per_million: model.output_price_per_million,
                                })
                                .collect::<Vec<_>>();
                            store.replace_catalog(name, &records)?;
                            if let Some(account) = fetch_account_limit(provider, key.as_deref())? {
                                store.record_account_limit(name, &account)?;
                            }
                            println!(
                                "Online provider check passed: {name} ({} models)",
                                records.len()
                            );
                        }
                        Err(error) => {
                            println!("Online provider check failed: {name} ({error})");
                            failures.push(name.as_str());
                        }
                    }
                }
                ConnectionCheck::ConfigurationOnly => println!(
                    "Online provider check skipped: {name} (no documented zero-credit endpoint)"
                ),
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "{} provider connection check(s) failed: {}",
                failures.len(),
                failures.join(", ")
            )
            .into());
        }
    }
    Ok(())
}

fn config_show() -> Result<(), Box<dyn Error>> {
    let path = Config::default_path();
    let config = match Config::read(&path) {
        Ok(config) => config,
        Err(ConfigError::Missing(_)) => Config::default(),
        Err(error) => return Err(error.into()),
    };
    println!("# Canonical non-secret configuration: {}", path.display());
    print!("{}", config.to_toml()?);
    Ok(())
}

fn credentials(command: CredentialCommand) -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    match command {
        CredentialCommand::Set { name } => {
            let value = Password::new()
                .with_prompt(format!("Value for {name}"))
                .interact()?;
            resolver.set_preferred(&name, value.trim())?;
            println!("Stored {name} without displaying its value");
        }
        CredentialCommand::Remove { name } => {
            resolver.remove(&name)?;
            println!("Removed {name} from writable secret stores");
        }
        CredentialCommand::List => {
            let config = match Config::read(Config::default_path()) {
                Ok(config) => config,
                Err(ConfigError::Missing(_)) => {
                    println!("No configured credentials");
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            let names = config
                .providers
                .values()
                .filter_map(|provider| provider.api_key_secret.as_deref())
                .collect::<std::collections::BTreeSet<_>>();
            for name in names {
                let source = resolver.source(name)?.unwrap_or("unavailable");
                println!("{name}: {source}");
            }
        }
    }
    io::stdout().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use model_gateway::config::{Config, ModelConfig, TargetConfig};

    use super::config_diff;

    #[test]
    fn config_diff_contains_no_secret_values() {
        let mut after = Config::default();
        after.models.insert(
            "public-alias".to_owned(),
            ModelConfig {
                targets: vec![TargetConfig {
                    provider: "provider".to_owned(),
                    model: "upstream".to_owned(),
                }],
            },
        );
        let diff = config_diff(None, &after).expect("diff");
        assert!(diff.contains("public-alias"));
        assert!(!diff.contains("password"));
        assert!(!diff.contains("Bearer"));
    }
}
