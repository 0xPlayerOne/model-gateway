use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;

use crate::benchmarks::{
    BenchmarkImport, BenchmarkModel, Complexity, ScoredCandidate, TaskKind, classify,
    is_frontier_model, is_preview_model, pareto_rank, parse_artificial_analysis, quality_for,
};
use crate::config::{BillingMode, Config, ProviderConfig, QuotaKind, ServerConfig, TargetConfig};
use crate::providers::prepare_request;
use crate::routing::{
    CatalogOffering, ReservationOutcome, ReservationRelease, ReservationToken, RoutingError,
    RoutingStore, is_verified_free, quota_reference,
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
            routing.upsert_offering(provider_name, model, true)?;
        }
    }
    for model in config.models.values() {
        for target in &model.targets {
            if let Some(provider) = config.providers.get(&target.provider) {
                if is_verified_free(provider, &target.model, false) {
                    routing.upsert_offering(&target.provider, &target.model, true)?;
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
        .route("/v1/models", get(list_models))
        .route("/v1/providers", get(list_providers))
        .route("/v1/rankings", get(list_rankings))
        .route("/v1/auto-models", get(list_auto_models))
        .route("/v1/free-models", get(list_free_models))
        .route("/v1/paid-models", get(list_paid_models))
        .route("/v1/chat/completions", post(chat_completions))
        .layer(DefaultBodyLimit::max(state.config.server.max_body_bytes))
        .with_state(state))
}

#[derive(Debug, Deserialize)]
struct ProvidersQuery {
    available: Option<bool>,
}

fn find_benchmark<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
    task: TaskKind,
) -> Option<&'a BenchmarkModel> {
    // Try original model first
    if let Some(result) = find_benchmark_raw(benchmarks, model, task) {
        return Some(result);
    }
    // Fall back to noise-stripped model (strip quantization, date codes, -latest)
    let stripped = strip_model_noise(model);
    if stripped != normalize_identifier(model) {
        find_benchmark_raw(benchmarks, &stripped, task)
    } else {
        None
    }
}

fn find_benchmark_raw<'a>(
    benchmarks: &'a BTreeMap<String, Vec<BenchmarkModel>>,
    model: &str,
    task: TaskKind,
) -> Option<&'a BenchmarkModel> {
    let mut best_exact: Option<(&BenchmarkModel, f64, usize)> = None;
    let mut best_fallback: Option<(&BenchmarkModel, f64, usize)> = None;

    for (benchmark_id, models) in benchmarks {
        let is_exact = is_exact_benchmark_match(model, benchmark_id);
        let matches = is_exact || benchmark_ids_match(model, benchmark_id);
        if !matches {
            continue;
        }

        for benchmark in models {
            let Some(score) = quality_for(benchmark, task) else {
                continue;
            };

            let mut effective_score = score;
            let non_null_count = [
                benchmark.intelligence,
                benchmark.coding_quality,
                benchmark.agentic_quality,
            ]
            .iter()
            .flatten()
            .count();

            if !is_exact {
                effective_score -= variant_prefix_penalty(model, benchmark_id);
            }

            let target = if is_exact {
                &mut best_exact
            } else {
                &mut best_fallback
            };

            let should_update = match target {
                None => true,
                Some((_, best_score, best_count)) => {
                    effective_score > *best_score
                        || (effective_score == *best_score && non_null_count > *best_count)
                }
            };
            if should_update {
                *target = Some((benchmark, effective_score, non_null_count));
            }
        }
    }

    best_exact.or(best_fallback).map(|(b, _, _)| b)
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

const VARIANT_KEYWORDS: &[&str] = &[
    "free",
    "pro",
    "flash",
    "lite",
    "reasoning",
    "non-reasoning",
    "preview",
    "medium",
    "thinking",
    "deep-think",
    "max",
    "mini",
    "highspeed",
    "omni",
];

const NOISE_TOKENS: &[&str] = &[
    "fp8", "fp16", "bf16", "int4", "int8", "latest", "thinking", "free",
];

fn strip_model_noise(model: &str) -> String {
    let segments: Vec<&str> = model.split(['/', ':']).collect();
    let stripped: Vec<String> = segments
        .iter()
        .map(|segment| {
            let normalized = normalize_identifier(segment);
            let mut tokens: Vec<&str> = normalized.split('-').collect();

            // Remove known quantization/version tokens
            tokens.retain(|t| !NOISE_TOKENS.contains(t));

            // Remove trailing date codes (4 consecutive digits, e.g. 2512, 2407)
            // Only from the LAST segment of a multi-segment ID
            while let Some(last) = tokens.last() {
                if last.len() == 4 && last.chars().all(|c| c.is_ascii_digit()) {
                    tokens.pop();
                } else {
                    break;
                }
            }

            tokens.join("-")
        })
        .collect();
    stripped.join("/")
}

fn variant_prefix_penalty(catalog_id: &str, benchmark_id: &str) -> f64 {
    let catalog_normalized = normalize_identifier(catalog_id);
    let benchmark_normalized = normalize_identifier(benchmark_id);
    let catalog_tokens = catalog_normalized
        .split('-')
        .collect::<std::collections::BTreeSet<_>>();
    let benchmark_tokens = benchmark_normalized
        .split('-')
        .collect::<std::collections::BTreeSet<_>>();

    let additional: Vec<_> = benchmark_tokens.difference(&catalog_tokens).collect();
    let mut penalty = 0.0;
    for token in additional {
        if VARIANT_KEYWORDS.contains(token) {
            penalty += 100.0;
        }
    }
    penalty
}

