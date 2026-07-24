use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::benchmarks::Complexity;
use crate::providers::PROFILE_DEFINITIONS;
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PerTaskFloor {
    #[serde(default = "default_quality_floor_general")]
    pub general: f64,
    #[serde(default = "default_quality_floor_coding")]
    pub coding: f64,
    #[serde(default = "default_quality_floor_agentic")]
    pub agentic: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieredQualityFloors {
    #[serde(default)]
    pub simple: PerTaskFloor,
    #[serde(default)]
    pub medium: PerTaskFloor,
    #[serde(default)]
    pub complex: PerTaskFloor,
    #[serde(default)]
    pub very_complex: PerTaskFloor,
}

impl TieredQualityFloors {
    pub fn floor_for(&self, task: crate::benchmarks::TaskKind, complexity: Complexity) -> f64 {
        let task_floors = match complexity {
            Complexity::Simple => &self.simple,
            Complexity::Medium => &self.medium,
            Complexity::Complex => &self.complex,
            Complexity::VeryComplex => &self.very_complex,
        };
        match task {
            crate::benchmarks::TaskKind::General => task_floors.general,
            crate::benchmarks::TaskKind::Coding => task_floors.coding,
            crate::benchmarks::TaskKind::Agentic => task_floors.agentic,
        }
    }
}

impl Default for TieredQualityFloors {
    fn default() -> Self {
        Self {
            simple: PerTaskFloor {
                general: 40.0,
                coding: 35.0,
                agentic: 25.0,
            },
            medium: PerTaskFloor {
                general: 60.0,
                coding: 55.0,
                agentic: 45.0,
            },
            complex: PerTaskFloor {
                general: 75.0,
                coding: 70.0,
                agentic: 60.0,
            },
            very_complex: PerTaskFloor {
                general: 85.0,
                coding: 80.0,
                agentic: 75.0,
            },
        }
    }
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
    #[serde(default)]
    pub quality_floor: TieredQualityFloors,
    #[serde(default)]
    pub frontier_quality_floor: TieredQualityFloors,
    #[serde(default)]
    pub free_quality_floor: TieredQualityFloors,
    #[serde(default = "default_true")]
    pub auto_frontier_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_free_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_efficient_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_balanced_enabled: bool,
    #[serde(default = "default_efficient_quality_floor")]
    pub efficient_quality_floor: f64,
    #[serde(default = "default_balanced_quality_floor")]
    pub balanced_quality_floor: f64,
    #[serde(default = "default_frontier_quality_floor")]
    pub frontier_quality_floor_single: f64,
    #[serde(default)]
    pub free_models_quality: FreeModelsQualityBar,
    #[serde(default)]
    pub model_denylist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreeModelsQualityBar {
    #[serde(default = "default_free_quality_min_general")]
    pub min_general_index: f64,
    #[serde(default = "default_free_quality_min_coding")]
    pub min_coding_index: f64,
    #[serde(default = "default_free_quality_min_agentic")]
    pub min_agentic_index: f64,
    #[serde(default = "default_free_quality_min_context")]
    pub min_context_length: u64,
    #[serde(default = "default_free_quality_min_model_size")]
    pub min_model_size_b: u64,
    #[serde(default = "default_free_quality_max_age_months")]
    pub max_age_months: u64,
    #[serde(default = "default_free_quality_max_input_price")]
    pub max_input_price_per_million: f64,
    #[serde(default = "default_free_quality_max_output_price")]
    pub max_output_price_per_million: f64,
}

impl Default for FreeModelsQualityBar {
    fn default() -> Self {
        Self {
            min_general_index: default_free_quality_min_general(),
            min_coding_index: default_free_quality_min_coding(),
            min_agentic_index: default_free_quality_min_agentic(),
            min_context_length: default_free_quality_min_context(),
            min_model_size_b: default_free_quality_min_model_size(),
            max_age_months: default_free_quality_max_age_months(),
            max_input_price_per_million: default_free_quality_max_input_price(),
            max_output_price_per_million: default_free_quality_max_output_price(),
        }
    }
}

impl FreeModelsQualityBar {
    /// Returns the task-specific minimum quality index.
    fn threshold_for(&self, task: crate::benchmarks::TaskKind) -> f64 {
        match task {
            crate::benchmarks::TaskKind::General => self.min_general_index,
            crate::benchmarks::TaskKind::Coding => self.min_coding_index,
            crate::benchmarks::TaskKind::Agentic => self.min_agentic_index,
        }
    }

    /// Returns `true` if the model passes the quality bar and should be included
    /// in the free-models response. Models without benchmark data always pass
    /// the quality/age filters (new models are not penalized).
    #[allow(clippy::too_many_arguments)]
    pub fn passes(
        &self,
        task: crate::benchmarks::TaskKind,
        benchmark: Option<&crate::benchmarks::BenchmarkModel>,
        refreshed_at: i64,
        effective_input_price: Option<f64>,
        effective_output_price: Option<f64>,
        context_length: Option<u64>,
        model_id: &str,
    ) -> bool {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(i64::MAX);

        // Quality filter: skip if benchmark exists but score is below threshold
        if let Some(benchmark) = benchmark {
            let quality = crate::benchmarks::quality_for(benchmark, task);
            if let Some(score) = quality {
                if score < self.threshold_for(task) {
                    return false;
                }
            }
        }

        // Age filter: use benchmark release_date if available, else refreshed_at
        if self.max_age_months > 0 {
            let max_age_seconds = i64::try_from(self.max_age_months)
                .unwrap_or(i64::MAX)
                .saturating_mul(2_592_000); // 30 days per month
            let cutoff = now_seconds.saturating_sub(max_age_seconds);
            let release_ok = benchmark
                .and_then(|b| b.release_date.as_deref())
                .and_then(parse_date_seconds)
                .map(|release_seconds| release_seconds >= cutoff)
                .unwrap_or(true);
            if !release_ok {
                return false;
            }
            // Fall back to refreshed_at if no release_date
            if benchmark.and_then(|b| b.release_date.as_deref()).is_none() && refreshed_at < cutoff
            {
                return false;
            }
        }

        // Cost filters: skip if effective price exceeds threshold
        if self.max_input_price_per_million > 0.0 {
            if let Some(price) = effective_input_price {
                if price > self.max_input_price_per_million {
                    return false;
                }
            }
        }
        if self.max_output_price_per_million > 0.0 {
            if let Some(price) = effective_output_price {
                if price > self.max_output_price_per_million {
                    return false;
                }
            }
        }

        // Context length filter: skip models with tiny context windows
        if self.min_context_length > 0 {
            if let Some(ctx) = context_length {
                if ctx < self.min_context_length {
                    return false;
                }
            }
        }

        // Model size filter: skip tiny models based on parameter count in ID
        if self.min_model_size_b > 0 {
            if let Some(size_b) = parse_model_size(model_id) {
                if size_b < self.min_model_size_b {
                    return false;
                }
            }
        }

        true
    }
}

/// Parse model size in billions of parameters from a model ID.
pub fn parse_model_size(model_id: &str) -> Option<u64> {
    let lower = model_id.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'b' {
                let num_str = std::str::from_utf8(&bytes[start..i]).ok()?;
                let size: u64 = num_str.parse().ok()?;
                if size > 0 && size <= 1000 {
                    return Some(size);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn parse_date_seconds(date: &str) -> Option<i64> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: u64 = parts[1].parse().ok()?;
    let day: u64 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days from Unix epoch (1970-01-01) using a simple algorithm
    let days_from_epoch = |y: i64, m: u64, d: u64| -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let m = if m <= 2 { m + 12 } else { m };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (m - 3) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + i64::try_from(doy).unwrap_or(0);
        era * 146_097 + doe - 719_468
    };

    let days = days_from_epoch(year, month, day);
    Some(days.saturating_mul(86_400))
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
    #[serde(default = "default_true")]
    pub allow_preview_models: bool,
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
            allow_preview_models: true,
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
    Concurrency,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaBoundary {
    #[default]
    Rolling,
    UtcMinute,
    UtcHour,
    UtcDay,
    UtcWeek,
    UtcMonth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuotaLimit {
    pub kind: QuotaKind,
    pub limit: u64,
    pub window_seconds: u64,
    #[serde(default)]
    pub boundary: QuotaBoundary,
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
    Zai,
    GoogleGemini,
    KiloCode,
    OpenCode,
    OpenCodeGo,
    Mistral,
    NousPortal,
    NvidiaNim,
    Groq,
    OrcaRouter,
    OllamaCloud,
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
            quality_floor: TieredQualityFloors::default(),
            frontier_quality_floor: TieredQualityFloors::default(),
            free_quality_floor: TieredQualityFloors::default(),
            auto_frontier_enabled: true,
            auto_free_enabled: true,
            auto_efficient_enabled: true,
            auto_balanced_enabled: true,
            efficient_quality_floor: default_efficient_quality_floor(),
            balanced_quality_floor: default_balanced_quality_floor(),
            frontier_quality_floor_single: default_frontier_quality_floor(),
            free_models_quality: FreeModelsQualityBar::default(),
            model_denylist: Vec::new(),
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
        let mut config = match Self::read(path) {
            Ok(config) => config,
            Err(ConfigError::Missing(_)) => Self::default(),
            Err(error) => return Err(error),
        };
        config.apply_environment_overrides(
            env::var("MODEL_GATEWAY_BIND").ok().as_deref(),
            env::var("MODEL_GATEWAY_LOCAL_BASE_URL").ok().as_deref(),
            env::var("MODEL_GATEWAY_LOCAL_MODEL").ok().as_deref(),
        )?;
        apply_server_environment_overrides(&mut config.server)?;
        if let Some(path) = env::var_os("MODEL_GATEWAY_STATE_PATH") {
            config.server.state_path = Some(PathBuf::from(path));
        } else if config.server.state_path.is_none() {
            config.server.state_path = Some(home_dir().join("routing.sqlite3"));
        }
        discover_environment_providers(&mut config, secrets)?;
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
            || !valid_quality_floor(self.server.quality_floor.simple.general)
            || !valid_quality_floor(self.server.quality_floor.simple.coding)
            || !valid_quality_floor(self.server.quality_floor.simple.agentic)
            || !valid_quality_floor(self.server.quality_floor.medium.general)
            || !valid_quality_floor(self.server.quality_floor.medium.coding)
            || !valid_quality_floor(self.server.quality_floor.medium.agentic)
            || !valid_quality_floor(self.server.quality_floor.complex.general)
            || !valid_quality_floor(self.server.quality_floor.complex.coding)
            || !valid_quality_floor(self.server.quality_floor.complex.agentic)
            || !valid_quality_floor(self.server.quality_floor.very_complex.general)
            || !valid_quality_floor(self.server.quality_floor.very_complex.coding)
            || !valid_quality_floor(self.server.quality_floor.very_complex.agentic)
            || self.server.quality_floor.simple.general > self.server.quality_floor.medium.general
            || self.server.quality_floor.medium.general > self.server.quality_floor.complex.general
            || self.server.quality_floor.complex.general
                > self.server.quality_floor.very_complex.general
            || self.server.quality_floor.simple.coding > self.server.quality_floor.medium.coding
            || self.server.quality_floor.medium.coding > self.server.quality_floor.complex.coding
            || self.server.quality_floor.complex.coding
                > self.server.quality_floor.very_complex.coding
            || self.server.quality_floor.simple.agentic > self.server.quality_floor.medium.agentic
            || self.server.quality_floor.medium.agentic > self.server.quality_floor.complex.agentic
            || self.server.quality_floor.complex.agentic
                > self.server.quality_floor.very_complex.agentic
            || !valid_quality_floor(self.server.frontier_quality_floor.simple.general)
            || !valid_quality_floor(self.server.frontier_quality_floor.simple.coding)
            || !valid_quality_floor(self.server.frontier_quality_floor.simple.agentic)
            || !valid_quality_floor(self.server.frontier_quality_floor.medium.general)
            || !valid_quality_floor(self.server.frontier_quality_floor.medium.coding)
            || !valid_quality_floor(self.server.frontier_quality_floor.medium.agentic)
            || !valid_quality_floor(self.server.frontier_quality_floor.complex.general)
            || !valid_quality_floor(self.server.frontier_quality_floor.complex.coding)
            || !valid_quality_floor(self.server.frontier_quality_floor.complex.agentic)
            || !valid_quality_floor(self.server.frontier_quality_floor.very_complex.general)
            || !valid_quality_floor(self.server.frontier_quality_floor.very_complex.coding)
            || !valid_quality_floor(self.server.frontier_quality_floor.very_complex.agentic)
            || self.server.frontier_quality_floor.simple.general
                > self.server.frontier_quality_floor.medium.general
            || self.server.frontier_quality_floor.medium.general
                > self.server.frontier_quality_floor.complex.general
            || self.server.frontier_quality_floor.complex.general
                > self.server.frontier_quality_floor.very_complex.general
            || self.server.frontier_quality_floor.simple.coding
                > self.server.frontier_quality_floor.medium.coding
            || self.server.frontier_quality_floor.medium.coding
                > self.server.frontier_quality_floor.complex.coding
            || self.server.frontier_quality_floor.complex.coding
                > self.server.frontier_quality_floor.very_complex.coding
            || self.server.frontier_quality_floor.simple.agentic
                > self.server.frontier_quality_floor.medium.agentic
            || self.server.frontier_quality_floor.medium.agentic
                > self.server.frontier_quality_floor.complex.agentic
            || self.server.frontier_quality_floor.complex.agentic
                > self.server.frontier_quality_floor.very_complex.agentic
            || !valid_quality_floor(self.server.free_quality_floor.simple.general)
            || !valid_quality_floor(self.server.free_quality_floor.simple.coding)
            || !valid_quality_floor(self.server.free_quality_floor.simple.agentic)
            || !valid_quality_floor(self.server.free_quality_floor.medium.general)
            || !valid_quality_floor(self.server.free_quality_floor.medium.coding)
            || !valid_quality_floor(self.server.free_quality_floor.medium.agentic)
            || !valid_quality_floor(self.server.free_quality_floor.complex.general)
            || !valid_quality_floor(self.server.free_quality_floor.complex.coding)
            || !valid_quality_floor(self.server.free_quality_floor.complex.agentic)
            || !valid_quality_floor(self.server.free_quality_floor.very_complex.general)
            || !valid_quality_floor(self.server.free_quality_floor.very_complex.coding)
            || !valid_quality_floor(self.server.free_quality_floor.very_complex.agentic)
            || self.server.free_quality_floor.simple.general
                > self.server.free_quality_floor.medium.general
            || self.server.free_quality_floor.medium.general
                > self.server.free_quality_floor.complex.general
            || self.server.free_quality_floor.complex.general
                > self.server.free_quality_floor.very_complex.general
            || self.server.free_quality_floor.simple.coding
                > self.server.free_quality_floor.medium.coding
            || self.server.free_quality_floor.medium.coding
                > self.server.free_quality_floor.complex.coding
            || self.server.free_quality_floor.complex.coding
                > self.server.free_quality_floor.very_complex.coding
            || self.server.free_quality_floor.simple.agentic
                > self.server.free_quality_floor.medium.agentic
            || self.server.free_quality_floor.medium.agentic
                > self.server.free_quality_floor.complex.agentic
            || self.server.free_quality_floor.complex.agentic
                > self.server.free_quality_floor.very_complex.agentic
            || !valid_quality_floor(self.server.efficient_quality_floor)
            || !valid_quality_floor(self.server.balanced_quality_floor)
            || !valid_quality_floor(self.server.frontier_quality_floor_single)
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
        let mut environment_names = BTreeSet::new();
        for (name, provider) in &self.providers {
            if !environment_names.insert(provider_environment_suffix(name)) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}' collides with another provider's environment override name"
                )));
            }
            validate_provider(name, provider, secrets)?;
        }
        for (alias, model) in &self.models {
            validate_identifier(alias, "model alias")?;
            if matches!(
                alias.as_str(),
                "local" | "auto-free" | "auto-efficient" | "auto-balanced" | "auto-frontier"
            ) {
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
    if let Ok(value) = env::var("MODEL_GATEWAY_PAID_BILLING_MODE") {
        for name in value.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Some(provider) = config.providers.get_mut(name) {
                if provider.billing_mode == BillingMode::Free {
                    provider.billing_mode = BillingMode::Paid;
                }
            } else {
                return Err(ConfigError::Invalid(format!(
                    "MODEL_GATEWAY_PAID_BILLING_MODE: unknown provider '{name}'"
                )));
            }
        }
    }
    for (name, provider) in &mut config.providers {
        let suffix = provider_environment_suffix(name);
        let variable = |field| format!("MODEL_GATEWAY_{suffix}_{field}");

        let billing_variable = variable("BILLING_MODE");
        if let Ok(value) = env::var(&billing_variable) {
            provider.billing_mode = match value.as_str() {
                "free" => BillingMode::Free,
                "paid" => BillingMode::Paid,
                "subscription" => BillingMode::Subscription,
                _ => {
                    return Err(ConfigError::Invalid(format!(
                        "{billing_variable} must be free, paid, or subscription"
                    )));
                }
            };
        }
        let account_scope_variable = variable("ACCOUNT_SCOPE");
        if let Ok(value) = env::var(&account_scope_variable) {
            if value.trim().is_empty() || value.len() > 256 {
                return Err(ConfigError::Invalid(format!(
                    "{account_scope_variable} must be 1-256 non-whitespace characters"
                )));
            }
            provider.account_scope = Some(value);
        }
        for (suffix_name, destination) in [
            ("FREE_MODELS", &mut provider.free_models),
            ("MODEL_ALLOWLIST", &mut provider.model_allowlist),
            ("MODEL_DENYLIST", &mut provider.model_denylist),
        ] {
            let list_variable = variable(suffix_name);
            if let Ok(value) = env::var(&list_variable) {
                *destination = value
                    .split(',')
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
        }
        let preview_variable = variable("ALLOW_PREVIEW_MODELS");
        apply_env_bool(&preview_variable, &mut provider.allow_preview_models)?;

        let base_url_variable = variable("BASE_URL");
        if let Ok(value) = env::var(&base_url_variable) {
            provider.base_url = value;
        }
        let secret_variable = variable("API_KEY_SECRET");
        if let Ok(value) = env::var(&secret_variable) {
            validate_secret_name(&value).map_err(|error| {
                ConfigError::Invalid(format!("{secret_variable} is invalid: {error}"))
            })?;
            provider.api_key_secret = Some(value);
        }
        let passthrough_variable = variable("ALLOW_MODEL_PASSTHROUGH");
        apply_env_bool(&passthrough_variable, &mut provider.allow_model_passthrough)?;
        let insecure_variable = variable("ALLOW_INSECURE_HTTP");
        apply_env_bool(&insecure_variable, &mut provider.allow_insecure_http)?;
        let max_in_flight_variable = variable("MAX_IN_FLIGHT");
        if let Ok(value) = env::var(&max_in_flight_variable) {
            provider.max_in_flight = if value.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(value.parse().map_err(|_| {
                    ConfigError::Invalid(format!(
                        "{max_in_flight_variable} must be none or a non-negative integer"
                    ))
                })?)
            };
        }
        apply_env_u64(
            &variable("CONNECT_TIMEOUT_SECONDS"),
            &mut provider.connect_timeout_seconds,
        )?;
        apply_env_u64(
            &variable("RESPONSE_HEADER_TIMEOUT_SECONDS"),
            &mut provider.response_header_timeout_seconds,
        )?;
        apply_env_u64(
            &variable("STREAM_IDLE_TIMEOUT_SECONDS"),
            &mut provider.stream_idle_timeout_seconds,
        )?;
        apply_env_extra_headers(&variable("EXTRA_HEADERS"), &mut provider.extra_headers)?;
        apply_env_model_mappings(&variable("MODEL_MAPPINGS"), &mut provider.model_mappings)?;
        apply_env_quotas(&variable("QUOTAS"), &mut provider.quotas)?;
    }
    Ok(())
}

fn apply_env_extra_headers(
    name: &str,
    destination: &mut BTreeMap<String, String>,
) -> Result<(), ConfigError> {
    let Ok(value) = env::var(name) else {
        return Ok(());
    };
    let mut headers = BTreeMap::new();
    for entry in value.split(',').filter(|entry| !entry.trim().is_empty()) {
        let (header, header_value) = entry.split_once('=').ok_or_else(|| {
            ConfigError::Invalid(format!("{name} entries must use Header=Value format"))
        })?;
        if header.trim().is_empty() || header_value.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "{name} entries must have non-empty names and values"
            )));
        }
        headers.insert(header.trim().to_owned(), header_value.trim().to_owned());
    }
    *destination = headers;
    Ok(())
}

