use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;

use crate::benchmarks::{
    BenchmarkImport, BenchmarkModel, ScoredCandidate, TaskKind, classify, composite_quality,
    pareto_rank, parse_artificial_analysis, quality_for,
};
use crate::config::{
    BillingMode, Config, ProviderConfig, ProviderProfileId, ServerConfig, TargetConfig,
};
use crate::pricing::{EffectivePrice, PriceScope, PriceSourceKind, normalize_price_id};
use crate::providers::BuiltinProvider;
use crate::providers::prepare_request;
use crate::routing::{
    AccessKind, AccountLimitSnapshot, CatalogOffering, IdentityAliasEvidence, ReservationOutcome,
    ReservationRelease, ReservationToken, RoutingError, RoutingStore, classify_access,
    quota_reference,
};
use crate::secrets::{SecretError, SecretResolver};

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const LOCAL_RUNTIME_PROVIDER: &str = "\0local";

#[derive(Debug, Error)]
pub enum GatewayBuildError {
    #[error("configuration error: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("provider '{provider}' client could not be built: {message}")]
    Client { provider: String, message: String },
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    providers: Arc<BTreeMap<String, ProviderRuntime>>,
    global_permits: Arc<Semaphore>,
    local_model: Arc<Mutex<Option<CachedLocalModel>>>,
    routing: Arc<RoutingStore>,
}

struct CachedLocalModel {
    model: String,
    expires_at: Instant,
}

struct ProviderRuntime {
    config: ProviderConfig,
    api_key: Option<String>,
    api_key_source: Option<&'static str>,
    client: Client,
    permits: Arc<Semaphore>,
    available: bool,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<&'static str>,
}

pub fn build_app(config: Config, secrets: &SecretResolver) -> Result<Router, GatewayBuildError> {
    if config.server.exposure == crate::config::Exposure::LocalContainer
        && env::var("MODEL_GATEWAY_CONTAINER_MODE").as_deref() != Ok("1")
    {
        return Err(GatewayBuildError::Config(
            crate::config::ConfigError::Invalid(
                "local_container exposure requires MODEL_GATEWAY_CONTAINER_MODE=1".to_owned(),
            ),
        ));
    }
    config.validate(secrets)?;
    let mut providers = BTreeMap::new();
    for (name, provider) in &config.providers {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(provider.connect_timeout_seconds))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| GatewayBuildError::Client {
                provider: name.clone(),
                message: error.to_string(),
            })?;
        let (api_key, api_key_source) = match provider.api_key_secret.as_deref() {
            Some(name) => (secrets.get(name)?, secrets.source(name)?),
            None => (None, None),
        };
        let available = provider.api_key_secret.is_none()
            || api_key
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        let provider_limit = provider.max_in_flight.unwrap_or_else(|| {
            if provider.billing_mode == crate::config::BillingMode::Free
                && quota_reference(provider, "").is_none()
            {
                1
            } else {
                config.server.max_in_flight
            }
        });
        providers.insert(
            name.clone(),
            ProviderRuntime {
                config: provider.clone(),
                api_key,
                api_key_source,
                client,
                permits: Arc::new(Semaphore::new(provider_limit)),
                available,
            },
        );
    }
    let local_config = ProviderConfig {
        base_url: config.server.local_base_url.clone(),
        allow_insecure_http: config
            .server
            .local_base_url
            .starts_with("http://host.docker.internal"),
        ..ProviderConfig::default()
    };
    let local_client = Client::builder()
        .connect_timeout(Duration::from_secs(local_config.connect_timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| GatewayBuildError::Client {
            provider: "local".to_owned(),
            message: error.to_string(),
        })?;
    providers.insert(
        LOCAL_RUNTIME_PROVIDER.to_owned(),
        ProviderRuntime {
            config: local_config,
            api_key: None,
            api_key_source: None,
            client: local_client,
            permits: Arc::new(Semaphore::new(config.server.max_in_flight)),
            available: true,
        },
    );
    let routing = Arc::new(RoutingStore::open(config.server.state_path.as_deref())?);
    for (provider_name, provider) in &config.providers {
        for model in &provider.free_models {
            routing.upsert_offering(provider_name, model, AccessKind::ZeroPrice)?;
        }
    }
    for model in config.models.values() {
        for target in &model.targets {
            if let Some(provider) = config.providers.get(&target.provider) {
                let access_kind = classify_access(provider, &target.model, false);
                if access_kind.is_free() {
                    routing.upsert_offering(&target.provider, &target.model, access_kind)?;
                }
            }
        }
    }
    let state = AppState {
        global_permits: Arc::new(Semaphore::new(config.server.max_in_flight)),
        config: Arc::new(config),
        providers: Arc::new(providers),
        local_model: Arc::new(Mutex::new(None)),
        routing,
    };
    Ok(Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route(
            "/openapi.json",
            get(|| async {
                (
                    [("content-type", "application/json; charset=utf-8")],
                    include_str!("../docs/openapi.json"),
                )
            }),
        )
        .route("/v1/models", get(list_models))
        .route("/v1/catalog/models", get(list_catalog_models))
        .route("/v1/catalog/models/{*model_id}", get(get_catalog_model))
        .route("/v1/providers", get(list_providers))
        .route("/v1/rankings", get(list_rankings))
        .route("/v1/auto-models", get(list_auto_models))
        .route("/v1/chat/completions", post(chat_completions))
        .layer(DefaultBodyLimit::max(state.config.server.max_body_bytes))
        .with_state(state))
}

#[derive(Debug, Deserialize)]
struct ProvidersQuery {
    available: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelMatchKind {
    Exact,
    Configured,
    Approved,
    Suggested,
    Ambiguous,
    Unmatched,
}

impl ModelMatchKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Configured => "configured",
            Self::Approved => "approved",
            Self::Suggested => "suggested",
            Self::Ambiguous => "ambiguous",
            Self::Unmatched => "unmatched",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelMatchReport {
    pub provider: String,
    pub catalog_model: String,
    pub status: ModelMatchKind,
    pub benchmark_model: Option<String>,
    pub alternatives: Vec<String>,
    pub source: Option<String>,
    pub identity_evidence: Vec<IdentityAliasEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingCoverageKind {
    Complete,
    Incomplete,
    Missing,
}

impl PricingCoverageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingCoverageReport {
    pub provider: String,
    pub catalog_model: String,
    pub status: PricingCoverageKind,
    pub catalog_input_price_per_million: Option<f64>,
    pub catalog_output_price_per_million: Option<f64>,
    pub effective_input_price_per_million: Option<f64>,
    pub effective_output_price_per_million: Option<f64>,
    pub effective_cache_read_price_per_million: Option<f64>,
    pub effective_cache_write_price_per_million: Option<f64>,
    pub effective_source: Option<String>,
    pub effective_scope: Option<PriceScope>,
    pub estimated: Option<bool>,
}

pub fn report_pricing_coverage(
    config: &Config,
    routing: &RoutingStore,
    provider_filter: Option<&str>,
) -> Result<Vec<PricingCoverageReport>, RoutingError> {
    let offerings = routing.all_candidates(config.server.catalog_max_age_seconds)?;
    offerings
        .into_iter()
        .filter(|offering| {
            provider_filter.is_none_or(|provider| provider == offering.provider)
                && !is_provider_auto_route(&offering.model)
        })
        .map(|offering| {
            let provider_config = config.providers.get(&offering.provider);
            let profile_key = provider_config.and_then(|provider| {
                provider
                    .pricing_profile
                    .as_deref()
                    .or_else(|| provider.profile.and_then(BuiltinProvider::models_dev_key))
            });
            let canonical = provider_config.and_then(|provider| {
                provider
                    .model_mappings
                    .get(&offering.model)
                    .map(String::as_str)
            });
            let effective = routing.effective_price(
                &offering.provider,
                profile_key,
                &offering.model,
                canonical,
                config.server.pricing_max_age_seconds,
            )?;
            let incomplete_observation = if effective.is_none() {
                routing.has_incomplete_price_observation(
                    &offering.provider,
                    profile_key,
                    &offering.model,
                    canonical,
                    config.server.pricing_max_age_seconds,
                )?
            } else {
                false
            };
            let direct_complete = offering.input_price_per_million.is_some()
                && offering.output_price_per_million.is_some();
            let direct_incomplete = offering.input_price_per_million.is_some()
                || offering.output_price_per_million.is_some();
            let status = if effective.is_some() || direct_complete {
                PricingCoverageKind::Complete
            } else if direct_incomplete || incomplete_observation {
                PricingCoverageKind::Incomplete
            } else {
                PricingCoverageKind::Missing
            };
            Ok(PricingCoverageReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status,
                catalog_input_price_per_million: offering.input_price_per_million,
                catalog_output_price_per_million: offering.output_price_per_million,
                effective_input_price_per_million: effective
                    .as_ref()
                    .map(|price| price.input_price_per_million),
                effective_output_price_per_million: effective
                    .as_ref()
                    .map(|price| price.output_price_per_million),
                effective_cache_read_price_per_million: effective
                    .as_ref()
                    .and_then(|price| price.cache_read_price_per_million),
                effective_cache_write_price_per_million: effective
                    .as_ref()
                    .and_then(|price| price.cache_write_price_per_million),
                effective_source: effective.as_ref().map(|price| price.source.clone()),
                effective_scope: effective.as_ref().map(|price| price.scope),
                estimated: effective.as_ref().map(|price| price.estimated),
            })
        })
        .collect()
}

enum BenchmarkResolution<'a> {
    Exact(Vec<&'a BenchmarkModel>),
    Suggested(Vec<&'a BenchmarkModel>),
    Ambiguous(Vec<String>),
    Unmatched,
}

struct BenchmarkIdentityIndex {
    models: Vec<BenchmarkModel>,
    raw: BTreeMap<String, Vec<usize>>,
    identities: BTreeMap<String, Vec<usize>>,
}

impl BenchmarkIdentityIndex {
    fn new(models: Vec<BenchmarkModel>) -> Self {
        let mut index = Self {
            models,
            raw: BTreeMap::new(),
            identities: BTreeMap::new(),
        };
        for (position, benchmark) in index.models.iter().enumerate() {
            for variant in normalized_identifier_variants(&benchmark.id) {
                index.raw.entry(variant).or_default().push(position);
            }
            for variant in normalized_identifier_variants(&benchmark_identity_id(benchmark)) {
                index.identities.entry(variant).or_default().push(position);
            }
        }
        index
    }

    fn exact_matches(&self, model: &str, raw: bool) -> Vec<&BenchmarkModel> {
        let lookup = if raw { &self.raw } else { &self.identities };
        let mut positions = BTreeSet::new();
        for variant in normalized_identifier_variants(model) {
            if let Some(matches) = lookup.get(&variant) {
                positions.extend(matches.iter().copied());
            }
        }
        positions
            .into_iter()
            .map(|position| &self.models[position])
            .collect()
    }
}

pub fn reconcile_model_matches(
    config: &Config,
    routing: &RoutingStore,
    provider_filter: Option<&str>,
) -> Result<Vec<ModelMatchReport>, RoutingError> {
    let offerings = routing.all_candidates(config.server.catalog_max_age_seconds)?;
    let benchmark_index = BenchmarkIdentityIndex::new(
        routing.benchmark_models(config.server.benchmark_max_age_seconds)?,
    );
    let mappings = identity_mapping_indexes(routing);
    let identity_aliases = routing.active_identity_aliases().unwrap_or_default();
    let mut report = Vec::new();
    for offering in offerings {
        if provider_filter.is_some_and(|provider| provider != offering.provider)
            || is_provider_auto_route(&offering.model)
        {
            continue;
        }
        let Some(provider) = config.providers.get(&offering.provider) else {
            continue;
        };
        let canonical = canonical_match(provider, &offering.provider, &offering.model, &mappings);
        let identity_evidence = identity_provider_key(provider)
            .map(|provider_key| {
                identity_aliases
                    .iter()
                    .filter(|alias| {
                        alias.provider_key == provider_key
                            && is_exact_model_identity(&alias.provider_model_id, &offering.model)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if canonical.kind != ModelMatchKind::Exact {
            let exact = find_exact_matching_benchmarks_indexed(
                &benchmark_index,
                &canonical.benchmark_model,
            );
            report.push(ModelMatchReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status: if exact.is_empty() {
                    ModelMatchKind::Unmatched
                } else {
                    canonical.kind
                },
                benchmark_model: (!exact.is_empty()).then_some(canonical.benchmark_model),
                alternatives: exact.into_iter().map(|model| model.id.clone()).collect(),
                source: Some(canonical.source.to_owned()),
                identity_evidence,
            });
            continue;
        }

        let resolution = resolve_benchmark_identity_indexed(&benchmark_index, &offering.model);
        let identity_conflict = identity_provider_key(provider).and_then(|provider_key| {
            mappings
                .conflicts
                .get(&(provider_key.to_owned(), offering.model.clone()))
                .cloned()
        });
        if !matches!(resolution, BenchmarkResolution::Exact(_)) {
            if let Some(alternatives) = identity_conflict {
                report.push(ModelMatchReport {
                    provider: offering.provider,
                    catalog_model: offering.model,
                    status: ModelMatchKind::Ambiguous,
                    benchmark_model: None,
                    alternatives,
                    source: Some("canonical_entity_conflict".to_owned()),
                    identity_evidence,
                });
                continue;
            }
        }
        let entry = match resolution {
            BenchmarkResolution::Exact(models) => ModelMatchReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status: ModelMatchKind::Exact,
                benchmark_model: models.first().map(|model| benchmark_identity_id(model)),
                alternatives: models.into_iter().map(|model| model.id.clone()).collect(),
                source: Some("normalized_exact".to_owned()),
                identity_evidence,
            },
            BenchmarkResolution::Suggested(models) => ModelMatchReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status: ModelMatchKind::Suggested,
                benchmark_model: models.first().map(|model| benchmark_identity_id(model)),
                alternatives: models.into_iter().map(|model| model.id.clone()).collect(),
                source: Some("offline_heuristic".to_owned()),
                identity_evidence,
            },
            BenchmarkResolution::Ambiguous(alternatives) => ModelMatchReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status: ModelMatchKind::Ambiguous,
                benchmark_model: None,
                alternatives,
                source: Some("offline_heuristic".to_owned()),
                identity_evidence,
            },
            BenchmarkResolution::Unmatched => ModelMatchReport {
                provider: offering.provider,
                catalog_model: offering.model,
                status: ModelMatchKind::Unmatched,
                benchmark_model: None,
                alternatives: Vec::new(),
                source: None,
                identity_evidence,
            },
        };
        report.push(entry);
    }
    Ok(report)
}

fn find_exact_matching_benchmarks_indexed<'a>(
    benchmarks: &'a BenchmarkIdentityIndex,
    model: &str,
) -> Vec<&'a BenchmarkModel> {
    match resolve_benchmark_identity_indexed(benchmarks, model) {
        BenchmarkResolution::Exact(models) => models,
        BenchmarkResolution::Suggested(_)
        | BenchmarkResolution::Ambiguous(_)
        | BenchmarkResolution::Unmatched => Vec::new(),
    }
}

fn resolve_benchmark_identity_indexed<'a>(
    benchmarks: &'a BenchmarkIdentityIndex,
    model: &str,
) -> BenchmarkResolution<'a> {
    let normalized = normalize_identifier(model);
    let stripped = strip_model_noise(model);
    let mut lookups = vec![model.to_owned()];
    if normalize_identifier(&stripped) != normalized {
        lookups.push(stripped);
    }

    if has_explicit_effort_suffix(model) {
        let exact = benchmarks.exact_matches(model, true);
        if !exact.is_empty() {
            return BenchmarkResolution::Exact(exact);
        }
    }

    for lookup in &lookups {
        let exact = benchmarks.exact_matches(lookup, false);
        if !exact.is_empty() {
            return BenchmarkResolution::Exact(exact);
        }
    }

    if has_dynamic_or_release_suffix(model) {
        return BenchmarkResolution::Unmatched;
    }

    for lookup in &lookups {
        let mut groups = BTreeMap::<String, Vec<&BenchmarkModel>>::new();
        for benchmark in &benchmarks.models {
            let identity = benchmark_identity_id(benchmark);
            if benchmark_ids_match(lookup, &identity) {
                groups
                    .entry(normalize_identifier(&identity))
                    .or_default()
                    .push(benchmark);
            }
        }
        if groups.len() == 1 {
            return BenchmarkResolution::Suggested(groups.into_values().next().unwrap_or_default());
        }
        if groups.len() > 1 {
            return BenchmarkResolution::Ambiguous(groups.into_keys().collect());
        }
    }
    BenchmarkResolution::Unmatched
}

fn find_benchmark<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
) -> Option<&'a BenchmarkModel> {
    best_benchmark(find_exact_matching_benchmarks(benchmarks, model))
}

#[cfg(test)]
fn find_suggested_benchmark<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
) -> Option<&'a BenchmarkModel> {
    best_benchmark(find_all_matching_benchmarks(benchmarks, model))
}

fn best_benchmark(benchmarks: Vec<&BenchmarkModel>) -> Option<&BenchmarkModel> {
    benchmarks
        .into_iter()
        .filter_map(|benchmark| Some((benchmark, composite_quality(benchmark)?)))
        .max_by(|(left, left_quality), (right, right_quality)| {
            left_quality
                .total_cmp(right_quality)
                .then_with(|| right.id.cmp(&left.id))
        })
        .map(|(benchmark, _)| benchmark)
}

#[cfg(test)]
fn find_all_matching_benchmarks<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
) -> Vec<&'a BenchmarkModel> {
    match resolve_benchmark_identity(benchmarks, model) {
        BenchmarkResolution::Exact(models) | BenchmarkResolution::Suggested(models) => models,
        BenchmarkResolution::Ambiguous(_) | BenchmarkResolution::Unmatched => Vec::new(),
    }
}

fn find_exact_matching_benchmarks<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
) -> Vec<&'a BenchmarkModel> {
    match resolve_benchmark_identity(benchmarks, model) {
        BenchmarkResolution::Exact(models) => models,
        BenchmarkResolution::Suggested(_)
        | BenchmarkResolution::Ambiguous(_)
        | BenchmarkResolution::Unmatched => Vec::new(),
    }
}

fn resolve_benchmark_identity<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
) -> BenchmarkResolution<'a> {
    let normalized = normalize_identifier(model);
    let stripped = strip_model_noise(model);
    let mut lookups = vec![model.to_owned()];
    if normalize_identifier(&stripped) != normalized {
        lookups.push(stripped);
    }

    if has_explicit_effort_suffix(model) {
        let mut exact = Vec::new();
        for (benchmark_id, models) in benchmarks {
            if is_exact_benchmark_match(model, benchmark_id) {
                exact.extend(models);
            }
        }
        if !exact.is_empty() {
            return BenchmarkResolution::Exact(exact);
        }
    }

    for lookup in &lookups {
        let mut exact = Vec::new();
        for models in benchmarks.values() {
            for benchmark in models {
                if is_exact_benchmark_match(lookup, &benchmark_identity_id(benchmark)) {
                    exact.push(benchmark);
                }
            }
        }
        if !exact.is_empty() {
            return BenchmarkResolution::Exact(exact);
        }
    }

    if has_dynamic_or_release_suffix(model) {
        return BenchmarkResolution::Unmatched;
    }

    for lookup in &lookups {
        let mut groups = BTreeMap::<String, Vec<&BenchmarkModel>>::new();
        for models in benchmarks.values() {
            for benchmark in models {
                let identity = benchmark_identity_id(benchmark);
                if benchmark_ids_match(lookup, &identity) {
                    groups
                        .entry(normalize_identifier(&identity))
                        .or_default()
                        .push(benchmark);
                }
            }
        }
        if groups.len() == 1 {
            return BenchmarkResolution::Suggested(groups.into_values().next().unwrap_or_default());
        }
        if groups.len() > 1 {
            return BenchmarkResolution::Ambiguous(groups.into_keys().collect());
        }
    }
    BenchmarkResolution::Unmatched
}

fn benchmarks_for_effort<'a>(
    benchmarks: Vec<&'a BenchmarkModel>,
    requested_effort: Option<&str>,
) -> Vec<&'a BenchmarkModel> {
    let Some(requested_effort) = requested_effort else {
        return benchmarks;
    };
    if !benchmarks
        .iter()
        .any(|benchmark| benchmark.reasoning_effort.is_some())
    {
        return benchmarks;
    }
    benchmarks
        .into_iter()
        .filter(|benchmark| benchmark.reasoning_effort.as_deref() == Some(requested_effort))
        .collect()
}

const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

fn has_explicit_effort_suffix(model: &str) -> bool {
    let normalized = normalize_identifier(model);
    REASONING_EFFORTS.iter().any(|effort| {
        normalized.ends_with(&format!("-{effort}"))
            || normalized.ends_with(&format!("-{effort}-effort"))
    })
}

fn benchmark_identity_id(benchmark: &BenchmarkModel) -> String {
    let normalized = normalize_identifier(&benchmark.id);
    let Some(effort) = benchmark.reasoning_effort.as_deref() else {
        return normalized;
    };
    normalized
        .strip_suffix(&format!("-{effort}-effort"))
        .or_else(|| normalized.strip_suffix(&format!("-{effort}")))
        .unwrap_or(&normalized)
        .to_owned()
}

fn has_dynamic_or_release_suffix(model: &str) -> bool {
    let normalized = normalize_identifier(model);
    let tokens = normalized.split('-').collect::<Vec<_>>();
    let iso_month_suffix = tokens.len() >= 2
        && tokens[tokens.len() - 2].len() == 4
        && tokens[tokens.len() - 2]
            .chars()
            .all(|character| character.is_ascii_digit())
        && tokens[tokens.len() - 1].len() == 2
        && tokens[tokens.len() - 1]
            .chars()
            .all(|character| character.is_ascii_digit());
    let iso_day_suffix = tokens.len() >= 3
        && tokens[tokens.len() - 3].len() == 4
        && tokens[tokens.len() - 3]
            .chars()
            .all(|character| character.is_ascii_digit())
        && tokens[tokens.len() - 2].len() == 2
        && tokens[tokens.len() - 2]
            .chars()
            .all(|character| character.is_ascii_digit())
        && tokens[tokens.len() - 1].len() == 2
        && tokens[tokens.len() - 1]
            .chars()
            .all(|character| character.is_ascii_digit());
    tokens.contains(&"latest")
        || tokens.last().is_some_and(|token| {
            token.len() == 4 && token.chars().all(|character| character.is_ascii_digit())
        })
        || iso_month_suffix
        || iso_day_suffix
}

fn is_exact_benchmark_match(catalog_id: &str, benchmark_id: &str) -> bool {
    let catalog_variants = normalized_identifier_variants(catalog_id);
    let benchmark_variants = normalized_identifier_variants(benchmark_id);
    for catalog in &catalog_variants {
        for benchmark in &benchmark_variants {
            if catalog == benchmark {
                return true;
            }
        }
    }
    false
}

pub fn is_exact_model_identity(left: &str, right: &str) -> bool {
    is_exact_benchmark_match(left, right)
}

fn is_exact_runtime_model_identity(left: &str, right: &str) -> bool {
    is_exact_benchmark_match(left, right)
        || is_exact_benchmark_match(&strip_model_noise(left), right)
}

const IDENTITY_VARIANT_TOKENS: &[&str] = &[
    "audio",
    "base",
    "chat",
    "coder",
    "discounted",
    "distill",
    "embed",
    "embedding",
    "flash",
    "highspeed",
    "image",
    "instruct",
    "large",
    "lite",
    "max",
    "medium",
    "mini",
    "nano",
    "next",
    "non",
    "ocr",
    "omni",
    "plus",
    "preview",
    "pro",
    "realtime",
    "reasoning",
    "rerank",
    "research",
    "small",
    "super",
    "thinking",
    "turbo",
    "ultra",
    "vision",
    "vl",
];

const NOISE_TOKENS: &[&str] = &["fp8", "fp16", "bf16", "int4", "int8", "free"];

