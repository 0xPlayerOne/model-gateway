use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::secrets::{SecretError, SecretResolver, validate_secret_name};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration file does not exist: {0}")]
    Missing(PathBuf),
    #[error("could not read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialize configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("required secret '{name}' is unavailable")]
    MissingSecret { name: String },
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub exposure: Exposure,
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
    #[serde(default = "default_admission_timeout_ms")]
    pub admission_timeout_ms: u64,
    #[serde(default = "default_shutdown_grace_seconds")]
    pub shutdown_grace_seconds: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    #[default]
    Loopback,
    LocalContainer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub adapter: AdapterKind,
    pub base_url: String,
    #[serde(default)]
    pub api_key_secret: Option<String>,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub allow_model_passthrough: bool,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub max_in_flight: Option<usize>,
    #[serde(default = "default_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_response_header_timeout_seconds")]
    pub response_header_timeout_seconds: u64,
    #[serde(default = "default_stream_idle_timeout_seconds")]
    pub stream_idle_timeout_seconds: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            adapter: AdapterKind::OpenaiChat,
            base_url: "http://localhost:8000/v1".to_owned(),
            api_key_secret: None,
            extra_headers: BTreeMap::new(),
            allow_model_passthrough: false,
            allow_insecure_http: false,
            max_in_flight: None,
            connect_timeout_seconds: default_connect_timeout_seconds(),
            response_header_timeout_seconds: default_response_header_timeout_seconds(),
            stream_idle_timeout_seconds: default_stream_idle_timeout_seconds(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    OpenaiChat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub targets: Vec<TargetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub provider: String,
    pub model: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            exposure: Exposure::Loopback,
            max_body_bytes: default_max_body_bytes(),
            max_in_flight: default_max_in_flight(),
            admission_timeout_ms: default_admission_timeout_ms(),
            shutdown_grace_seconds: default_shutdown_grace_seconds(),
        }
    }
}

impl Config {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::Missing(path.to_path_buf()));
        }
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn load(path: impl AsRef<Path>, secrets: &SecretResolver) -> Result<Self, ConfigError> {
        let config = Self::read(path)?;
        config.validate(secrets)?;
        Ok(config)
    }

    pub fn validate(&self, secrets: &SecretResolver) -> Result<(), ConfigError> {
        validate_server(&self.server)?;
        if self.providers.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one provider is required".to_owned(),
            ));
        }
        for (name, provider) in &self.providers {
            validate_provider(name, provider, secrets)?;
        }
        if self.models.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one model alias is required".to_owned(),
            ));
        }
        for (alias, model) in &self.models {
            if alias.trim().is_empty() || alias.contains('/') {
                return Err(ConfigError::Invalid(format!(
                    "model alias '{alias}' must be non-empty and contain no '/': aliases are public names"
                )));
            }
            if model.targets.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "model alias '{alias}' must have at least one target"
                )));
            }
            for target in &model.targets {
                if !self.providers.contains_key(&target.provider) {
                    return Err(ConfigError::Invalid(format!(
                        "model alias '{alias}' references unknown provider '{}'",
                        target.provider
                    )));
                }
                if target.model.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "model alias '{alias}' has an empty upstream model"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, self.to_toml()?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        if let Some(path) = env::var_os("MODEL_GATEWAY_CONFIG") {
            return PathBuf::from(path);
        }
        home_dir().join("config.toml")
    }

    pub fn home_dir() -> PathBuf {
        home_dir()
    }
}