fn benchmark_ids_match(catalog_id: &str, benchmark_id: &str) -> bool {
    let catalog_variants = normalized_identifier_variants(catalog_id);
    let benchmark_variants = normalized_identifier_variants(benchmark_id);
    for catalog in &catalog_variants {
        for benchmark in &benchmark_variants {
            if catalog == benchmark {
                return true;
            }
            let catalog_tokens = catalog.split('-').collect::<BTreeSet<_>>();
            let benchmark_tokens = benchmark.split('-').collect::<BTreeSet<_>>();
            if (catalog_tokens.len() >= 2 || benchmark_tokens.len() >= 2)
                && (catalog.starts_with(&format!("{benchmark}-"))
                    || benchmark.starts_with(&format!("{catalog}-")))
            {
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

    // Token-level suffix prefix matching: when provider prefix differs
    // between catalog and AA (e.g. "nemotron-3-ultra" vs
    // "nvidia-nemotron-3-ultra-550b-a55b"). Uses prefix-only to avoid
    // false exact matches on shared generic suffixes.
    for catalog in &catalog_variants {
        let cat_tokens: Vec<&str> = catalog.split('-').collect();
        for benchmark in &benchmark_variants {
            let bench_tokens: Vec<&str> = benchmark.split('-').collect();
            for cat_start in 0..cat_tokens.len() {
                let cat_suffix = cat_tokens[cat_start..].join("-");
                let cat_suffix_len = cat_tokens.len() - cat_start;
                for bench_start in 0..bench_tokens.len() {
                    let bench_suffix = bench_tokens[bench_start..].join("-");
                    let bench_suffix_len = bench_tokens.len() - bench_start;
                    if cat_suffix == bench_suffix {
                        continue;
                    }
                    // Require at least 3 tokens in the shorter suffix
                    // to avoid false matches on short generic suffixes
                    if cat_suffix_len.min(bench_suffix_len) < 3 {
                        continue;
                    }
                    if cat_suffix.starts_with(&format!("{bench_suffix}-"))
                        || bench_suffix.starts_with(&format!("{cat_suffix}-"))
                    {
                        return true;
                    }
                }
            }
        }
    }

    false
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
    (0..segments.len())
        .map(|index| segments[index..].join("-"))
        .collect()
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
    match state
        .routing
        .all_candidates(state.config.server.catalog_max_age_seconds)
    {
        Ok(offerings) => {
            for offering in offerings {
                let counts = model_counts
                    .entry(offering.provider)
                    .or_insert((0usize, 0usize));
                counts.0 += 1;
                if offering.is_free {
                    counts.1 += 1;
                }
            }
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": error.to_string(),
                        "type": "server_error",
                        "code": "provider_catalog_unavailable"
                    }
                })),
            )
                .into_response();
        }
    }
    let providers = state
        .providers
        .iter()
        .filter(|(_, runtime)| {
            runtime.config.api_key_secret.is_some()
                && query
                    .available
                    .is_none_or(|available| runtime.available == available)
        })
        .map(|(code_name, runtime)| {
            let config = &runtime.config;
            let (model_count, free_model_count) =
                model_counts.get(code_name).copied().unwrap_or_default();
            json!({
                "id": code_name,
                "name": config
                    .profile
                    .map(|profile| profile.display_name())
                    .unwrap_or("Custom provider"),
                "adapter": config.adapter,
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
                "allow_preview_models": config.allow_preview_models,
                "available": runtime.available,
            })
        })
        .collect::<Vec<_>>();
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
    if state.config.server.auto_frontier_enabled {
        ids.push("auto-frontier".to_owned());
    }
    ids.extend(
        state
            .config
            .models
            .keys()
            .filter(|id| {
                !matches!(
                    id.as_str(),
                    "local" | "auto-free" | "auto-efficient" | "auto-frontier"
                )
            })
            .cloned(),
    );
    if let Ok(offerings) = routing_operation(state.routing.clone(), {
        let max_age = state.config.server.catalog_max_age_seconds;
        move |routing| routing.all_candidates(max_age)
    })
    .await
    {
        let model_denylist = &state.config.server.model_denylist;
        for offering in &offerings {
            if offering.is_free {
                continue;
            }
            let Some(config) = state.config.providers.get(&offering.provider) else {
                continue;
            };
            if !matches!(
                config.billing_mode,
                BillingMode::Paid | BillingMode::Subscription
            ) {
                continue;
            }
            let model_id = format!("{}/{}", offering.provider, offering.model);
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

#[derive(Debug, Deserialize)]
struct RankingQuery {
    task: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct FreeModelsQuery {
    task: Option<String>,
    limit: Option<usize>,
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaidModelsQuery {
    task: Option<String>,
    limit: Option<usize>,
    provider: Option<String>,
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
                "latency_seconds": model.latency_seconds,
                "reasoning_effort": model.reasoning_effort,
                "as_of": model.as_of,
                "release_date": model.release_date
            })
        })
        .collect()
}

fn select_efficient_model(
    models: &[BenchmarkModel],
    task: TaskKind,
    quality_floor: f64,
) -> Option<Value> {
    let scored: Vec<ScoredCandidate<&BenchmarkModel>> = models
        .iter()
        .filter_map(|model| {
            let quality = quality_for(model, task)?;
            if quality < quality_floor {
                return None;
            }
            let cost = match (
                model.input_price_per_million,
                model.output_price_per_million,
            ) {
                (Some(input), Some(output)) => {
                    ((1000.0 * input + 500.0 * output) / 1_000_000.0 * 1_000_000.0).max(0.0) as u64
                }
                _ => 0,
            };
            let latency = model.latency_seconds.unwrap_or(f64::MAX);
            Some(ScoredCandidate {
                quality,
                expected_cost_microusd: cost.max(1),
                latency_seconds: latency,
                value: model,
            })
        })
        .collect();
    let mut ranked = pareto_rank(scored);
    ranked.sort_by(|a, b| {
        a.expected_cost_microusd
            .cmp(&b.expected_cost_microusd)
            .then_with(|| a.latency_seconds.total_cmp(&b.latency_seconds))
            .then_with(|| b.quality.total_cmp(&a.quality))
    });
    ranked.into_iter().next().map(|c| {
        json!({
            "model": c.value.id,
            "creator": c.value.creator,
            "quality": c.quality,
            "expected_cost_microusd": c.expected_cost_microusd,
            "latency_seconds": c.value.latency_seconds,
            "input_price_per_million": c.value.input_price_per_million,
            "output_price_per_million": c.value.output_price_per_million,
        })
    })
}