fn strip_model_noise(model: &str) -> String {
    let segments: Vec<&str> = model.split(['/', ':']).collect();
    let stripped: Vec<String> = segments
        .iter()
        .map(|segment| {
            let normalized = normalize_identifier(segment);
            let mut tokens: Vec<&str> = normalized.split('-').collect();

            // Remove terminal transport/billing decorations, never semantic
            // tokens that happen to use the same word internally.
            while tokens
                .last()
                .is_some_and(|token| NOISE_TOKENS.contains(token))
            {
                tokens.pop();
            }

            tokens.join("-")
        })
        .collect();
    stripped.join("/")
}

fn benchmark_ids_match(catalog_id: &str, benchmark_id: &str) -> bool {
    let catalog_variants = normalized_identifier_variants(catalog_id);
    let benchmark_variants = normalized_identifier_variants(benchmark_id);
    for catalog in &catalog_variants {
        for benchmark in &benchmark_variants {
            if identity_variant_tokens(catalog) != identity_variant_tokens(benchmark) {
                continue;
            }
            if catalog == benchmark {
                return true;
            }
            let catalog_tokens = catalog.split('-').collect::<BTreeSet<_>>();
            let benchmark_tokens = benchmark.split('-').collect::<BTreeSet<_>>();
            if safe_benchmark_extension(catalog, benchmark) {
                return true;
            }
            if catalog_tokens.len() >= 2
                && catalog_tokens.len() == benchmark_tokens.len()
                && catalog_tokens == benchmark_tokens
            {
                return true;
            }
        }
    }

    // Permit one creator/provider prefix, but never scan arbitrary suffixes:
    // that can cross families such as Nemotron and Qwen Omni.
    for catalog in &catalog_variants {
        let cat_tokens: Vec<&str> = catalog.split('-').collect();
        for benchmark in &benchmark_variants {
            if identity_variant_tokens(catalog) != identity_variant_tokens(benchmark) {
                continue;
            }
            let bench_tokens: Vec<&str> = benchmark.split('-').collect();
            if catalog_provider_prefix(&cat_tokens, &bench_tokens)
                || benchmark_creator_prefix(&bench_tokens, &cat_tokens)
            {
                return true;
            }
        }
    }

    false
}

fn catalog_provider_prefix(catalog: &[&str], benchmark: &[&str]) -> bool {
    if catalog.len() != benchmark.len() + 1 {
        return false;
    }
    catalog.get(1..).is_some_and(|aligned| aligned == benchmark)
}

fn benchmark_creator_prefix(benchmark: &[&str], catalog: &[&str]) -> bool {
    if benchmark.len() <= catalog.len() || catalog.len() < 2 {
        return false;
    }
    benchmark
        .get(1..)
        .is_some_and(|aligned| aligned == catalog || safe_token_extension(catalog, aligned))
}

fn safe_benchmark_extension(catalog: &str, benchmark: &str) -> bool {
    let catalog_tokens = catalog.split('-').collect::<Vec<_>>();
    let benchmark_tokens = benchmark.split('-').collect::<Vec<_>>();
    safe_token_extension(&catalog_tokens, &benchmark_tokens)
}

fn safe_token_extension(base: &[&str], candidate: &[&str]) -> bool {
    if base.len() < 2 || candidate.len() <= base.len() || !candidate.starts_with(base) {
        return false;
    }
    candidate[base.len()..]
        .iter()
        .all(|token| token.chars().any(|character| character.is_ascii_digit()))
}

fn identity_variant_tokens(identifier: &str) -> BTreeSet<&str> {
    identifier
        .split('-')
        .filter(|token| IDENTITY_VARIANT_TOKENS.contains(token))
        .collect()
}

/// Known model ID normalizations for cases where the catalog and AA
/// use different naming conventions for the same model.
const MODEL_ID_NORMALIZATIONS: &[(&str, &str)] =
    &[("gemma4:", "gemma-4:"), ("gemma4/", "gemma-4/")];

fn normalized_identifier_variants(identifier: &str) -> Vec<String> {
    let mut normalized = identifier.to_owned();
    for (from, to) in MODEL_ID_NORMALIZATIONS {
        if normalized.contains(from) {
            normalized = normalized.replace(from, to);
            break;
        }
    }
    let segments = normalized
        .split(['/', ':'])
        .map(normalize_identifier)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let full = segments.join("-");
    let mut variants = vec![full];
    // A provider namespace is the only prefix that exact identity matching may
    // discard. Removing arbitrary suffixes makes unrelated IDs such as
    // `vendor-a/model:free` and `vendor-b/other:free` collide.
    if segments.len() > 1 {
        variants.push(segments[1..].join("-"));
    }
    variants
}

fn normalize_identifier(identifier: &str) -> String {
    let mut normalized = String::new();
    for character in identifier.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
        } else if !normalized.ends_with('-') {
            normalized.push('-');
        }
    }
    normalized.trim_matches('-').to_owned()
}

async fn list_providers(
    State(state): State<AppState>,
    Query(query): Query<ProvidersQuery>,
) -> Response {
    let mut model_counts = BTreeMap::new();
    let account_limits = state.routing.account_limits().unwrap_or_default();
    if let Ok(offerings) = state
        .routing
        .all_candidates(state.config.server.catalog_max_age_seconds)
    {
        for offering in offerings {
            let is_free = state
                .config
                .providers
                .get(&offering.provider)
                .is_some_and(|provider| {
                    let access_kind = effective_access_kind(provider, &offering);
                    access_kind.is_free()
                        && account_allows_free_access(
                            access_kind,
                            account_limits.get(&offering.provider),
                        )
                });
            let counts = model_counts
                .entry(offering.provider)
                .or_insert((0usize, 0usize));
            counts.0 += 1;
            if is_free {
                counts.1 += 1;
            }
        }
    }

    // Collect configured profile IDs to avoid duplicates in unconfigured section
    let mut configured_profiles: Vec<ProviderProfileId> = Vec::new();
    for runtime in state.providers.values() {
        if let Some(profile) = runtime.config.profile {
            if !configured_profiles.contains(&profile) {
                configured_profiles.push(profile);
            }
        }
    }

    let mut providers = Vec::new();

    // Configured providers
    for (code_name, runtime) in &*state.providers {
        if query.available.is_some_and(|a| a != runtime.available) {
            continue;
        }
        let config = &runtime.config;
        let (model_count, free_model_count) = model_counts
            .get(code_name.as_str())
            .copied()
            .unwrap_or_default();
        providers.push(json!({
            "id": code_name,
            "name": config.profile
                .map(|p| p.display_name())
                .unwrap_or("Custom OpenAI-compatible"),
            "adapter": format!("{:?}", config.adapter).to_lowercase(),
            "base_url": config.base_url,
            "billing_mode": match config.billing_mode {
                BillingMode::Free => "free",
                BillingMode::Paid => "paid",
                BillingMode::Subscription => "subscription",
            },
            "api_key_secret": config.api_key_secret,
            "api_key_source": runtime.api_key_source,
            "account_scope": config.account_scope,
            "model_count": model_count,
            "free_model_count": free_model_count,
            "model_allowlist_count": config.model_allowlist.len(),
            "model_denylist_count": config.model_denylist.len(),
            "available": runtime.available,
        }));
    }

    // Unconfigured built-in profiles
    if query.available.is_none_or(|a| !a) {
        for profile in ProviderProfileId::all() {
            if profile == ProviderProfileId::Custom || configured_profiles.contains(&profile) {
                continue;
            }
            let definition = profile.definition();
            providers.push(json!({
                "id": definition.config_key,
                "name": definition.display_name,
                "adapter": format!("{:?}", definition.adapter).to_lowercase(),
                "base_url": definition.native_base_url,
                "billing_mode": "free",
                "api_key_secret": null,
                "api_key_source": "none",
                "account_scope": null,
                "model_count": 0,
                "free_model_count": 0,
                "model_allowlist_count": 0,
                "model_denylist_count": 0,
                "available": false,
            }));
        }
    }

    Json(json!({
        "object": "list",
        "data": providers,
    }))
    .into_response()
}

pub async fn run_server(
    config: Config,
    secrets: &SecretResolver,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind: std::net::SocketAddr = config.server.bind.parse()?;
    let shutdown_grace = Duration::from_secs(config.server.shutdown_grace_seconds);
    let state_path = config.server.state_path.clone();
    let benchmark_max_age = config.server.benchmark_max_age_seconds;
    let aa_api_key = secrets.get("ARTIFICIAL_ANALYSIS_API_KEY")?;
    let app = build_app(config, secrets)?;

    // Background benchmark auto-refresh
    tokio::spawn(async move {
        auto_refresh_benchmarks(state_path, benchmark_max_age, aa_api_key).await;
    });

    let listener = tokio::net::TcpListener::bind(bind).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = shutdown_rx.await;
    });
    let mut task = tokio::spawn(server.into_future());
    tokio::select! {
        result = &mut task => {
            result??;
        }
        _ = shutdown_signal() => {
            let _ = shutdown_tx.send(());
            if tokio::time::timeout(shutdown_grace, &mut task).await.is_err() {
                task.abort();
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn health_live() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn health_ready(State(state): State<AppState>) -> impl IntoResponse {
    match state.routing.catalog_summary() {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "ready"}))).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "not_ready"})),
        )
            .into_response(),
    }
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let mut ids = vec!["local".to_owned()];
    if state.config.server.auto_free_enabled {
        ids.push("auto-free".to_owned());
    }
    if state.config.server.auto_efficient_enabled {
        ids.push("auto-efficient".to_owned());
    }
    if state.config.server.auto_balanced_enabled {
        ids.push("auto-balanced".to_owned());
    }
    if state.config.server.auto_frontier_enabled {
        ids.push("auto-frontier".to_owned());
    }

    let catalog_offerings = routing_operation(state.routing.clone(), {
        let max_age = state.config.server.catalog_max_age_seconds;
        move |routing| routing.all_candidates(max_age)
    })
    .await;

    let mut catalog_paid_models: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    if let Ok(ref offerings) = catalog_offerings {
        for offering in offerings {
            if let Some(config) = state.config.providers.get(&offering.provider) {
                if effective_access_kind(config, offering).is_free() {
                    continue;
                }
                if matches!(
                    config.billing_mode,
                    BillingMode::Paid | BillingMode::Subscription
                ) && !is_provider_auto_route(&offering.model)
                {
                    catalog_paid_models.insert((offering.provider.clone(), offering.model.clone()));
                    catalog_paid_models.insert((
                        offering.provider.clone(),
                        format!("{}/{}", offering.provider, offering.model),
                    ));
                }
            }
        }
    }

    for (alias_name, model_config) in &state.config.models {
        if matches!(
            alias_name.as_str(),
            "local" | "auto-free" | "auto-efficient" | "auto-balanced" | "auto-frontier"
        ) || is_provider_auto_route(alias_name)
        {
            continue;
        }
        let Some(target) = model_config.targets.first() else {
            continue;
        };
        let Some(provider_config) = state.config.providers.get(&target.provider) else {
            continue;
        };
        if provider_config.billing_mode == BillingMode::Free && provider_config.profile.is_some() {
            continue;
        }
        if catalog_paid_models.contains(&(target.provider.clone(), target.model.clone())) {
            continue;
        }
        ids.push(alias_name.clone());
    }

    if let Ok(offerings) = catalog_offerings {
        let model_denylist = &state.config.server.model_denylist;
        let alias_names: std::collections::HashSet<String> =
            state.config.models.keys().cloned().collect();
        for offering in &offerings {
            let Some(config) = state.config.providers.get(&offering.provider) else {
                continue;
            };
            if effective_access_kind(config, offering).is_free() {
                continue;
            }
            if !matches!(
                config.billing_mode,
                BillingMode::Paid | BillingMode::Subscription
            ) {
                continue;
            }
            if is_provider_auto_route(&offering.model) {
                continue;
            }
            let model_id = format!("{}/{}", offering.provider, offering.model);
            if alias_names.contains(&model_id) {
                continue;
            }
            if model_denylist
                .iter()
                .any(|d| d == &model_id || d == &offering.model)
            {
                continue;
            }
            ids.push(model_id);
        }
    }
    let data = ids
        .into_iter()
        .map(|id| json!({"id": id, "object": "model", "owned_by": "model-gateway"}))
        .collect::<Vec<_>>();
    Json(json!({"object": "list", "data": data}))
}

fn is_model_denied(model: &str, provider: &str, server: &ServerConfig) -> bool {
    let full_id = format!("{provider}/{model}");
    server
        .model_denylist
        .iter()
        .any(|d| d == model || d == &full_id)
}

fn is_provider_auto_route(model: &str) -> bool {
    model.starts_with("kilo-auto/")
        || matches!(
            model,
            "openrouter/auto"
                | "openrouter/auto-beta"
                | "openrouter/free"
                | "orcarouter/auto"
                | "orcarouter/free"
        )
}

#[derive(Debug, Deserialize)]
struct RankingQuery {
    task: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AutoModelsQuery {
    route: Option<String>,
    view: Option<ModelView>,
}

#[derive(Clone, Copy)]
struct ModelResponseContext<'a> {
    view: ModelView,
    origin: &'a str,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CatalogAccess {
    Free,
    Paid,
    All,
}

#[derive(Debug, Deserialize)]
struct CatalogModelsQuery {
    access: Option<CatalogAccess>,
    task: Option<String>,
    provider: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    view: Option<ModelView>,
    variants: Option<CatalogVariants>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CatalogVariants {
    #[default]
    Collapsed,
    All,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ModelView {
    #[default]
    Summary,
    Full,
}

impl ModelView {
    fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

async fn list_rankings(
    State(state): State<AppState>,
    Query(query): Query<RankingQuery>,
) -> Response {
    let task = match query.task.as_deref().unwrap_or("general") {
        "general" => TaskKind::General,
        "coding" => TaskKind::Coding,
        "agentic" => TaskKind::Agentic,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "task must be one of general, coding, agentic",
                        "type": "invalid_request_error",
                        "code": "invalid_task"
                    }
                })),
            )
                .into_response();
        }
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 1_000);
    let models = match state
        .routing
        .benchmark_models(state.config.server.benchmark_max_age_seconds)
    {
        Ok(models) => models,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "benchmark rankings are unavailable",
                        "type": "server_error",
                        "code": "benchmark_state_unavailable"
                    }
                })),
            )
                .into_response();
        }
    };
    let snapshots = state
        .routing
        .benchmark_status()
        .unwrap_or_default()
        .into_iter()
        .map(|(source, fetched_at, models, attribution)| {
            json!({
                "source": source,
                "fetched_at": fetched_at,
                "models": models,
                "attribution": attribution
            })
        })
        .collect::<Vec<_>>();
    let data = rank_benchmark_models(models, task, limit);
    Json(json!({
        "object": "benchmark.rankings",
        "task": task.as_str(),
        "max_age_seconds": state.config.server.benchmark_max_age_seconds,
        "snapshots": snapshots,
        "data": data
    }))
    .into_response()
}

fn rank_benchmark_models(models: Vec<BenchmarkModel>, task: TaskKind, limit: usize) -> Vec<Value> {
    let mut models = models
        .into_iter()
        .filter_map(|model| {
            let quality = quality_for(&model, task)?;
            Some((quality, model))
        })
        .collect::<Vec<_>>();
    models.sort_by(|(left_quality, left), (right_quality, right)| {
        right_quality
            .total_cmp(left_quality)
            .then_with(|| {
                let left_cost = left.input_price_per_million.unwrap_or(f64::MAX)
                    + left.output_price_per_million.unwrap_or(f64::MAX);
                let right_cost = right.input_price_per_million.unwrap_or(f64::MAX)
                    + right.output_price_per_million.unwrap_or(f64::MAX);
                left_cost.total_cmp(&right_cost)
            })
            .then_with(|| left.id.cmp(&right.id))
    });
    models
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(index, (_quality, model))| {
            json!({
                "rank": index + 1,
                "id": model.id,
                "creator": model.creator,
                "scores": {
                    "intelligence": model.intelligence,
                    "coding": model.coding_quality,
                    "agentic": model.agentic_quality
                },
                "input_price_per_million": model.input_price_per_million,
                "output_price_per_million": model.output_price_per_million,
                "cache_read_price_per_million": model.cache_read_price_per_million,
                "cache_write_price_per_million": model.cache_write_price_per_million,
                "cost_per_task_usd": model.cost_per_task_usd,
                "latency_seconds": model.latency_seconds,
                "time_to_first_answer_seconds": model.time_to_first_answer_seconds,
                "end_to_end_response_seconds": model.end_to_end_response_seconds,
                "output_tokens_per_second": model.output_tokens_per_second,
                "reasoning_effort": model.reasoning_effort,
                "as_of": model.as_of,
                "release_date": model.release_date
            })
        })
        .collect()
}

struct CatalogModelEntry<'a> {
    offering: &'a CatalogOffering,
    benchmark: Option<&'a BenchmarkModel>,
    price: Option<EffectivePrice>,
    composite_quality: Option<f64>,
    rank: usize,
    effort_level: Option<&'a str>,
    parameters: Option<String>,
    match_kind: Option<ModelMatchKind>,
    account_limit: Option<AccountLimitSnapshot>,
}

fn catalog_model_json(entry: &CatalogModelEntry, origin: &str) -> Value {
    let has_zero_effective_price = entry.offering.access_kind.has_zero_effective_price();
    let model_id = model_resource_id(entry.offering);
    let reference_input = entry
        .offering
        .input_price_per_million
        .or_else(|| entry.benchmark.and_then(|b| b.input_price_per_million));
    let reference_output = entry
        .offering
        .output_price_per_million
        .or_else(|| entry.benchmark.and_then(|b| b.output_price_per_million));
    let reference_source = if entry.offering.input_price_per_million.is_some()
        || entry.offering.output_price_per_million.is_some()
    {
        Some("provider_catalog")
    } else if reference_input.is_some() || reference_output.is_some() {
        Some("benchmark")
    } else {
        None
    };
    let input_price = if has_zero_effective_price {
        Some(0.0)
    } else {
        entry
            .price
            .as_ref()
            .map(|price| price.input_price_per_million)
            .or(entry.offering.input_price_per_million)
            .or_else(|| entry.benchmark.and_then(|b| b.input_price_per_million))
    };
    let output_price = if has_zero_effective_price {
        Some(0.0)
    } else {
        entry
            .price
            .as_ref()
            .map(|price| price.output_price_per_million)
            .or(entry.offering.output_price_per_million)
            .or_else(|| entry.benchmark.and_then(|b| b.output_price_per_million))
    };
    json!({
        "id": model_id,
        "object": "model",
        "links": {
            "self": model_detail_path(entry.offering, origin),
        },
        "model": {
            "name": entry.offering.model,
            "provider": entry.offering.provider,
            "effort_level": entry.effort_level,
            "parameters": entry.parameters,
        },
        "composite": {
            "quality": entry.composite_quality,
            "rank": entry.rank,
        },
        "scores": {
            "general": entry.benchmark.and_then(|b| b.intelligence),
            "coding": entry.benchmark.and_then(|b| b.coding_quality),
            "agentic": entry.benchmark.and_then(|b| b.agentic_quality),
        },
        "capabilities": catalog_capabilities_json(entry.offering),
        "price_per_million": {
            "input": input_price,
            "output": output_price,
            "cache_read": entry
                .price
                .as_ref()
                .and_then(|price| price.cache_read_price_per_million)
                .or_else(|| {
                    entry
                        .benchmark
                        .and_then(|benchmark| benchmark.cache_read_price_per_million)
                }),
            "cache_write": entry
                .price
                .as_ref()
                .and_then(|price| price.cache_write_price_per_million)
                .or_else(|| {
                    entry
                        .benchmark
                        .and_then(|benchmark| benchmark.cache_write_price_per_million)
                }),
            "source": if has_zero_effective_price {
                Some(match entry.offering.access_kind {
                    AccessKind::ZeroPrice => "provider_free",
                    AccessKind::QuotaLimitedFreeTier => "free_tier",
                    AccessKind::SubscriptionIncluded => "subscription",
                    AccessKind::Paid | AccessKind::Unknown => "unknown",
                })
            } else {
                entry.price.as_ref().map(|price| price.source.as_str())
            },
            "estimated": if has_zero_effective_price {
                Some(false)
            } else {
                entry.price.as_ref().map(|price| price.estimated)
            },
            "pricing_eligible": input_price.is_some() && output_price.is_some(),
        },
        "reference_price_per_million": {
            "input": reference_input,
            "output": reference_output,
            "source": reference_source,
        },
        "access": {
            "kind": entry.offering.access_kind,
            "overage": match entry.offering.access_kind {
                AccessKind::ZeroPrice | AccessKind::QuotaLimitedFreeTier => "gateway_blocked",
                AccessKind::SubscriptionIncluded => "subscription_limited",
                AccessKind::Paid | AccessKind::Unknown => "paid",
            },
            "remaining": entry.account_limit.and_then(|account| account.remaining),
            "is_free_tier": entry.account_limit.and_then(|account| account.is_free_tier),
            "account_status_fetched_at": entry.account_limit.map(|account| account.fetched_at),
        },
        "benchmark_match": entry.match_kind.map(ModelMatchKind::as_str),
        "benchmark_id": entry.benchmark.map(|benchmark| benchmark.id.clone()),
        "benchmarks": benchmark_metrics_json(entry.benchmark),
    })
}

fn model_resource_id(offering: &CatalogOffering) -> String {
    format!("{}/{}", offering.provider, offering.model)
}

fn model_detail_path(offering: &CatalogOffering, origin: &str) -> String {
    catalog_model_link(offering, origin)
}

fn catalog_model_summary_json(entry: &CatalogModelEntry, origin: &str) -> Value {
    json!({
        "id": model_resource_id(entry.offering),
        "links": {
            "self": model_detail_path(entry.offering, origin),
        },
        "quality": {
            "score": entry.composite_quality,
            "rank": entry.rank,
        },
        "reasoning_effort": entry.effort_level,
        "benchmarks": benchmark_metrics_json(entry.benchmark),
    })
}

fn benchmark_metrics_json(benchmark: Option<&BenchmarkModel>) -> Value {
    json!({
        "cost_per_task_usd": benchmark.and_then(|b| b.cost_per_task_usd),
        "latency_seconds": benchmark.and_then(|b| b.latency_seconds),
        "time_to_first_answer_seconds": benchmark
            .and_then(|b| b.time_to_first_answer_seconds),
        "end_to_end_response_seconds": benchmark
            .and_then(|b| b.end_to_end_response_seconds),
        "output_tokens_per_second": benchmark
            .and_then(|b| b.output_tokens_per_second),
    })
}

fn catalog_capabilities_json(offering: &CatalogOffering) -> Option<Value> {
    let mut capabilities = serde_json::Map::new();
    if let Some(context_length) = offering.context_length {
        capabilities.insert("context_length".to_owned(), json!(context_length));
    }
    if let Some(supports_tools) = offering.supports_tools {
        capabilities.insert("supports_tools".to_owned(), json!(supports_tools));
    }
    if let Some(supports_vision) = offering.supports_vision {
        capabilities.insert("supports_vision".to_owned(), json!(supports_vision));
    }
    if let Some(supports_structured_output) = offering.supports_structured_output {
        capabilities.insert(
            "supports_structured_output".to_owned(),
            json!(supports_structured_output),
        );
    }
    (!capabilities.is_empty()).then_some(Value::Object(capabilities))
}

async fn load_paid_candidates(
    state: &AppState,
    provider_filter: Option<&str>,
) -> Result<Vec<ModelCandidate>, ()> {
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;
    let (offerings, benchmarks) = tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        })
    )
    .map_err(|_| ())?;

    let mut benchmark_map = BTreeMap::new();
    for benchmark in benchmarks {
        benchmark_map
            .entry(benchmark.id.clone())
            .or_insert_with(Vec::new)
            .push(benchmark);
    }
    let mappings = identity_mapping_indexes(&state.routing);
    Ok(collect_paid_candidates(
        &offerings,
        &benchmark_map,
        PaidCandidateContext {
            providers: &state.config.providers,
            runtimes: &state.providers,
            cfg: &state.config.server,
            provider_filter,
            routing: &state.routing,
            pricing_max_age_seconds: state.config.server.pricing_max_age_seconds,
            mappings: &mappings,
        },
    ))
}