fn validate_server(server: &ServerConfig) -> Result<(), ConfigError> {
    let bind: std::net::SocketAddr = server
        .bind
        .parse()
        .map_err(|error| ConfigError::Invalid(format!("invalid server bind: {error}")))?;
    let is_loopback = match bind.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    if !is_loopback && server.exposure != Exposure::LocalContainer {
        return Err(ConfigError::Invalid(
            "only loopback binds are supported outside local_container exposure".to_owned(),
        ));
    }
    if server.max_body_bytes == 0 || server.max_in_flight == 0 {
        return Err(ConfigError::Invalid(
            "server body and concurrency limits must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider(
    name: &str,
    provider: &ProviderConfig,
    secrets: &SecretResolver,
) -> Result<(), ConfigError> {
    let url = Url::parse(&provider.base_url)
        .map_err(|error| ConfigError::Invalid(format!("provider '{name}' URL: {error}")))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(url.host_str()) => {}
        "http" if provider.allow_insecure_http => {}
        scheme => {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' must use HTTPS or explicitly allow loopback/insecure HTTP, got {scheme}"
            )));
        }
    }
    if url.host_str().is_none() {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' URL has no host"
        )));
    }
    if provider.connect_timeout_seconds == 0
        || provider.response_header_timeout_seconds == 0
        || provider.stream_idle_timeout_seconds == 0
    {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' timeouts must be greater than zero"
        )));
    }
    if provider.max_in_flight == Some(0) {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' max_in_flight must be greater than zero"
        )));
    }
    for (header, value) in &provider.extra_headers {
        if !is_safe_extra_header(header) || value.contains('\r') || value.contains('\n') {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' contains unsafe extra header '{header}'"
            )));
        }
    }
    if let Some(secret) = &provider.api_key_secret {
        validate_secret_name(secret)?;
        if secrets.get(secret)?.is_none() {
            return Err(ConfigError::MissingSecret {
                name: secret.clone(),
            });
        }
    }
    Ok(())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    match host {
        Some("localhost") => true,
        Some(host) => host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn is_safe_extra_header(header: &str) -> bool {
    let lower = header.to_ascii_lowercase();
    !header.is_empty()
        && !lower.starts_with("proxy-")
        && !matches!(
            lower.as_str(),
            "authorization" | "host" | "content-length" | "transfer-encoding" | "connection"
        )
        && header
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn home_dir() -> PathBuf {
    if let Some(path) = env::var_os("MODEL_GATEWAY_HOME") {
        return PathBuf::from(path);
    }
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".config").join("model-gateway")
}

fn default_bind() -> String {
    "127.0.0.1:11434".to_owned()
}

const fn default_max_body_bytes() -> usize {
    32 * 1024 * 1024
}

const fn default_max_in_flight() -> usize {
    64
}

const fn default_admission_timeout_ms() -> u64 {
    250
}

const fn default_shutdown_grace_seconds() -> u64 {
    30
}

const fn default_connect_timeout_seconds() -> u64 {
    10
}

const fn default_response_header_timeout_seconds() -> u64 {
    300
}

const fn default_stream_idle_timeout_seconds() -> u64 {
    180
}

#[cfg(test)]
mod tests {
    use super::{Config, Exposure, ProviderConfig, ServerConfig, validate_server};
    use crate::secrets::SecretResolver;
    use std::collections::BTreeMap;

    fn provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            adapter: super::AdapterKind::OpenaiChat,
            base_url: base_url.to_owned(),
            api_key_secret: None,
            extra_headers: BTreeMap::new(),
            allow_model_passthrough: false,
            allow_insecure_http: false,
            max_in_flight: None,
            connect_timeout_seconds: 10,
            response_header_timeout_seconds: 300,
            stream_idle_timeout_seconds: 180,
        }
    }

    #[test]
    fn rejects_non_loopback_without_container_exposure() {
        let server = ServerConfig {
            bind: "0.0.0.0:11434".to_owned(),
            exposure: Exposure::Loopback,
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());
    }

    #[test]
    fn validates_alias_provider_references() {
        let mut config = Config {
            server: ServerConfig::default(),
            providers: BTreeMap::from([(
                String::from("local"),
                provider("http://localhost:11434/v1"),
            )]),
            models: BTreeMap::new(),
        };
        config.models.insert(
            "local-model".to_owned(),
            super::ModelConfig {
                targets: vec![super::TargetConfig {
                    provider: "missing".to_owned(),
                    model: "llama".to_owned(),
                }],
            },
        );
        assert!(config.validate(&SecretResolver::default()).is_err());
    }
}