fn select_free_model(
    offerings: &[CatalogOffering],
    benchmark_map: &BTreeMap<String, Vec<BenchmarkModel>>,
    providers: &BTreeMap<String, ProviderConfig>,
    runtimes: &BTreeMap<String, ProviderRuntime>,
    server: &ServerConfig,
    task: TaskKind,
    quality_floor: f64,
) -> Option<Value> {
    let mut scored = Vec::new();
    for offering in offerings {
        let Some(provider) = providers.get(&offering.provider) else {
            continue;
        };
        let Some(runtime) = runtimes.get(&offering.provider) else {
            continue;
        };
        if !runtime.available {
            continue;
        }
        let canonical = provider
            .model_mappings
            .get(&offering.model)
            .map(String::as_str)
            .unwrap_or(&offering.model);
        let benchmark = find_benchmark(benchmark_map, canonical, task);
        let quality = benchmark.and_then(|b| quality_for(b, task))?;
        if quality < quality_floor {
            continue;
        }
        if !server.free_models_quality.passes(
            task,
            benchmark,
            offering.refreshed_at,
            offering.input_price_per_million,
            offering.output_price_per_million,
            offering.context_length,
            canonical,
        ) {
            continue;
        }
        let latency = benchmark
            .and_then(|b| b.latency_seconds)
            .unwrap_or(f64::MAX);
        scored.push(ScoredCandidate {
            quality,
            expected_cost_microusd: 0,
            latency_seconds: latency,
            value: (offering.model.clone(), offering.provider.clone()),
        });
    }
    let mut ranked = pareto_rank(scored);
    ranked.sort_by(|a, b| {
        a.expected_cost_microusd
            .cmp(&b.expected_cost_microusd)
            .then_with(|| a.latency_seconds.total_cmp(&b.latency_seconds))
            .then_with(|| b.quality.total_cmp(&a.quality))
    });
    ranked.into_iter().next().map(|c| {
        json!({
            "model": c.value.0,
            "provider": c.value.1,
            "quality": c.quality,
            "latency_seconds": c.latency_seconds,
        })
    })
}

async fn list_auto_models(State(state): State<AppState>) -> Response {
    let max_age = state.config.server.benchmark_max_age_seconds;
    let catalog_age = state.config.server.catalog_max_age_seconds;

    let (benchmarks, free_offerings) = match tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.free_candidates(catalog_age)
        }),
    ) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "routing state unavailable", "type": "server_error", "code": "routing_unavailable"}})),
            )
                .into_response();
        }
    };

    let mut benchmark_map = BTreeMap::new();
    for b in &benchmarks {
        benchmark_map
            .entry(b.id.clone())
            .or_insert_with(Vec::new)
            .push(b.clone());
    }

    let tasks = [TaskKind::General, TaskKind::Coding, TaskKind::Agentic];
    let complexities = [Complexity::Simple, Complexity::Medium, Complexity::Complex];

    let mut efficient_routes = Vec::new();
    for &task in &tasks {
        for &complexity in &complexities {
            let floor = match complexity {
                Complexity::Simple => state.config.server.quality_floor_simple,
                Complexity::Medium => state.config.server.quality_floor_medium,
                Complexity::Complex => state.config.server.quality_floor_complex,
            };
            if let Some(entry) = select_efficient_model(&benchmarks, task, floor) {
                efficient_routes.push(json!({
                    "task": task.as_str(),
                    "complexity": complexity.as_str(),
                    "quality_floor": floor,
                    "selection": entry,
                }));
            }
        }
    }

    let mut free_routes = Vec::new();
    for &task in &tasks {
        for &complexity in &complexities {
            let floor = match complexity {
                Complexity::Simple => state.config.server.free_quality_floor_simple,
                Complexity::Medium => state.config.server.free_quality_floor_medium,
                Complexity::Complex => state.config.server.free_quality_floor_complex,
            };
            if let Some(entry) = select_free_model(
                &free_offerings,
                &benchmark_map,
                &state.config.providers,
                &state.providers,
                &state.config.server,
                task,
                floor,
            ) {
                free_routes.push(json!({
                    "task": task.as_str(),
                    "complexity": complexity.as_str(),
                    "quality_floor": floor,
                    "selection": entry,
                }));
            }
        }
    }

    Json(json!({
        "object": "auto_models",
        "routes": {
            "efficient": {
                "label": "Auto-Efficient",
                "tiers": {
                    "simple": {"quality_floor": state.config.server.quality_floor_simple},
                    "medium": {"quality_floor": state.config.server.quality_floor_medium},
                    "complex": {"quality_floor": state.config.server.quality_floor_complex}
                },
                "routing": efficient_routes
            },
            "free": {
                "label": "Auto-Free",
                "tiers": {
                    "simple": {"quality_floor": state.config.server.free_quality_floor_simple},
                    "medium": {"quality_floor": state.config.server.free_quality_floor_medium},
                    "complex": {"quality_floor": state.config.server.free_quality_floor_complex}
                },
                "routing": free_routes
            }
        }
    }))
    .into_response()
}