async fn load_free_candidates(
    state: &AppState,
    provider_filter: Option<&str>,
) -> Result<(Vec<ModelCandidate>, BTreeMap<String, AccountLimitSnapshot>), ()> {
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;
    let (offerings, benchmarks, account_limits) = tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.account_limits()
        })
    )
    .map_err(|_| ())?;

    let mut benchmark_map = BTreeMap::new();
    for benchmark in benchmarks {
        benchmark_map
            .entry(benchmark.id.clone())
            .or_insert_with(Vec::new)
            .push(benchmark);
    }
    let mappings = identity_mapping_indexes(&state.routing);
    let candidates = collect_free_candidates(
        &offerings,
        &benchmark_map,
        FreeCandidateContext {
            providers: &state.config.providers,
            runtimes: &state.providers,
            cfg: &state.config.server,
            provider_filter,
            mappings: &mappings,
            account_limits: &account_limits,
        },
    );
    Ok((candidates, account_limits))
}

#[derive(Debug)]
struct CatalogSnapshot {
    candidates: Vec<ModelCandidate>,
    account_limits: BTreeMap<String, AccountLimitSnapshot>,
    token: String,
    last_modified: i64,
}

fn catalog_snapshot(
    mut candidates: Vec<ModelCandidate>,
    account_limits: BTreeMap<String, AccountLimitSnapshot>,
    access: CatalogAccess,
    task: TaskKind,
    include_variants: bool,
) -> CatalogSnapshot {
    candidates.sort_by(|left, right| {
        let left_quality = left.benchmark.as_ref().and_then(|b| quality_for(b, task));
        let right_quality = right.benchmark.as_ref().and_then(|b| quality_for(b, task));
        right_quality
            .is_some()
            .cmp(&left_quality.is_some())
            .then_with(|| match (left_quality, right_quality) {
                (Some(left), Some(right)) => right.total_cmp(&left),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| left.offering.provider.cmp(&right.offering.provider))
            .then_with(|| left.offering.model.cmp(&right.offering.model))
    });
    if !include_variants {
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| {
            seen.insert((
                candidate.offering.provider.clone(),
                normalize_identifier(&candidate.offering.model),
            ))
        });
    }

    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}:{:?}:{include_variants}", access, task));
    hasher.update(format!("{:?}", account_limits).as_bytes());
    let mut last_modified = 0;
    for candidate in &candidates {
        hasher.update(format!("{:?}", candidate).as_bytes());
        last_modified = last_modified.max(candidate.offering.refreshed_at);
        if let Some(price) = candidate.price.as_ref() {
            last_modified = last_modified.max(price.fetched_at.unwrap_or_default());
        }
    }
    let token = digest_hex(hasher.finalize());
    for account in account_limits.values() {
        last_modified = last_modified.max(account.fetched_at);
    }
    CatalogSnapshot {
        candidates,
        account_limits,
        token,
        last_modified,
    }
}

fn digest_hex(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn catalog_access_name(access: CatalogAccess) -> &'static str {
    match access {
        CatalogAccess::Free => "free",
        CatalogAccess::Paid => "paid",
        CatalogAccess::All => "all",
    }
}

fn encode_uri_component(value: &str, allow_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (allow_slash && byte == b'/')
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn catalog_model_link(offering: &CatalogOffering, origin: &str) -> String {
    catalog_model_link_parts(&offering.provider, &offering.model, origin)
}

fn catalog_model_link_parts(provider: &str, model: &str, origin: &str) -> String {
    format!(
        "{}/v1/catalog/models/{}/{}",
        origin,
        encode_uri_component(provider, false),
        encode_uri_component(model, false)
    )
}

fn public_origin(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

fn catalog_links(
    query: &CatalogModelsQuery,
    access: CatalogAccess,
    snapshot: &str,
    offset: usize,
    limit: usize,
    total: usize,
    origin: &str,
) -> Value {
    let query_url = |cursor: Option<String>| {
        let mut params = vec![format!("access={}", catalog_access_name(access))];
        if let Some(task) = query.task.as_deref() {
            params.push(format!("task={}", encode_uri_component(task, false)));
        }
        if let Some(provider) = query.provider.as_deref() {
            params.push(format!(
                "provider={}",
                encode_uri_component(provider, false)
            ));
        }
        params.push(format!("limit={limit}"));
        if let Some(view) = query.view {
            params.push(format!(
                "view={}",
                if view.is_full() { "full" } else { "summary" }
            ));
        }
        if matches!(query.variants, Some(CatalogVariants::All)) {
            params.push("variants=all".to_owned());
        }
        if let Some(cursor) = cursor {
            params.push(format!("cursor={}", encode_uri_component(&cursor, false)));
        }
        format!("{origin}/v1/catalog/models?{}", params.join("&"))
    };
    let mut links = serde_json::Map::from_iter([(
        "self".to_owned(),
        Value::String(query_url(query.cursor.clone())),
    )]);
    if offset > 0 {
        let previous = offset.saturating_sub(limit);
        links.insert(
            "prev".to_owned(),
            Value::String(query_url(Some(format!("{snapshot}:{previous}")))),
        );
    }
    if offset.saturating_add(limit) < total {
        let next = offset.saturating_add(limit);
        links.insert(
            "next".to_owned(),
            Value::String(query_url(Some(format!("{snapshot}:{next}")))),
        );
    }
    Value::Object(links)
}

fn cached_json_response(value: Value, request_headers: &HeaderMap, last_modified: i64) -> Response {
    let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let etag = format!("\"{}\"", digest_hex(Sha256::digest(&body)));
    let modified_at = if last_modified > 0 {
        UNIX_EPOCH + Duration::from_secs(last_modified as u64)
    } else {
        SystemTime::now()
    };
    let last_modified = httpdate::fmt_http_date(modified_at);
    let cache_control = "private, max-age=30, must-revalidate";
    let not_modified = if let Some(if_none_match) = request_headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
    {
        if_none_match
            .split(',')
            .any(|candidate| candidate.trim() == etag)
    } else {
        request_headers
            .get("if-modified-since")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| httpdate::parse_http_date(value).ok())
            .and_then(|since| {
                httpdate::parse_http_date(&last_modified)
                    .ok()
                    .map(|modified| modified <= since)
            })
            .unwrap_or(false)
    };
    if not_modified {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("etag", etag)
            .header("last-modified", last_modified)
            .header("cache-control", cache_control)
            .body(Body::empty())
            .expect("valid cache response");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header("etag", etag)
        .header("last-modified", last_modified)
        .header("cache-control", cache_control)
        .body(Body::from(body))
        .expect("valid JSON response")
}

fn parse_catalog_task(task: Option<&str>) -> Result<TaskKind, Box<Response>> {
    match task.unwrap_or("general") {
        "general" => Ok(TaskKind::General),
        "coding" => Ok(TaskKind::Coding),
        "agentic" => Ok(TaskKind::Agentic),
        _ => Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "task must be one of general, coding, agentic",
                        "type": "invalid_request_error",
                        "code": "invalid_task"
                    }
                })),
            )
                .into_response(),
        )),
    }
}

fn catalog_query_error(rejection: QueryRejection) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "message": rejection.to_string(),
                "type": "invalid_request_error",
                "code": "invalid_query"
            }
        })),
    )
        .into_response()
}

async fn load_catalog_snapshot(
    state: &AppState,
    access: CatalogAccess,
    provider_filter: Option<&str>,
    task: TaskKind,
    include_variants: bool,
) -> Result<CatalogSnapshot, ()> {
    let (mut candidates, account_limits) = match access {
        CatalogAccess::Free => load_free_candidates(state, provider_filter).await?,
        CatalogAccess::Paid => (
            load_paid_candidates(state, provider_filter).await?,
            BTreeMap::new(),
        ),
        CatalogAccess::All => {
            let paid = load_paid_candidates(state, provider_filter).await?;
            let (mut free, account_limits) = load_free_candidates(state, provider_filter).await?;
            let mut candidates = paid;
            candidates.append(&mut free);
            (candidates, account_limits)
        }
    };
    if matches!(access, CatalogAccess::Paid) {
        candidates.retain(|candidate| candidate.offering.access_kind.is_paid_route_eligible());
    }
    Ok(catalog_snapshot(
        candidates,
        account_limits,
        access,
        task,
        include_variants,
    ))
}

fn catalog_model_response(
    candidate: &ModelCandidate,
    rank: usize,
    account_limit: Option<AccountLimitSnapshot>,
    view: ModelView,
    origin: &str,
) -> Value {
    let composite_quality = candidate.benchmark.as_ref().and_then(composite_quality);
    let effort_level = candidate
        .benchmark
        .as_ref()
        .and_then(|benchmark| benchmark.reasoning_effort.as_deref());
    let entry = CatalogModelEntry {
        offering: &candidate.offering,
        benchmark: candidate.benchmark.as_ref(),
        price: candidate.price.clone(),
        composite_quality,
        rank,
        effort_level,
        parameters: None,
        match_kind: candidate.match_kind,
        account_limit,
    };
    if view.is_full() {
        catalog_model_json(&entry, origin)
    } else {
        catalog_model_summary_json(&entry, origin)
    }
}

#[derive(Debug)]
struct ModelCandidate {
    quality: Option<f64>,
    benchmark: Option<BenchmarkModel>,
    price: Option<EffectivePrice>,
    offering: CatalogOffering,
    match_kind: Option<ModelMatchKind>,
}

type ApprovedMappingIndex = BTreeMap<(String, String), String>;
type EntityReferenceIndex = BTreeMap<(String, String), String>;

struct IdentityMappingIndexes {
    approved: ApprovedMappingIndex,
    references: EntityReferenceIndex,
    conflicts: BTreeMap<(String, String), Vec<String>>,
}

struct CanonicalMatch {
    benchmark_model: String,
    kind: ModelMatchKind,
    source: &'static str,
}

fn identity_provider_key(provider: &ProviderConfig) -> Option<&str> {
    provider.pricing_profile.as_deref().or_else(|| {
        provider
            .profile
            .and_then(|profile| profile.models_dev_key())
    })
}

fn identity_mapping_indexes(routing: &RoutingStore) -> IdentityMappingIndexes {
    let approved = routing
        .approved_model_mappings()
        .unwrap_or_default()
        .into_iter()
        .map(|mapping| {
            (
                (mapping.provider, mapping.catalog_model),
                mapping.benchmark_model,
            )
        })
        .collect();
    let mut reference_candidates = BTreeMap::<(String, String), Option<String>>::new();
    let mut conflicts = BTreeMap::<(String, String), Vec<String>>::new();
    for (provider_key, provider_model_id, benchmark_id) in
        routing.approved_identity_references().unwrap_or_default()
    {
        let key = (provider_key, provider_model_id);
        let entry = reference_candidates
            .entry(key.clone())
            .or_insert_with(|| Some(benchmark_id.clone()));
        if entry.as_deref() != Some(&benchmark_id) {
            let alternatives = conflicts.entry(key).or_default();
            if let Some(existing) = entry.as_ref() {
                alternatives.push(existing.clone());
            }
            alternatives.push(benchmark_id);
            alternatives.sort();
            alternatives.dedup();
            *entry = None;
        }
    }
    let references = reference_candidates
        .into_iter()
        .filter_map(|(key, benchmark_id)| Some((key, benchmark_id?)))
        .collect();
    IdentityMappingIndexes {
        approved,
        references,
        conflicts,
    }
}

fn canonical_match(
    provider: &ProviderConfig,
    provider_name: &str,
    catalog_model: &str,
    mappings: &IdentityMappingIndexes,
) -> CanonicalMatch {
    if let Some(benchmark_model) = provider.model_mappings.get(catalog_model) {
        return CanonicalMatch {
            benchmark_model: benchmark_model.clone(),
            kind: ModelMatchKind::Configured,
            source: "config",
        };
    }
    if let Some(benchmark_model) = mappings
        .approved
        .get(&(provider_name.to_owned(), catalog_model.to_owned()))
    {
        return CanonicalMatch {
            benchmark_model: benchmark_model.clone(),
            kind: ModelMatchKind::Approved,
            source: "registry",
        };
    }
    if let Some(benchmark_model) = identity_provider_key(provider).and_then(|provider_key| {
        mappings
            .references
            .get(&(provider_key.to_owned(), catalog_model.to_owned()))
    }) {
        let exact = is_exact_runtime_model_identity(catalog_model, benchmark_model);
        return CanonicalMatch {
            benchmark_model: benchmark_model.clone(),
            kind: if exact {
                ModelMatchKind::Exact
            } else {
                ModelMatchKind::Approved
            },
            source: if exact {
                "normalized_exact"
            } else {
                "canonical_entity"
            },
        };
    }
    CanonicalMatch {
        benchmark_model: catalog_model.to_owned(),
        kind: ModelMatchKind::Exact,
        source: "normalized_exact",
    }
}

struct PaidCandidateContext<'a> {
    providers: &'a BTreeMap<String, ProviderConfig>,
    runtimes: &'a BTreeMap<String, ProviderRuntime>,
    cfg: &'a ServerConfig,
    provider_filter: Option<&'a str>,
    routing: &'a RoutingStore,
    pricing_max_age_seconds: u64,
    mappings: &'a IdentityMappingIndexes,
}

struct FreeCandidateContext<'a> {
    providers: &'a BTreeMap<String, ProviderConfig>,
    runtimes: &'a BTreeMap<String, ProviderRuntime>,
    cfg: &'a ServerConfig,
    provider_filter: Option<&'a str>,
    mappings: &'a IdentityMappingIndexes,
    account_limits: &'a BTreeMap<String, AccountLimitSnapshot>,
}

#[derive(Clone, Copy)]
struct ModeModelValue<'a> {
    model: &'a str,
    provider: &'a str,
    price: Option<&'a EffectivePrice>,
    pricing_eligible: bool,
    match_kind: Option<ModelMatchKind>,
    access_kind: AccessKind,
    reference_input_price: Option<f64>,
    reference_output_price: Option<f64>,
    benchmark_cost_per_task_usd: Option<f64>,
    time_to_first_answer_seconds: Option<f64>,
    end_to_end_response_seconds: Option<f64>,
    output_tokens_per_second: Option<f64>,
    reasoning_effort: Option<&'a str>,
}

fn effective_access_kind(provider: &ProviderConfig, offering: &CatalogOffering) -> AccessKind {
    let zero_priced = offering.input_price_per_million == Some(0.0)
        && offering.output_price_per_million == Some(0.0);
    classify_access(provider, &offering.model, zero_priced)
}

fn account_allows_free_access(
    access_kind: AccessKind,
    account: Option<&AccountLimitSnapshot>,
) -> bool {
    if access_kind != AccessKind::QuotaLimitedFreeTier {
        return true;
    }
    account.is_none_or(|account| {
        account.is_free_tier != Some(false)
            && account.remaining.is_none_or(|remaining| remaining > 0.0)
    })
}

fn collect_free_candidates(
    offerings: &[CatalogOffering],
    benchmark_by_model: &BTreeMap<String, Vec<BenchmarkModel>>,
    context: FreeCandidateContext<'_>,
) -> Vec<ModelCandidate> {
    let mut candidates = Vec::new();
    for offering in offerings {
        if context
            .provider_filter
            .is_some_and(|p| p != offering.provider)
        {
            continue;
        }
        let Some(provider) = context.providers.get(&offering.provider) else {
            continue;
        };
        let access_kind = effective_access_kind(provider, offering);
        if !access_kind.is_free()
            || !account_allows_free_access(
                access_kind,
                context.account_limits.get(&offering.provider),
            )
        {
            continue;
        }
        let mut offering = offering.clone();
        offering.access_kind = access_kind;
        let Some(runtime) = context.runtimes.get(&offering.provider) else {
            continue;
        };
        if !runtime.available
            || (!provider.model_allowlist.is_empty()
                && !provider
                    .model_allowlist
                    .iter()
                    .any(|m| m == &offering.model))
            || provider.model_denylist.iter().any(|m| m == &offering.model)
        {
            continue;
        }
        if is_model_denied(&offering.model, &offering.provider, context.cfg) {
            continue;
        }
        let canonical = canonical_match(
            provider,
            &offering.provider,
            &offering.model,
            context.mappings,
        );
        let matching =
            find_exact_matching_benchmarks(benchmark_by_model, &canonical.benchmark_model);
        if matching.is_empty() {
            if context.cfg.free_models_quality.passes(
                None,
                offering.refreshed_at,
                offering.input_price_per_million,
                offering.output_price_per_million,
                offering.context_length,
                &canonical.benchmark_model,
            ) {
                candidates.push(ModelCandidate {
                    quality: None,
                    benchmark: None,
                    price: None,
                    offering: offering.clone(),
                    match_kind: None,
                });
            }
        } else {
            let mut has_quality = false;
            for benchmark in matching {
                let Some(quality) = composite_quality(benchmark) else {
                    continue;
                };
                has_quality = true;
                if !context.cfg.free_models_quality.passes(
                    Some(benchmark),
                    offering.refreshed_at,
                    offering.input_price_per_million,
                    offering.output_price_per_million,
                    offering.context_length,
                    &canonical.benchmark_model,
                ) {
                    continue;
                }
                candidates.push(ModelCandidate {
                    quality: Some(quality),
                    benchmark: Some(benchmark.clone()),
                    price: None,
                    offering: offering.clone(),
                    match_kind: Some(canonical.kind),
                });
            }
            if !has_quality
                && context.cfg.free_models_quality.passes(
                    None,
                    offering.refreshed_at,
                    offering.input_price_per_million,
                    offering.output_price_per_million,
                    offering.context_length,
                    &canonical.benchmark_model,
                )
            {
                candidates.push(ModelCandidate {
                    quality: None,
                    benchmark: None,
                    price: None,
                    offering: offering.clone(),
                    match_kind: None,
                });
            }
        }
    }
    candidates
}

fn collect_paid_candidates(
    offerings: &[CatalogOffering],
    benchmark_by_model: &BTreeMap<String, Vec<BenchmarkModel>>,
    context: PaidCandidateContext<'_>,
) -> Vec<ModelCandidate> {
    let mut candidates = Vec::new();
    for offering in offerings {
        if context
            .provider_filter
            .is_some_and(|p| p != offering.provider)
        {
            continue;
        }
        let Some(provider) = context.providers.get(&offering.provider) else {
            continue;
        };
        let access_kind = effective_access_kind(provider, offering);
        let Some(runtime) = context.runtimes.get(&offering.provider) else {
            continue;
        };
        if !runtime.available
            || !access_kind.is_paid_route_eligible()
            || !matches!(
                provider.billing_mode,
                BillingMode::Paid | BillingMode::Subscription
            )
            || (!provider.model_allowlist.is_empty()
                && !provider
                    .model_allowlist
                    .iter()
                    .any(|m| m == &offering.model))
            || provider.model_denylist.iter().any(|m| m == &offering.model)
        {
            continue;
        }
        let mut offering = offering.clone();
        offering.access_kind = access_kind;
        if is_model_denied(&offering.model, &offering.provider, context.cfg) {
            continue;
        }
        let pricing_mapping = provider.model_mappings.get(&offering.model);
        let canonical = canonical_match(
            provider,
            &offering.provider,
            &offering.model,
            context.mappings,
        );
        let price = context
            .routing
            .effective_price(
                &offering.provider,
                provider.pricing_profile.as_deref().or_else(|| {
                    provider
                        .profile
                        .and_then(|profile| profile.models_dev_key())
                }),
                &offering.model,
                pricing_mapping.map(String::as_str),
                context.pricing_max_age_seconds,
            )
            .ok()
            .flatten();
        let matching =
            find_exact_matching_benchmarks(benchmark_by_model, &canonical.benchmark_model);
        if matching.is_empty() {
            candidates.push(ModelCandidate {
                quality: None,
                benchmark: None,
                price: price.clone(),
                offering: offering.clone(),
                match_kind: None,
            });
        } else {
            for benchmark in matching {
                let Some(quality) = composite_quality(benchmark) else {
                    continue;
                };
                let effective_price = price
                    .clone()
                    .or_else(|| benchmark_price_for_model(&canonical.benchmark_model, benchmark));
                candidates.push(ModelCandidate {
                    quality: Some(quality),
                    benchmark: Some(benchmark.clone()),
                    price: effective_price,
                    offering: offering.clone(),
                    match_kind: Some(canonical.kind),
                });
            }
        }
    }
    candidates
}

async fn list_auto_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AutoModelsQuery>,
) -> Response {
    let view = query.view.unwrap_or_default();
    let origin = public_origin(&headers);
    let response_context = ModelResponseContext {
        view,
        origin: &origin,
    };
    let cfg = &state.config.server;
    let benchmark_max_age = cfg.benchmark_max_age_seconds;
    let catalog_max_age = cfg.catalog_max_age_seconds;

    let (free_offerings, paid_offerings, benchmarks, account_limits) = match tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(catalog_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(catalog_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.account_limits()
        }),
    ) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "routing unavailable", "type": "server_error", "code": "routing_unavailable"}})),
            )
                .into_response();
        }
    };

    let mut benchmark_by_model = BTreeMap::<String, Vec<BenchmarkModel>>::new();
    for benchmark in &benchmarks {
        benchmark_by_model
            .entry(benchmark.id.clone())
            .or_default()
            .push(benchmark.clone());
    }
    let mappings = identity_mapping_indexes(&state.routing);

    let free_candidates = collect_free_candidates(
        &free_offerings,
        &benchmark_by_model,
        FreeCandidateContext {
            providers: &state.config.providers,
            runtimes: &state.providers,
            cfg,
            provider_filter: None,
            mappings: &mappings,
            account_limits: &account_limits,
        },
    );
    let paid_candidates = collect_paid_candidates(
        &paid_offerings,
        &benchmark_by_model,
        PaidCandidateContext {
            providers: &state.config.providers,
            runtimes: &state.providers,
            cfg,
            provider_filter: None,
            routing: &state.routing,
            pricing_max_age_seconds: cfg.pricing_max_age_seconds,
            mappings: &mappings,
        },
    );

    let mut routes = BTreeMap::new();

    if query.route.as_deref().is_none_or(|r| r == "free") && cfg.auto_free_enabled {
        routes.insert(
            "free".to_owned(),
            select_mode_models(
                &free_candidates,
                "auto-free",
                "Auto-Free",
                cfg.free_models_quality.min_composite_quality,
                None,
                Some(cfg.free_models_quality.max_quality_regret),
                response_context,
            ),
        );
    }

    if query.route.as_deref().is_none_or(|r| r == "efficient") && cfg.auto_efficient_enabled {
        routes.insert(
            "efficient".to_owned(),
            select_mode_models(
                &paid_candidates,
                "auto-efficient",
                "Auto-Efficient",
                cfg.efficient_quality_floor,
                Some(cfg.balanced_quality_floor),
                None,
                response_context,
            ),
        );
    }

    if query.route.as_deref().is_none_or(|r| r == "balanced") && cfg.auto_balanced_enabled {
        routes.insert(
            "balanced".to_owned(),
            select_mode_models(
                &paid_candidates,
                "auto-balanced",
                "Auto-Balanced",
                cfg.balanced_quality_floor,
                Some(cfg.frontier_quality_floor_single),
                None,
                response_context,
            ),
        );
    }

    if query.route.as_deref().is_none_or(|r| r == "frontier") && cfg.auto_frontier_enabled {
        routes.insert(
            "frontier".to_owned(),
            select_mode_models(
                &paid_candidates,
                "auto-frontier",
                "Auto-Frontier",
                cfg.frontier_quality_floor_single,
                None,
                None,
                response_context,
            ),
        );
    }

    Json(json!({"object": "auto_models", "routes": routes, "view": if view.is_full() { "full" } else { "summary" }})).into_response()
}

