use std::error::Error;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Input, Password, Select};

use model_gateway::config::{Config, Exposure, ModelConfig, TargetConfig};
use model_gateway::providers::{BuiltinProvider, fetch_models};
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

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup(args) => setup(args)?,
        Command::Config {
            command: ConfigCommand::Check,
        } => config_check()?,
        Command::Credentials { command } => credentials(command)?,
        Command::Serve => {
            println!(
                "gateway server is added in the next milestone; run `model-gateway setup` first"
            );
        }
    }
    Ok(())
}

fn setup(args: SetupArgs) -> Result<(), Box<dyn Error>> {
    let config_path = Config::default_path();
    let resolver = SecretResolver::default();
    let mut config = if config_path.exists() {
        println!("Editing {}", config_path.display());
        Config::read(&config_path).unwrap_or_default()
    } else {
        Config::default()
    };
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
            .default(profile.default_base_url().to_owned())
            .interact_text()?;
        let secret_name = if profile.needs_api_key() {
            let secret_name: String = Input::new()
                .with_prompt("API key secret name")
                .default(format!(
                    "{}_API_KEY",
                    name.to_ascii_uppercase().replace('-', "_")
                ))
                .interact_text()?;
            let value = Password::new()
                .with_prompt("API key")
                .interact()?
                .trim()
                .to_owned();
            if value.is_empty() {
                return Err("an API key is required for this provider".into());
            }
            resolver.set_preferred(&secret_name, &value)?;
            Some(secret_name)
        } else {
            None
        };
        let provider = profile.config(base_url, secret_name);
        if !args.offline {
            let key = provider
                .api_key_secret
                .as_deref()
                .and_then(|name| resolver.get(name).ok().flatten());
            match fetch_models(&provider, key.as_deref()) {
                Ok(models) if !models.is_empty() => {
                    println!("Discovered {} model(s)", models.len());
                }
                Ok(_) => println!("Provider returned no models; enter one manually."),
                Err(error) => println!("Model discovery skipped: {error}"),
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

    config.validate(&resolver)?;
    if config_path.exists()
        && !Confirm::new()
            .with_prompt("Replace the existing configuration atomically?")
            .default(false)
            .interact()?
    {
        return Err("configuration was not changed".into());
    }
    config.save_atomic(&config_path)?;
    println!("Saved {}", config_path.display());
    println!(
        "Aliases: {}",
        config.models.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    println!("Hermes endpoint: http://localhost:11434/v1");
    println!("curl http://localhost:11434/health/live");
    Ok(())
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
            let config = Config::read(Config::default_path())?;
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