async fn list_free_models(
    State(state): State<AppState>,
    Query(query): Query<FreeModelsQuery>,
) -> Response {
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
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;

    let (offerings, benchmarks) = match tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.free_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        })
    ) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "routing state is unavailable",
                        "type": "server_error",
                        "code": "routing_state_unavailable"
                    }
                })),
            )
                .into_response();
        }
    };

    let mut benchmark_map = BTreeMap::new();
    for b in &benchmarks {
        benchmark_map
            .entry(b.id.clone())
            .or_insert_with(Vec::new)
            .push(b.clone());
    }

    let mut providers = BTreeMap::new();
    let mut data = Vec::new();

    for offering in &offerings {
        if provider_filter.is_some_and(|provider| provider != offering.provider) {
            continue;
        }
        let Some(config) = state.config.providers.get(&offering.provider) else {
            continue;
        };
        let Some(runtime) = state.providers.get(&offering.provider) else {
            continue;
        };
        if !runtime.available
            || (!config.model_allowlist.is_empty()
                && !config
                    .model_allowlist
                    .iter()
                    .any(|model| model == &offering.model))
            || config
                .model_denylist
                .iter()
                .any(|model| model == &offering.model)
        {
            continue;
        }
        if is_model_denied(&offering.model, &offering.provider, &state.config.server) {
            continue;
        }

        let canonical = config
            .model_mappings
            .get(&offering.model)
            .map(String::as_str)
            .unwrap_or(&offering.model);
        let benchmark = find_benchmark(&benchmark_map, canonical, task);
        let quality = benchmark.and_then(|benchmark| quality_for(benchmark, task));

        let effective_input = offering
            .input_price_per_million
            .or_else(|| benchmark.and_then(|b| b.input_price_per_million));
        let effective_output = offering
            .output_price_per_million
            .or_else(|| benchmark.and_then(|b| b.output_price_per_million));
        if !state.config.server.free_models_quality.passes(
            task,
            benchmark,
            offering.refreshed_at,
            effective_input,
            effective_output,
            offering.context_length,
            canonical,
        ) {
            continue;
        }

        let reference = quota_reference(config, &offering.model);
        let limits = reference
            .as_ref()
            .map(|r| r.rules.clone())
            .unwrap_or_default();
        let limit_kind = reference.as_ref().map(|r| r.as_of).unwrap_or("unknown");
        let source_url = reference.as_ref().map(|r| r.source_url);

        providers.entry(offering.provider.clone()).or_insert_with(|| {
            json!({
                "name": config.profile
                    .map(|profile| profile.definition().display_name.to_owned())
                    .unwrap_or_else(|| offering.provider.clone()),
                "billing_mode": match config.billing_mode {
                    BillingMode::Free => "free",
                    BillingMode::Paid => "paid",
                    BillingMode::Subscription => "subscription",
                },
                "limits": limits.iter().map(|limit| json!({
                    "kind": serde_json::to_string(&limit.kind).unwrap_or_else(|_| "unknown".to_owned()).trim_matches('"'),
                    "limit": limit.limit,
                    "window_seconds": limit.window_seconds,
                })).collect::<Vec<_>>(),
                "limit_status": limit_kind,
                "limit_reference": source_url,
            })
        });

        data.push((
            quality,
            benchmark.cloned(),
            offering.clone(),
            limit_kind,
            source_url,
        ));
    }

    // Deduplicate: if two offerings have the same normalized canonical ID,
    // keep the one with higher quality (or the first if equal).
    let mut seen = std::collections::HashSet::new();
    data.retain(|(_, _, offering, _, _)| {
        let normalized = normalize_identifier(&offering.model);
        seen.insert(normalized)
    });

    // Auto-route models (kilo-auto/free, openrouter/free, orcarouter/free, etc.)
    // can't be benchmarked individually. Assign them the best quality of any
    // non-auto-route model from the same provider, so they rank at the top
    // of their provider's offerings.
    // Note: :free suffixed models (kat-coder-pro-v2.5:free) are regular free-tier
    // models with benchmarks — they are NOT auto-routes.
    let is_auto_route = |model: &str| -> bool {
        model.ends_with("/free")
            && (model.contains("kilo-auto")
                || model.contains("openrouter")
                || model.contains("orcarouter"))
    };

    let mut best_per_provider: BTreeMap<String, (f64, BenchmarkModel)> = BTreeMap::new();
    for (quality, benchmark, offering, _, _) in &data {
        if is_auto_route(&offering.model) {
            continue;
        }
        if let (Some(quality), Some(benchmark)) = (quality, benchmark) {
            let entry = best_per_provider
                .entry(offering.provider.clone())
                .or_insert_with(|| (*quality, benchmark.clone()));
            if *quality > entry.0 {
                *entry = (*quality, benchmark.clone());
            }
        }
    }
    for entry in &mut data {
        if entry.0.is_some() || !is_auto_route(&entry.2.model) {
            continue;
        }
        if let Some((best_q, best_bm)) = best_per_provider.get(&entry.2.provider) {
            entry.0 = Some(*best_q);
            entry.1 = Some(best_bm.clone());
        }
    }

    data.sort_by(
        |(left_q, _, left_o, left_kind, _), (right_q, _, right_o, right_kind, _)| {
            let left_benchmarked = left_q.is_some();
            let right_benchmarked = right_q.is_some();
            right_benchmarked
                .cmp(&left_benchmarked)
                .then_with(|| right_q.unwrap_or(0.0).total_cmp(&left_q.unwrap_or(0.0)))
                .then_with(|| {
                    let left_known = *left_kind != "unknown";
                    let right_known = *right_kind != "unknown";
                    right_known.cmp(&left_known)
                })
                .then_with(|| left_o.provider.cmp(&right_o.provider))
                .then_with(|| left_o.model.cmp(&right_o.model))
        },
    );

    let ranked = data
        .into_iter()
        .take(limit)
        .enumerate()
        .map(
            |(index, (quality, benchmark, offering, limit_kind, source_url))| {
                json!({
                    "rank": index + 1,
                "provider": offering.provider,
                "model": offering.model,
                "quality": quality,
                "scores": {
                    "general": benchmark.as_ref().and_then(|benchmark| benchmark.intelligence),
                    "coding": benchmark.as_ref().and_then(|benchmark| benchmark.coding_quality),
                    "agentic": benchmark.as_ref().and_then(|benchmark| benchmark.agentic_quality),
                },
                    "capabilities": {
                        "context_length": offering.context_length,
                        "supports_tools": offering.supports_tools,
                        "supports_vision": offering.supports_vision,
                        "supports_structured_output": offering.supports_structured_output,
                    },
                    "input_price_per_million": offering.input_price_per_million
                        .or_else(|| benchmark.as_ref().and_then(|b| b.input_price_per_million)),
                    "output_price_per_million": offering.output_price_per_million
                        .or_else(|| benchmark.as_ref().and_then(|b| b.output_price_per_million)),
                    "limit_status": limit_kind,
                    "reference_url": source_url,
                })
            },
        )
        .collect::<Vec<_>>();

    Json(json!({
        "object": "free-models",
        "task": task.as_str(),
        "provider": provider_filter,
        "catalog_max_age_seconds": max_age,
        "benchmark_max_age_seconds": benchmark_max_age,
        "providers": providers,
        "data": ranked
    }))
    .into_response()
}

