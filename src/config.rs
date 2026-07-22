use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::secrets::{SecretError, SecretResolver, validate_secret_name};
use crate::storage::write_atomic;

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
    #[serde(default = "default_local_base_url")]
    pub local_base_url: String,
    #[serde(default)]
    pub local_model: Option<String>,
    #[serde(default = "default_local_model_cache_seconds")]
    pub local_model_cache_seconds: u64,
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
    #[serde(default)]
    pub profile: Option<ProviderProfileId>,
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
            profile: None,
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
pub enum ProviderProfileId {
    Custom,
    OpenRouter,
    Ollama,
    LmStudio,
    OpenaiApi,
    Deepseek,
    Fireworks,
    Novita,
    Zai,
    GoogleGemini,
    KiloCode,
    OpenCode,
    Cerebras,
    Mistral,
    NousPortal,
    NvidiaNim,
    Groq,
    OrcaRouter,
    OllamaCloud,
    Cline,
    Gitlawb,
    SiliconFlow,
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
            local_base_url: default_local_base_url(),
            local_model: None,
            local_model_cache_seconds: default_local_model_cache_seconds(),
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
        let mut config = Self::read(path)?;
        config.apply_environment_overrides(
            env::var("MODEL_GATEWAY_BIND").ok().as_deref(),
            env::var("MODEL_GATEWAY_LOCAL_BASE_URL").ok().as_deref(),
            env::var("MODEL_GATEWAY_LOCAL_MODEL").ok().as_deref(),
        )?;
        config.validate(secrets)?;
        Ok(config)
    }

    pub fn apply_environment_overrides(
        &mut self,
        bind: Option<&str>,
        local_base_url: Option<&str>,
        local_model: Option<&str>,
    ) -> Result<(), ConfigError> {
        if let Some(bind) = bind {
            self.server.bind = bind.to_owned();
        }
        if let Some(local_base_url) = local_base_url {
            self.server.local_base_url = local_base_url.to_owned();
        }
        if let Some(local_model) = local_model {
            if local_model.trim().is_empty() || local_model.len() > 512 {
                return Err(ConfigError::Invalid(
                    "MODEL_GATEWAY_LOCAL_MODEL must be 1-512 non-whitespace characters".to_owned(),
                ));
            }
            self.server.local_model = Some(local_model.to_owned());
        }
        Ok(())
    }

    pub fn validate(&self, secrets: &SecretResolver) -> Result<(), ConfigError> {
        self.validate_inner(Some(secrets))
    }

    pub fn validate_structure(&self) -> Result<(), ConfigError> {
        self.validate_inner(None)
    }

    fn validate_inner(&self, secrets: Option<&SecretResolver>) -> Result<(), ConfigError> {
        validate_server(&self.server)?;
        validate_provider(
            "local",
            &ProviderConfig {
                base_url: self.server.local_base_url.clone(),
                allow_insecure_http: self
                    .server
                    .local_base_url
                    .starts_with("http://host.docker.internal"),
                ..ProviderConfig::default()
            },
            None,
        )?;
        if self.server.local_model_cache_seconds == 0 {
            return Err(ConfigError::Invalid(
                "local model cache duration must be greater than zero".to_owned(),
            ));
        }
        if self
            .server
            .local_model
            .as_ref()
            .is_some_and(|model| model.trim().is_empty() || model.len() > 512)
        {
            return Err(ConfigError::Invalid(
                "local model must be 1-512 non-whitespace characters".to_owned(),
            ));
        }
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
            validate_identifier(alias, "model alias")?;
            if alias == "local" {
                return Err(ConfigError::Invalid(
                    "model alias 'local' is reserved for the built-in local route".to_owned(),
                ));
            }
            if alias.contains('/') {
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
                if target.model.trim().is_empty() || target.model.len() > 512 {
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
        write_atomic(path, self.to_toml()?.as_bytes())?;
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
    if server.max_body_bytes == 0
        || server.max_in_flight == 0
        || server.admission_timeout_ms == 0
        || server.shutdown_grace_seconds == 0
    {
        return Err(ConfigError::Invalid(
            "server limits and timeouts must be greater than zero".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider(
    name: &str,
    provider: &ProviderConfig,
    secrets: Option<&SecretResolver>,
) -> Result<(), ConfigError> {
    validate_identifier(name, "provider name")?;
    let url = Url::parse(&provider.base_url)
        .map_err(|error| ConfigError::Invalid(format!("provider '{name}' URL: {error}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' URL must not contain credentials"
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' base URL must not contain a query or fragment"
        )));
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(url.host_str()) => {}
        "http" if provider.allow_insecure_http && is_private_host(url.host_str()) => {}
        "http" if provider.allow_insecure_http => {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' public HTTP URL is not allowed"
            )));
        }
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
        if !is_safe_extra_header(header)
            || value.len() > 4096
            || reqwest::header::HeaderValue::try_from(value).is_err()
        {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' contains unsafe extra header '{header}'"
            )));
        }
    }
    if let Some(secret) = &provider.api_key_secret {
        validate_secret_name(secret)?;
        match secrets {
            Some(resolver) if resolver.get(secret)?.is_none() => {
                return Err(ConfigError::MissingSecret {
                    name: secret.clone(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    match host {
        Some("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn is_private_host(host: Option<&str>) -> bool {
    match host {
        Some("host.docker.internal") => true,
        Some(host) => match host.trim_matches(['[', ']']).parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => is_private_ipv4(ip),
            Ok(IpAddr::V6(ip)) => is_private_ipv6(ip),
            Err(_) => false,
        },
        None => false,
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_link_local()
        || ip.is_loopback()
        || matches!(ip.octets(), [100, 64..=127, _, _])
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
}

fn is_safe_extra_header(header: &str) -> bool {
    let lower = header.to_ascii_lowercase();
    !header.is_empty()
        && !lower.starts_with("proxy-")
        && !lower.contains("authorization")
        && !lower.contains("api-key")
        && !lower.contains("token")
        && !matches!(
            lower.as_str(),
            "authorization"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "cookie"
                | "set-cookie"
                | "x-api-key"
                | "api-key"
                | "x-auth-token"
        )
        && header
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn validate_identifier(value: &str, kind: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ConfigError::Invalid(format!(
            "{kind} '{value}' must be 1-128 ASCII characters from A-Z, a-z, 0-9, '.', '_' or '-'"
        )));
    }
    Ok(())
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
    "127.0.0.1:8008".to_owned()
}

fn default_local_base_url() -> String {
    "http://127.0.0.1:8000/v1".to_owned()
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

const fn default_local_model_cache_seconds() -> u64 {
    60
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
    use super::{
        Config, Exposure, ModelConfig, ProviderConfig, ServerConfig, TargetConfig, validate_server,
    };
    use crate::secrets::SecretResolver;
    use std::collections::BTreeMap;

    fn provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            profile: None,
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

    fn valid_config(base_url: &str) -> Config {
        Config {
            server: ServerConfig::default(),
            providers: BTreeMap::from([("local".to_owned(), provider(base_url))]),
            models: BTreeMap::from([(
                "local-model".to_owned(),
                ModelConfig {
                    targets: vec![TargetConfig {
                        provider: "local".to_owned(),
                        model: "upstream-model".to_owned(),
                    }],
                },
            )]),
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
    fn defaults_gateway_and_local_endpoint_to_distinct_ports() {
        let server = ServerConfig::default();
        assert_eq!(server.bind, "127.0.0.1:8008");
        assert_eq!(server.local_base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(server.local_model, None);
        assert!(server.local_model_cache_seconds > 0);
    }

    #[test]
    fn environment_overrides_only_runtime_server_values() {
        let mut config = valid_config("http://localhost:11434/v1");
        config
            .apply_environment_overrides(
                Some("127.0.0.1:9008"),
                Some("http://127.0.0.1:9000/v1"),
                Some("fixture-local"),
            )
            .expect("environment overrides");
        assert_eq!(config.server.bind, "127.0.0.1:9008");
        assert_eq!(config.server.local_base_url, "http://127.0.0.1:9000/v1");
        assert_eq!(config.server.local_model.as_deref(), Some("fixture-local"));
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

    #[test]
    fn rejects_alias_that_shadows_the_local_route() {
        let mut config = valid_config("http://localhost:11434/v1");
        let model = config.models.remove("local-model").expect("fixture model");
        config.models.insert("local".to_owned(), model);
        let error = config
            .validate(&SecretResolver::default())
            .expect_err("reserved local alias");
        assert!(error.to_string().contains("reserved"));
    }

    #[test]
    fn rejects_unknown_configuration_fields() {
        let error = toml::from_str::<Config>("unknown = true").expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unsafe_provider_and_alias_identifiers() {
        let mut config = valid_config("http://localhost:11434/v1");
        config.providers.insert(
            "provider/name".to_owned(),
            provider("http://localhost:11434/v1"),
        );
        assert!(config.validate(&SecretResolver::default()).is_err());

        let mut config = valid_config("http://localhost:11434/v1");
        config.models.insert(
            "alias with spaces".to_owned(),
            ModelConfig {
                targets: vec![TargetConfig {
                    provider: "local".to_owned(),
                    model: "upstream".to_owned(),
                }],
            },
        );
        assert!(config.validate(&SecretResolver::default()).is_err());
    }

    #[test]
    fn rejects_credential_like_extra_headers() {
        for header in ["x-authorization-note", "provider-token", "x-api-key-id"] {
            let mut config = valid_config("https://example.com/v1");
            config
                .providers
                .get_mut("local")
                .expect("provider")
                .extra_headers
                .insert(header.to_owned(), "metadata".to_owned());
            assert!(config.validate(&SecretResolver::default()).is_err());
        }
    }

    #[test]
    fn profile_identity_round_trips_and_legacy_config_defaults_to_none() {
        let legacy = toml::to_string(&valid_config("http://localhost:11434/v1")).expect("legacy");
        let legacy_config: Config = toml::from_str(&legacy).expect("legacy config");
        assert_eq!(legacy_config.providers["local"].profile, None);

        let mut config = valid_config("https://api.openai.com/v1");
        config.providers.get_mut("local").expect("provider").profile =
            Some(super::ProviderProfileId::OpenaiApi);
        let encoded = config.to_toml().expect("encoded config");
        let decoded: Config = toml::from_str(&encoded).expect("decoded config");
        assert_eq!(
            decoded.providers["local"].profile,
            Some(super::ProviderProfileId::OpenaiApi)
        );
    }

    #[test]
    fn core_provider_example_is_structurally_valid() {
        let config: Config = toml::from_str(include_str!("../gateway.core.example.toml"))
            .expect("CORE provider example must parse");
        config
            .validate_structure()
            .expect("CORE provider example must validate");
        assert_eq!(config.providers.len(), 5);
        assert_eq!(config.models.len(), 5);
        assert!(
            config
                .providers
                .values()
                .all(|provider| provider.profile.is_some() && provider.api_key_secret.is_some())
        );
    }

    #[test]
    fn secondary_provider_example_is_structurally_valid() {
        let config: Config = toml::from_str(include_str!("../gateway.secondary.example.toml"))
            .expect("secondary provider example must parse");
        config
            .validate_structure()
            .expect("secondary provider example must validate");
        assert_eq!(config.providers.len(), 6);
        assert_eq!(config.models.len(), 6);
        assert!(
            config.providers.values().all(|provider| {
                provider.profile.is_some() && provider.api_key_secret.is_some()
            })
        );
    }

    #[test]
    fn optional_provider_example_is_structurally_valid() {
        let config: Config = toml::from_str(include_str!("../gateway.optional.example.toml"))
            .expect("optional provider example must parse");
        config
            .validate_structure()
            .expect("optional provider example must validate");
        assert_eq!(config.providers.len(), 6);
        assert_eq!(config.models.len(), 6);
        assert!(
            config.providers.values().all(|provider| {
                provider.profile.is_some() && provider.api_key_secret.is_some()
            })
        );
    }

    #[test]
    fn rejects_public_http_even_when_insecure_http_is_enabled() {
        let mut config = valid_config("http://example.com/v1");
        config
            .providers
            .get_mut("local")
            .expect("provider")
            .allow_insecure_http = true;
        assert!(config.validate(&SecretResolver::default()).is_err());
    }

    #[test]
    fn permits_explicit_private_and_docker_http() {
        for url in [
            "http://192.168.1.10:11434/v1",
            "http://100.64.0.1:11434/v1",
            "http://[fd00::1]:11434/v1",
            "http://host.docker.internal:11434/v1",
        ] {
            let mut config = valid_config(url);
            config
                .providers
                .get_mut("local")
                .expect("provider")
                .allow_insecure_http = true;
            config
                .validate(&SecretResolver::default())
                .unwrap_or_else(|error| panic!("{url} should be allowed: {error}"));
        }
    }

    #[test]
    fn rejects_credentials_queries_and_sensitive_headers() {
        for url in [
            "https://user:password@example.com/v1",
            "https://example.com/v1?api_key=secret",
            "https://example.com/v1#secret",
        ] {
            assert!(
                valid_config(url)
                    .validate(&SecretResolver::default())
                    .is_err()
            );
        }

        let mut config = valid_config("https://example.com/v1");
        config
            .providers
            .get_mut("local")
            .expect("provider")
            .extra_headers
            .insert("x-api-key".to_owned(), "secret".to_owned());
        assert!(config.validate(&SecretResolver::default()).is_err());
    }

    #[test]
    fn rejects_zero_server_timeouts() {
        let mut server = ServerConfig {
            admission_timeout_ms: 0,
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());
        server.admission_timeout_ms = 1;
        server.shutdown_grace_seconds = 0;
        assert!(validate_server(&server).is_err());
    }

    #[test]
    fn atomic_save_sets_protected_permissions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state").join("config.toml");
        valid_config("http://localhost:11434/v1")
            .save_atomic(&path)
            .expect("save config");
        assert!(Config::read(&path).is_ok());
        assert!(!path.with_extension("toml.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("config metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().expect("config parent"))
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
}