fn select_mode_models(
    candidates: &[ModelCandidate],
    mode: &str,
    label: &str,
    quality_floor: f64,
    quality_ceiling: Option<f64>,
    max_quality_regret: Option<f64>,
    response_context: ModelResponseContext<'_>,
) -> Value {
    let mut scored: Vec<ScoredCandidate<ModeModelValue<'_>>> = Vec::new();

    for candidate in candidates {
        let Some(quality) = candidate.quality else {
            continue;
        };
        if quality < quality_floor {
            continue;
        }
        if quality_ceiling.is_some_and(|ceiling| quality >= ceiling) {
            continue;
        }
        let benchmark = candidate.benchmark.as_ref();
        let latency = benchmark
            .and_then(BenchmarkModel::frontier_latency_seconds)
            .unwrap_or(f64::MAX);
        let reference_input_price = candidate
            .offering
            .input_price_per_million
            .or_else(|| benchmark.and_then(|b| b.input_price_per_million));
        let reference_output_price = candidate
            .offering
            .output_price_per_million
            .or_else(|| benchmark.and_then(|b| b.output_price_per_million));
        let token_cost = || match candidate.offering.access_kind {
            AccessKind::ZeroPrice => Some(0),
            AccessKind::QuotaLimitedFreeTier | AccessKind::SubscriptionIncluded => {
                match (reference_input_price, reference_output_price) {
                    (Some(input), Some(output)) => Some(expected_cost_microusd(
                        256,
                        benchmark
                            .and_then(|b| b.output_tokens_per_task)
                            .unwrap_or(256)
                            .min(256),
                        input,
                        output,
                    )),
                    _ => None,
                }
            }
            AccessKind::Paid | AccessKind::Unknown => candidate.price.as_ref().map(|price| {
                expected_cost_microusd(
                    256,
                    benchmark
                        .and_then(|b| b.output_tokens_per_task)
                        .unwrap_or(256)
                        .min(256),
                    price.input_price_per_million,
                    price.output_price_per_million,
                )
            }),
        };
        let expected_cost_microusd = benchmark
            .and_then(BenchmarkModel::cost_per_task_microusd)
            .or_else(token_cost)
            .unwrap_or(u64::MAX);
        scored.push(ScoredCandidate {
            quality,
            expected_cost_microusd,
            latency_seconds: latency,
            value: ModeModelValue {
                model: candidate.offering.model.as_str(),
                provider: candidate.offering.provider.as_str(),
                price: candidate.price.as_ref(),
                pricing_eligible: candidate.offering.access_kind.has_zero_effective_price()
                    || candidate.price.is_some(),
                match_kind: candidate.match_kind,
                access_kind: candidate.offering.access_kind,
                reference_input_price,
                reference_output_price,
                benchmark_cost_per_task_usd: benchmark.and_then(|b| b.cost_per_task_usd),
                time_to_first_answer_seconds: benchmark
                    .and_then(|b| b.time_to_first_answer_seconds),
                end_to_end_response_seconds: benchmark.and_then(|b| b.end_to_end_response_seconds),
                output_tokens_per_second: benchmark.and_then(|b| b.output_tokens_per_second),
                reasoning_effort: benchmark.and_then(|b| b.reasoning_effort.as_deref()),
            },
        });
    }

    if let Some(max_regret) = max_quality_regret {
        if let Some(best_quality) = scored
            .iter()
            .map(|candidate| candidate.quality)
            .reduce(f64::max)
        {
            scored.retain(|candidate| best_quality - candidate.quality <= max_regret);
        }
    }
    let eligible = scored;
    let mut ranked = pareto_rank(eligible.clone());
    let rank_order = |a: &ScoredCandidate<ModeModelValue<'_>>,
                      b: &ScoredCandidate<ModeModelValue<'_>>| {
        a.expected_cost_microusd
            .cmp(&b.expected_cost_microusd)
            .then_with(|| a.latency_seconds.total_cmp(&b.latency_seconds))
            .then_with(|| b.quality.total_cmp(&a.quality))
    };
    ranked.sort_by(rank_order);
    let selected = ranked
        .iter()
        .map(|candidate| (candidate.value.provider, candidate.value.model))
        .collect::<BTreeSet<_>>();
    let mut dominated = eligible
        .into_iter()
        .filter(|candidate| !selected.contains(&(candidate.value.provider, candidate.value.model)))
        .collect::<Vec<_>>();
    dominated.sort_by(rank_order);
    ranked.extend(dominated);

    let mut iter = ranked.into_iter();
    let primary = iter.next();
    let fallbacks: Vec<Value> = iter
        .take(2)
        .map(|f| mode_model_entry(&f, response_context))
        .collect();

    let primary_entry = primary.map(|p| mode_model_entry(&p, response_context));

    json!({
        "label": label,
        "enabled": true,
        "mode": mode,
        "quality_floor": quality_floor,
        "max_quality_regret": max_quality_regret,
        "primary": primary_entry,
        "fallbacks": fallbacks,
    })
}

fn mode_model_entry(
    candidate: &ScoredCandidate<ModeModelValue<'_>>,
    response_context: ModelResponseContext<'_>,
) -> Value {
    let id = format!("{}/{}", candidate.value.provider, candidate.value.model);
    let link = catalog_model_link_parts(
        candidate.value.provider,
        candidate.value.model,
        response_context.origin,
    );
    if !response_context.view.is_full() {
        return json!({
            "id": id,
            "links": {"self": link},
            "quality": {
                "score": candidate.quality,
            },
            "reasoning_effort": candidate.value.reasoning_effort,
        });
    }
    let entry = json!({
        "id": id,
        "model": candidate.value.model,
        "provider": candidate.value.provider,
        "links": {
            "self": link,
        },
        "reasoning_effort": candidate.value.reasoning_effort,
        "quality": candidate.quality,
        "expected_cost_microusd": if candidate.value.access_kind.has_zero_effective_price() {
            0
        } else {
            candidate.expected_cost_microusd
        },
        "reference_cost_microusd": if candidate.value.access_kind.uses_reference_cost() {
            (candidate.expected_cost_microusd != u64::MAX).then_some(candidate.expected_cost_microusd)
        } else {
            None
        },
        "latency_seconds": candidate.latency_seconds,
        "benchmark_cost_per_task_usd": candidate.value.benchmark_cost_per_task_usd,
        "time_to_first_answer_seconds": candidate.value.time_to_first_answer_seconds,
        "end_to_end_response_seconds": candidate.value.end_to_end_response_seconds,
        "output_tokens_per_second": candidate.value.output_tokens_per_second,
        "pricing_eligible": candidate.value.pricing_eligible,
        "benchmark_match": candidate.value.match_kind.map(ModelMatchKind::as_str),
        "access": {
            "kind": candidate.value.access_kind,
            "overage": match candidate.value.access_kind {
                AccessKind::ZeroPrice | AccessKind::QuotaLimitedFreeTier => "gateway_blocked",
                AccessKind::SubscriptionIncluded => "subscription_limited",
                AccessKind::Paid | AccessKind::Unknown => "paid",
            },
        },
        "reference_price_per_million": {
            "input": candidate.value.reference_input_price,
            "output": candidate.value.reference_output_price,
        },
        "price_per_million": if candidate.value.access_kind.has_zero_effective_price() {
            Some(json!({
                "input": 0.0,
                "output": 0.0,
                "source": match candidate.value.access_kind {
                    AccessKind::ZeroPrice => "provider_free",
                    AccessKind::QuotaLimitedFreeTier => "free_tier",
                    AccessKind::SubscriptionIncluded => "subscription",
                    AccessKind::Paid | AccessKind::Unknown => "unknown",
                },
                "estimated": false,
            }))
        } else {
            candidate.value.price.map(|price| json!({
                "input": price.input_price_per_million,
                "output": price.output_price_per_million,
                "cache_read": price.cache_read_price_per_million,
                "cache_write": price.cache_write_price_per_million,
                "source": price.source,
                "estimated": price.estimated,
            }))
        },
    });
    entry
}

async fn list_catalog_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<CatalogModelsQuery>, QueryRejection>,
) -> Response {
    let origin = public_origin(&headers);
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return catalog_query_error(rejection),
    };
    let access = query.access.unwrap_or(CatalogAccess::All);
    let provider_filter = match query.provider.as_deref() {
        None | Some("all") => None,
        Some(provider) => Some(provider),
    };
    if let Some(provider) = provider_filter {
        if !state.config.providers.contains_key(provider) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": format!("unknown provider '{provider}'"),
                        "type": "invalid_request_error",
                        "code": "invalid_provider"
                    }
                })),
            )
                .into_response();
        }
    }
    let task = match parse_catalog_task(query.task.as_deref()) {
        Ok(task) => task,
        Err(response) => return *response,
    };
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let view = query.view.unwrap_or_default();
    let include_variants = matches!(query.variants, Some(CatalogVariants::All));
    let snapshot = match load_catalog_snapshot(
        &state,
        access,
        provider_filter,
        task,
        include_variants,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(()) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "catalog state is unavailable",
                        "type": "server_error",
                        "code": "catalog_state_unavailable"
                    }
                })),
            )
                .into_response();
        }
    };
    let offset = match query.cursor.as_deref() {
        None => 0,
        Some(cursor) => {
            let Some((token, offset)) = cursor.split_once(':') else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": "cursor is invalid",
                            "type": "invalid_request_error",
                            "code": "invalid_cursor"
                        }
                    })),
                )
                    .into_response();
            };
            if token != snapshot.token {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": {
                            "message": "catalog changed; restart pagination from the first page",
                            "type": "invalid_request_error",
                            "code": "stale_cursor"
                        }
                    })),
                )
                    .into_response();
            }
            match offset.parse::<usize>() {
                Ok(offset) => offset,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "error": {
                                "message": "cursor is invalid",
                                "type": "invalid_request_error",
                                "code": "invalid_cursor"
                            }
                        })),
                    )
                        .into_response();
                }
            }
        }
    };
    let total = snapshot.candidates.len();
    let data = snapshot
        .candidates
        .iter()
        .skip(offset)
        .take(limit)
        .enumerate()
        .map(|(index, candidate)| {
            catalog_model_response(
                candidate,
                offset + index + 1,
                snapshot
                    .account_limits
                    .get(&candidate.offering.provider)
                    .copied(),
                view,
                &origin,
            )
        })
        .collect::<Vec<_>>();
    cached_json_response(
        json!({
            "object": "model.collection",
            "view": if view.is_full() { "full" } else { "summary" },
            "access": catalog_access_name(access),
            "task": task.as_str(),
            "variants": if include_variants { "all" } else { "collapsed" },
            "meta": {
                "snapshot": snapshot.token,
                "total": total,
                "limit": limit,
                "returned": data.len(),
            },
            "links": catalog_links(
                &query,
                access,
                &snapshot.token,
                offset,
                limit,
                total,
                &origin,
            ),
            "data": data,
        }),
        &headers,
        snapshot.last_modified,
    )
}

async fn get_catalog_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    let Some((provider, model)) = model_id.split_once('/') else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "message": "model ID must use the provider/model form",
                    "type": "invalid_request_error",
                    "code": "invalid_model_id"
                }
            })),
        )
            .into_response();
    };
    if !state.config.providers.contains_key(provider) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("model '{model_id}' was not found"),
                    "type": "invalid_request_error",
                    "code": "model_not_found"
                }
            })),
        )
            .into_response();
    }
    let snapshot = match load_catalog_snapshot(
        &state,
        CatalogAccess::All,
        Some(provider),
        TaskKind::General,
        false,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(()) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "catalog state is unavailable",
                        "type": "server_error",
                        "code": "catalog_state_unavailable"
                    }
                })),
            )
                .into_response();
        }
    };
    let Some((rank, candidate)) = snapshot
        .candidates
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate.offering.model == model)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("model '{model_id}' was not found"),
                    "type": "invalid_request_error",
                    "code": "model_not_found"
                }
            })),
        )
            .into_response();
    };
    let resource = catalog_model_response(
        candidate,
        rank + 1,
        snapshot.account_limits.get(provider).copied(),
        ModelView::Full,
        &public_origin(&headers),
    );
    cached_json_response(
        json!({
            "object": "model",
            "id": model_resource_id(&candidate.offering),
            "links": {"self": catalog_model_link(&candidate.offering, &public_origin(&headers))},
            "data": resource,
            "meta": {"snapshot": snapshot.token},
        }),
        &headers,
        snapshot.last_modified,
    )
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let started_at = Instant::now();
    let request_id = request_id(&headers);
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            log_request(
                &request_id,
                "",
                "",
                StatusCode::PAYLOAD_TOO_LARGE,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                request_id,
                "request body exceeded the configured limit",
                "invalid_request_error",
                Some("body_too_large"),
            );
        }
        Err(_) => {
            log_request(
                &request_id,
                "",
                "",
                StatusCode::BAD_REQUEST,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "request body could not be read",
                "invalid_request_error",
                Some("invalid_body"),
            );
        }
    };
    let request: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            log_request(
                &request_id,
                "",
                "",
                StatusCode::BAD_REQUEST,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "request body must be an object",
                "invalid_request_error",
                Some("invalid_request"),
            );
        }
        Err(_) => {
            log_request(
                &request_id,
                "",
                "",
                StatusCode::BAD_REQUEST,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "invalid JSON body",
                "invalid_request_error",
                Some("invalid_json"),
            );
        }
    };
    let model = match request.get("model").and_then(Value::as_str) {
        Some(model) if !model.is_empty() => model.to_owned(),
        _ => {
            log_request(
                &request_id,
                "",
                "",
                StatusCode::BAD_REQUEST,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "field 'model' is required",
                "invalid_request_error",
                Some("model"),
            );
        }
    };
    let is_stream = match request.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            log_request(
                &request_id,
                &model,
                "",
                StatusCode::BAD_REQUEST,
                started_at,
                false,
                0,
            );
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "field 'stream' must be a boolean",
                "invalid_request_error",
                Some("stream"),
            );
        }
    };
    let session_hash = match session_material(&headers, &request) {
        Some(material) => routing_operation(state.routing.clone(), move |routing| {
            routing.session_hash(&material)
        })
        .await
        .ok(),
        None => None,
    };
    let targets = match resolve_targets(&state, &model, &request, session_hash.as_deref()).await {
        Ok(targets) => targets,
        Err((status, message, code)) => {
            log_request(&request_id, &model, "", status, started_at, is_stream, 0);
            return error_response(
                status,
                request_id,
                &message,
                "invalid_request_error",
                Some(code),
            );
        }
    };
    let global_permit = match timeout(
        Duration::from_millis(state.config.server.admission_timeout_ms),
        state.global_permits.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            log_request(
                &request_id,
                &model,
                "",
                StatusCode::TOO_MANY_REQUESTS,
                started_at,
                is_stream,
                0,
            );
            return admission_error(
                request_id,
                "gateway is at capacity",
                state.config.server.admission_timeout_ms,
            );
        }
    };
    let mut attempts = 0usize;
    let mut last_error = None;
    let mut targets = targets;
    let mut target_index = 0;
    while target_index < targets.len() {
        let target = targets[target_index].clone();
        target_index += 1;
        let estimated_tokens = estimate_request_tokens(&request);
        let mut reservation = None;
        if target.managed {
            let provider = target.quota_scope.clone();
            let upstream_model = target.model.clone();
            let quotas = target.quotas.clone();
            match routing_operation(state.routing.clone(), move |routing| {
                routing.reserve(
                    &provider,
                    &upstream_model,
                    estimated_tokens,
                    target.expected_cost_microusd,
                    &quotas,
                )
            })
            .await
            {
                Ok(ReservationOutcome::Reserved(token)) => reservation = Some(token),
                Ok(ReservationOutcome::Cooldown) | Ok(ReservationOutcome::QuotaExceeded(_)) => {
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        event = "routing_state_error",
                        provider = target.provider,
                        error = %error
                    );
                    continue;
                }
            }
        }
        attempts += 1;
        let mut target_request = request.clone();
        let Some(provider) = state.providers.get(&target.runtime_provider) else {
            release_reservation(&state, reservation, ReservationRelease::BeforeDispatch).await;
            last_error = Some((
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                Bytes::new(),
                target.provider.clone(),
            ));
            continue;
        };
        if !provider.available {
            release_reservation(&state, reservation, ReservationRelease::BeforeDispatch).await;
            invalidate_session_pin(&state.routing, session_hash.as_deref(), &model).await;
            if target_index >= targets.len() {
                drop(global_permit);
                log_request(
                    &request_id,
                    &model,
                    &target.provider,
                    StatusCode::SERVICE_UNAVAILABLE,
                    started_at,
                    is_stream,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    request_id,
                    "configured provider credential is unavailable",
                    &model,
                    &target.provider,
                    attempts,
                );
            }
            last_error = Some((
                StatusCode::SERVICE_UNAVAILABLE,
                HeaderMap::new(),
                Bytes::new(),
                target.provider.clone(),
            ));
            continue;
        }
        if target_request.get("reasoning_effort").is_none() {
            if let Some(effort) = &target.reasoning_effort {
                target_request["reasoning_effort"] = Value::String(effort.clone());
            }
        }
        if prepare_request(provider.config.adapter, &mut target_request, &target.model).is_err() {
            release_reservation(&state, reservation, ReservationRelease::BeforeDispatch).await;
            drop(global_permit);
            log_request(
                &request_id,
                &model,
                &target.provider,
                StatusCode::INTERNAL_SERVER_ERROR,
                started_at,
                is_stream,
                attempts.saturating_sub(1),
            );
            return selected_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                request_id,
                "provider adapter could not prepare the request",
                &model,
                &target.provider,
                attempts,
            );
        }
        let provider_permit = match timeout(
            Duration::from_millis(state.config.server.admission_timeout_ms),
            provider.permits.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
                release_reservation(&state, reservation, ReservationRelease::BeforeDispatch).await;
                log_request(
                    &request_id,
                    &model,
                    &target.provider,
                    StatusCode::TOO_MANY_REQUESTS,
                    started_at,
                    is_stream,
                    attempts.saturating_sub(1),
                );
                return admission_error(
                    request_id,
                    "provider is at capacity",
                    state.config.server.admission_timeout_ms,
                );
            }
        };
        let url = format!(
            "{}/chat/completions",
            provider.config.base_url.trim_end_matches('/')
        );
        let mut upstream = provider.client.post(url).json(&target_request);
        if let Some(api_key) = &provider.api_key {
            upstream = upstream.bearer_auth(api_key);
        }
        for (name, value) in &provider.config.extra_headers {
            upstream = upstream.header(name, value);
        }
        upstream = upstream.header("x-request-id", request_id.clone());
        let response = match timeout(
            Duration::from_secs(provider.config.response_header_timeout_seconds),
            upstream.send(),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                drop(provider_permit);
                log_request(
                    &request_id,
                    &model,
                    &target.provider,
                    StatusCode::BAD_GATEWAY,
                    started_at,
                    is_stream,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::BAD_GATEWAY,
                    request_id,
                    "upstream request failed",
                    &model,
                    &target.provider,
                    attempts,
                );
            }
            Err(_) => {
                drop(provider_permit);
                log_request(
                    &request_id,
                    &model,
                    &target.provider,
                    StatusCode::GATEWAY_TIMEOUT,
                    started_at,
                    is_stream,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    request_id,
                    "upstream response headers timed out",
                    &model,
                    &target.provider,
                    attempts,
                );
            }
        };
        let status = response.status();
        let response_headers = response.headers().clone();
        if status.is_success() {
            return relay_response(
                response,
                status,
                response_headers,
                StreamContext {
                    request_id,
                    alias: model,
                    provider: target.provider.clone(),
                    model_metadata: ModelMetadata::from_target(&target, &request),
                    attempts,
                    idle_timeout_seconds: provider.config.stream_idle_timeout_seconds,
                    is_stream,
                    started_at,
                    global_permit,
                    provider_permit,
                    reservation,
                    session_hash: session_hash.clone(),
                    input_price_per_million: target.input_price_per_million,
                    output_price_per_million: target.output_price_per_million,
                    routing: state.routing.clone(),
                },
            )
            .await;
        }
        let response_body = match read_bounded(
            response,
            Duration::from_secs(provider.config.stream_idle_timeout_seconds),
        )
        .await
        {
            Ok(body) => body,
            Err(_) if is_fallback_status(status) => Bytes::new(),
            Err(_) => {
                drop(provider_permit);
                release_reservation(&state, reservation, ReservationRelease::KnownFailure).await;
                log_request(
                    &request_id,
                    &model,
                    &target.provider,
                    StatusCode::BAD_GATEWAY,
                    started_at,
                    is_stream,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::BAD_GATEWAY,
                    request_id,
                    "upstream response body failed",
                    &model,
                    &target.provider,
                    attempts,
                );
            }
        };
        drop(provider_permit);
        release_reservation(&state, reservation, ReservationRelease::KnownFailure).await;
        if target.managed
            && matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            )
        {
            let retry_after = response_headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let retry_after = retry_after
                .or_else(|| rate_limit_reset_delay(&response_headers))
                .or_else(|| {
                    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                        .then_some(300)
                });
            let provider = target.provider.clone();
            let upstream_model = target.model.clone();
            let _ = routing_operation(state.routing.clone(), move |routing| {
                routing.apply_cooldown(&provider, &upstream_model, retry_after)
            })
            .await;
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                invalidate_session_pin(&state.routing, session_hash.as_deref(), &model).await;
            }
        }
        if target.managed
            && response_headers
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim() == "0")
        {
            if let Some(delay) = rate_limit_reset_delay(&response_headers) {
                let provider = target.provider.clone();
                let upstream_model = target.model.clone();
                let _ = routing_operation(state.routing.clone(), move |routing| {
                    routing.apply_cooldown(&provider, &upstream_model, Some(delay))
                })
                .await;
            }
        }
        if target.runtime_provider == LOCAL_RUNTIME_PROVIDER
            && status == StatusCode::NOT_FOUND
            && state.config.server.local_model.is_none()
            && attempts == 1
        {
            *state.local_model.lock().await = None;
            if let Ok(model) = resolve_local_model(&state).await {
                targets.push(SelectedTarget {
                    model,
                    ..target.clone()
                });
            }
        }
        if !is_fallback_status(status) {
            log_request(
                &request_id,
                &model,
                &target.provider,
                status,
                started_at,
                is_stream,
                attempts.saturating_sub(1),
            );
            return upstream_error_response(
                status,
                response_headers,
                response_body,
                request_id,
                &model,
                &target.provider,
                attempts,
            );
        }
        tracing::warn!(
            request_id = %request_id,
            alias = %model,
            provider = %target.provider,
            attempt = attempts,
            status = status.as_u16(),
            "upstream fallback"
        );
        last_error = Some((
            status,
            response_headers,
            response_body,
            target.provider.clone(),
        ));
    }
    let response = match last_error {
        Some((status, headers, body, provider)) if !body.is_empty() => upstream_error_response(
            status, headers, body, request_id, &model, &provider, attempts,
        ),
        Some((status, _, _, provider)) => selected_error_response(
            status,
            request_id,
            "upstream provider returned an error",
            &model,
            &provider,
            attempts,
        ),
        None => error_response(
            StatusCode::BAD_GATEWAY,
            request_id,
            "no route was available",
            "upstream_error",
            None,
        ),
    };
    log_request(
        &request_id_from_response(&response),
        &model,
        response
            .headers()
            .get("x-model-gateway-provider")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
        response.status(),
        started_at,
        is_stream,
        attempts.saturating_sub(1),
    );
    response
}

#[derive(Clone)]
struct SelectedTarget {
    runtime_provider: String,
    provider: String,
    quota_scope: String,
    provider_display: String,
    model: String,
    managed: bool,
    quotas: Vec<crate::config::QuotaLimit>,
    expected_cost_microusd: u64,
    input_price_per_million: Option<f64>,
    output_price_per_million: Option<f64>,
    reasoning_effort: Option<String>,
    selection: Option<SelectionMetadata>,
}

#[derive(Clone)]
struct SelectionMetadata {
    canonical_model: String,
    task: &'static str,
    complexity: &'static str,
    classifier_version: &'static str,
    quality_floor: f64,
    quality: f64,
    expected_cost_microusd: u64,
    benchmark_snapshot_id: i64,
    benchmark_as_of: i64,
    match_kind: Option<ModelMatchKind>,
}

