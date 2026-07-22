use std::collections::{BTreeMap, BTreeSet};
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
    #[serde(default)]
    pub state_path: Option<PathBuf>,
    #[serde(default = "default_catalog_max_age_seconds")]
    pub catalog_max_age_seconds: u64,
    #[serde(default = "default_benchmark_max_age_seconds")]
    pub benchmark_max_age_seconds: u64,
    #[serde(default = "default_quality_floor_simple")]
    pub quality_floor_simple: f64,
    #[serde(default = "default_quality_floor_medium")]
    pub quality_floor_medium: f64,
    #[serde(default = "default_quality_floor_complex")]
    pub quality_floor_complex: f64,
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
    #[serde(default)]
    pub billing_mode: BillingMode,
    #[serde(default)]
    pub account_scope: Option<String>,
    #[serde(default)]
    pub free_models: Vec<String>,
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    #[serde(default)]
    pub model_denylist: Vec<String>,
    #[serde(default)]
    pub quotas: Vec<QuotaLimit>,
    #[serde(default)]
    pub model_mappings: BTreeMap<String, String>,
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
            billing_mode: BillingMode::Free,
            account_scope: None,
            free_models: Vec::new(),
            model_allowlist: Vec::new(),
            model_denylist: Vec::new(),
            quotas: Vec::new(),
            model_mappings: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingMode {
    #[default]
    Free,
    Paid,
    Subscription,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaKind {
    Requests,
    Tokens,
    CostMicrousd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuotaLimit {
    pub kind: QuotaKind,
    pub limit: u64,
    pub window_seconds: u64,
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
            state_path: None,
            catalog_max_age_seconds: default_catalog_max_age_seconds(),
            benchmark_max_age_seconds: default_benchmark_max_age_seconds(),
            quality_floor_simple: default_quality_floor_simple(),
            quality_floor_medium: default_quality_floor_medium(),
            quality_floor_complex: default_quality_floor_complex(),
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
        if let Some(path) = env::var_os("MODEL_GATEWAY_STATE_PATH") {
            config.server.state_path = Some(PathBuf::from(path));
        } else if config.server.state_path.is_none() {
            config.server.state_path = Some(home_dir().join("routing.sqlite3"));
        }
        apply_provider_environment_overrides(&mut config)?;
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
        if self.server.catalog_max_age_seconds == 0 {
            return Err(ConfigError::Invalid(
                "catalog maximum age must be greater than zero".to_owned(),
            ));
        }
        if self.server.benchmark_max_age_seconds == 0
            || !valid_quality_floor(self.server.quality_floor_simple)
            || !valid_quality_floor(self.server.quality_floor_medium)
            || !valid_quality_floor(self.server.quality_floor_complex)
            || self.server.quality_floor_simple > self.server.quality_floor_medium
            || self.server.quality_floor_medium > self.server.quality_floor_complex
        {
            return Err(ConfigError::Invalid(
                "benchmark age and ordered quality floors must be valid (0-100)".to_owned(),
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
        let mut environment_names = BTreeSet::new();
        for (name, provider) in &self.providers {
            if !environment_names.insert(provider_environment_suffix(name)) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}' collides with another provider's environment override name"
                )));
            }
            validate_provider(name, provider, secrets)?;
        }
        if self.models.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one model alias is required".to_owned(),
            ));
        }
        for (alias, model) in &self.models {
            validate_identifier(alias, "model alias")?;
            if matches!(alias.as_str(), "local" | "auto-free" | "auto-efficient") {
                return Err(ConfigError::Invalid(format!(
                    "model alias '{alias}' is reserved for a built-in route"
                )));
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

fn apply_provider_environment_overrides(config: &mut Config) -> Result<(), ConfigError> {
    for (name, provider) in &mut config.providers {
        let suffix = provider_environment_suffix(name);
        let variable = format!("MODEL_GATEWAY_{suffix}_BILLING_MODE");
        if let Ok(value) = env::var(&variable) {
            provider.billing_mode = match value.as_str() {
                "free" => BillingMode::Free,
                "paid" => BillingMode::Paid,
                "subscription" => BillingMode::Subscription,
                _ => {
                    return Err(ConfigError::Invalid(format!(
                        "{variable} must be free, paid, or subscription"
                    )));
                }
            };
        }
    }
    Ok(())
}

fn provider_environment_suffix(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn valid_quality_floor(value: f64) -> bool {
    value.is_finite() && (0.0..=100.0).contains(&value)
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
    for model in provider
        .free_models
        .iter()
        .chain(&provider.model_allowlist)
        .chain(&provider.model_denylist)
    {
        if model.trim().is_empty() || model.len() > 512 {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' contains an invalid routing model ID"
            )));
        }
    }
    if provider
        .model_allowlist
        .iter()
        .any(|model| provider.model_denylist.iter().any(|denied| denied == model))
    {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' allows and denies the same model"
        )));
    }
    if provider
        .quotas
        .iter()
        .any(|quota| quota.limit == 0 || quota.window_seconds == 0)
    {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' quota limits and windows must be greater than zero"
        )));
    }
    if provider.billing_mode == BillingMode::Free
        && provider
            .quotas
            .iter()
            .any(|quota| quota.kind == QuotaKind::CostMicrousd)
    {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' cost quotas require paid or subscription billing"
        )));
    }
    for (offering, canonical) in &provider.model_mappings {
        if offering.trim().is_empty()
            || offering.len() > 512
            || canonical.trim().is_empty()
            || canonical.len() > 512
        {
            return Err(ConfigError::Invalid(format!(
                "provider '{name}' contains an invalid benchmark model mapping"
            )));
        }
    }
    if provider
        .account_scope
        .as_ref()
        .is_some_and(|scope| scope.trim().is_empty() || scope.len() > 128)
    {
        return Err(ConfigError::Invalid(format!(
            "provider '{name}' account scope must be 1-128 characters"
        )));
    }
    if let Some(secret) = &provider.api_key_secret {
        validate_secret_name(secret)?;
        let _ = secrets;
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

const fn default_catalog_max_age_seconds() -> u64 {
    86_400
}

const fn default_benchmark_max_age_seconds() -> u64 {
    604_800
}

const fn default_quality_floor_simple() -> f64 {
    40.0
}

const fn default_quality_floor_medium() -> f64 {
    60.0
}

const fn default_quality_floor_complex() -> f64 {
    75.0
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
        BillingMode, Config, Exposure, ModelConfig, ProviderConfig, QuotaKind, QuotaLimit,
        ServerConfig, TargetConfig, validate_server,
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
            billing_mode: BillingMode::Free,
            account_scope: None,
            free_models: Vec::new(),
            model_allowlist: Vec::new(),
            model_denylist: Vec::new(),
            quotas: Vec::new(),
            model_mappings: BTreeMap::new(),
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
    fn providers_default_to_free_billing_with_no_unverified_models() {
        let provider = ProviderConfig::default();
        assert_eq!(provider.billing_mode, BillingMode::Free);
        assert!(provider.free_models.is_empty());
        assert!(provider.model_allowlist.is_empty());
        assert!(provider.model_denylist.is_empty());
        assert!(provider.quotas.is_empty());
    }

    #[test]
    fn validates_typed_quota_overrides() {
        let mut config = valid_config("https://example.com/v1");
        config.providers.get_mut("local").expect("provider").quotas = vec![QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 50,
            window_seconds: 86_400,
        }];
        config.validate_structure().expect("valid quota override");
        config.providers.get_mut("local").expect("provider").quotas[0].limit = 0;
        assert!(config.validate_structure().is_err());
        config.providers.get_mut("local").expect("provider").quotas[0].limit = 50;
        config.providers.get_mut("local").expect("provider").quotas[0].kind =
            QuotaKind::CostMicrousd;
        assert!(config.validate_structure().is_err());
        config
            .providers
            .get_mut("local")
            .expect("provider")
            .billing_mode = BillingMode::Paid;
        config
            .validate_structure()
            .expect("paid provider cost quota");
    }

    #[test]
    fn rejects_provider_environment_name_collisions() {
        let mut config = valid_config("https://example.com/v1");
        let provider = config.providers.get("local").expect("provider").clone();
        config.providers.clear();
        config.providers.insert("a-b".to_owned(), provider.clone());
        config.providers.insert("a_b".to_owned(), provider);
        for model in config.models.values_mut() {
            model.targets[0].provider = "a-b".to_owned();
        }
        assert!(config.validate_structure().is_err());
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
    fn primary_example_includes_valid_efficiency_policy() {
        let config: Config = toml::from_str(include_str!("../gateway.example.toml"))
            .expect("primary example must parse");
        config
            .validate_structure()
            .expect("primary example must validate");
        let openrouter = &config.providers["openrouter"];
        assert_eq!(openrouter.billing_mode, BillingMode::Paid);
        assert_eq!(
            openrouter.model_mappings["anthropic/claude-sonnet-4.6"],
            "claude-sonnet-4-6"
        );
        assert!(
            openrouter
                .quotas
                .iter()
                .any(|quota| quota.kind == QuotaKind::CostMicrousd)
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