fn apply_env_model_mappings(
    name: &str,
    destination: &mut BTreeMap<String, String>,
) -> Result<(), ConfigError> {
    let Ok(value) = env::var(name) else {
        return Ok(());
    };
    let mut mappings = BTreeMap::new();
    for entry in value.split(',').filter(|entry| !entry.trim().is_empty()) {
        let (source, target) = entry.split_once('=').ok_or_else(|| {
            ConfigError::Invalid(format!("{name} entries must use source=target format"))
        })?;
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "{name} entries must have non-empty source and target models"
            )));
        }
        mappings.insert(source.trim().to_owned(), target.trim().to_owned());
    }
    *destination = mappings;
    Ok(())
}

fn apply_env_quotas(name: &str, destination: &mut Vec<QuotaLimit>) -> Result<(), ConfigError> {
    let Ok(value) = env::var(name) else {
        return Ok(());
    };
    let mut quotas = Vec::new();
    for entry in value.split(';').filter(|entry| !entry.trim().is_empty()) {
        let parts: Vec<_> = entry.split(':').map(str::trim).collect();
        if !(3..=4).contains(&parts.len()) {
            return Err(ConfigError::Invalid(format!(
                "{name} entries must use kind:limit:window_seconds[:boundary] format"
            )));
        }
        let kind = match parts[0] {
            "requests" => QuotaKind::Requests,
            "tokens" => QuotaKind::Tokens,
            "cost_microusd" => QuotaKind::CostMicrousd,
            "concurrency" => QuotaKind::Concurrency,
            _ => {
                return Err(ConfigError::Invalid(format!(
                    "{name} has unknown quota kind '{}'",
                    parts[0]
                )));
            }
        };
        let limit = parts[1].parse().map_err(|_| {
            ConfigError::Invalid(format!("{name} quota limit must be a non-negative integer"))
        })?;
        let window_seconds = parts[2].parse().map_err(|_| {
            ConfigError::Invalid(format!(
                "{name} quota window_seconds must be a non-negative integer"
            ))
        })?;
        let boundary = match parts.get(3).copied().unwrap_or("rolling") {
            "rolling" => QuotaBoundary::Rolling,
            "utc_minute" => QuotaBoundary::UtcMinute,
            "utc_hour" => QuotaBoundary::UtcHour,
            "utc_day" => QuotaBoundary::UtcDay,
            "utc_week" => QuotaBoundary::UtcWeek,
            "utc_month" => QuotaBoundary::UtcMonth,
            _ => {
                return Err(ConfigError::Invalid(format!(
                    "{name} has an unknown quota boundary"
                )));
            }
        };
        quotas.push(QuotaLimit {
            kind,
            limit,
            window_seconds,
            boundary,
        });
    }
    *destination = quotas;
    Ok(())
}