async fn resolve_targets(
    state: &AppState,
    model: &str,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    if model == "local" {
        let local_model = resolve_local_model(state).await?;
        return Ok(vec![SelectedTarget {
            runtime_provider: LOCAL_RUNTIME_PROVIDER.to_owned(),
            provider: "local".to_owned(),
            quota_scope: "local".to_owned(),
            provider_display: "Local".to_owned(),
            model: local_model,
            managed: false,
            quotas: Vec::new(),
            expected_cost_microusd: 0,
            input_price_per_million: None,
            output_price_per_million: None,
            reasoning_effort: None,
            selection: None,
        }]);
    }
    if model == "auto-free" {
        if !state.config.server.auto_free_enabled {
            return Err((
                StatusCode::NOT_FOUND,
                "model 'auto-free' is disabled".to_owned(),
                "route_disabled",
            ));
        }
        return resolve_auto_free_targets(state, request, session_hash).await;
    }
    if model == "auto-efficient" {
        if !state.config.server.auto_efficient_enabled {
            return Err((
                StatusCode::NOT_FOUND,
                "model 'auto-efficient' is disabled".to_owned(),
                "route_disabled",
            ));
        }
        return resolve_auto_efficient_targets(state, request, session_hash).await;
    }
    if model == "auto-balanced" {
        if !state.config.server.auto_balanced_enabled {
            return Err((
                StatusCode::NOT_FOUND,
                "model 'auto-balanced' is disabled".to_owned(),
                "route_disabled",
            ));
        }
        return resolve_auto_balanced_targets(state, request, session_hash).await;
    }
    if model == "auto-frontier" {
        if !state.config.server.auto_frontier_enabled {
            return Err((
                StatusCode::NOT_FOUND,
                "model 'auto-frontier' is disabled".to_owned(),
                "route_disabled",
            ));
        }
        return resolve_benchmark_targets(state, request, session_hash, BenchmarkPolicy::Frontier)
            .await;
    }
    if let Some(config) = state.config.models.get(model) {
        return Ok(config
            .targets
            .iter()
            .map(|target| selected_target(state, target))
            .collect());
    }
    if let Some((provider_name, upstream_model)) = model.split_once('/') {
        let provider = state.config.providers.get(provider_name);
        let is_allowed = provider.is_some_and(|p| {
            p.allow_model_passthrough
                || matches!(
                    p.billing_mode,
                    BillingMode::Paid | BillingMode::Subscription
                )
        });
        if is_allowed {
            return Ok(vec![selected_target(
                state,
                &TargetConfig {
                    provider: provider_name.to_owned(),
                    model: upstream_model.to_owned(),
                },
            )]);
        }
    }
    Err((
        StatusCode::NOT_FOUND,
        format!("model '{model}' is not configured"),
        "model_not_found",
    ))
}

fn selected_target(state: &AppState, target: &TargetConfig) -> SelectedTarget {
    let provider_display = state
        .config
        .providers
        .get(&target.provider)
        .and_then(|provider| provider.profile)
        .map(|profile| profile.definition().display_name.to_owned())
        .unwrap_or_else(|| target.provider.clone());
    let price = state
        .config
        .providers
        .get(&target.provider)
        .and_then(|provider| {
            state
                .routing
                .effective_price(
                    &target.provider,
                    provider.pricing_profile.as_deref().or_else(|| {
                        provider
                            .profile
                            .and_then(|profile| profile.models_dev_key())
                    }),
                    &target.model,
                    provider
                        .model_mappings
                        .get(&target.model)
                        .map(String::as_str),
                    state.config.server.pricing_max_age_seconds,
                )
                .ok()
                .flatten()
        });
    SelectedTarget {
        runtime_provider: target.provider.clone(),
        provider: target.provider.clone(),
        quota_scope: target.provider.clone(),
        provider_display,
        model: target.model.clone(),
        managed: false,
        quotas: Vec::new(),
        expected_cost_microusd: 0,
        input_price_per_million: price.as_ref().map(|price| price.input_price_per_million),
        output_price_per_million: price.as_ref().map(|price| price.output_price_per_million),
        reasoning_effort: None,
        selection: None,
    }
}

struct FreeCandidate {
    target: SelectedTarget,
    quality: Option<f64>,
    latency_seconds: f64,
    reference_cost_microusd: u64,
}

async fn resolve_auto_free_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;
    let (offerings, benchmarks, benchmark_snapshot, account_limits) = tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.active_benchmark_snapshot(benchmark_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.account_limits()
        })
    )
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "routing state is unavailable".to_owned(),
            "routing_state_unavailable",
        )
    })?;
    let (benchmark_snapshot_id, benchmark_as_of) = benchmark_snapshot.unwrap_or((0, 0));
    let mut benchmark_map = BTreeMap::new();
    for b in &benchmarks {
        benchmark_map
            .entry(b.id.clone())
            .or_insert_with(Vec::new)
            .push(b.clone());
    }
    let mappings = identity_mapping_indexes(&state.routing);
    let classification = classify(request);
    let requirements = RequestRequirements::from_request(request);
    let candidates = offerings
        .into_iter()
        .filter_map(|mut offering| {
            let provider = state.config.providers.get(&offering.provider)?;
            let access_kind = effective_access_kind(provider, &offering);
            if !access_kind.is_free()
                || !account_allows_free_access(access_kind, account_limits.get(&offering.provider))
            {
                return None;
            }
            offering.access_kind = access_kind;
            let runtime = state.providers.get(&offering.provider)?;
            if !runtime.available
                || (!provider.model_allowlist.is_empty()
                    && !provider
                        .model_allowlist
                        .iter()
                        .any(|model| model == &offering.model))
                || provider
                    .model_denylist
                    .iter()
                    .any(|model| model == &offering.model)
            {
                return None;
            }
            if offering
                .context_length
                .is_some_and(|context| context < requirements.estimated_tokens)
                || requirements.tools && offering.supports_tools == Some(false)
                || requirements.vision && offering.supports_vision == Some(false)
                || requirements.structured && offering.supports_structured_output == Some(false)
            {
                return None;
            }
            if is_model_denied(&offering.model, &offering.provider, &state.config.server) {
                return None;
            }
            let reference = quota_reference(provider, &offering.model);
            let canonical =
                canonical_match(provider, &offering.provider, &offering.model, &mappings);
            let benchmark = find_benchmark(&benchmark_map, &canonical.benchmark_model);
            let quality = benchmark.and_then(composite_quality);
            let effective_input = offering
                .input_price_per_million
                .or_else(|| benchmark.and_then(|b| b.input_price_per_million));
            let effective_output = offering
                .output_price_per_million
                .or_else(|| benchmark.and_then(|b| b.output_price_per_million));
            if !state.config.server.free_models_quality.passes(
                benchmark,
                offering.refreshed_at,
                effective_input,
                effective_output,
                offering.context_length,
                &canonical.benchmark_model,
            ) {
                return None;
            }
            let latency = benchmark
                .and_then(BenchmarkModel::frontier_latency_seconds)
                .unwrap_or(f64::MAX);
            let reference_cost_microusd = if access_kind == AccessKind::QuotaLimitedFreeTier {
                benchmark
                    .and_then(BenchmarkModel::cost_per_task_microusd)
                    .or_else(|| match (effective_input, effective_output) {
                        (Some(input), Some(output)) => Some(expected_cost_microusd(
                            requirements.estimated_input_tokens,
                            benchmark
                                .and_then(|b| b.output_tokens_per_task)
                                .unwrap_or(requirements.estimated_output_tokens)
                                .min(requirements.estimated_output_tokens),
                            input,
                            output,
                        )),
                        _ => None,
                    })
                    .unwrap_or(u64::MAX)
            } else {
                0
            };
            Some(FreeCandidate {
                quality,
                latency_seconds: latency,
                reference_cost_microusd,
                target: SelectedTarget {
                    runtime_provider: offering.provider.clone(),
                    provider: offering.provider.clone(),
                    quota_scope: provider
                        .account_scope
                        .clone()
                        .unwrap_or_else(|| offering.provider.clone()),
                    provider_display: provider
                        .profile
                        .map(|profile| profile.definition().display_name.to_owned())
                        .unwrap_or_else(|| "Custom OpenAI-compatible".to_owned()),
                    model: offering.model,
                    managed: true,
                    quotas: reference
                        .map(|reference| reference.rules)
                        .unwrap_or_default(),
                    expected_cost_microusd: 0,
                    input_price_per_million: offering.input_price_per_million,
                    output_price_per_million: offering.output_price_per_million,
                    reasoning_effort: None,
                    selection: Some(SelectionMetadata {
                        canonical_model: canonical.benchmark_model,
                        task: classification.task.as_str(),
                        complexity: classification.complexity.as_str(),
                        classifier_version: classification.version,
                        quality_floor: 0.0,
                        quality: quality.unwrap_or(0.0),
                        expected_cost_microusd: 0,
                        benchmark_snapshot_id,
                        benchmark_as_of,
                        match_kind: benchmark.map(|_| canonical.kind),
                    }),
                },
            })
        })
        .collect::<Vec<_>>();
    let pinned = match session_hash {
        Some(session_hash) => {
            let session_hash = session_hash.to_owned();
            routing_operation(state.routing.clone(), move |routing| {
                routing.session_pin(&session_hash, "auto-free")
            })
            .await
            .ok()
            .flatten()
        }
        None => None,
    };
    let mut scored = Vec::new();
    let mut unbenchmarked = Vec::new();
    for c in candidates {
        match c.quality {
            Some(quality) => scored.push(ScoredCandidate {
                quality,
                expected_cost_microusd: c.reference_cost_microusd,
                latency_seconds: c.latency_seconds,
                value: c,
            }),
            None => unbenchmarked.push(c),
        }
    }
    if let Some(best_quality) = scored
        .iter()
        .map(|candidate| candidate.quality)
        .reduce(f64::max)
    {
        let max_regret = state.config.server.free_models_quality.max_quality_regret;
        scored.retain(|candidate| best_quality - candidate.quality <= max_regret);
    }
    let mut ranked = pareto_rank(scored);
    ranked.sort_by(|left, right| {
        let left_pinned = pinned.as_ref().is_some_and(|pin| {
            pin.0 == left.value.target.provider && pin.1 == left.value.target.model
        });
        let right_pinned = pinned.as_ref().is_some_and(|pin| {
            pin.0 == right.value.target.provider && pin.1 == right.value.target.model
        });
        right_pinned
            .cmp(&left_pinned)
            .then_with(|| {
                left.expected_cost_microusd
                    .cmp(&right.expected_cost_microusd)
            })
            .then_with(|| left.latency_seconds.total_cmp(&right.latency_seconds))
            .then_with(|| right.quality.total_cmp(&left.quality))
    });
    let mut targets: Vec<SelectedTarget> = ranked.into_iter().map(|c| c.value.target).collect();
    targets.extend(unbenchmarked.into_iter().map(|c| c.target));
    match resolve_local_model(state).await {
        Ok(model) => targets.push(SelectedTarget {
            runtime_provider: LOCAL_RUNTIME_PROVIDER.to_owned(),
            provider: "local".to_owned(),
            quota_scope: "local".to_owned(),
            provider_display: "Local".to_owned(),
            model,
            managed: false,
            quotas: Vec::new(),
            expected_cost_microusd: 0,
            input_price_per_million: None,
            output_price_per_million: None,
            reasoning_effort: None,
            selection: None,
        }),
        Err(error) if targets.is_empty() => return Err(error),
        Err(_) => {}
    }
    Ok(targets)
}

async fn resolve_auto_efficient_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let mut targets =
        resolve_benchmark_targets(state, request, session_hash, BenchmarkPolicy::Efficient).await?;
    let selected = targets
        .iter()
        .map(|target| (target.provider.clone(), target.model.clone()))
        .collect::<BTreeSet<_>>();
    if !state.config.server.auto_free_enabled {
        return Ok(targets);
    }
    match resolve_auto_free_targets(state, request, session_hash).await {
        Ok(fallbacks) => {
            for target in fallbacks {
                if !selected.contains(&(target.provider.clone(), target.model.clone())) {
                    targets.push(target);
                }
            }
        }
        Err(error) if targets.is_empty() => return Err(error),
        Err(_) => {}
    }
    Ok(targets)
}

async fn resolve_auto_balanced_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let mut targets =
        resolve_benchmark_targets(state, request, session_hash, BenchmarkPolicy::Balanced).await?;
    let selected = targets
        .iter()
        .map(|target| (target.provider.clone(), target.model.clone()))
        .collect::<BTreeSet<_>>();
    if !state.config.server.auto_free_enabled {
        return Ok(targets);
    }
    match resolve_auto_free_targets(state, request, session_hash).await {
        Ok(fallbacks) => {
            for target in fallbacks {
                if !selected.contains(&(target.provider.clone(), target.model.clone())) {
                    targets.push(target);
                }
            }
        }
        Err(error) if targets.is_empty() => return Err(error),
        Err(_) => {}
    }
    Ok(targets)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BenchmarkPolicy {
    Efficient,
    Balanced,
    Frontier,
}

impl BenchmarkPolicy {
    const fn route(self) -> &'static str {
        match self {
            Self::Efficient => "auto-efficient",
            Self::Balanced => "auto-balanced",
            Self::Frontier => "auto-frontier",
        }
    }
}

async fn resolve_benchmark_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
    policy: BenchmarkPolicy,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let catalog_age = state.config.server.catalog_max_age_seconds;
    let benchmark_age = state.config.server.benchmark_max_age_seconds;
    let (offerings, benchmarks, benchmark_snapshot) = tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(catalog_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.active_benchmark_snapshot(benchmark_age)
        })
    )
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "routing state is unavailable".to_owned(),
            "routing_state_unavailable",
        )
    })?;
    let classification = classify(request);
    let quality_floor = match policy {
        BenchmarkPolicy::Efficient => state.config.server.efficient_quality_floor,
        BenchmarkPolicy::Balanced => state.config.server.balanced_quality_floor,
        BenchmarkPolicy::Frontier => state.config.server.frontier_quality_floor_single,
    };
    let requirements = RequestRequirements::from_request(request);
    let requested_effort = request
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .filter(|effort| is_reasoning_effort(effort));
    let mut benchmark_by_model = BTreeMap::<String, Vec<_>>::new();
    let (benchmark_snapshot_id, benchmark_as_of) = benchmark_snapshot.unwrap_or((0, 0));
    for benchmark in benchmarks {
        benchmark_by_model
            .entry(benchmark.id.clone())
            .or_default()
            .push(benchmark);
    }
    let mappings = identity_mapping_indexes(&state.routing);
    let mut candidates = Vec::new();
    for offering in offerings {
        let Some(provider) = state.config.providers.get(&offering.provider) else {
            continue;
        };
        let Some(runtime) = state.providers.get(&offering.provider) else {
            continue;
        };
        if (!provider.model_allowlist.is_empty()
            && !provider
                .model_allowlist
                .iter()
                .any(|model| model == &offering.model))
            || provider
                .model_denylist
                .iter()
                .any(|model| model == &offering.model)
        {
            continue;
        }
        let pricing_mapping = provider.model_mappings.get(&offering.model);
        let canonical = canonical_match(provider, &offering.provider, &offering.model, &mappings);
        let model_benchmarks = benchmarks_for_effort(
            find_exact_matching_benchmarks(&benchmark_by_model, &canonical.benchmark_model),
            requested_effort,
        );
        if model_benchmarks.is_empty() {
            continue;
        }
        let access_kind = effective_access_kind(provider, &offering);
        if !runtime.available
            || !access_kind.is_paid_route_eligible()
            || provider.billing_mode == BillingMode::Free
        {
            continue;
        }
        let capability_mismatch = offering
            .context_length
            .is_some_and(|context| context < requirements.estimated_tokens)
            || (requirements.tools && offering.supports_tools != Some(true))
            || (requirements.vision && offering.supports_vision != Some(true))
            || (requirements.structured && offering.supports_structured_output != Some(true));
        if capability_mismatch {
            continue;
        }
        if is_model_denied(&offering.model, &offering.provider, &state.config.server) {
            continue;
        }
        let effective_price = state
            .routing
            .effective_price(
                &offering.provider,
                provider.pricing_profile.as_deref().or_else(|| {
                    provider
                        .profile
                        .and_then(|profile| profile.models_dev_key())
                }),
                &offering.model,
                pricing_mapping.map(String::as_str),
                state.config.server.pricing_max_age_seconds,
            )
            .ok()
            .flatten();
        for benchmark in model_benchmarks {
            let Some(raw_quality) = composite_quality(benchmark) else {
                continue;
            };
            let quality = raw_quality;
            if quality < quality_floor {
                continue;
            }
            let Some(effective_price) = effective_price
                .clone()
                .or_else(|| benchmark_price_for_model(&canonical.benchmark_model, benchmark))
            else {
                continue;
            };
            let token_cost_microusd = expected_cost_microusd(
                requirements.estimated_input_tokens,
                benchmark
                    .output_tokens_per_task
                    .unwrap_or(requirements.estimated_output_tokens)
                    .min(requirements.estimated_output_tokens),
                effective_price.input_price_per_million,
                effective_price.output_price_per_million,
            );
            let reference_cost_microusd = benchmark
                .cost_per_task_microusd()
                .unwrap_or(token_cost_microusd);
            let expected_cost_microusd = if access_kind == AccessKind::SubscriptionIncluded {
                0
            } else {
                reference_cost_microusd
            };
            let reference = quota_reference(provider, &offering.model);
            candidates.push(ScoredCandidate {
                value: SelectedTarget {
                    runtime_provider: offering.provider.clone(),
                    provider: offering.provider.clone(),
                    quota_scope: provider
                        .account_scope
                        .clone()
                        .unwrap_or_else(|| offering.provider.clone()),
                    provider_display: provider
                        .profile
                        .map(|profile| profile.definition().display_name.to_owned())
                        .unwrap_or_else(|| "Custom OpenAI-compatible".to_owned()),
                    model: offering.model.clone(),
                    managed: true,
                    quotas: reference
                        .map(|reference| reference.rules)
                        .unwrap_or_default(),
                    expected_cost_microusd,
                    input_price_per_million: Some(
                        if access_kind == AccessKind::SubscriptionIncluded {
                            0.0
                        } else {
                            effective_price.input_price_per_million
                        },
                    ),
                    output_price_per_million: Some(
                        if access_kind == AccessKind::SubscriptionIncluded {
                            0.0
                        } else {
                            effective_price.output_price_per_million
                        },
                    ),
                    reasoning_effort: benchmark.reasoning_effort.clone(),
                    selection: Some(SelectionMetadata {
                        canonical_model: canonical.benchmark_model.clone(),
                        task: classification.task.as_str(),
                        complexity: classification.complexity.as_str(),
                        classifier_version: classification.version,
                        quality_floor,
                        quality,
                        expected_cost_microusd,
                        benchmark_snapshot_id,
                        benchmark_as_of,
                        match_kind: Some(canonical.kind),
                    }),
                },
                quality,
                expected_cost_microusd: reference_cost_microusd,
                latency_seconds: benchmark.frontier_latency_seconds().unwrap_or(f64::MAX),
            });
        }
    }
    let pinned = match session_hash {
        Some(session_hash) => {
            let session_hash = session_hash.to_owned();
            let route = policy.route();
            routing_operation(state.routing.clone(), move |routing| {
                routing.session_pin(&session_hash, route)
            })
            .await
            .ok()
            .flatten()
        }
        None => None,
    };
    let eligible = candidates;
    let mut ranked = pareto_rank(eligible.clone());
    let rank_order = |left: &ScoredCandidate<SelectedTarget>,
                      right: &ScoredCandidate<SelectedTarget>| {
        let left_pinned = pinned
            .as_ref()
            .is_some_and(|pin| pin.0 == left.value.provider && pin.1 == left.value.model);
        let right_pinned = pinned
            .as_ref()
            .is_some_and(|pin| pin.0 == right.value.provider && pin.1 == right.value.model);
        left.expected_cost_microusd
            .cmp(&right.expected_cost_microusd)
            .then_with(|| right_pinned.cmp(&left_pinned))
            .then_with(|| {
                (&left.value.provider, &left.value.model)
                    .cmp(&(&right.value.provider, &right.value.model))
            })
    };
    ranked.sort_by(rank_order);
    let selected = ranked
        .iter()
        .map(|candidate| {
            (
                candidate.value.provider.as_str(),
                candidate.value.model.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut dominated = eligible
        .into_iter()
        .filter(|candidate| {
            !selected.contains(&(
                candidate.value.provider.as_str(),
                candidate.value.model.as_str(),
            ))
        })
        .collect::<Vec<_>>();
    dominated.sort_by(rank_order);
    ranked.extend(dominated);
    Ok(ranked
        .into_iter()
        .map(|candidate| candidate.value)
        .collect())
}

fn is_reasoning_effort(effort: &str) -> bool {
    matches!(
        effort.to_ascii_lowercase().as_str(),
        "low" | "medium" | "high" | "xhigh" | "max"
    )
}

fn benchmark_price(benchmark: &BenchmarkModel) -> Option<EffectivePrice> {
    if benchmark.input_price_per_million == Some(0.0)
        && benchmark.output_price_per_million == Some(0.0)
    {
        return None;
    }
    Some(EffectivePrice {
        input_price_per_million: benchmark.input_price_per_million?,
        output_price_per_million: benchmark.output_price_per_million?,
        cache_read_price_per_million: benchmark.cache_read_price_per_million,
        cache_write_price_per_million: benchmark.cache_write_price_per_million,
        source: "benchmark".to_owned(),
        source_kind: PriceSourceKind::Benchmark,
        scope: PriceScope::Canonical,
        provider_key: None,
        model_id: benchmark.id.clone(),
        fetched_at: None,
        valid_from: None,
        valid_until: None,
        estimated: true,
    })
}

fn benchmark_price_for_model(model: &str, benchmark: &BenchmarkModel) -> Option<EffectivePrice> {
    (normalize_price_id(model) == normalize_price_id(&benchmark.id))
        .then(|| benchmark_price(benchmark))?
}

fn expected_cost_microusd(
    input_tokens: u64,
    output_tokens: u64,
    input_price_per_million: f64,
    output_price_per_million: f64,
) -> u64 {
    let cost = (input_tokens as f64 * input_price_per_million)
        + (output_tokens as f64 * output_price_per_million);
    if !cost.is_finite() || cost >= u64::MAX as f64 {
        u64::MAX
    } else {
        cost.ceil().max(0.0) as u64
    }
}

fn rate_limit_reset_delay(headers: &HeaderMap) -> Option<u64> {
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())?
        .parse::<u64>()
        .ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(if reset > now {
        reset.saturating_sub(now).min(86_400)
    } else {
        reset.min(86_400)
    })
}

fn estimate_request_tokens(request: &Value) -> u64 {
    let input_bytes = request
        .get("messages")
        .and_then(|messages| serde_json::to_vec(messages).ok())
        .map_or(0, |messages| messages.len());
    let input_tokens = u64::try_from(input_bytes.div_ceil(4)).unwrap_or(u64::MAX);
    let output_tokens = request
        .get("max_completion_tokens")
        .or_else(|| request.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(1_024);
    input_tokens.saturating_add(output_tokens)
}

async fn routing_operation<T, F>(
    routing: Arc<RoutingStore>,
    operation: F,
) -> Result<T, RoutingError>
where
    T: Send + 'static,
    F: FnOnce(Arc<RoutingStore>) -> Result<T, RoutingError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(routing))
        .await
        .map_err(|error| RoutingError::Background(error.to_string()))?
}

async fn release_reservation(
    state: &AppState,
    reservation: Option<ReservationToken>,
    release: ReservationRelease,
) {
    let Some(reservation) = reservation else {
        return;
    };
    let _ = routing_operation(state.routing.clone(), move |routing| {
        routing.release_reservation(reservation, release)
    })
    .await;
}