async fn list_paid_models(
    State(state): State<AppState>,
    Query(query): Query<PaidModelsQuery>,
) -> Response {
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
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;

    let (offerings, benchmarks) = match tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.all_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        })
    ) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "routing state is unavailable",
                        "type": "server_error",
                        "code": "routing_state_unavailable"
                    }
                })),
            )
                .into_response();
        }
    };

    let mut benchmark_map = BTreeMap::new();
    for b in &benchmarks {
        benchmark_map
            .entry(b.id.clone())
            .or_insert_with(Vec::new)
            .push(b.clone());
    }

    let mut providers = BTreeMap::new();
    let mut data = Vec::new();

    for offering in &offerings {
        if provider_filter.is_some_and(|provider| provider != offering.provider) {
            continue;
        }
        let Some(config) = state.config.providers.get(&offering.provider) else {
            continue;
        };
        let Some(runtime) = state.providers.get(&offering.provider) else {
            continue;
        };
        if !runtime.available
            || !matches!(
                config.billing_mode,
                BillingMode::Paid | BillingMode::Subscription
            )
            || (!config.model_allowlist.is_empty()
                && !config
                    .model_allowlist
                    .iter()
                    .any(|model| model == &offering.model))
            || config
                .model_denylist
                .iter()
                .any(|model| model == &offering.model)
        {
            continue;
        }
        if offering.is_free {
            continue;
        }
        if is_model_denied(&offering.model, &offering.provider, &state.config.server) {
            continue;
        }

        let canonical = config
            .model_mappings
            .get(&offering.model)
            .map(String::as_str)
            .unwrap_or(&offering.model);
        let benchmark = find_benchmark(&benchmark_map, canonical, task);
        let quality = benchmark.and_then(|benchmark| quality_for(benchmark, task));

        providers
            .entry(offering.provider.clone())
            .or_insert_with(|| {
                let billing_label = match config.billing_mode {
                    BillingMode::Paid => "paid",
                    BillingMode::Subscription => "subscription",
                    BillingMode::Free => "free",
                };
                json!({
                    "name": config.profile
                        .map(|profile| profile.definition().display_name.to_owned())
                        .unwrap_or_else(|| offering.provider.clone()),
                    "billing_mode": billing_label,
                })
            });

        data.push((quality, benchmark.cloned(), offering.clone()));
    }

    let mut seen = std::collections::HashSet::new();
    data.retain(|(_, _, offering)| {
        let normalized = normalize_identifier(&offering.model);
        seen.insert(normalized)
    });

    data.sort_by(|left, right| {
        let (left_quality, _, left_offering) = left;
        let (right_quality, _, right_offering) = right;
        match (left_quality, right_quality) {
            (Some(lq), Some(rq)) => rq
                .total_cmp(lq)
                .then_with(|| left_offering.model.cmp(&right_offering.model)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left_offering.model.cmp(&right_offering.model),
        }
    });

    let ranked = data
        .into_iter()
        .take(limit)
        .map(|(_quality, benchmark, offering)| {
            let input_price = offering
                .input_price_per_million
                .or_else(|| benchmark.as_ref().and_then(|b| b.input_price_per_million));
            let output_price = offering
                .output_price_per_million
                .or_else(|| benchmark.as_ref().and_then(|b| b.output_price_per_million));
            json!({
                "model": offering.model,
                "provider": offering.provider,
                "context_length": offering.context_length,
                "supports_tools": offering.supports_tools,
                "supports_vision": offering.supports_vision,
                "supports_structured_output": offering.supports_structured_output,
                "input_price_per_million": input_price,
                "output_price_per_million": output_price,
                "scores": if let Some(b) = benchmark {
                    json!({
                        "general": b.intelligence,
                        "coding": b.coding_quality,
                        "agentic": b.agentic_quality
                    })
                } else {
                    json!(null)
                },
            })
        })
        .collect::<Vec<_>>();

    Json(json!({
        "object": "paid-models",
        "task": task.as_str(),
        "provider": provider_filter,
        "catalog_max_age_seconds": max_age,
        "benchmark_max_age_seconds": benchmark_max_age,
        "providers": providers,
        "data": ranked
    }))
    .into_response()
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
    let mut frontier_exhaustion_code = None;
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
                Ok(ReservationOutcome::Cooldown) => {
                    frontier_exhaustion_code.get_or_insert("frontier_all_candidates_unhealthy");
                    continue;
                }
                Ok(ReservationOutcome::QuotaExceeded(QuotaKind::CostMicrousd)) => {
                    frontier_exhaustion_code = Some("frontier_spend_cap_reached");
                    continue;
                }
                Ok(ReservationOutcome::QuotaExceeded(_)) => {
                    if frontier_exhaustion_code != Some("frontier_spend_cap_reached") {
                        frontier_exhaustion_code = Some("frontier_quota_exhausted");
                    }
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
        None if model == "auto-frontier" => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            request_id,
            "all eligible frontier candidates are exhausted or unhealthy",
            "upstream_error",
            Some(frontier_exhaustion_code.unwrap_or("frontier_all_candidates_unhealthy")),
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
    if model == "auto-frontier" {
        if !state.config.server.auto_frontier_enabled {
            return Err((
                StatusCode::NOT_FOUND,
                "model 'auto-frontier' is disabled".to_owned(),
                "route_disabled",
            ));
        }
        return resolve_auto_frontier_targets(state, request, session_hash).await;
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
    SelectedTarget {
        runtime_provider: target.provider.clone(),
        provider: target.provider.clone(),
        quota_scope: target.provider.clone(),
        provider_display,
        model: target.model.clone(),
        managed: false,
        quotas: Vec::new(),
        expected_cost_microusd: 0,
        input_price_per_million: None,
        output_price_per_million: None,
        reasoning_effort: None,
        selection: None,
    }
}

struct FreeCandidate {
    target: SelectedTarget,
    quality: Option<f64>,
    latency_seconds: f64,
}

async fn resolve_auto_free_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let max_age = state.config.server.catalog_max_age_seconds;
    let benchmark_max_age = state.config.server.benchmark_max_age_seconds;
    let (offerings, benchmarks, benchmark_snapshot) = tokio::try_join!(
        routing_operation(state.routing.clone(), move |routing| {
            routing.free_candidates(max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.benchmark_models(benchmark_max_age)
        }),
        routing_operation(state.routing.clone(), move |routing| {
            routing.active_benchmark_snapshot(benchmark_max_age)
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
    let classification = classify(request);
    let quality_floor = match classification.complexity {
        Complexity::Simple => state.config.server.free_quality_floor_simple,
        Complexity::Medium => state.config.server.free_quality_floor_medium,
        Complexity::Complex => state.config.server.free_quality_floor_complex,
    };
    let requirements = RequestRequirements::from_request(request);
    let candidates = offerings
        .into_iter()
        .filter_map(|offering| {
            let provider = state.config.providers.get(&offering.provider)?;
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
            let canonical = provider
                .model_mappings
                .get(&offering.model)
                .cloned()
                .unwrap_or_else(|| offering.model.clone());
            let benchmark = find_benchmark(&benchmark_map, &canonical, classification.task);
            let quality = benchmark.and_then(|b| quality_for(b, classification.task));
            let effective_input = offering
                .input_price_per_million
                .or_else(|| benchmark.and_then(|b| b.input_price_per_million));
            let effective_output = offering
                .output_price_per_million
                .or_else(|| benchmark.and_then(|b| b.output_price_per_million));
            if !state.config.server.free_models_quality.passes(
                classification.task,
                benchmark,
                offering.refreshed_at,
                effective_input,
                effective_output,
                offering.context_length,
                &canonical,
            ) {
                return None;
            }
            if quality.is_some_and(|q| q < quality_floor) {
                return None;
            }
            let latency = benchmark
                .and_then(|b| b.latency_seconds)
                .unwrap_or(f64::MAX);
            Some(FreeCandidate {
                quality,
                latency_seconds: latency,
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
                        canonical_model: canonical,
                        task: classification.task.as_str(),
                        complexity: classification.complexity.as_str(),
                        classifier_version: classification.version,
                        quality_floor,
                        quality: quality.unwrap_or(0.0),
                        expected_cost_microusd: 0,
                        benchmark_snapshot_id,
                        benchmark_as_of,
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
                expected_cost_microusd: 0,
                latency_seconds: c.latency_seconds,
                value: c,
            }),
            None => unbenchmarked.push(c),
        }
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

async fn resolve_auto_frontier_targets(
    state: &AppState,
    request: &Value,
    session_hash: Option<&str>,
) -> Result<Vec<SelectedTarget>, (StatusCode, String, &'static str)> {
    let targets =
        resolve_benchmark_targets(state, request, session_hash, BenchmarkPolicy::Frontier).await?;
    Ok(targets)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BenchmarkPolicy {
    Efficient,
    Frontier,
}

impl BenchmarkPolicy {
    const fn route(self) -> &'static str {
        match self {
            Self::Efficient => "auto-efficient",
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
    let quality_floor = match (policy, classification.complexity) {
        (BenchmarkPolicy::Efficient, Complexity::Simple) => {
            state.config.server.quality_floor_simple
        }
        (BenchmarkPolicy::Efficient, Complexity::Medium) => {
            state.config.server.quality_floor_medium
        }
        (BenchmarkPolicy::Efficient, Complexity::Complex) => {
            state.config.server.quality_floor_complex
        }
        (BenchmarkPolicy::Frontier, Complexity::Simple) => {
            state.config.server.frontier_quality_floor_simple
        }
        (BenchmarkPolicy::Frontier, Complexity::Medium) => {
            state.config.server.frontier_quality_floor_medium
        }
        (BenchmarkPolicy::Frontier, Complexity::Complex) => {
            state.config.server.frontier_quality_floor_complex
        }
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
    let mut candidates = Vec::new();
    let mut frontier_saw_mapping = false;
    let mut frontier_saw_identity = false;
    let mut frontier_saw_billing = false;
    let mut frontier_saw_available = false;
    let mut frontier_preview_blocked = false;
    let mut frontier_reached_capability = false;
    let mut frontier_saw_capability = false;
    let mut frontier_saw_quality = false;
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
        let canonical = provider
            .model_mappings
            .get(&offering.model)
            .map(String::as_str)
            .unwrap_or(&offering.model);
        let Some(model_benchmarks) = benchmark_by_model.get(canonical) else {
            continue;
        };
        if policy == BenchmarkPolicy::Frontier {
            frontier_saw_mapping = true;
            if !model_benchmarks
                .iter()
                .any(|benchmark| is_frontier_model(benchmark.creator.as_deref(), canonical))
            {
                continue;
            }
            frontier_saw_identity = true;
            if provider.billing_mode == BillingMode::Free {
                continue;
            }
            frontier_saw_billing = true;
            if !runtime.available {
                continue;
            }
            frontier_saw_available = true;
            if (is_preview_model(canonical) || is_preview_model(&offering.model))
                && !provider.allow_preview_models
            {
                frontier_preview_blocked = true;
                continue;
            }
        } else if !runtime.available
            || (!offering.is_free && provider.billing_mode == BillingMode::Free)
        {
            continue;
        }
        let capability_mismatch = offering
            .context_length
            .is_some_and(|context| context < requirements.estimated_tokens)
            || (requirements.tools && offering.supports_tools != Some(true))
            || (requirements.vision && offering.supports_vision != Some(true))
            || (requirements.structured && offering.supports_structured_output != Some(true));
        if policy == BenchmarkPolicy::Frontier {
            frontier_reached_capability = true;
        }
        if capability_mismatch {
            continue;
        }
        if is_model_denied(&offering.model, &offering.provider, &state.config.server) {
            continue;
        }
        if policy == BenchmarkPolicy::Frontier {
            frontier_saw_capability = true;
        }
        let has_effort_variants = model_benchmarks
            .iter()
            .any(|benchmark| benchmark.reasoning_effort.is_some());
        let requested_effort_supported = requested_effort.is_some_and(|effort| {
            model_benchmarks
                .iter()
                .any(|benchmark| benchmark.reasoning_effort.as_deref() == Some(effort))
        });
        for benchmark in model_benchmarks {
            if policy == BenchmarkPolicy::Frontier
                && !is_frontier_model(benchmark.creator.as_deref(), canonical)
            {
                continue;
            }
            if requested_effort_supported
                && has_effort_variants
                && benchmark.reasoning_effort.as_deref() != requested_effort
            {
                continue;
            }
            let Some(raw_quality) = quality_for(benchmark, classification.task) else {
                continue;
            };
            let quality = raw_quality;
            if quality < quality_floor {
                continue;
            }
            if policy == BenchmarkPolicy::Frontier {
                frontier_saw_quality = true;
            }
            let expected_cost_microusd = if offering.is_free {
                0
            } else {
                let (Some(input_price), Some(output_price)) = (
                    offering
                        .input_price_per_million
                        .or(benchmark.input_price_per_million),
                    offering
                        .output_price_per_million
                        .or(benchmark.output_price_per_million),
                ) else {
                    continue;
                };
                expected_cost_microusd(
                    requirements.estimated_input_tokens,
                    benchmark
                        .output_tokens_per_task
                        .unwrap_or(requirements.estimated_output_tokens)
                        .min(requirements.estimated_output_tokens),
                    input_price,
                    output_price,
                )
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
                    input_price_per_million: offering
                        .input_price_per_million
                        .or(benchmark.input_price_per_million),
                    output_price_per_million: offering
                        .output_price_per_million
                        .or(benchmark.output_price_per_million),
                    reasoning_effort: benchmark.reasoning_effort.clone(),
                    selection: Some(SelectionMetadata {
                        canonical_model: canonical.to_owned(),
                        task: classification.task.as_str(),
                        complexity: classification.complexity.as_str(),
                        classifier_version: classification.version,
                        quality_floor,
                        quality,
                        expected_cost_microusd,
                        benchmark_snapshot_id,
                        benchmark_as_of,
                    }),
                },
                quality,
                expected_cost_microusd,
                latency_seconds: benchmark.latency_seconds.unwrap_or(f64::MAX),
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
    let mut targets = pareto_rank(candidates)
        .into_iter()
        .map(|candidate| candidate.value)
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        let left_pinned = pinned
            .as_ref()
            .is_some_and(|pin| pin.0 == left.provider && pin.1 == left.model);
        let right_pinned = pinned
            .as_ref()
            .is_some_and(|pin| pin.0 == right.provider && pin.1 == right.model);
        left.expected_cost_microusd
            .cmp(&right.expected_cost_microusd)
            .then_with(|| right_pinned.cmp(&left_pinned))
            .then_with(|| (&left.provider, &left.model).cmp(&(&right.provider, &right.model)))
    });
    if policy == BenchmarkPolicy::Frontier && targets.is_empty() {
        let (message, code) = if !frontier_saw_mapping {
            (
                "no configured offering has a fresh canonical benchmark mapping",
                "frontier_no_benchmark_mapping",
            )
        } else if !frontier_saw_identity {
            (
                "no mapping identifies an OpenAI GPT/reasoning or Anthropic Claude model",
                "frontier_access_unconfigured",
            )
        } else if !frontier_saw_billing {
            (
                "frontier provider billing is not explicitly authorized",
                "frontier_billing_not_authorized",
            )
        } else if !frontier_saw_available {
            (
                "configured frontier provider credentials are unavailable",
                "frontier_access_unavailable",
            )
        } else if frontier_preview_blocked && !frontier_reached_capability {
            (
                "frontier preview models require explicit provider authorization",
                "frontier_preview_not_authorized",
            )
        } else if frontier_reached_capability && !frontier_saw_capability {
            (
                "no frontier candidate satisfies the request capabilities",
                "frontier_capability_mismatch",
            )
        } else if !frontier_saw_quality {
            (
                "no frontier candidate clears the configured quality floor",
                "frontier_quality_floor_not_met",
            )
        } else {
            (
                "no frontier candidate is safely available",
                "frontier_no_candidate",
            )
        };
        return Err((StatusCode::SERVICE_UNAVAILABLE, message.to_owned(), code));
    }
    Ok(targets)
}

fn is_reasoning_effort(effort: &str) -> bool {
    matches!(
        effort.to_ascii_lowercase().as_str(),
        "low" | "medium" | "high" | "xhigh"
    )
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
        } else if let Some(position) = buffer.windows(2).position(|window| window == b"\n\n") {
            (position, 2)
        } else {
            return None;
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
        RequestRequirements, StreamChoice, benchmark_ids_match, decorate_json_response,
        estimate_request_tokens, expected_cost_microusd, find_benchmark, find_benchmark_raw,
        is_fallback_status, log_request, malformed_sse_event, rank_benchmark_models,
        rate_limit_reset_delay, request_id, session_material, strip_model_noise, take_sse_event,
        transform_sse_event,
    };
    use crate::benchmarks::{BenchmarkModel, TaskKind};
    use axum::http::{HeaderMap, StatusCode};
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
            find_benchmark(&benchmarks, "models/gemini-2.5-flash", TaskKind::General)
                .expect("Gemini benchmark")
                .intelligence,
            Some(80.0)
        );
    }

    #[test]
    fn find_benchmark_prefers_exact_over_prefix_and_penalizes_variant_keywords() {
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
        let result = find_benchmark(&benchmarks, "xiaomimimo/mimo-v2-flash", TaskKind::General)
            .expect("mimo-v2-flash");
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
        let result = find_benchmark(&benchmarks, "xiaomimimo/mimo-v2.5", TaskKind::General)
            .expect("mimo-v2.5");
        assert_eq!(result.intelligence, Some(37.2));
        assert_eq!(
            result.id, "mimo-v2-5-0424",
            "should prefer base variant over penalized keywords"
        );

        // When only variant-keyword benchmarks exist, pick the least-penalized
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
        let result = find_benchmark(&benchmarks, "xiaomimimo/mimo-v2.5", TaskKind::General)
            .expect("mimo-v2.5 pro only");
        assert_eq!(result.intelligence, Some(42.2));
        assert_eq!(result.id, "mimo-v2-5-pro");
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
    fn strip_model_noise_removes_thinking_suffix() {
        assert_eq!(
            strip_model_noise("qwen/qwen3-235b-a22b-thinking-2507"),
            "qwen/qwen3-235b-a22b"
        );
        assert_eq!(
            strip_model_noise("qwen/qwen3-omni-30b-a3b-thinking"),
            "qwen/qwen3-omni-30b-a3b"
        );
        assert_eq!(
            strip_model_noise("qwen/qwen3-vl-235b-a22b-thinking"),
            "qwen/qwen3-vl-235b-a22b"
        );
    }

    #[test]
    fn strip_model_noise_removes_latest_suffix() {
        // "latest" is a noise token removed from any segment
        assert_eq!(strip_model_noise("mistral-tiny-latest"), "mistral-tiny");
        assert_eq!(strip_model_noise("ministral-14b-latest"), "ministral-14b");
    }

    #[test]
    fn strip_model_noise_removes_date_codes() {
        assert_eq!(strip_model_noise("ministral-14b-2512"), "ministral-14b");
        assert_eq!(strip_model_noise("mistral-tiny-2407"), "mistral-tiny");
        assert_eq!(
            strip_model_noise("qwen/qwen3-30b-a3b-2507"),
            "qwen/qwen3-30b-a3b"
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
    fn find_benchmark_falls_back_to_noise_stripped_catalog_id() {
        let mut benchmarks = std::collections::BTreeMap::new();
        // AA benchmark without quantization suffix
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
        // Catalog has -fp8 suffix that should be stripped
        let result = find_benchmark(&benchmarks, "qwen/qwen3-30b-a3b-fp8", TaskKind::General)
            .expect("noise-stripped match");
        assert_eq!(result.intelligence, Some(6.8));
        assert_eq!(result.id, "qwen3-30b-a3b-instruct");
    }

    #[test]
    fn noise_stripped_matching_uses_original_model_when_match_exists() {
        let mut benchmarks = std::collections::BTreeMap::new();
        // Benchmark for the original (unstoned) model ID
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
        // Benchmark for the noise-stripped version
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
        // Should prefer the original match (stripped = "qwen-qwen3-30b-a3b" matches "qwen3-30b-a3b")
        let result = find_benchmark(&benchmarks, "qwen/qwen3-30b-a3b-fp8", TaskKind::General)
            .expect("noise-stripped match");
        // The noise-stripped model "qwen-qwen3-30b-a3b" has an exact match
        // with the benchmark "qwen3-30b-a3b", which has higher score (10.0 vs 6.8)
        assert_eq!(result.intelligence, Some(10.0));
    }

    #[test]
    fn find_benchmark_raw_still_works_without_noise() {
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
        // find_benchmark_raw should still match correctly
        let result = find_benchmark_raw(&benchmarks, "models/gemini-2.5-flash", TaskKind::General)
            .expect("raw match");
        assert_eq!(result.intelligence, Some(80.0));
    }
}