fn discover_environment_providers(
    config: &mut Config,
    secrets: &SecretResolver,
) -> Result<(), ConfigError> {
    for definition in PROFILE_DEFINITIONS {
        let Some(secret_name) = definition.default_secret_name else {
            continue;
        };
        if secrets
            .get(secret_name)?
            .is_none_or(|value| value.trim().is_empty())
        {
            continue;
        }

        let provider_name = definition.config_key.to_owned();
        let was_present = config.providers.contains_key(&provider_name);
        let provider = config
            .providers
            .entry(provider_name)
            .or_insert_with(|| ProviderConfig {
                profile: Some(definition.id),
                adapter: definition.adapter,
                base_url: definition.native_base_url.to_owned(),
                api_key_secret: Some(secret_name.to_owned()),
                ..ProviderConfig::default()
            });
        if provider.api_key_secret.is_none() {
            provider.api_key_secret = Some(secret_name.to_owned());
        }
        if !was_present {
            provider.billing_mode = default_billing_mode(definition.id);
        }
    }
    Ok(())
}

fn default_billing_mode(_profile: ProviderProfileId) -> BillingMode {
    BillingMode::Free
}

fn apply_server_environment_overrides(server: &mut ServerConfig) -> Result<(), ConfigError> {
    apply_env_string(
        "MODEL_GATEWAY_EXPOSURE",
        &mut server.exposure,
        |value| match value {
            "loopback" => Ok(Exposure::Loopback),
            "local_container" => Ok(Exposure::LocalContainer),
            _ => Err("must be loopback or local_container".to_owned()),
        },
    )?;
    apply_env_usize("MODEL_GATEWAY_MAX_BODY_BYTES", &mut server.max_body_bytes)?;
    apply_env_usize("MODEL_GATEWAY_MAX_IN_FLIGHT", &mut server.max_in_flight)?;
    apply_env_u64(
        "MODEL_GATEWAY_ADMISSION_TIMEOUT_MS",
        &mut server.admission_timeout_ms,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_SHUTDOWN_GRACE_SECONDS",
        &mut server.shutdown_grace_seconds,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_LOCAL_MODEL_CACHE_SECONDS",
        &mut server.local_model_cache_seconds,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_CATALOG_MAX_AGE_SECONDS",
        &mut server.catalog_max_age_seconds,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_BENCHMARK_MAX_AGE_SECONDS",
        &mut server.benchmark_max_age_seconds,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_SIMPLE_GENERAL",
        &mut server.quality_floor.simple.general,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_SIMPLE_CODING",
        &mut server.quality_floor.simple.coding,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_SIMPLE_AGENTIC",
        &mut server.quality_floor.simple.agentic,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_MEDIUM_GENERAL",
        &mut server.quality_floor.medium.general,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_MEDIUM_CODING",
        &mut server.quality_floor.medium.coding,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_MEDIUM_AGENTIC",
        &mut server.quality_floor.medium.agentic,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_COMPLEX_GENERAL",
        &mut server.quality_floor.complex.general,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_COMPLEX_CODING",
        &mut server.quality_floor.complex.coding,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_COMPLEX_AGENTIC",
        &mut server.quality_floor.complex.agentic,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_VERY_COMPLEX_GENERAL",
        &mut server.quality_floor.very_complex.general,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_VERY_COMPLEX_CODING",
        &mut server.quality_floor.very_complex.coding,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_QUALITY_FLOOR_VERY_COMPLEX_AGENTIC",
        &mut server.quality_floor.very_complex.agentic,
    )?;
    apply_env_bool(
        "MODEL_GATEWAY_AUTO_FRONTIER_ENABLED",
        &mut server.auto_frontier_enabled,
    )?;
    apply_env_bool(
        "MODEL_GATEWAY_AUTO_FREE_ENABLED",
        &mut server.auto_free_enabled,
    )?;
    apply_env_bool(
        "MODEL_GATEWAY_AUTO_EFFICIENT_ENABLED",
        &mut server.auto_efficient_enabled,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FREE_QUALITY_MIN_GENERAL",
        &mut server.free_models_quality.min_general_index,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FREE_QUALITY_MIN_CODING",
        &mut server.free_models_quality.min_coding_index,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FREE_QUALITY_MIN_AGENTIC",
        &mut server.free_models_quality.min_agentic_index,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_FREE_QUALITY_MIN_CONTEXT",
        &mut server.free_models_quality.min_context_length,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_FREE_QUALITY_MIN_MODEL_SIZE",
        &mut server.free_models_quality.min_model_size_b,
    )?;
    apply_env_u64(
        "MODEL_GATEWAY_FREE_QUALITY_MAX_AGE_MONTHS",
        &mut server.free_models_quality.max_age_months,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FREE_QUALITY_MAX_INPUT_PRICE",
        &mut server.free_models_quality.max_input_price_per_million,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FREE_QUALITY_MAX_OUTPUT_PRICE",
        &mut server.free_models_quality.max_output_price_per_million,
    )?;
    if let Ok(value) = env::var("MODEL_GATEWAY_MODEL_DENYLIST") {
        server.model_denylist = value
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
    }
    apply_env_bool(
        "MODEL_GATEWAY_AUTO_BALANCED_ENABLED",
        &mut server.auto_balanced_enabled,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_EFFICIENT_QUALITY_FLOOR",
        &mut server.efficient_quality_floor,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_BALANCED_QUALITY_FLOOR",
        &mut server.balanced_quality_floor,
    )?;
    apply_env_f64(
        "MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR",
        &mut server.frontier_quality_floor_single,
    )?;
    Ok(())
}