async fn invalidate_session_pin(
    routing: &Arc<RoutingStore>,
    session_hash: Option<&str>,
    route: &str,
) {
    let Some(session_hash) = session_hash else {
        return;
    };
    let routing = routing.clone();
    let session_hash = session_hash.to_owned();
    let route = route.to_owned();
    let _ = routing_operation(routing, move |routing| {
        routing.remove_session_pin(&session_hash, &route)
    })
    .await;
}

struct RequestRequirements {
    estimated_tokens: u64,
    estimated_input_tokens: u64,
    estimated_output_tokens: u64,
    tools: bool,
    vision: bool,
    structured: bool,
}

impl RequestRequirements {
    fn from_request(request: &Value) -> Self {
        let messages = request.get("messages");
        let serialized_messages = messages
            .and_then(|messages| serde_json::to_string(messages).ok())
            .unwrap_or_default();
        let estimated_input_tokens =
            u64::try_from(serialized_messages.len().div_ceil(4)).unwrap_or(u64::MAX);
        let estimated_output_tokens = request
            .get("max_completion_tokens")
            .or_else(|| request.get("max_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(1_024);
        Self {
            estimated_tokens: estimated_input_tokens.saturating_add(estimated_output_tokens),
            estimated_input_tokens,
            estimated_output_tokens,
            tools: request
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty()),
            vision: serialized_messages.contains("image_url")
                || serialized_messages.contains("input_image"),
            structured: request
                .get("response_format")
                .is_some_and(|format| !format.is_null()),
        }
    }
}

fn session_material(headers: &HeaderMap, request: &Value) -> Option<String> {
    if let Some(session_id) = request.get("session_id").and_then(Value::as_str) {
        return (!session_id.is_empty()).then(|| format!("body:{session_id}"));
    }
    if let Some(session_id) = headers
        .get("x-session-id")
        .and_then(|value| value.to_str().ok())
    {
        return (!session_id.is_empty()).then(|| format!("header:{session_id}"));
    }
    let messages = request.get("messages")?.as_array()?;
    let first = messages
        .iter()
        .filter(|message| {
            message
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| matches!(role, "system" | "user"))
        })
        .take(2)
        .collect::<Vec<_>>();
    (!first.is_empty()).then(|| {
        serde_json::to_string(&first).unwrap_or_else(|_| "unserializable-session".to_owned())
    })
}

async fn resolve_local_model(
    state: &AppState,
) -> Result<String, (StatusCode, String, &'static str)> {
    if let Some(model) = &state.config.server.local_model {
        return Ok(model.clone());
    }
    let mut cache = state.local_model.lock().await;
    if let Some(cached) = cache.as_ref() {
        if cached.expires_at > Instant::now() {
            return Ok(cached.model.clone());
        }
    }
    let provider = state
        .providers
        .get(LOCAL_RUNTIME_PROVIDER)
        .expect("local runtime is always built");
    let url = format!("{}/models", provider.config.base_url.trim_end_matches('/'));
    let response = timeout(
        Duration::from_secs(provider.config.response_header_timeout_seconds),
        provider.client.get(url).send(),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "local model discovery timed out".to_owned(),
            "local_model_unavailable",
        )
    })?
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "local model endpoint is unavailable".to_owned(),
            "local_model_unavailable",
        )
    })?;
    if !response.status().is_success() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("local model discovery returned HTTP {}", response.status()),
            "local_model_unavailable",
        ));
    }
    let body = read_bounded(
        response,
        Duration::from_secs(provider.config.stream_idle_timeout_seconds),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "local model catalog could not be read".to_owned(),
            "local_model_unavailable",
        )
    })?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "local model catalog was invalid JSON".to_owned(),
            "local_model_unavailable",
        )
    })?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let model = match models.as_slice() {
        [model] => model.clone(),
        [] => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "local endpoint reported no loaded models; set MODEL_GATEWAY_LOCAL_MODEL"
                    .to_owned(),
                "local_model_unavailable",
            ));
        }
        _ => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "local endpoint reported multiple models; set MODEL_GATEWAY_LOCAL_MODEL".to_owned(),
                "local_model_ambiguous",
            ));
        }
    };
    *cache = Some(CachedLocalModel {
        model: model.clone(),
        expires_at: Instant::now()
            + Duration::from_secs(state.config.server.local_model_cache_seconds),
    });
    Ok(model)
}

async fn read_bounded(
    response: reqwest::Response,
    idle_timeout: Duration,
) -> Result<Bytes, String> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let chunk = match timeout(idle_timeout, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) => return Err("upstream response body failed".to_owned()),
            Ok(None) => break,
            Err(_) => return Err("upstream response body was idle".to_owned()),
        };
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("upstream response exceeded the gateway response limit".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

fn is_fallback_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

struct StreamContext {
    request_id: String,
    alias: String,
    provider: String,
    model_metadata: ModelMetadata,
    attempts: usize,
    idle_timeout_seconds: u64,
    is_stream: bool,
    started_at: Instant,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    provider_permit: tokio::sync::OwnedSemaphorePermit,
    reservation: Option<ReservationToken>,
    session_hash: Option<String>,
    input_price_per_million: Option<f64>,
    output_price_per_million: Option<f64>,
    routing: Arc<RoutingStore>,
}

async fn finalize_reservation(
    routing: &Arc<RoutingStore>,
    reservation: Option<ReservationToken>,
    actual_tokens: Option<u64>,
    actual_cost_microusd: Option<u64>,
) {
    let Some(reservation) = reservation else {
        return;
    };
    let routing = routing.clone();
    let _ = routing_operation(routing, move |routing| {
        routing.finalize_reservation(reservation, actual_tokens, actual_cost_microusd)
    })
    .await;
}

async fn finalize_success(
    routing: &Arc<RoutingStore>,
    session_hash: Option<&str>,
    route: &str,
    provider: &str,
    model: &str,
) {
    let routing = routing.clone();
    let session_hash = session_hash.map(ToOwned::to_owned);
    let route = route.to_owned();
    let provider = provider.to_owned();
    let model = model.to_owned();
    let _ = routing_operation(routing, move |routing| {
        routing.clear_cooldown(&provider, &model)?;
        if let Some(session_hash) = session_hash {
            routing.set_session_pin(&session_hash, &route, &provider, &model, 1_800)?;
        }
        Ok(())
    })
    .await;
}

fn usage_cost(
    usage: Option<(u64, u64)>,
    input_price_per_million: Option<f64>,
    output_price_per_million: Option<f64>,
) -> Option<u64> {
    let (input, output) = usage?;
    Some(expected_cost_microusd(
        input,
        output,
        input_price_per_million?,
        output_price_per_million?,
    ))
}

fn parse_json_usage(body: &[u8]) -> Option<(u64, u64)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    parse_usage_value(&value)
}

fn parse_sse_usage(event: &[u8]) -> Option<(u64, u64)> {
    let text = std::str::from_utf8(event).ok()?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    parse_usage_value(&serde_json::from_str(&payload).ok()?)
}

fn sse_model(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str::<Value>(&payload)
        .ok()?
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn malformed_sse_event(event: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(event) else {
        return event.starts_with(b"data:");
    };
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    !payload.is_empty()
        && payload.trim() != "[DONE]"
        && serde_json::from_str::<Value>(&payload).is_err()
}

fn parse_usage_value(value: &Value) -> Option<(u64, u64)> {
    let usage = value.get("usage")?;
    Some((
        usage.get("prompt_tokens")?.as_u64()?,
        usage.get("completion_tokens")?.as_u64()?,
    ))
}

#[derive(Clone)]
struct ModelMetadata {
    upstream_model: String,
    canonical_model: String,
    family: String,
    display: String,
    reasoning_effort: String,
    provider_display: String,
    selection: Option<SelectionMetadata>,
}

impl ModelMetadata {
    fn from_target(target: &SelectedTarget, request: &Value) -> Self {
        let canonical_model = target
            .selection
            .as_ref()
            .map(|selection| selection.canonical_model.clone())
            .unwrap_or_else(|| target.model.clone());
        let (family, display) = model_name_parts(&canonical_model);
        let effort = request
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or_else(|| {
                request
                    .get("reasoning")
                    .and_then(|reasoning| reasoning.get("effort"))
                    .and_then(Value::as_str)
            })
            .or(target.reasoning_effort.as_deref())
            .map(title_word)
            .unwrap_or_else(|| "Default".to_owned());
        Self {
            upstream_model: target.model.clone(),
            canonical_model,
            family,
            display,
            reasoning_effort: effort,
            provider_display: target.provider_display.clone(),
            selection: target.selection.clone(),
        }
    }

    fn footer(&self) -> String {
        format!(
            "- {}: {} {}, {}",
            self.family, self.display, self.reasoning_effort, self.provider_display
        )
    }

    fn with_served_model(mut self, model: &str) -> Self {
        self.upstream_model = model.to_owned();
        let (family, display) = model_name_parts(model);
        self.family = family;
        self.display = display;
        self
    }
}

fn model_name_parts(model: &str) -> (String, String) {
    let model = model.rsplit('/').next().unwrap_or(model);
    let mut parts = model.split(['-', ':']).filter(|part| !part.is_empty());
    let first = parts.next().unwrap_or("Model");
    let family = match first.to_ascii_lowercase().as_str() {
        "gpt" => "GPT".to_owned(),
        "mtplx" => "MTPLX".to_owned(),
        "glm" => "GLM".to_owned(),
        other
            if other.len() <= 5
                && other
                    .chars()
                    .all(|character| character.is_ascii_alphabetic()) =>
        {
            other.to_ascii_uppercase()
        }
        _ => title_word(first),
    };
    let remainder = parts.map(title_word).collect::<Vec<_>>();
    let display = if remainder.is_empty() {
        title_word(first)
    } else {
        remainder.join(" ")
    };
    (family, display)
}

fn title_word(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let mut characters = lower.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            format!("{}{}", first.to_ascii_uppercase(), characters.as_str())
        }
        Some(_) | None => value.to_owned(),
    }
}

async fn relay_response(
    response: reqwest::Response,
    status: StatusCode,
    upstream_headers: HeaderMap,
    context: StreamContext,
) -> Response {
    let idle_timeout = Duration::from_secs(context.idle_timeout_seconds);
    let StreamContext {
        request_id,
        alias,
        provider,
        mut model_metadata,
        attempts,
        is_stream,
        started_at,
        global_permit,
        provider_permit,
        reservation,
        session_hash,
        input_price_per_million,
        output_price_per_million,
        routing,
        ..
    } = context;
    if !is_stream {
        let body = match read_bounded(response, idle_timeout).await {
            Ok(body) => body,
            Err(_) => {
                finalize_reservation(&routing, reservation, None, None).await;
                drop(provider_permit);
                drop(global_permit);
                log_request(
                    &request_id,
                    &alias,
                    &provider,
                    StatusCode::BAD_GATEWAY,
                    started_at,
                    false,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::BAD_GATEWAY,
                    request_id,
                    "upstream response body failed",
                    &alias,
                    &provider,
                    attempts,
                );
            }
        };
        let usage = parse_json_usage(&body);
        let actual_tokens = usage.map(|(input, output)| input.saturating_add(output));
        let actual_cost_microusd =
            usage_cost(usage, input_price_per_million, output_price_per_million);
        let served_model = response_model(&body)
            .or_else(|| provider_routed_model(&upstream_headers).map(ToOwned::to_owned));
        if let Some(served_model) = served_model.as_deref() {
            model_metadata = model_metadata.with_served_model(served_model);
        }
        let body = match decorate_json_response(&body, &model_metadata.footer()) {
            Ok(body) => body,
            Err(message) => {
                finalize_reservation(&routing, reservation, actual_tokens, actual_cost_microusd)
                    .await;
                drop(provider_permit);
                drop(global_permit);
                log_request(
                    &request_id,
                    &alias,
                    &provider,
                    StatusCode::BAD_GATEWAY,
                    started_at,
                    false,
                    attempts.saturating_sub(1),
                );
                return selected_error_response(
                    StatusCode::BAD_GATEWAY,
                    request_id,
                    message,
                    &alias,
                    &provider,
                    attempts,
                );
            }
        };
        finalize_reservation(&routing, reservation, actual_tokens, actual_cost_microusd).await;
        finalize_success(
            &routing,
            session_hash.as_deref(),
            &alias,
            &provider,
            &model_metadata.upstream_model,
        )
        .await;
        drop(provider_permit);
        drop(global_permit);
        log_request(
            &request_id,
            &alias,
            &provider,
            status,
            started_at,
            false,
            attempts.saturating_sub(1),
        );
        let mut downstream = Response::new(body.into());
        *downstream.status_mut() = status;
        copy_safe_headers(&upstream_headers, downstream.headers_mut());
        add_gateway_headers(
            downstream.headers_mut(),
            request_id,
            &alias,
            &provider,
            attempts.saturating_sub(1),
        );
        add_model_headers(downstream.headers_mut(), &model_metadata);
        if let Some(served_model) = served_model {
            downstream
                .headers_mut()
                .insert("x-model-gateway-served-model", header_value(&served_model));
        }
        return downstream;
    }
    let request_log = RequestLog {
        request_id: request_id.clone(),
        alias: alias.clone(),
        provider: provider.clone(),
        status,
        started_at,
        is_stream,
        fallbacks: attempts.saturating_sub(1),
    };
    let mut upstream = response.bytes_stream();
    let mut footer = model_metadata.footer();
    let stream_alias = alias.clone();
    let stream_provider = provider.clone();
    let stream_model = model_metadata.upstream_model.clone();
    let stream_session_hash = session_hash.clone();
    let mut stream_metadata = model_metadata.clone();
    let stream = async_stream::stream! {
        let mut buffer = Vec::new();
        let mut choices = BTreeMap::new();
        let mut usage: Option<(u64, u64)> = None;
        'stream: loop {
            match timeout(idle_timeout, upstream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some(event) = take_sse_event(&mut buffer) {
                        if malformed_sse_event(&event) {
                            let actual_tokens =
                                usage.map(|(input, output)| input.saturating_add(output));
                            let actual_cost_microusd = usage_cost(
                                usage,
                                input_price_per_million,
                                output_price_per_million,
                            );
                            finalize_reservation(
                                &routing,
                                reservation,
                                actual_tokens,
                                actual_cost_microusd,
                            )
                            .await;
                            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"data: {\"error\":{\"message\":\"upstream returned invalid Chat Completions SSE\",\"type\":\"upstream_error\",\"code\":\"invalid_upstream_stream\"}}\n\n",
                            ));
                            break 'stream;
                        }
                        if let Some(served_model) = sse_model(&event) {
                            stream_metadata = stream_metadata.with_served_model(&served_model);
                            footer = stream_metadata.footer();
                        }
                        if let Some(event_usage) = parse_sse_usage(&event) {
                            usage = Some(event_usage);
                        }
                        for transformed in transform_sse_event(&event, &footer, &mut choices) {
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(transformed));
                        }
                    }
                }
                Ok(Some(Err(error))) => {
                    let actual_tokens =
                        usage.map(|(input, output)| input.saturating_add(output));
                    let actual_cost_microusd = usage_cost(
                        usage,
                        input_price_per_million,
                        output_price_per_million,
                    );
                    finalize_reservation(
                        &routing,
                        reservation,
                        actual_tokens,
                        actual_cost_microusd,
                    )
                    .await;
                    yield Err(std::io::Error::other(error));
                    break;
                }
                Ok(None) => {
                    if !buffer.is_empty() {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(std::mem::take(&mut buffer)));
                    }
                    let actual_tokens = usage.map(|(input, output)| input.saturating_add(output));
                    let actual_cost_microusd = usage_cost(
                        usage,
                        input_price_per_million,
                        output_price_per_million,
                    );
                    finalize_reservation(
                        &routing,
                        reservation,
                        actual_tokens,
                        actual_cost_microusd,
                    )
                    .await;
                    finalize_success(
                        &routing,
                        stream_session_hash.as_deref(),
                        &stream_alias,
                        &stream_provider,
                        &stream_model,
                    )
                    .await;
                    break;
                },
                Err(_) => {
                    let actual_tokens =
                        usage.map(|(input, output)| input.saturating_add(output));
                    let actual_cost_microusd = usage_cost(
                        usage,
                        input_price_per_million,
                        output_price_per_million,
                    );
                    finalize_reservation(
                        &routing,
                        reservation,
                        actual_tokens,
                        actual_cost_microusd,
                    )
                    .await;
                    yield Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upstream stream was idle",
                    ));
                    break;
                }
            }
        }
        drop(provider_permit);
        drop(global_permit);
        drop(request_log);
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    copy_safe_headers(&upstream_headers, response.headers_mut());
    add_gateway_headers(
        response.headers_mut(),
        request_id,
        &alias,
        &provider,
        attempts.saturating_sub(1),
    );
    add_model_headers(response.headers_mut(), &model_metadata);
    if let Some(served_model) = provider_routed_model(&upstream_headers) {
        response
            .headers_mut()
            .insert("x-model-gateway-served-model", header_value(served_model));
    }
    if is_stream {
        response
            .headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    response
}

fn decorate_json_response(body: &[u8], footer: &str) -> Result<Bytes, &'static str> {
    let mut value: Value = serde_json::from_slice(body)
        .map_err(|_| "upstream returned invalid Chat Completions JSON")?;
    let choices = value
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .ok_or("upstream response did not contain Chat Completions choices")?;
    for choice in choices {
        let Some(content) = choice
            .get_mut("message")
            .and_then(|message| message.get_mut("content"))
            .and_then(|content| content.as_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        if content.is_empty() || content.trim_end().ends_with(footer) {
            continue;
        }
        let decorated = format!("{content}\n{footer}");
        choice["message"]["content"] = Value::String(decorated);
    }
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|_| "upstream response could not be decorated")
}

fn response_model(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn provider_routed_model(headers: &HeaderMap) -> Option<&str> {
    ["x-openrouter-model", "x-provider-model", "x-model-id"]
        .into_iter()
        .find_map(|name| headers.get(name)?.to_str().ok())
}

#[derive(Default)]
struct StreamChoice {
    tail: String,
    saw_content: bool,
    appended: bool,
    source: Option<Value>,
}

fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (position, delimiter_len) =
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            (position, 4)
        } else {
            let position = buffer.windows(2).position(|window| window == b"\n\n")?;
            (position, 2)
        };
    Some(buffer.drain(..position + delimiter_len).collect())
}

fn transform_sse_event(
    event: &[u8],
    footer: &str,
    choices: &mut BTreeMap<u64, StreamChoice>,
) -> Vec<Vec<u8>> {
    let text = match std::str::from_utf8(event) {
        Ok(text) => text,
        Err(_) => return vec![event.to_vec()],
    };
    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if payload.is_empty() {
        return vec![event.to_vec()];
    }
    if payload.trim() == "[DONE]" {
        let pending = choices
            .iter()
            .filter_map(|(index, state)| {
                (state.saw_content && !state.appended && !state.tail.trim_end().ends_with(footer))
                    .then_some(*index)
            })
            .collect::<Vec<_>>();
        let mut output = pending
            .into_iter()
            .map(|index| {
                let state = choices.get_mut(&index).expect("known choice");
                state.appended = true;
                footer_sse_event(index, footer, line_ending, state.source.as_ref())
            })
            .collect::<Vec<_>>();
        output.push(event.to_vec());
        return output;
    }
    let Ok(value) = serde_json::from_str::<Value>(&payload) else {
        return vec![event.to_vec()];
    };
    let mut finishing = BTreeSet::new();
    if let Some(items) = value.get("choices").and_then(Value::as_array) {
        for item in items {
            let index = item.get("index").and_then(Value::as_u64).unwrap_or(0);
            let state = choices.entry(index).or_default();
            state.source = Some(value.clone());
            if let Some(content) = item
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
            {
                if !content.is_empty() {
                    state.saw_content = true;
                    state.tail.push_str(content);
                    if state.tail.len() > footer.len() * 2 + 32 {
                        let keep = footer.len() * 2 + 32;
                        state.tail = state
                            .tail
                            .chars()
                            .rev()
                            .take(keep)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                    }
                }
            }
            if item
                .get("finish_reason")
                .is_some_and(|reason| !reason.is_null())
                && state.saw_content
                && !state.appended
                && !state.tail.trim_end().ends_with(footer)
            {
                finishing.insert(index);
            }
        }
    }
    let mut output = finishing
        .into_iter()
        .map(|index| {
            choices.get_mut(&index).expect("known choice").appended = true;
            footer_sse_event(index, footer, line_ending, Some(&value))
        })
        .collect::<Vec<_>>();
    output.push(event.to_vec());
    output
}

fn footer_sse_event(
    index: u64,
    footer: &str,
    line_ending: &str,
    source: Option<&Value>,
) -> Vec<u8> {
    let mut value = json!({
        "object": "chat.completion.chunk",
        "choices": [{"index": index, "delta": {"content": format!("\n{footer}")}}]
    });
    if let (Some(source), Some(object)) = (source, value.as_object_mut()) {
        for key in ["id", "created", "model", "system_fingerprint"] {
            if let Some(field) = source.get(key) {
                object.insert(key.to_owned(), field.clone());
            }
        }
    }
    format!("data: {}{line_ending}{line_ending}", value).into_bytes()
}

struct RequestLog {
    request_id: String,
    alias: String,
    provider: String,
    status: StatusCode,
    started_at: Instant,
    is_stream: bool,
    fallbacks: usize,
}

impl Drop for RequestLog {
    fn drop(&mut self) {
        log_request(
            &self.request_id,
            &self.alias,
            &self.provider,
            self.status,
            self.started_at,
            self.is_stream,
            self.fallbacks,
        );
    }
}

fn log_request(
    request_id: &str,
    alias: &str,
    provider: &str,
    status: StatusCode,
    started_at: Instant,
    is_stream: bool,
    fallbacks: usize,
) {
    tracing::info!(
        request_id,
        alias,
        provider,
        status_class = status.as_u16() / 100,
        latency_ms = started_at.elapsed().as_millis() as u64,
        stream = is_stream,
        fallback_count = fallbacks,
        "request complete"
    );
}

fn request_id_from_response(response: &Response) -> String {
    response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("invalid")
        .to_owned()
}

fn upstream_error_response(
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    request_id: String,
    alias: &str,
    provider: &str,
    attempts: usize,
) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    copy_safe_headers(&headers, response.headers_mut());
    add_gateway_headers(
        response.headers_mut(),
        request_id,
        alias,
        provider,
        attempts.saturating_sub(1),
    );
    response
}

fn copy_safe_headers(source: &HeaderMap, target: &mut HeaderMap) {
    for name in [
        "content-type",
        "cache-control",
        "retry-after",
        "x-request-id",
        "x-ratelimit-limit",
        "x-ratelimit-remaining",
        "x-ratelimit-reset",
    ] {
        if let Some(value) = source.get(name) {
            target.insert(HeaderName::from_static(name), value.clone());
        }
    }
}

fn add_model_headers(headers: &mut HeaderMap, metadata: &ModelMetadata) {
    headers.insert(
        "x-model-gateway-model",
        header_value(&metadata.upstream_model),
    );
    headers.insert(
        "x-model-gateway-canonical-model",
        header_value(&metadata.canonical_model),
    );
    headers.insert(
        "x-model-gateway-reasoning-effort",
        header_value(&metadata.reasoning_effort),
    );
    if let Some(selection) = &metadata.selection {
        headers.insert("x-model-gateway-task", header_value(selection.task));
        headers.insert(
            "x-model-gateway-complexity",
            header_value(selection.complexity),
        );
        headers.insert(
            "x-model-gateway-classifier",
            header_value(selection.classifier_version),
        );
        headers.insert(
            "x-model-gateway-quality-floor",
            header_value(&selection.quality_floor.to_string()),
        );
        headers.insert(
            "x-model-gateway-quality",
            header_value(&selection.quality.to_string()),
        );
        headers.insert(
            "x-model-gateway-expected-cost-microusd",
            header_value(&selection.expected_cost_microusd.to_string()),
        );
        headers.insert(
            "x-model-gateway-benchmark-snapshot",
            header_value(&selection.benchmark_snapshot_id.to_string()),
        );
        headers.insert(
            "x-model-gateway-benchmark-as-of",
            header_value(&selection.benchmark_as_of.to_string()),
        );
        if let Some(match_kind) = selection.match_kind {
            headers.insert(
                "x-model-gateway-benchmark-match",
                header_value(match_kind.as_str()),
            );
        }
    }
}

