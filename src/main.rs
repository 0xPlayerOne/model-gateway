use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Input, Password, Select};

use model_gateway::config::{Config, ConfigError, Exposure, ModelConfig, TargetConfig};
use model_gateway::gateway::run_server;
use model_gateway::providers::BuiltinProvider;
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
    Check,
}

#[derive(Debug, Subcommand)]
enum CredentialCommand {
    Set { name: String },
    Remove { name: String },
    List,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup(args) => setup(args)?,
        Command::Config {
            command: ConfigCommand::Check,
        } => config_check()?,
        Command::Credentials { command } => credentials(command)?,
        Command::Serve => serve().await?,
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
        "0.0.0.0:11434".to_owned()
    } else {
        "127.0.0.1:11434".to_owned()
    };

    loop {
        let choices: Vec<&str> = BuiltinProvider::all()
            .iter()
            .map(|provider| provider.display_name())
            .collect();
        let selection = Select::new()
            .with_prompt("Provider")
            .items(&choices)
            .default(0)
            .interact()?;
        let profile = BuiltinProvider::all()[selection];
        let default_name = match profile {
            BuiltinProvider::Custom => "custom",
            BuiltinProvider::OpenRouter => "openrouter",
            BuiltinProvider::Ollama => "ollama",
            BuiltinProvider::LmStudio => "lmstudio",
        };
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
                .default(format!(
                    "{}_API_KEY",
                    name.to_ascii_uppercase().replace('-', "_")
                ))
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
        if !args.offline {
            let key = provider.api_key_secret.as_deref().and_then(|name| {
                pending_secrets
                    .get(name)
                    .cloned()
                    .or_else(|| resolver.get(name).ok().flatten())
            });
            match profile.validate_and_fetch_models(&provider, key.as_deref()) {
                Ok(models) if !models.is_empty() => {
                    println!("Discovered {} model(s)", models.len());
                }
                Ok(_) => println!("Provider returned no models; enter one manually."),
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
        let model: String = Input::new()
            .with_prompt("Upstream model ID")
            .default(profile.suggested_model().to_owned())
            .interact_text()?;
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
    for (name, value) in pending_secrets {
        resolver.set_preferred(&name, &value)?;
    }
    config.validate(&resolver)?;
    config.save_atomic(&config_path)?;
    println!("Saved {}", config_path.display());
    println!(
        "Aliases: {}",
        config.models.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    let endpoint = "http://127.0.0.1:11434/v1";
    let default_alias = config.models.keys().next().expect("validated alias");
    println!("Hermes custom-endpoint YAML:");
    println!("model:");
    println!("  provider: custom");
    println!("  base_url: {endpoint}");
    println!("  default: {default_alias}");
    println!("curl http://127.0.0.1:11434/health/live");
    println!("curl http://127.0.0.1:11434/v1/models");
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

fn config_check() -> Result<(), Box<dyn Error>> {
    let path = Config::default_path();
    let resolver = SecretResolver::default();
    let config = Config::load(&path, &resolver)?;
    println!("Configuration is valid: {}", path.display());
    println!("Providers: {}", config.providers.len());
    println!("Aliases: {}", config.models.len());
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
