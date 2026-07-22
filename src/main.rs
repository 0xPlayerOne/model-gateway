use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Input, Password, Select};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use model_gateway::config::{Config, ConfigError, Exposure, ModelConfig, TargetConfig};
use model_gateway::gateway::run_server;
use model_gateway::providers::{BuiltinProvider, ConnectionCheck, fetch_catalog};
use model_gateway::routing::{
    CatalogRecord, RoutingStore, is_verified_free, provider_limit_reference,
};
use model_gateway::secrets::SecretResolver;

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
        Command::Healthcheck { endpoint } => healthcheck(&endpoint)?,
        Command::Serve => tokio::runtime::Runtime::new()?.block_on(serve())?,
    }
    Ok(())
}

fn catalog(command: CatalogCommand) -> Result<(), Box<dyn Error>> {
    let resolver = SecretResolver::default();
    let config = Config::load(Config::default_path(), &resolver)?;
    let store = RoutingStore::open(config.server.state_path.as_deref())?;
    match command {
        CatalogCommand::Refresh { provider } => {
            let mut refreshed = 0usize;
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
                let models = fetch_catalog(provider_config, api_key.as_deref())?;
                let models = models
                    .into_iter()
                    .map(|model| {
                        let is_free =
                            is_verified_free(provider_config, &model.id, model.zero_priced);
                        CatalogRecord {
                            model: model.id,
                            is_free,
                            context_length: model.context_length,
                            supports_tools: model.supports_tools,
                            supports_vision: model.supports_vision,
                            supports_structured_output: model.supports_structured_output,
                        }
                    })
                    .collect::<Vec<_>>();
                store.replace_catalog(name, &models)?;
                println!("Refreshed {name}: {} models", models.len());
                refreshed += 1;
            }
            if provider.is_some() && refreshed == 0 {
                return Err("selected provider was not refreshed".into());
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
            .items(&actions)
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
        let provider = profile.config(base_url, secret_name);
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
    println!(
        "Models: local, auto-free, {}",
        config.models.keys().cloned().collect::<Vec<_>>().join(", ")
    );
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
        let mut failures = Vec::new();
        for (name, provider) in &config.providers {
            let profile = BuiltinProvider::from_profile_id(provider.profile);
            let key = provider
                .api_key_secret
                .as_deref()
                .and_then(|secret| resolver.get(secret).ok().flatten());
            match profile.validate_and_fetch_models(provider, key.as_deref()) {
                Ok(Some(models)) => println!(
                    "Online provider check passed: {name} ({} models)",
                    models.len()
                ),
                Ok(None) => println!(
                    "Online provider check skipped: {name} (no documented zero-credit endpoint)"
                ),
                Err(error) => {
                    println!("Online provider check failed: {name} ({error})");
                    failures.push(name.as_str());
                }
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
    let config = Config::read(&path)?;
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