fn add_gateway_headers(
    headers: &mut HeaderMap,
    request_id: String,
    alias: &str,
    provider: &str,
    fallbacks: usize,
) {
    headers.insert(REQUEST_ID_HEADER, header_value(&request_id));
    headers.insert("x-model-gateway-alias", header_value(alias));
    headers.insert("x-model-gateway-provider", header_value(provider));
    headers.insert(
        "x-model-gateway-fallbacks",
        header_value(&fallbacks.to_string()),
    );
}

fn admission_error(request_id: String, message: &str, admission_timeout_ms: u64) -> Response {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        request_id,
        message,
        "server_error",
        Some("admission"),
    );
    let retry_after = admission_timeout_ms.div_ceil(1000).max(1);
    response
        .headers_mut()
        .insert("retry-after", header_value(&retry_after.to_string()));
    response
}

fn selected_error_response(
    status: StatusCode,
    request_id: String,
    message: &str,
    alias: &str,
    provider: &str,
    attempts: usize,
) -> Response {
    let mut response = error_response(status, request_id.clone(), message, "upstream_error", None);
    add_gateway_headers(
        response.headers_mut(),
        request_id,
        alias,
        provider,
        attempts.saturating_sub(1),
    );
    response
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 128 && !value.contains(['\r', '\n']))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("mg-{}", next_request_id()))
}

fn next_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::try_from(value).unwrap_or_else(|_| HeaderValue::from_static("invalid"))
}

fn error_response(
    status: StatusCode,
    request_id: String,
    message: &str,
    kind: &'static str,
    code: Option<&'static str>,
) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            kind,
            message: message.to_owned(),
            code,
            param: None,
        },
    };
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, header_value(&request_id));
    response
}

async fn auto_refresh_benchmarks(
    state_path: Option<PathBuf>,
    benchmark_max_age_seconds: u64,
    aa_api_key: Option<String>,
) {
    let refresh_interval = Duration::from_secs(benchmark_max_age_seconds.max(3_600) / 2);

    loop {
        let routing = match RoutingStore::open(state_path.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Benchmark auto-refresh: cannot open routing store: {e}");
                tokio::time::sleep(Duration::from_secs(3_600)).await;
                continue;
            }
        };

        let needs_refresh = routing
            .active_benchmark_snapshot(benchmark_max_age_seconds)
            .ok()
            .flatten()
            .is_none();

        if needs_refresh {
            if let Some(ref key) = aa_api_key {
                match fetch_aa_benchmarks(&routing, key).await {
                    Ok(count) => {
                        tracing::info!("Auto-refreshed {count} benchmark models");
                    }
                    Err(e) => {
                        tracing::warn!("Benchmark auto-refresh failed (will retry): {e}");
                    }
                }
            }
        }

        tokio::time::sleep(refresh_interval).await;
    }
}