fn apply_env_string<T>(
    name: &str,
    destination: &mut T,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<(), ConfigError> {
    if let Ok(value) = env::var(name) {
        *destination =
            parse(&value).map_err(|message| ConfigError::Invalid(format!("{name} {message}")))?;
    }
    Ok(())
}

fn apply_env_bool(name: &str, destination: &mut bool) -> Result<(), ConfigError> {
    apply_env_string(name, destination, |value| match value {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err("must be true or false".to_owned()),
    })
}

fn apply_env_usize(name: &str, destination: &mut usize) -> Result<(), ConfigError> {
    apply_env_string(name, destination, |value| {
        value
            .parse()
            .map_err(|_| "must be a non-negative integer".to_owned())
    })
}

fn apply_env_u64(name: &str, destination: &mut u64) -> Result<(), ConfigError> {
    apply_env_string(name, destination, |value| {
        value
            .parse()
            .map_err(|_| "must be a non-negative integer".to_owned())
    })
}

fn apply_env_f64(name: &str, destination: &mut f64) -> Result<(), ConfigError> {
    apply_env_string(name, destination, |value| {
        value.parse().map_err(|_| "must be a number".to_owned())
    })
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
    if !valid_quality_floor(server.free_models_quality.min_general_index) {
        return Err(ConfigError::Invalid(
            "free_models_quality.min_general_index must be between 0 and 100".to_owned(),
        ));
    }
    if !valid_quality_floor(server.free_models_quality.min_coding_index) {
        return Err(ConfigError::Invalid(
            "free_models_quality.min_coding_index must be between 0 and 100".to_owned(),
        ));
    }
    if !valid_quality_floor(server.free_models_quality.min_agentic_index) {
        return Err(ConfigError::Invalid(
            "free_models_quality.min_agentic_index must be between 0 and 100".to_owned(),
        ));
    }
    if !server
        .free_models_quality
        .max_input_price_per_million
        .is_finite()
        || server.free_models_quality.max_input_price_per_million < 0.0
    {
        return Err(ConfigError::Invalid(
            "free_models_quality.max_input_price_per_million must be non-negative".to_owned(),
        ));
    }
    if !server
        .free_models_quality
        .max_output_price_per_million
        .is_finite()
        || server.free_models_quality.max_output_price_per_million < 0.0
    {
        return Err(ConfigError::Invalid(
            "free_models_quality.max_output_price_per_million must be non-negative".to_owned(),
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

const fn default_quality_floor_general() -> f64 {
    40.0
}

const fn default_quality_floor_coding() -> f64 {
    35.0
}

const fn default_quality_floor_agentic() -> f64 {
    25.0
}

const fn default_free_quality_min_general() -> f64 {
    25.0
}

const fn default_free_quality_min_coding() -> f64 {
    35.0
}

const fn default_free_quality_min_agentic() -> f64 {
    15.0
}

const fn default_free_quality_min_context() -> u64 {
    8_192
}

const fn default_free_quality_min_model_size() -> u64 {
    27
}

const fn default_free_quality_max_age_months() -> u64 {
    18
}

const fn default_free_quality_max_input_price() -> f64 {
    5.0
}

const fn default_free_quality_max_output_price() -> f64 {
    15.0
}

const fn default_true() -> bool {
    true
}

const fn default_efficient_quality_floor() -> f64 {
    40.0
}

const fn default_balanced_quality_floor() -> f64 {
    60.0
}

const fn default_frontier_quality_floor() -> f64 {
    80.0
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
        BillingMode, Config, Exposure, ModelConfig, ProviderConfig, QuotaBoundary, QuotaKind,
        QuotaLimit, ServerConfig, TargetConfig, apply_server_environment_overrides,
        validate_server,
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
            allow_preview_models: false,
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
        assert!(server.auto_frontier_enabled);
        assert!(server.auto_free_enabled);
        assert!(server.auto_efficient_enabled);
    }

    #[test]
    fn local_only_configuration_is_valid() {
        let config = Config {
            server: ServerConfig::default(),
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
        };
        config
            .validate_structure()
            .expect("built-in local route should not require aliases");
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
    fn optional_paid_profiles_have_paid_defaults() {
        for profile in [
            super::ProviderProfileId::Deepseek,
            super::ProviderProfileId::Fireworks,
            super::ProviderProfileId::OpenaiApi,
            super::ProviderProfileId::OrcaRouter,
            super::ProviderProfileId::Zai,
            super::ProviderProfileId::OpenCodeGo,
        ] {
            assert_eq!(
                super::default_billing_mode(profile),
                BillingMode::Free,
                "all providers default to Free; set MODEL_GATEWAY_PAID_BILLING_MODE or per-provider BILLING_MODE to enable paid"
            );
        }
    }

    #[test]
    fn validates_typed_quota_overrides() {
        let mut config = valid_config("https://example.com/v1");
        config.providers.get_mut("local").expect("provider").quotas = vec![QuotaLimit {
            kind: QuotaKind::Requests,
            limit: 50,
            window_seconds: 86_400,
            boundary: QuotaBoundary::Rolling,
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
    fn free_models_quality_bar_defaults_are_permissive() {
        let quality = super::FreeModelsQualityBar::default();
        assert_eq!(quality.min_general_index, 25.0);
        assert_eq!(quality.min_coding_index, 35.0);
        assert_eq!(quality.min_agentic_index, 15.0);
        assert_eq!(quality.max_age_months, 18);
        assert_eq!(quality.max_input_price_per_million, 5.0);
        assert_eq!(quality.max_output_price_per_million, 15.0);
    }

    #[test]
    fn free_models_quality_bar_validation_accepts_valid_values() {
        let server = ServerConfig {
            free_models_quality: super::FreeModelsQualityBar {
                min_general_index: 30.0,
                min_context_length: 0,
                min_model_size_b: 0,
                min_coding_index: 35.0,
                min_agentic_index: 15.0,
                max_age_months: 12,
                max_input_price_per_million: 10.0,
                max_output_price_per_million: 20.0,
            },
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_ok());
    }

    #[test]
    fn free_models_quality_bar_rejects_invalid_min_general_index() {
        let server = ServerConfig {
            free_models_quality: super::FreeModelsQualityBar {
                min_general_index: 150.0,
                ..super::FreeModelsQualityBar::default()
            },
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());

        let server = ServerConfig {
            free_models_quality: super::FreeModelsQualityBar {
                min_general_index: -1.0,
                ..super::FreeModelsQualityBar::default()
            },
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());
    }

    #[test]
    fn free_models_quality_bar_passes_unbenchmarked_models() {
        use crate::benchmarks::TaskKind;
        let quality = super::FreeModelsQualityBar {
            min_general_index: 50.0,
            min_context_length: 0,
            min_model_size_b: 0,
            ..super::FreeModelsQualityBar::default()
        };
        // Model without benchmark always passes quality check
        assert!(quality.passes(
            TaskKind::General,
            None,
            9999999999,
            None,
            None,
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_filters_low_quality_benchmarked_models() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let quality = super::FreeModelsQualityBar {
            min_general_index: 50.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            max_age_months: 0,                // disable age filter
            max_input_price_per_million: 0.0, // disable price filter
            max_output_price_per_million: 0.0,
        };
        let model = BenchmarkModel::fixture("weak-model", 30.0, 10.0, 5.0, 1.0, 1.0);
        assert!(!quality.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
        assert!(!quality.passes(
            TaskKind::Coding,
            Some(&model),
            9999999999,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_passes_high_quality_benchmarked_models() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let quality = super::FreeModelsQualityBar {
            min_general_index: 50.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            max_age_months: 0,
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        let model = BenchmarkModel::fixture("strong-model", 80.0, 85.0, 70.0, 1.0, 1.0);
        assert!(quality.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_filters_expensive_models() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let quality = super::FreeModelsQualityBar {
            min_general_index: 0.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 0.0,
            min_agentic_index: 0.0,
            max_age_months: 0,
            max_input_price_per_million: 2.0,
            max_output_price_per_million: 10.0,
        };
        let model = BenchmarkModel::fixture("expensive-model", 70.0, 70.0, 70.0, 0.5, 15.0);
        // Output price (15.0) exceeds limit (10.0)
        assert!(!quality.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            Some(0.5),
            Some(15.0),
            None,
            "test"
        ));

        let model = BenchmarkModel::fixture("cheap-model", 70.0, 70.0, 70.0, 3.0, 5.0);
        // Input price (3.0) exceeds limit (2.0)
        assert!(!quality.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            Some(3.0),
            Some(5.0),
            None,
            "test"
        ));

        let model = BenchmarkModel::fixture("affordable-model", 70.0, 70.0, 70.0, 1.0, 5.0);
        // Both prices within limits
        assert!(quality.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            Some(1.0),
            Some(5.0),
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_filters_old_models() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let quality = super::FreeModelsQualityBar {
            min_general_index: 0.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            max_age_months: 12, // 1 year
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        // Model with recent release_date passes
        let recent = BenchmarkModel {
            release_date: Some("2026-06-01".to_owned()),
            ..BenchmarkModel::fixture("recent", 70.0, 70.0, 70.0, 1.0, 1.0)
        };
        assert!(quality.passes(
            TaskKind::General,
            Some(&recent),
            9999999999,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));

        // Model with old release_date fails
        let old = BenchmarkModel {
            release_date: Some("2024-01-01".to_owned()),
            ..BenchmarkModel::fixture("old", 70.0, 70.0, 70.0, 1.0, 1.0)
        };
        assert!(!quality.passes(
            TaskKind::General,
            Some(&old),
            9999999999,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_uses_refreshed_at_fallback() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let quality = super::FreeModelsQualityBar {
            min_general_index: 0.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            max_age_months: 6, // 6 months
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        let model = BenchmarkModel::fixture("any", 70.0, 70.0, 70.0, 1.0, 1.0);

        // Fresh refreshed_at passes
        assert!(quality.passes(
            TaskKind::General,
            Some(&model),
            now,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
        // Very old refreshed_at fails
        assert!(!quality.passes(
            TaskKind::General,
            Some(&model),
            now - 365 * 86400 * 3,
            Some(1.0),
            Some(1.0),
            None,
            "test"
        ));
    }

    #[test]
    fn free_models_quality_bar_uses_per_task_thresholds() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let bar = super::FreeModelsQualityBar {
            min_general_index: 25.0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            min_context_length: 0,
            min_model_size_b: 0,
            max_age_months: 0,
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };

        // high general, low coding, low agentic
        let model = BenchmarkModel::fixture("model", 28.0, 20.0, 12.0, 1.0, 1.0);
        assert!(bar.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(!bar.passes(
            TaskKind::Coding,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(!bar.passes(
            TaskKind::Agentic,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));

        // high general, high coding, low agentic
        let model = BenchmarkModel::fixture("model", 28.0, 38.0, 12.0, 1.0, 1.0);
        assert!(bar.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(bar.passes(
            TaskKind::Coding,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(!bar.passes(
            TaskKind::Agentic,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));

        // low general, high coding, high agentic
        let model = BenchmarkModel::fixture("model", 20.0, 38.0, 18.0, 1.0, 1.0);
        assert!(!bar.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(bar.passes(
            TaskKind::Coding,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(bar.passes(
            TaskKind::Agentic,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
    }

    #[test]
    fn parse_date_seconds_understands_iso_dates() {
        use super::parse_date_seconds;
        // Unix epoch
        let epoch = parse_date_seconds("1970-01-01").expect("epoch");
        assert_eq!(epoch, 0);
        // Known date
        let known = parse_date_seconds("2025-03-15").expect("2025-03-15");
        // Just verify it's roughly correct
        assert!(known > 1_700_000_000);
        assert!(known < 1_800_000_000);
        // Invalid
        assert!(parse_date_seconds("not-a-date").is_none());
        assert!(parse_date_seconds("2025-13-01").is_none());
        assert!(parse_date_seconds("2025-00-01").is_none());
        assert!(parse_date_seconds("2025-01-00").is_none());
    }

    #[test]
    fn rejects_invalid_per_task_quality_thresholds() {
        let server = ServerConfig {
            free_models_quality: super::FreeModelsQualityBar {
                min_coding_index: 150.0,
                ..super::FreeModelsQualityBar::default()
            },
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());

        let server = ServerConfig {
            free_models_quality: super::FreeModelsQualityBar {
                min_agentic_index: -5.0,
                ..super::FreeModelsQualityBar::default()
            },
            ..ServerConfig::default()
        };
        assert!(validate_server(&server).is_err());
    }

    #[test]
    fn environment_overrides_apply_to_free_quality_per_task_thresholds() {
        let mut config = valid_config("http://localhost:11434/v1");
        apply_server_environment_overrides(&mut config.server).expect("overrides");
        assert_eq!(config.server.free_models_quality.min_general_index, 25.0);

        unsafe {
            std::env::set_var("MODEL_GATEWAY_FREE_QUALITY_MIN_CODING", "42.0");
            std::env::set_var("MODEL_GATEWAY_FREE_QUALITY_MIN_AGENTIC", "8.0");
        }
        let mut config = valid_config("http://localhost:11434/v1");
        apply_server_environment_overrides(&mut config.server).expect("overrides");
        assert_eq!(config.server.free_models_quality.min_coding_index, 42.0);
        assert_eq!(config.server.free_models_quality.min_agentic_index, 8.0);
        unsafe {
            std::env::remove_var("MODEL_GATEWAY_FREE_QUALITY_MIN_CODING");
            std::env::remove_var("MODEL_GATEWAY_FREE_QUALITY_MIN_AGENTIC");
        }
    }

    #[test]
    fn toml_round_trip_preserves_free_models_quality() {
        let original = super::FreeModelsQualityBar {
            min_general_index: 30.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 40.0,
            min_agentic_index: 20.0,
            max_age_months: 24,
            max_input_price_per_million: 3.0,
            max_output_price_per_million: 12.0,
        };
        let encoded = toml::to_string(&original).expect("serialize");
        let decoded: super::FreeModelsQualityBar = toml::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.min_general_index, 30.0);
        assert_eq!(decoded.min_coding_index, 40.0);
        assert_eq!(decoded.min_agentic_index, 20.0);
        assert_eq!(decoded.max_age_months, 24);
        assert_eq!(decoded.max_input_price_per_million, 3.0);
        assert_eq!(decoded.max_output_price_per_million, 12.0);
    }

    #[test]
    fn null_quality_scores_pass_through_quality_bar() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        let bar = super::FreeModelsQualityBar {
            min_general_index: 50.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 50.0,
            min_agentic_index: 50.0,
            max_age_months: 0,
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        // Model with all null scores
        let model = BenchmarkModel {
            intelligence: None,
            coding_quality: None,
            agentic_quality: None,
            ..BenchmarkModel::fixture("null-scores", 0.0, 0.0, 0.0, 1.0, 1.0)
        };
        assert!(bar.passes(
            TaskKind::General,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(bar.passes(
            TaskKind::Coding,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
        assert!(bar.passes(
            TaskKind::Agentic,
            Some(&model),
            9999999999,
            None,
            None,
            None,
            "test"
        ));
    }

    #[test]
    fn release_date_takes_precedence_over_refreshed_at() {
        use crate::benchmarks::{BenchmarkModel, TaskKind};
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let bar = super::FreeModelsQualityBar {
            min_general_index: 0.0,
            min_context_length: 0,
            min_model_size_b: 0,
            min_coding_index: 35.0,
            min_agentic_index: 15.0,
            max_age_months: 12, // 1 year
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        let old_refreshed = now - 365 * 86400 * 2; // 2 years old
        // If release_date is recent, model passes even with ancient refreshed_at
        let model = BenchmarkModel {
            release_date: Some("2026-01-01".to_owned()),
            ..BenchmarkModel::fixture("recent-release", 70.0, 70.0, 70.0, 1.0, 1.0)
        };
        assert!(bar.passes(
            TaskKind::General,
            Some(&model),
            old_refreshed,
            None,
            None,
            None,
            "test"
        ));

        // Without release_date, ancient refreshed_at fails
        let no_release = BenchmarkModel::fixture("old-refreshed", 70.0, 70.0, 70.0, 1.0, 1.0);
        assert!(!bar.passes(
            TaskKind::General,
            Some(&no_release),
            old_refreshed,
            None,
            None,
            None,
            "test"
        ));
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
        assert!(
            config.server.frontier_quality_floor.simple.general
                < config.server.frontier_quality_floor.complex.general
        );
        assert!(openrouter.allow_preview_models);
    }

    #[test]
    fn secondary_provider_example_is_structurally_valid() {
        let config: Config = toml::from_str(include_str!("../gateway.secondary.example.toml"))
            .expect("secondary provider example must parse");
        config
            .validate_structure()
            .expect("secondary provider example must validate");
        assert_eq!(config.providers.len(), 5);
        assert_eq!(config.models.len(), 5);
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

    #[test]
    fn context_filter_rejects_tiny_context_when_available() {
        use crate::benchmarks::BenchmarkModel;
        let filter = super::FreeModelsQualityBar {
            min_general_index: 0.0,
            min_coding_index: 0.0,
            min_agentic_index: 0.0,
            min_context_length: 8_000,
            min_model_size_b: 0,
            max_age_months: 0,
            max_input_price_per_million: 0.0,
            max_output_price_per_million: 0.0,
        };
        let model = BenchmarkModel::fixture("m", 50.0, 50.0, 50.0, 1.0, 1.0);
        assert!(!filter.passes(
            super::super::benchmarks::TaskKind::General,
            Some(&model),
            9999999999,
            None,
            None,
            Some(4096),
            "test",
        ));
        // null context passes (unknown)
        assert!(filter.passes(
            super::super::benchmarks::TaskKind::General,
            None,
            9999999999,
            None,
            None,
            None,
            "test"
        ));
    }

    #[test]
    fn parse_model_size_detects_parameter_counts() {
        use super::parse_model_size;
        assert_eq!(parse_model_size("llama-3.1-8b-instant"), Some(8));
        assert_eq!(parse_model_size("allam-2-7b"), Some(7));
        assert_eq!(parse_model_size("qwen/qwen3-70b-a3b"), Some(70));
        assert_eq!(parse_model_size("deepseek/deepseek-v4-flash"), None);
        assert_eq!(parse_model_size("ministral-14b-2512"), Some(14));
        assert_eq!(parse_model_size("small-model"), None);
        assert_eq!(parse_model_size(""), None);
    }
}