async fn fetch_aa_benchmarks(routing: &RoutingStore, api_key: &str) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("model-gateway/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let mut all_models = Vec::new();
    let mut page = 1u64;
    loop {
        let body: serde_json::Value = client
            .get(format!(
                "https://artificialanalysis.ai/api/v2/language/models/free?page={page}"
            ))
            .header("x-api-key", api_key)
            .send()
            .await
            .map_err(|e| format!("AA request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("AA request failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("AA response parse failed: {e}"))?;

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
        source: "artificial-analysis".to_owned(),
        attribution: "Artificial Analysis (https://artificialanalysis.ai/)".to_owned(),
        models: all_models,
    }
    .normalize()?;

    let count = import.models.len();
    routing
        .replace_benchmarks(&import.source, &import.attribution, &import.models)
        .map_err(|e| e.to_string())?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::{
        BenchmarkIdentityIndex, ModelMatchKind, ModelMetadata, RequestRequirements,
        SelectionMetadata, StreamChoice, add_model_headers, benchmark_ids_match,
        benchmark_price_for_model, benchmarks_for_effort, catalog_capabilities_json,
        copy_safe_headers, decorate_json_response, encode_uri_component, estimate_request_tokens,
        expected_cost_microusd, find_all_matching_benchmarks, find_benchmark,
        find_exact_matching_benchmarks, find_exact_matching_benchmarks_indexed,
        find_suggested_benchmark, footer_sse_event, has_dynamic_or_release_suffix, header_value,
        identity_mapping_indexes, is_exact_model_identity, is_fallback_status, is_model_denied,
        is_provider_auto_route, is_reasoning_effort, log_request, malformed_sse_event,
        parse_json_usage, parse_sse_usage, parse_usage_value, rank_benchmark_models,
        rate_limit_reset_delay, request_id, request_id_from_response, session_material, sse_model,
        strip_model_noise, take_sse_event, transform_sse_event,
    };
    use crate::benchmarks::{BenchmarkModel, TaskKind};
    use crate::identity::{
        IdentityAliasRecord, IdentityConfidence, IdentityEntityRecord, IdentityImport,
    };
    use crate::routing::{AccessKind, CatalogOffering, RoutingStore};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use serde_json::json;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    struct TestGuard(Arc<Mutex<Vec<u8>>>);

    impl Write for TestGuard {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for TestWriter {
        type Writer = TestGuard;

        fn make_writer(&'a self) -> Self::Writer {
            TestGuard(self.0.clone())
        }
    }

    #[test]
    fn catalog_links_percent_encode_path_and_query_delimiters() {
        assert_eq!(
            encode_uri_component("model/name?x=y", false),
            "model%2Fname%3Fx%3Dy"
        );
        assert_eq!(encode_uri_component("model/name", true), "model/name");
        assert_eq!(encode_uri_component("coding task", false), "coding%20task");
    }

    fn resolves_single(catalog_id: &str, benchmark_id: &str) -> bool {
        let benchmarks = BTreeMap::from([(
            benchmark_id.to_owned(),
            vec![BenchmarkModel::fixture(
                benchmark_id,
                50.0,
                50.0,
                50.0,
                1.0,
                1.0,
            )],
        )]);
        find_all_matching_benchmarks(&benchmarks, catalog_id).len() == 1
    }

    #[test]
    fn indexed_exact_matching_preserves_strict_identity_results() {
        let models = vec![
            BenchmarkModel::fixture("gpt-4o-2024-08-06", 50.0, 50.0, 50.0, 1.0, 1.0),
            BenchmarkModel::fixture("gemini-2-5-flash", 50.0, 50.0, 50.0, 1.0, 1.0),
            BenchmarkModel::fixture("model-family-40k", 50.0, 50.0, 50.0, 1.0, 1.0),
            BenchmarkModel::fixture("model-family-80k", 50.0, 50.0, 50.0, 1.0, 1.0),
        ];
        let map: BTreeMap<String, Vec<BenchmarkModel>> =
            models
                .iter()
                .cloned()
                .fold(BTreeMap::new(), |mut map, model| {
                    map.entry(model.id.clone()).or_default().push(model);
                    map
                });
        let index = BenchmarkIdentityIndex::new(models);

        for catalog_id in [
            "openai/gpt-4o-2024-08-06",
            "gemini-2.5-flash",
            "provider/model-family-40k",
            "model-family",
        ] {
            let indexed = find_exact_matching_benchmarks_indexed(&index, catalog_id)
                .into_iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            let scanned = find_exact_matching_benchmarks(&map, catalog_id)
                .into_iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                indexed, scanned,
                "identity behavior changed for {catalog_id}"
            );
        }
    }

    #[test]
    #[ignore = "manual performance benchmark; run with --release --ignored --nocapture"]
    fn benchmark_indexed_exact_matching_against_scan() {
        let models = (0..1_000)
            .map(|index| {
                BenchmarkModel::fixture(
                    &format!("vendor/model-{index}-2025"),
                    50.0,
                    50.0,
                    50.0,
                    1.0,
                    1.0,
                )
            })
            .collect::<Vec<_>>();
        let map: BTreeMap<String, Vec<BenchmarkModel>> =
            models
                .iter()
                .cloned()
                .fold(BTreeMap::new(), |mut map, model| {
                    map.entry(model.id.clone()).or_default().push(model);
                    map
                });
        let index = BenchmarkIdentityIndex::new(models);
        let lookups = (0..1_000)
            .map(|index| format!("vendor/model-{index}-2025"))
            .collect::<Vec<_>>();

        let started = Instant::now();
        let indexed_matches = lookups
            .iter()
            .map(|lookup| find_exact_matching_benchmarks_indexed(&index, lookup).len())
            .sum::<usize>();
        let indexed_elapsed = started.elapsed();

        let started = Instant::now();
        let scanned_matches = lookups
            .iter()
            .map(|lookup| find_exact_matching_benchmarks(&map, lookup).len())
            .sum::<usize>();
        let scanned_elapsed = started.elapsed();

        assert_eq!(indexed_matches, scanned_matches);
        println!(
            "indexed_exact_matching: indexed={indexed_elapsed:?}, scan={scanned_elapsed:?}, matches={indexed_matches}"
        );
    }

    fn resolves_exact_single(catalog_id: &str, benchmark_id: &str) -> bool {
        let benchmarks = BTreeMap::from([(
            benchmark_id.to_owned(),
            vec![BenchmarkModel::fixture(
                benchmark_id,
                50.0,
                50.0,
                50.0,
                1.0,
                1.0,
            )],
        )]);
        find_exact_matching_benchmarks(&benchmarks, catalog_id).len() == 1
    }

    #[test]
    fn opencode_zen_free_ids_require_exact_or_approved_runtime_identity() {
        assert!(resolves_exact_single(
            "deepseek-v4-flash-free",
            "deepseek-v4-flash"
        ));
        assert!(resolves_exact_single(
            "north-mini-code-free",
            "north-mini-code"
        ));
        assert!(resolves_exact_single("laguna-s-2.1-free", "laguna-s-2-1"));
        assert!(resolves_exact_single(
            "ling-3.0-flash-free",
            "ling-3-0-flash"
        ));
        for (catalog, benchmark) in [
            ("mimo-v2.5-free", "mimo-v2-5-0424"),
            ("nemotron-3-ultra-free", "nvidia-nemotron-3-ultra-550b-a55b"),
            ("big-pickle", "claude-sonnet-5"),
        ] {
            assert!(
                !resolves_exact_single(catalog, benchmark),
                "unexpected runtime identity {catalog} -> {benchmark}"
            );
        }
    }

    #[test]
    fn fallback_statuses_are_explicit() {
        assert!(is_fallback_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_fallback_status(StatusCode::BAD_GATEWAY));
        assert!(!is_fallback_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn benchmark_matching_handles_provider_prefixes_and_normalized_versions() {
        let mut benchmarks = BTreeMap::new();
        benchmarks.insert(
            "gemini-2-5-flash".to_owned(),
            vec![BenchmarkModel::fixture(
                "gemini-2-5-flash",
                80.0,
                70.0,
                60.0,
                1.0,
                1.0,
            )],
        );
        benchmarks.insert(
            "claude-4-5-sonnet".to_owned(),
            vec![BenchmarkModel::fixture(
                "claude-4-5-sonnet",
                90.0,
                85.0,
                75.0,
                1.0,
                1.0,
            )],
        );
        assert!(benchmark_ids_match(
            "models/gemini-2.5-flash",
            "gemini-2-5-flash"
        ));
        assert!(benchmark_ids_match(
            "anthropic/claude-sonnet-4-5",
            "claude-4-5-sonnet"
        ));
        assert_eq!(
            find_benchmark(&benchmarks, "models/gemini-2.5-flash")
                .expect("Gemini benchmark")
                .intelligence,
            Some(80.0)
        );
    }

    #[test]
    fn find_benchmark_prefers_exact_and_rejects_other_variants() {
        let mut benchmarks = BTreeMap::new();
        // Exact match: mimo-v2-flash → mimo-v2-flash (G=24.7), NOT mimo-v2-flash-reasoning (G=31.2)
        benchmarks.insert(
            "mimo-v2-flash".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-flash",
                24.7,
                49.8,
                12.0,
                1.0,
                1.0,
            )],
        );
        benchmarks.insert(
            "mimo-v2-flash-reasoning".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-flash-reasoning",
                31.2,
                86.8,
                95.0,
                1.0,
                1.0,
            )],
        );
        let result =
            find_benchmark(&benchmarks, "xiaomimimo/mimo-v2-flash").expect("mimo-v2-flash");
        assert_eq!(result.intelligence, Some(24.7));
        assert_eq!(result.coding_quality, Some(49.8));

        // Prefix match with variant-keyword penalty: mimo-v2.5 should get mimo-v2-5-0424,
        // NOT the higher-scoring mimo-v2-5-pro (penalized for "pro" keyword)
        let mut benchmarks = BTreeMap::new();
        benchmarks.insert(
            "mimo-v2-5-0424".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-5-0424",
                37.2,
                56.8,
                23.7,
                1.0,
                1.0,
            )],
        );
        benchmarks.insert(
            "mimo-v2-5-pro".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-5-pro",
                42.2,
                60.2,
                29.1,
                1.0,
                1.0,
            )],
        );
        benchmarks.insert(
            "mimo-v2-5-pro-non-reasoning".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-5-pro-non-reasoning",
                27.9,
                39.1,
                72.5,
                1.0,
                1.0,
            )],
        );
        let result =
            find_suggested_benchmark(&benchmarks, "xiaomimimo/mimo-v2.5").expect("mimo-v2.5");
        assert_eq!(result.intelligence, Some(37.2));
        assert_eq!(
            result.id, "mimo-v2-5-0424",
            "should prefer base variant over penalized keywords"
        );

        // A base model must not inherit quality from a different product variant.
        let mut benchmarks = BTreeMap::new();
        benchmarks.insert(
            "mimo-v2-5-pro".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-5-pro",
                42.2,
                60.2,
                29.1,
                1.0,
                1.0,
            )],
        );
        benchmarks.insert(
            "mimo-v2-5-pro-non-reasoning".to_owned(),
            vec![BenchmarkModel::fixture(
                "mimo-v2-5-pro-non-reasoning",
                27.9,
                39.1,
                72.5,
                1.0,
                1.0,
            )],
        );
        assert!(find_suggested_benchmark(&benchmarks, "xiaomimimo/mimo-v2.5").is_none());
        assert!(!benchmark_ids_match("mimo-v2.5", "mimo-v2-5-pro"));
        assert!(!benchmark_ids_match("mimo-v2.5-pro", "mimo-v2-5-0424"));
        assert!(!benchmark_ids_match("deepseek-v4-flash", "deepseek-v4-pro"));
        assert!(resolves_single(
            "stepfun/step-3.7-flash:free",
            "step-3-7-flash"
        ));
    }

    #[test]
    fn matcher_rejects_cross_family_suffixes_and_ambiguous_groups() {
        assert!(!benchmark_ids_match(
            "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
            "qwen3-omni-30b-a3b-reasoning"
        ));
        assert!(!benchmark_ids_match(
            "qwen/qwen3-30b-a3b-instruct",
            "qwen3-coder-30b-a3b-instruct"
        ));
        assert!(!benchmark_ids_match(
            "qwen/qwen3-30b-a3b-instruct",
            "qwen3-vl-30b-a3b-instruct"
        ));

        let mut benchmarks = BTreeMap::new();
        for id in ["devstral-2", "devstral-small-2"] {
            benchmarks.insert(
                id.to_owned(),
                vec![BenchmarkModel::fixture(id, 40.0, 40.0, 40.0, 1.0, 1.0)],
            );
        }
        assert!(find_all_matching_benchmarks(&benchmarks, "devstral").is_empty());
    }

    #[test]
    fn matcher_accepts_exact_normalized_and_safe_live_identities() {
        let cases = [
            ("gpt-4o", "gpt-4o"),
            ("GPT_4O", "gpt-4o"),
            ("gemini-2.5-flash", "gemini-2-5-flash"),
            (
                "models/gemini-3.1-flash-lite-preview",
                "gemini-3-1-flash-lite-preview",
            ),
            ("anthropic/claude-sonnet-4-5", "claude-4-5-sonnet"),
            ("deepseek/deepseek-v4-flash", "deepseek-v4-flash"),
            (
                "Qwen/Qwen3-Coder-30B-A3B-Instruct",
                "qwen3-coder-30b-a3b-instruct",
            ),
            (
                "qwen/qwen3-vl-30b-a3b-instruct",
                "qwen3-vl-30b-a3b-instruct",
            ),
            (
                "qwen/qwen3-omni-30b-a3b-instruct",
                "qwen3-omni-30b-a3b-instruct",
            ),
            ("stepfun/step-3.7-flash:free", "step-3-7-flash"),
            ("xiaomimimo/mimo-v2-pro", "mimo-v2-pro"),
            ("xiaomi/mimo-v2.5-pro", "mimo-v2-5-pro"),
            ("xiaomimimo/mimo-v2.5", "mimo-v2-5-0424"),
            ("MiniMaxAI/MiniMax-M2.5", "minimax-m2-5"),
            ("moonshotai/kimi-k2-0905", "kimi-k2-0905"),
            (
                "qwen/qwen3-235b-a22b-instruct-2507",
                "qwen3-235b-a22b-instruct-2507",
            ),
            ("deepseek/deepseek-v3-0324", "deepseek-v3-0324"),
            ("openai/gpt-4o-2024-08-06", "gpt-4o-2024-08-06"),
            ("openai/o3-mini-high", "o3-mini-high"),
            ("opencode-go/glm-5.2", "glm-5-2"),
            ("qwen/qwen3.7-max", "qwen3-7-max"),
            ("zai-org/glm-4.7-flash", "glm-4-7-flash"),
            (
                "nvidia/nemotron-3-ultra",
                "nvidia-nemotron-3-ultra-550b-a55b",
            ),
            ("provider/model-fp16-bf16", "model"),
            ("provider/model-int4:free", "model"),
        ];

        for (catalog, benchmark) in cases {
            assert!(
                resolves_single(catalog, benchmark),
                "expected '{catalog}' to resolve uniquely to '{benchmark}'"
            );
        }
    }

    #[test]
    fn matcher_rejects_semantic_release_and_family_collisions() {
        let cases = [
            ("deepseek/deepseek-v4-flash", "deepseek-v4-pro"),
            (
                "qwen/qwen3-30b-a3b-instruct",
                "qwen3-coder-30b-a3b-instruct",
            ),
            ("qwen/qwen3-30b-a3b-instruct", "qwen3-vl-30b-a3b-instruct"),
            (
                "qwen/qwen3-vl-30b-a3b-thinking",
                "qwen3-vl-30b-a3b-reasoning",
            ),
            (
                "qwen/qwen3-omni-30b-a3b-thinking",
                "qwen3-omni-30b-a3b-reasoning",
            ),
            (
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
                "qwen3-omni-30b-a3b-reasoning",
            ),
            (
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
                "nemotron-3-nano-omni-30b-a3b",
            ),
            ("qwen/qwen3-235b-a22b-fp8", "qwen3-235b-a22b-instruct"),
            (
                "meta/llama-4-maverick-17b-128e-instruct-fp8",
                "llama-4-maverick",
            ),
            ("minimax/minimax-m2.5-highspeed", "minimax-m2-5"),
            ("qwen/qwen3.6-max-preview", "qwen3-6-max"),
            ("qwen/qwen3-coder-flash", "qwen3-coder-next"),
            ("kwaipilot/kat-coder-pro-v2.5", "kat-coder-pro-v2"),
            ("mistralai/ministral-14b-2512", "ministral-3-14b"),
            ("mistral/ministral-14b-latest", "ministral-3-14b"),
            ("openai/gpt-4o-mini-2024-07-18", "gpt-4o-mini"),
            ("openai/gpt-4o-2024-11-20", "gpt-4o"),
            ("qwen/qwen3.7-max-2026-05-20", "qwen3-7-max"),
            ("deepseek/deepseek-v4-flash:discounted", "deepseek-v4-flash"),
            ("openai/o4-mini-deep-research", "o4-mini"),
            ("gpt-4o-audio-preview", "gpt-4o-audio"),
            ("model-thinking", "model"),
            ("gpt-4", "gpt-4-turbo"),
            ("a", "a-b"),
        ];

        for (catalog, benchmark) in cases {
            assert!(
                !resolves_single(catalog, benchmark),
                "expected '{catalog}' to reject '{benchmark}'"
            );
        }
    }

    #[test]
    fn matcher_prefers_exact_and_fails_closed_on_ambiguous_fuzzy_groups() {
        let benchmark = |id: &str, quality: f64| {
            BenchmarkModel::fixture(id, quality, quality, quality, 1.0, 1.0)
        };
        let benchmarks = BTreeMap::from([
            ("gpt-4o".to_owned(), vec![benchmark("gpt-4o", 50.0)]),
            (
                "gpt-4o-mini".to_owned(),
                vec![benchmark("gpt-4o-mini", 90.0)],
            ),
        ]);
        let exact = find_all_matching_benchmarks(&benchmarks, "openai/gpt-4o");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].id, "gpt-4o");

        let ambiguous = BTreeMap::from([
            (
                "model-family-1".to_owned(),
                vec![benchmark("model-family-1", 50.0)],
            ),
            (
                "model-family-2".to_owned(),
                vec![benchmark("model-family-2", 60.0)],
            ),
        ]);
        assert!(find_all_matching_benchmarks(&ambiguous, "model-family").is_empty());

        let unique = BTreeMap::from([(
            "model-family-2025".to_owned(),
            vec![benchmark("model-family-2025", 50.0)],
        )]);
        let resolved = find_all_matching_benchmarks(&unique, "model-family");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "model-family-2025");
    }

    #[test]
    fn release_guards_allow_exact_ids_but_block_base_fallbacks() {
        let releases = [
            "model-latest",
            "model-2512",
            "model-2024-08",
            "model-2024-08-06",
            "model-2026-05-20",
            "model-latest-2512",
        ];
        for release in releases {
            assert!(
                has_dynamic_or_release_suffix(release),
                "missing release guard for {release}"
            );
            assert!(
                resolves_single(release, release),
                "exact release must resolve: {release}"
            );
            assert!(
                !resolves_single(release, "model"),
                "release must not borrow base: {release}"
            );
        }
        for stable in [
            "model-v2",
            "model-32b",
            "model-rc1",
            "model-2024v1",
            "model-2000-05-variant",
        ] {
            assert!(
                !has_dynamic_or_release_suffix(stable),
                "stable ID misclassified: {stable}"
            );
        }
    }

    #[test]
    fn exact_normalization_is_symmetric_but_fuzzy_extensions_are_directional() {
        let exact_pairs = [
            ("models/gemini-2.5-flash", "gemini-2-5-flash"),
            ("UPPER_MODEL", "upper-model"),
            ("claude-sonnet-4-5", "claude-4-5-sonnet"),
            ("stepfun/step-3.7-flash", "step-3-7-flash"),
        ];
        for (left, right) in exact_pairs {
            assert_eq!(
                benchmark_ids_match(left, right),
                benchmark_ids_match(right, left)
            );
        }
        assert!(benchmark_ids_match("mimo-v2.5", "mimo-v2-5-0424"));
        assert!(!benchmark_ids_match("mimo-v2-5-0424", "mimo-v2.5"));
    }

    #[test]
    fn exact_identity_does_not_collide_on_shared_suffixes() {
        assert!(!is_exact_model_identity(
            "nvidia/nemotron-3-nano:free",
            "baidu/cobuddy:free"
        ));
        assert!(!is_exact_model_identity(
            "vendor-a/model:free",
            "vendor-b/other:free"
        ));
        assert!(is_exact_model_identity(
            "models/gemini-2.5-flash",
            "gemini-2-5-flash"
        ));
    }

    #[test]
    fn benchmark_price_matching_stays_exact_even_when_quality_can_match_safely() {
        let benchmark = BenchmarkModel::fixture("gemini-2-5-flash", 50.0, 50.0, 50.0, 1.2, 3.4);
        assert!(benchmark_price_for_model("GEMINI-2-5-FLASH", &benchmark).is_some());
        assert!(benchmark_price_for_model("gemini-2.5-flash", &benchmark).is_none());
        assert!(resolves_single("gemini-2.5-flash", "gemini-2-5-flash"));
    }

    #[test]
    fn conflicting_canonical_entity_links_fail_closed() {
        let store = RoutingStore::open(None).expect("store");
        for (source, entity_id, benchmark_id) in [
            ("models.dev", "hf:vendor/model-a", "benchmark-a"),
            ("openrouter", "hf:vendor/model-b", "benchmark-b"),
        ] {
            store
                .replace_identity_source(&IdentityImport {
                    source: source.to_owned(),
                    attribution: "fixture".to_owned(),
                    entities: vec![IdentityEntityRecord {
                        id: entity_id.to_owned(),
                        creator: Some("vendor".to_owned()),
                        family: Some("model".to_owned()),
                        version: None,
                        variant: None,
                        release_date: None,
                        hugging_face_id: Some(entity_id.trim_start_matches("hf:").to_owned()),
                    }],
                    aliases: vec![IdentityAliasRecord {
                        source: source.to_owned(),
                        provider_key: "provider".to_owned(),
                        provider_model_id: "model".to_owned(),
                        entity_id: entity_id.to_owned(),
                        confidence: IdentityConfidence::CanonicalReference,
                        provenance_url: "fixture".to_owned(),
                        observed_at: 100,
                    }],
                })
                .expect("identity source");
            store
                .approve_benchmark_identity_link(entity_id, benchmark_id, "fixture")
                .expect("approve link");
        }
        let indexes = identity_mapping_indexes(&store);
        assert!(
            !indexes
                .references
                .contains_key(&("provider".to_owned(), "model".to_owned()))
        );
        assert_eq!(
            indexes
                .conflicts
                .get(&("provider".to_owned(), "model".to_owned())),
            Some(&vec!["benchmark-a".to_owned(), "benchmark-b".to_owned()])
        );
    }

    #[test]
    fn matcher_groups_real_effort_suffixed_benchmarks() {
        let benchmarks = ["max", "high", "medium", "low", "xhigh"]
            .into_iter()
            .map(|effort| {
                let id = if effort == "max" {
                    "gpt-5-6-sol".to_owned()
                } else {
                    format!("gpt-5-6-sol-{effort}")
                };
                let mut model = BenchmarkModel::fixture(&id, 60.0, 60.0, 60.0, 1.0, 1.0);
                model.reasoning_effort = Some(effort.to_owned());
                (id, vec![model])
            })
            .collect::<BTreeMap<_, _>>();

        let grouped = find_all_matching_benchmarks(&benchmarks, "gpt-5.6-sol");
        assert_eq!(grouped.len(), 5);
        for effort in ["max", "high", "medium", "low", "xhigh"] {
            let filtered = benchmarks_for_effort(grouped.clone(), Some(effort));
            assert_eq!(filtered.len(), 1, "missing effort {effort}");
            assert_eq!(filtered[0].reasoning_effort.as_deref(), Some(effort));
            if effort != "max" {
                let explicit =
                    find_all_matching_benchmarks(&benchmarks, &format!("gpt-5.6-sol-{effort}"));
                assert_eq!(explicit.len(), 1);
                assert_eq!(explicit[0].reasoning_effort.as_deref(), Some(effort));
            }
        }
        assert!(benchmarks_for_effort(grouped, Some("minimal")).is_empty());

        let plain = BenchmarkModel::fixture("plain-model", 50.0, 50.0, 50.0, 1.0, 1.0);
        assert_eq!(benchmarks_for_effort(vec![&plain], Some("high")).len(), 1);
    }

    #[test]
    fn malformed_sse_payloads_fail_closed() {
        assert!(malformed_sse_event(b"data: not-json\n\n"));
        assert!(!malformed_sse_event(b"data: {\"choices\":[]}\n\n"));
        assert!(!malformed_sse_event(b"data: [DONE]\n\n"));
    }

    #[test]
    fn rate_limit_reset_headers_are_converted_to_bounded_delays() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", "60".parse().expect("header"));
        assert_eq!(rate_limit_reset_delay(&headers), Some(60));
        headers.insert("x-ratelimit-reset", "not-a-number".parse().expect("header"));
        assert_eq!(rate_limit_reset_delay(&headers), None);
    }

    #[test]
    fn rankings_are_quality_sorted_with_deterministic_ties() {
        let strong = BenchmarkModel::fixture("strong", 90.0, 90.0, 90.0, 3.0, 3.0);
        let cheap = BenchmarkModel::fixture("cheap", 90.0, 90.0, 90.0, 1.0, 1.0);
        let rankings = rank_benchmark_models(vec![strong, cheap], TaskKind::General, 10);
        assert_eq!(rankings[0]["id"], "cheap");
        assert_eq!(rankings[0]["rank"], 1);
    }

    #[test]
    fn request_id_is_generated_or_preserved() {
        let empty = HeaderMap::new();
        assert!(request_id(&empty).starts_with("mg-"));
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "client-request".parse().expect("header"));
        assert_eq!(request_id(&headers), "client-request");
    }

    #[test]
    fn completion_logs_use_a_fixed_body_free_schema() {
        let writer = TestWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        log_request(
            "request-id",
            "public-alias",
            "provider-name",
            StatusCode::OK,
            Instant::now(),
            true,
            2,
        );
        let output = String::from_utf8(writer.0.lock().expect("log buffer").clone())
            .expect("utf8 log output");
        for field in [
            "request_id",
            "alias",
            "provider",
            "status_class",
            "latency_ms",
            "stream",
            "fallback_count",
        ] {
            assert!(output.contains(field), "missing {field}: {output}");
        }
        assert!(!output.contains("messages"));
        assert!(!output.contains("authorization"));
        assert!(!output.contains("tool_calls"));
    }

    #[test]
    fn decorates_terminal_json_text_once_and_skips_tool_only_choices() {
        let footer = "- GPT: 5.6 Sol Medium, Kilo Code";
        let body = serde_json::json!({
            "id": "fixture",
            "choices": [
                {"message": {"content": "answer"}, "finish_reason": "stop"},
                {"message": {"content": null, "tool_calls": [{"id": "call"}]}, "finish_reason": "tool_calls"},
                {"message": {"content": format!("already\n{footer}")}, "finish_reason": "stop"}
            ],
            "unknown": {"preserved": true}
        });
        let decorated = decorate_json_response(&serde_json::to_vec(&body).expect("body"), footer)
            .expect("decorated response");
        let value: serde_json::Value = serde_json::from_slice(&decorated).expect("json");
        assert_eq!(
            value["choices"][0]["message"]["content"],
            format!("answer\n{footer}")
        );
        assert!(value["choices"][1]["message"]["content"].is_null());
        assert_eq!(
            value["choices"][2]["message"]["content"],
            format!("already\n{footer}")
        );
        assert_eq!(value["unknown"]["preserved"], true);
    }

    #[test]
    fn frames_split_sse_and_injects_footer_before_finish() {
        let footer = "- GPT: 5.6 Sol Medium, Kilo Code";
        let mut buffer =
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n".to_vec();
        assert!(take_sse_event(&mut buffer).is_none());
        buffer.extend_from_slice(
            b"\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let first = take_sse_event(&mut buffer).expect("first event");
        let second = take_sse_event(&mut buffer).expect("second event");
        let mut state = BTreeMap::<u64, StreamChoice>::new();
        let first_output = transform_sse_event(&first, footer, &mut state);
        assert_eq!(first_output, vec![first]);
        let second_output = transform_sse_event(&second, footer, &mut state);
        assert_eq!(second_output.len(), 2);
        assert!(String::from_utf8_lossy(&second_output[0]).contains(footer));
        assert!(String::from_utf8_lossy(&second_output[1]).contains("finish_reason"));
    }

    #[test]
    fn sse_done_decorates_unfinished_text_without_duplicates() {
        let footer = "- Local: Model Default, Local";
        let mut state = BTreeMap::<u64, StreamChoice>::new();
        let content = format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"answer\\n{footer}\"}}}}]}}\n\n"
        );
        let _ = transform_sse_event(content.as_bytes(), footer, &mut state);
        let output = transform_sse_event(b"data: [DONE]\n\n", footer, &mut state);
        assert_eq!(output, vec![b"data: [DONE]\n\n".to_vec()]);
    }

    #[test]
    fn unknown_capabilities_are_not_inferred() {
        let offering = CatalogOffering {
            provider: "cli-proxy".to_owned(),
            model: "gpt-5.4".to_owned(),
            refreshed_at: 0,
            access_kind: AccessKind::SubscriptionIncluded,
            context_length: None,
            supports_tools: None,
            supports_vision: None,
            supports_structured_output: None,
            input_price_per_million: None,
            output_price_per_million: None,
        };
        assert_eq!(catalog_capabilities_json(&offering), None);
    }

    #[test]
    fn request_estimates_and_capabilities_are_deterministic() {
        let request = serde_json::json!({
            "messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "x"}}]}],
            "max_tokens": 50,
            "tools": [{"type": "function"}],
            "response_format": {"type": "json_object"}
        });
        assert!(estimate_request_tokens(&request) >= 50);
        let requirements = RequestRequirements::from_request(&request);
        assert!(requirements.tools);
        assert!(requirements.vision);
        assert!(requirements.structured);
    }

    #[test]
    fn expected_cost_is_microdollars_and_saturates() {
        assert_eq!(expected_cost_microusd(500, 500, 1.0, 3.0), 2_000);
        assert_eq!(expected_cost_microusd(500, 500, 0.0, 0.0), 0);
        assert_eq!(
            expected_cost_microusd(u64::MAX, u64::MAX, f64::MAX, f64::MAX),
            u64::MAX
        );
    }

    #[test]
    fn session_material_prefers_body_then_header_then_messages() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "header-session".parse().expect("header"));
        let body = serde_json::json!({
            "session_id": "body-session",
            "messages": [{"role": "user", "content": "private"}]
        });
        assert_eq!(
            session_material(&headers, &body).as_deref(),
            Some("body:body-session")
        );
        let without_body = serde_json::json!({
            "messages": [{"role": "user", "content": "private"}]
        });
        assert_eq!(
            session_material(&headers, &without_body).as_deref(),
            Some("header:header-session")
        );
        headers.remove("x-session-id");
        let material = session_material(&headers, &without_body).expect("message material");
        assert!(material.contains("private"));
    }

    #[test]
    fn strip_model_noise_removes_quantization_suffixes() {
        assert_eq!(
            strip_model_noise("qwen/qwen3-30b-a3b-fp8"),
            "qwen/qwen3-30b-a3b"
        );
        assert_eq!(strip_model_noise("qwen/qwen3-32b-fp8"), "qwen/qwen3-32b");
        assert_eq!(
            strip_model_noise("qwen/qwen3-235b-a22b-fp8"),
            "qwen/qwen3-235b-a22b"
        );
    }

    #[test]
    fn strip_model_noise_preserves_thinking_and_release_identity() {
        assert_eq!(
            strip_model_noise("qwen/qwen3-235b-a22b-thinking-2507"),
            "qwen/qwen3-235b-a22b-thinking-2507"
        );
        assert_eq!(
            strip_model_noise("qwen/qwen3-omni-30b-a3b-thinking"),
            "qwen/qwen3-omni-30b-a3b-thinking"
        );
        assert_eq!(
            strip_model_noise("qwen/qwen3-vl-235b-a22b-thinking"),
            "qwen/qwen3-vl-235b-a22b-thinking"
        );
    }

    #[test]
    fn strip_model_noise_preserves_dynamic_aliases() {
        assert_eq!(
            strip_model_noise("mistral-tiny-latest"),
            "mistral-tiny-latest"
        );
        assert_eq!(
            strip_model_noise("ministral-14b-latest"),
            "ministral-14b-latest"
        );
    }

    #[test]
    fn strip_model_noise_preserves_release_codes() {
        assert_eq!(
            strip_model_noise("ministral-14b-2512"),
            "ministral-14b-2512"
        );
        assert_eq!(strip_model_noise("mistral-tiny-2407"), "mistral-tiny-2407");
        assert_eq!(
            strip_model_noise("qwen/qwen3-30b-a3b-2507"),
            "qwen/qwen3-30b-a3b-2507"
        );
    }

    #[test]
    fn strip_model_noise_leaves_pure_model_ids_unchanged() {
        // Single segment with no noise: unchanged
        assert_eq!(
            strip_model_noise("qwen/qwen3.6-35b-a3b"),
            "qwen/qwen3-6-35b-a3b"
        );
        // No noise tokens to strip
        assert_eq!(
            strip_model_noise("deepseek/deepseek-v4-flash"),
            "deepseek/deepseek-v4-flash"
        );
        // No /, single segment, no noise
        assert_eq!(strip_model_noise("gemini-2-5-flash"), "gemini-2-5-flash");
    }

    #[test]
    fn strip_model_noise_preserves_internal_noise_tokens() {
        assert_eq!(
            strip_model_noise("free-model-fp8-instruct"),
            "free-model-fp8-instruct"
        );
        assert_eq!(strip_model_noise("model-int8-chat"), "model-int8-chat");
    }

    #[test]
    fn find_benchmark_falls_back_to_noise_stripped_catalog_id() {
        let mut benchmarks = std::collections::BTreeMap::new();
        // AA benchmark without quantization suffix
        benchmarks.insert(
            "qwen3-30b-a3b".to_owned(),
            vec![BenchmarkModel::fixture(
                "qwen3-30b-a3b",
                6.8,
                10.2,
                8.5,
                1.0,
                1.0,
            )],
        );
        // Catalog has -fp8 suffix that should be stripped
        let result =
            find_benchmark(&benchmarks, "qwen/qwen3-30b-a3b-fp8").expect("noise-stripped match");
        assert_eq!(result.intelligence, Some(6.8));
        assert_eq!(result.id, "qwen3-30b-a3b");
    }

    #[test]
    fn noise_stripped_matching_uses_original_model_when_match_exists() {
        let mut benchmarks = std::collections::BTreeMap::new();
        // Exact identity after removing the quantization decoration wins.
        benchmarks.insert(
            "qwen3-30b-a3b".to_owned(),
            vec![BenchmarkModel::fixture(
                "qwen3-30b-a3b",
                10.0,
                12.0,
                9.0,
                1.0,
                1.0,
            )],
        );
        // A semantically different instruct variant must not be considered.
        benchmarks.insert(
            "qwen3-30b-a3b-instruct".to_owned(),
            vec![BenchmarkModel::fixture(
                "qwen3-30b-a3b-instruct",
                6.8,
                10.2,
                8.5,
                1.0,
                1.0,
            )],
        );
        let result =
            find_benchmark(&benchmarks, "qwen/qwen3-30b-a3b-fp8").expect("noise-stripped match");
        assert_eq!(result.intelligence, Some(10.0));
    }

    #[test]
    fn find_benchmark_still_works_without_noise() {
        let mut benchmarks = std::collections::BTreeMap::new();
        benchmarks.insert(
            "gemini-2-5-flash".to_owned(),
            vec![BenchmarkModel::fixture(
                "gemini-2-5-flash",
                80.0,
                70.0,
                60.0,
                1.0,
                1.0,
            )],
        );
        let result =
            find_benchmark(&benchmarks, "models/gemini-2.5-flash").expect("benchmark match");
        assert_eq!(result.intelligence, Some(80.0));
    }

    #[test]
    fn is_provider_auto_route_detects_free_and_auto_routes() {
        for model in [
            "kilo-auto/free",
            "kilo-auto/efficient",
            "kilo-auto/balanced",
            "kilo-auto/frontier",
            "openrouter/auto",
            "openrouter/auto-beta",
            "openrouter/free",
            "orcarouter/auto",
            "orcarouter/free",
        ] {
            assert!(
                is_provider_auto_route(model),
                "missed virtual route {model}"
            );
        }
        for model in [
            "big-pickle",
            "deepseek-v4-flash-free",
            "laguna-s-2.1-free",
            "ling-3.0-flash-free",
            "mimo-v2.5-free",
            "nemotron-3-ultra-free",
            "north-mini-code-free",
            "kat-coder-pro-v2.5:free",
            "gpt-4o",
            "",
        ] {
            assert!(
                !is_provider_auto_route(model),
                "misclassified real model {model}"
            );
        }
    }

    #[test]
    fn is_model_denied_matches_model_or_full_id() {
        let server = crate::config::ServerConfig {
            model_denylist: vec!["gpt-4o".to_owned(), "openai/gpt-4-turbo".to_owned()],
            ..crate::config::ServerConfig::default()
        };
        // Denied by model name
        assert!(is_model_denied("gpt-4o", "any-provider", &server));
        // Denied by full provider/model ID
        assert!(is_model_denied("gpt-4-turbo", "openai", &server));
        // Not denied when name doesn't match
        assert!(!is_model_denied("gpt-4o-mini", "openai", &server));
        // Empty denylist allows everything
        let empty = crate::config::ServerConfig {
            model_denylist: vec![],
            ..crate::config::ServerConfig::default()
        };
        assert!(!is_model_denied("anything", "provider", &empty));
    }

    #[test]
    fn footer_sse_event_creates_formatted_event_with_optional_source() {
        let event = footer_sse_event(0, "- GPT: 5.6 Sol Medium, Kilo Code", "\n", None);
        let text = String::from_utf8_lossy(&event);
        assert!(text.starts_with("data: "));
        assert!(text.contains("- GPT: 5.6 Sol Medium, Kilo Code"));
        assert!(text.ends_with("\n\n"));

        // With source metadata
        let source = json!({
            "id": "chatcmpl-abc123",
            "model": "gpt-4o",
            "created": 1700000000,
            "system_fingerprint": "fp_abc"
        });
        let event = footer_sse_event(1, "- Local: Model Default, Local", "\n", Some(&source));
        let text = String::from_utf8_lossy(&event);
        assert!(text.contains("chatcmpl-abc123"));
        assert!(text.contains("gpt-4o"));
    }

    #[test]
    fn parse_usage_value_extracts_tokens_from_json() {
        let value = json!({
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 50
            }
        });
        let (input, output) = parse_usage_value(&value).expect("usage");
        assert_eq!(input, 150);
        assert_eq!(output, 50);
        assert!(parse_usage_value(&json!({})).is_none());
        assert!(parse_usage_value(&json!({"usage": {"prompt_tokens": 10}})).is_none());
    }

    #[test]
    fn parse_json_usage_handles_well_formed_and_malformed_input() {
        let body = br#"{"usage": {"prompt_tokens": 200, "completion_tokens": 100}}"#;
        let (input, output) = parse_json_usage(body).expect("usage");
        assert_eq!(input, 200);
        assert_eq!(output, 100);

        assert!(parse_json_usage(b"not-json").is_none());
        assert!(parse_json_usage(b"{}").is_none());
    }

    #[test]
    fn parse_sse_usage_extracts_from_data_lines() {
        let event = b"data: {\"usage\": {\"prompt_tokens\": 75, \"completion_tokens\": 25}}\n\n";
        let (input, output) = parse_sse_usage(event).expect("usage");
        assert_eq!(input, 75);
        assert_eq!(output, 25);

        // Non-data lines
        let event =
            b":comment\ndata: {\"usage\": {\"prompt_tokens\": 10, \"completion_tokens\": 5}}\n\n";
        let (input, output) = parse_sse_usage(event).expect("usage");
        assert_eq!(input, 10);
        assert_eq!(output, 5);

        assert!(parse_sse_usage(b"not-data\n\n").is_none());
        assert!(parse_sse_usage(b"data: {}").is_none());
    }

    #[test]
    fn sse_model_extracts_model_from_event() {
        let event = b"data: {\"model\": \"gpt-4o\", \"choices\": []}\n\n";
        assert_eq!(sse_model(event).as_deref(), Some("gpt-4o"));

        assert!(sse_model(b"data: {}").is_none());
        assert!(sse_model(b"not-data").is_none());
        assert!(sse_model(b"").is_none());
    }

    #[test]
    fn header_value_handles_valid_and_invalid_values() {
        assert_eq!(header_value("test"), HeaderValue::from_static("test"));
        assert_eq!(header_value(""), HeaderValue::from_static(""));
        // Values with invalid characters produce a safe fallback
        let bad = HeaderValue::try_from("invalid\x00value")
            .unwrap_or_else(|_| HeaderValue::from_static("invalid"));
        assert_eq!(header_value("invalid\x00value"), bad);
    }

    #[test]
    fn request_id_from_response_extracts_header_or_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "req-123".parse().unwrap());
        let response = axum::response::Response::builder()
            .header("x-request-id", "req-123")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(request_id_from_response(&response), "req-123");
    }

    #[test]
    fn is_reasoning_effort_recognizes_standard_levels() {
        assert!(is_reasoning_effort("low"));
        assert!(is_reasoning_effort("medium"));
        assert!(is_reasoning_effort("high"));
        assert!(is_reasoning_effort("xhigh"));
        assert!(is_reasoning_effort("max"));
        assert!(is_reasoning_effort("LOW"));
        assert!(is_reasoning_effort("MAX"));
        assert!(is_reasoning_effort("Medium"));
        assert!(!is_reasoning_effort("extreme"));
        assert!(!is_reasoning_effort(""));
    }

    #[test]
    fn copy_safe_headers_copies_whitelisted_headers() {
        let mut source = HeaderMap::new();
        source.insert("content-type", "application/json".parse().unwrap());
        source.insert("x-ratelimit-remaining", "99".parse().unwrap());
        source.insert("x-custom-not-safe", "secret".parse().unwrap());

        let mut target = HeaderMap::new();
        copy_safe_headers(&source, &mut target);

        assert_eq!(
            target.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(
            target
                .get("x-ratelimit-remaining")
                .unwrap()
                .to_str()
                .unwrap(),
            "99"
        );
        assert!(target.get("x-custom-not-safe").is_none());
    }

    #[test]
    fn add_model_headers_sets_all_metadata_headers() {
        let metadata = ModelMetadata {
            upstream_model: "gpt-4o".to_owned(),
            canonical_model: "gpt-4o-2024-08".to_owned(),
            family: "GPT".to_owned(),
            display: "4o".to_owned(),
            reasoning_effort: "Medium".to_owned(),
            provider_display: "OpenAI".to_owned(),
            selection: Some(SelectionMetadata {
                canonical_model: "gpt-4o".to_owned(),
                task: "general",
                complexity: "simple",
                classifier_version: "v1",
                quality_floor: 50.0,
                quality: 90.0,
                expected_cost_microusd: 1_000,
                benchmark_snapshot_id: 42,
                benchmark_as_of: 1700000000,
                match_kind: Some(ModelMatchKind::Approved),
            }),
        };
        let mut headers = HeaderMap::new();
        add_model_headers(&mut headers, &metadata);

        assert_eq!(
            headers
                .get("x-model-gateway-model")
                .unwrap()
                .to_str()
                .unwrap(),
            "gpt-4o"
        );
        assert_eq!(
            headers
                .get("x-model-gateway-canonical-model")
                .unwrap()
                .to_str()
                .unwrap(),
            "gpt-4o-2024-08"
        );
        assert_eq!(
            headers
                .get("x-model-gateway-reasoning-effort")
                .unwrap()
                .to_str()
                .unwrap(),
            "Medium"
        );
        assert_eq!(
            headers
                .get("x-model-gateway-task")
                .unwrap()
                .to_str()
                .unwrap(),
            "general"
        );
        assert_eq!(
            headers
                .get("x-model-gateway-benchmark-match")
                .unwrap()
                .to_str()
                .unwrap(),
            "approved"
        );
        assert_eq!(
            headers
                .get("x-model-gateway-quality")
                .unwrap()
                .to_str()
                .unwrap(),
            "90"
        );

        // Without selection, only basic headers are set
        let minimal = ModelMetadata {
            selection: None,
            ..metadata
        };
        let mut headers = HeaderMap::new();
        add_model_headers(&mut headers, &minimal);
        assert!(headers.get("x-model-gateway-task").is_none());
        assert!(headers.get("x-model-gateway-quality").is_none());
    }
}
