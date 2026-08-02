use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, Bytes};
use axum::extract::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, serve};
use futures_util::{StreamExt, stream};
use model_gateway::benchmarks::BenchmarkModel;
use model_gateway::config::{
    BillingMode, Config, ModelConfig, ProviderConfig, ProviderProfileId, QuotaBoundary, QuotaKind,
    QuotaLimit, ServerConfig, TargetConfig,
};
use model_gateway::gateway::build_app;
use model_gateway::identity::{
    IdentityAliasRecord, IdentityConfidence, IdentityEntityRecord, IdentityImport,
};
use model_gateway::pricing::{PriceObservation, PriceRates, PriceScope, PriceSourceKind};
use model_gateway::providers::AccountLimit;
use model_gateway::routing::{AccessKind, CatalogRecord, RoutingStore};
use model_gateway::secrets::SecretResolver;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::Duration;

async fn spawn_provider(response: ProviderResponse) -> SocketAddr {
    let response = Arc::new(response);
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let response = response.clone();
            async move { response.as_ref().clone().respond(body) }
        }),
    );
    spawn_router(router).await
}

async fn spawn_local_provider(models: Vec<&'static str>) -> SocketAddr {
    let router = Router::new()
        .route(
            "/v1/models",
            get(move || async move {
                Json(json!({
                    "object": "list",
                    "data": models
                        .iter()
                        .map(|model| json!({"id": model, "object": "model"}))
                        .collect::<Vec<_>>()
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move { ProviderResponse::Success.respond(body) }),
        );
    spawn_router(router).await
}

async fn spawn_reloading_local_provider() -> (SocketAddr, Arc<AtomicUsize>) {
    let discoveries = Arc::new(AtomicUsize::new(0));
    let get_discoveries = discoveries.clone();
    let router = Router::new()
        .route(
            "/v1/models",
            get(move || {
                let discoveries = get_discoveries.clone();
                async move {
                    let model = if discoveries.fetch_add(1, Ordering::SeqCst) == 0 {
                        "unloaded-model"
                    } else {
                        "loaded-model"
                    };
                    Json(json!({"object": "list", "data": [{"id": model}]}))
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                if body["model"] == "unloaded-model" {
                    return ProviderResponse::Failure(StatusCode::NOT_FOUND, "model unloaded")
                        .respond(body);
                }
                ProviderResponse::Success.respond(body)
            }),
        );
    (spawn_router(router).await, discoveries)
}

async fn spawn_header_echo_provider() -> (SocketAddr, Arc<AtomicUsize>) {
    let authorization_seen = Arc::new(AtomicUsize::new(0));
    let seen = authorization_seen.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                if headers.contains_key(header::AUTHORIZATION)
                    || headers.contains_key(header::COOKIE)
                    || headers.contains_key("x-forwarded-for")
                {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                Json(json!({
                    "model": body["model"],
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
                }))
            }
        }),
    );
    (spawn_router(router).await, authorization_seen)
}

async fn spawn_router(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider bind");
    let address = listener.local_addr().expect("provider address");
    tokio::spawn(async move {
        serve(listener, router).await.expect("provider server");
    });
    address
}

#[derive(Clone)]
enum ProviderResponse {
    Success,
    Failure(StatusCode, &'static str),
    Stream,
    HoldStream,
    TimedStream,
}

impl ProviderResponse {
    fn respond(self, body: Value) -> Response {
        match self {
            Self::Success => Json(json!({
                "id": "chatcmpl-smoke",
                "object": "chat.completion",
                "model": body["model"],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            }))
            .into_response(),
            Self::Failure(status, message) => {
                (status, Json(json!({"error": {"message": message}}))).into_response()
            }
            Self::Stream => {
                let chunks = stream::iter([
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n")),
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n")),
                ]);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .expect("stream response")
            }
            Self::HoldStream => {
                let chunks = async_stream::stream! {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"choices\":[]}\n\n"));
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .expect("held stream response")
            }
            Self::TimedStream => {
                let chunks = async_stream::stream! {
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n"));
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"two\"}}]}\n\n"));
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .expect("timed stream response")
            }
        }
    }
}

fn config_for(providers: BTreeMap<String, ProviderConfig>, targets: Vec<TargetConfig>) -> Config {
    Config {
        server: ServerConfig::default(),
        providers,
        models: BTreeMap::from([("smoke".to_owned(), ModelConfig { targets })]),
    }
}

fn provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        profile: None,
        base_url,
        ..ProviderConfig::default()
    }
}

async fn spawn_gateway(config: Config) -> String {
    let app = build_app(config, &SecretResolver::default()).expect("gateway app");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("gateway bind");
    let address = listener.local_addr().expect("gateway address");
    tokio::spawn(async move {
        serve(listener, app).await.expect("gateway server");
    });
    format!("http://{address}")
}

#[tokio::test]
async fn free_models_can_be_filtered_by_provider() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let mut alpha = provider("https://alpha.example/v1".to_owned());
    alpha.profile = Some(ProviderProfileId::OpenRouter);
    let mut beta = provider("https://beta.example/v1".to_owned());
    beta.profile = Some(ProviderProfileId::Groq);
    let mut config = config_for(
        BTreeMap::from([("alpha".to_owned(), alpha), ("beta".to_owned(), beta)]),
        vec![TargetConfig {
            provider: "alpha".to_owned(),
            model: "alpha-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path.clone());

    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "alpha",
            &[CatalogRecord {
                model: "shared-free".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: None,
                supports_tools: None,
                supports_vision: None,
                supports_structured_output: None,
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("alpha catalog");
    store
        .replace_catalog(
            "beta",
            &[CatalogRecord {
                model: "shared-free".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: None,
                supports_tools: None,
                supports_vision: None,
                supports_structured_output: None,
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("beta catalog");

    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=alpha&limit=10&view=full"
        ))
        .send()
        .await
        .expect("provider-filtered response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("provider-filtered body");
    assert_eq!(body["data"][0]["model"]["provider"], "alpha");
    assert_eq!(body["data"].as_array().expect("data").len(), 1);
    assert_eq!(body["data"][0]["model"]["provider"], "alpha");

    let all_response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=all&limit=10&view=full"
        ))
        .send()
        .await
        .expect("all-provider response");
    assert_eq!(all_response.status(), StatusCode::OK);
    let all_body: Value = all_response.json().await.expect("all-provider body");
    assert_eq!(all_body["data"].as_array().expect("all data").len(), 2);

    let unfiltered_response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&limit=10&view=full"
        ))
        .send()
        .await
        .expect("unfiltered response");
    let unfiltered_body: Value = unfiltered_response.json().await.expect("unfiltered body");
    assert_eq!(all_body["data"], unfiltered_body["data"]);

    let response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=missing"
        ))
        .send()
        .await
        .expect("unknown provider response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.expect("unknown provider body");
    assert_eq!(body["error"]["code"], "invalid_provider");
}

#[tokio::test]
async fn quota_limited_free_access_preserves_reference_prices_and_blocks_paid_reclassification() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "ollama-cloud",
            &[CatalogRecord {
                model: "quota-model".to_owned(),
                access_kind: AccessKind::QuotaLimitedFreeTier,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(5.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "quota-model",
                60.0,
                60.0,
                60.0,
                1.0,
                5.0,
            )],
        )
        .expect("benchmarks");
    store
        .record_account_limit(
            "ollama-cloud",
            &AccountLimit {
                limit: Some(100.0),
                usage: Some(58.0),
                remaining: Some(42.0),
                is_free_tier: Some(true),
            },
        )
        .expect("account limit");
    drop(store);

    let mut free_provider = provider("https://example.com/v1".to_owned());
    free_provider.profile = Some(ProviderProfileId::OllamaCloud);
    let mut free_config = config_for(
        BTreeMap::from([("ollama-cloud".to_owned(), free_provider.clone())]),
        vec![TargetConfig {
            provider: "ollama-cloud".to_owned(),
            model: "quota-model".to_owned(),
        }],
    );
    free_config.server.state_path = Some(state_path.clone());
    let gateway = spawn_gateway(free_config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=ollama-cloud&view=full"
        ))
        .send()
        .await
        .expect("free models response");
    let body: Value = response.json().await.expect("free models body");
    assert_eq!(body["data"][0]["access"]["kind"], "quota_limited_free_tier");
    assert_eq!(body["data"][0]["access"]["overage"], "gateway_blocked");
    assert_eq!(body["data"][0]["access"]["remaining"], 42.0);
    assert_eq!(body["data"][0]["access"]["is_free_tier"], true);
    assert_eq!(body["data"][0]["price_per_million"]["input"], 0.0);
    assert_eq!(body["data"][0]["price_per_million"]["source"], "free_tier");
    assert_eq!(body["data"][0]["reference_price_per_million"]["input"], 1.0);
    assert_eq!(
        body["data"][0]["reference_price_per_million"]["output"],
        5.0
    );

    free_provider.billing_mode = BillingMode::Paid;
    let mut paid_config = config_for(
        BTreeMap::from([("ollama-cloud".to_owned(), free_provider)]),
        vec![TargetConfig {
            provider: "ollama-cloud".to_owned(),
            model: "quota-model".to_owned(),
        }],
    );
    paid_config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(paid_config).await;
    let free_response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=ollama-cloud&view=full"
        ))
        .send()
        .await
        .expect("paid-account free response");
    let free_body: Value = free_response.json().await.expect("paid-account free body");
    assert!(free_body["data"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn exhausted_account_snapshot_excludes_quota_limited_free_models() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "ollama-cloud",
            &[CatalogRecord {
                model: "quota-model".to_owned(),
                access_kind: AccessKind::QuotaLimitedFreeTier,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.5),
                output_price_per_million: Some(2.0),
            }],
        )
        .expect("catalog");
    store
        .record_account_limit(
            "ollama-cloud",
            &AccountLimit {
                limit: Some(100.0),
                usage: Some(100.0),
                remaining: Some(0.0),
                is_free_tier: Some(true),
            },
        )
        .expect("account limit");
    drop(store);
    rusqlite::Connection::open(&state_path)
        .expect("database")
        .execute(
            "UPDATE provider_account_limits SET fetched_at = 1 WHERE provider = 'ollama-cloud'",
            [],
        )
        .expect("stale account snapshot");

    let mut ollama = provider("https://example.com/v1".to_owned());
    ollama.profile = Some(ProviderProfileId::OllamaCloud);
    let mut config = config_for(
        BTreeMap::from([("ollama-cloud".to_owned(), ollama)]),
        vec![TargetConfig {
            provider: "ollama-cloud".to_owned(),
            model: "quota-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=ollama-cloud"
        ))
        .send()
        .await
        .expect("free models response");
    let body: Value = response.json().await.expect("free models body");
    assert!(body["data"].as_array().is_some_and(Vec::is_empty));
    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/providers"))
        .send()
        .await
        .expect("providers response");
    let body: Value = response.json().await.expect("providers body");
    let ollama = body["data"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|provider| provider["id"] == "ollama-cloud")
        })
        .expect("Ollama provider");
    assert_eq!(ollama["free_model_count"], 0);
}

#[tokio::test]
async fn persisted_access_kind_does_not_override_current_provider_configuration() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "custom",
            &[CatalogRecord {
                model: "formerly-free".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    drop(store);

    let mut config = config_for(
        BTreeMap::from([(
            "custom".to_owned(),
            provider("https://example.com/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "custom".to_owned(),
            model: "formerly-free".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=custom&view=full"
        ))
        .send()
        .await
        .expect("free models response");
    let body: Value = response.json().await.expect("free models body");
    assert!(body["data"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn free_models_quality_bar_filters_low_quality_models() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "provider-a",
            &[
                CatalogRecord {
                    model: "great-model".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(2.0),
                },
                CatalogRecord {
                    model: "weak-model".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: None,
                    supports_tools: None,
                    supports_vision: None,
                    supports_structured_output: None,
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(2.0),
                },
            ],
        )
        .expect("catalog");
    let great = BenchmarkModel::fixture("great-model", 90.0, 90.0, 90.0, 1.0, 2.0);
    let weak = BenchmarkModel::fixture("weak-model", 10.0, 10.0, 10.0, 1.0, 2.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[great, weak])
        .expect("benchmarks");
    drop(store);

    let mut p = provider("https://example.com/v1".to_owned());
    p.profile = Some(ProviderProfileId::OllamaCloud);
    let mut config = config_for(
        BTreeMap::from([("provider-a".to_owned(), p)]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "great-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    // Raise the quality bar: only models >= 50.0 should pass
    config.server.free_models_quality.min_composite_quality = 50.0;
    config.server.free_models_quality.max_age_months = 0; // disable age filter
    config
        .server
        .free_models_quality
        .max_input_price_per_million = 0.0; // disable price filters
    config
        .server
        .free_models_quality
        .max_output_price_per_million = 0.0;

    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&limit=10&view=full"
        ))
        .send()
        .await
        .expect("free models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("free models body");
    let models: Vec<&str> = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .filter_map(|entry| entry["model"]["name"].as_str())
        .collect();
    assert!(
        models.contains(&"great-model"),
        "high-quality model should be present: {models:?}"
    );
    assert!(
        !models.contains(&"weak-model"),
        "low-quality model should be excluded: {models:?}"
    );
}

#[tokio::test]
async fn free_model_listing_task_changes_ranking_not_identity() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let catalog = |model: &str| CatalogRecord {
        model: model.to_owned(),
        access_kind: AccessKind::ZeroPrice,
        context_length: Some(128_000),
        supports_tools: Some(true),
        supports_vision: Some(false),
        supports_structured_output: Some(false),
        input_price_per_million: Some(0.0),
        output_price_per_million: Some(0.0),
    };
    store
        .replace_catalog(
            "provider-a",
            &[catalog("general-model"), catalog("coding-model")],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("general-model", 90.0, 20.0, 50.0, 0.0, 0.0),
                BenchmarkModel::fixture("coding-model", 60.0, 95.0, 50.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    drop(store);

    let mut config = config_for(
        BTreeMap::from([(
            "provider-a".to_owned(),
            provider("https://example.com/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "general-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.free_models_quality.min_composite_quality = 0.0;
    config.server.free_models_quality.max_age_months = 0;
    let gateway = spawn_gateway(config).await;

    for (task, expected) in [("general", "general-model"), ("coding", "coding-model")] {
        let response = reqwest::Client::new()
            .get(format!(
                "{gateway}/v1/catalog/models?access=free&task={task}&view=full"
            ))
            .send()
            .await
            .expect("free models response");
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.expect("free models body");
        assert_eq!(body["data"][0]["model"]["name"], expected);
    }
}

#[tokio::test]
async fn paid_model_listing_task_changes_ranking_not_identity() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let catalog = |model: &str| CatalogRecord {
        model: model.to_owned(),
        access_kind: AccessKind::Paid,
        context_length: Some(128_000),
        supports_tools: Some(true),
        supports_vision: Some(false),
        supports_structured_output: Some(false),
        input_price_per_million: Some(1.0),
        output_price_per_million: Some(2.0),
    };
    store
        .replace_catalog(
            "paid-provider",
            &[catalog("general-model"), catalog("coding-model")],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("general-model", 90.0, 20.0, 50.0, 1.0, 2.0),
                BenchmarkModel::fixture("coding-model", 60.0, 95.0, 50.0, 1.0, 2.0),
            ],
        )
        .expect("benchmarks");
    drop(store);

    let mut paid = provider("https://example.com/v1".to_owned());
    paid.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("paid-provider".to_owned(), paid)]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "general-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;

    for (task, expected) in [("general", "general-model"), ("coding", "coding-model")] {
        let response = reqwest::Client::new()
            .get(format!(
                "{gateway}/v1/catalog/models?access=paid&provider=paid-provider&task={task}&view=full"
            ))
            .send()
            .await
            .expect("paid models response");
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.expect("paid models body");
        assert_eq!(body["data"][0]["model"]["name"], expected);
    }
}

#[tokio::test]
async fn opencode_zen_free_models_recover_only_reviewed_benchmarks() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let free_catalog = |model: &str| CatalogRecord {
        model: model.to_owned(),
        access_kind: AccessKind::ZeroPrice,
        context_length: Some(128_000),
        supports_tools: Some(true),
        supports_vision: Some(false),
        supports_structured_output: Some(false),
        input_price_per_million: Some(0.0),
        output_price_per_million: Some(0.0),
    };
    let free_models = [
        "big-pickle",
        "deepseek-v4-flash-free",
        "laguna-s-2.1-free",
        "ling-3.0-flash-free",
        "mimo-v2.5-free",
        "nemotron-3-ultra-free",
        "north-mini-code-free",
    ];
    store
        .replace_catalog(
            "opencode-zen",
            &free_models
                .iter()
                .map(|model| free_catalog(model))
                .collect::<Vec<_>>(),
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("deepseek-v4-flash", 41.0, 41.0, 41.0, 0.0, 0.0),
                BenchmarkModel::fixture("mimo-v2-5-0424", 38.0, 38.0, 38.0, 0.0, 0.0),
                BenchmarkModel::fixture(
                    "nvidia-nemotron-3-ultra-550b-a55b",
                    38.0,
                    38.0,
                    38.0,
                    0.0,
                    0.0,
                ),
                BenchmarkModel::fixture("north-mini-code", 32.0, 32.0, 32.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    let entities = [
        ("hf:xiaomimimo/mimo-v2.5", "mimo-v2-5-0424"),
        (
            "hf:nvidia/nvidia-nemotron-3-ultra-550b-a55b-bf16",
            "nvidia-nemotron-3-ultra-550b-a55b",
        ),
    ];
    store
        .replace_identity_source(&IdentityImport {
            source: "fixture".to_owned(),
            attribution: "Fixture".to_owned(),
            entities: entities
                .iter()
                .map(|(entity_id, _)| IdentityEntityRecord {
                    id: (*entity_id).to_owned(),
                    creator: None,
                    family: None,
                    version: None,
                    variant: None,
                    release_date: None,
                    hugging_face_id: Some(entity_id.trim_start_matches("hf:").to_owned()),
                })
                .collect(),
            aliases: entities
                .iter()
                .map(|(entity_id, _)| IdentityAliasRecord {
                    source: "fixture".to_owned(),
                    provider_key: "canonical".to_owned(),
                    provider_model_id: (*entity_id).to_owned(),
                    entity_id: (*entity_id).to_owned(),
                    confidence: IdentityConfidence::CanonicalReference,
                    provenance_url: "fixture".to_owned(),
                    observed_at: 100,
                })
                .collect(),
        })
        .expect("identities");
    for (entity_id, benchmark_id) in entities {
        store
            .approve_benchmark_identity_link(entity_id, benchmark_id, "fixture")
            .expect("entity benchmark");
    }
    store
        .approve_entity_alias(
            "opencode",
            "mimo-v2.5-free",
            "hf:xiaomimimo/mimo-v2.5",
            "fixture",
        )
        .expect("MiMo alias");
    store
        .approve_entity_alias(
            "opencode",
            "nemotron-3-ultra-free",
            "hf:nvidia/nvidia-nemotron-3-ultra-550b-a55b-bf16",
            "fixture",
        )
        .expect("Nemotron alias");
    drop(store);

    let mut zen = provider("https://example.com/v1".to_owned());
    zen.pricing_profile = Some("opencode".to_owned());
    let mut config = config_for(
        BTreeMap::from([("opencode-zen".to_owned(), zen)]),
        vec![TargetConfig {
            provider: "opencode-zen".to_owned(),
            model: "deepseek-v4-flash-free".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.free_models_quality.min_composite_quality = 0.0;
    config.server.free_models_quality.max_age_months = 0;
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=opencode-zen&task=general&limit=100&view=full"
        ))
        .send()
        .await
        .expect("Zen free models");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("Zen body");
    let models = body["data"]
        .as_array()
        .expect("model array")
        .iter()
        .map(|entry| {
            (
                entry["model"]["name"].as_str().expect("model name"),
                entry["benchmark_match"].as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in body["data"].as_array().expect("model array") {
        assert_eq!(entry["price_per_million"]["input"], 0.0);
        assert_eq!(entry["price_per_million"]["output"], 0.0);
        assert_eq!(entry["price_per_million"]["source"], "provider_free");
        assert_eq!(entry["price_per_million"]["estimated"], false);
    }
    assert_eq!(models.len(), 7);
    assert_eq!(models["deepseek-v4-flash-free"], Some("exact"));
    assert_eq!(models["north-mini-code-free"], Some("exact"));
    assert_eq!(models["mimo-v2.5-free"], Some("approved"));
    assert_eq!(models["nemotron-3-ultra-free"], Some("approved"));
    assert_eq!(models["laguna-s-2.1-free"], None);
    assert_eq!(models["ling-3.0-flash-free"], None);
    assert_eq!(models["big-pickle"], None);
}

#[tokio::test]
async fn free_models_keep_a_high_quality_effort_variant() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "provider-a",
            &[CatalogRecord {
                model: "variant-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    let low = BenchmarkModel::fixture("variant-model", 10.0, 10.0, 10.0, 0.0, 0.0);
    let mut high = BenchmarkModel::fixture("variant-model", 80.0, 80.0, 80.0, 0.0, 0.0);
    high.reasoning_effort = Some("high".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[low, high])
        .expect("benchmarks");
    drop(store);

    let mut config = config_for(
        BTreeMap::from([(
            "provider-a".to_owned(),
            provider("https://example.com/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "variant-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.free_models_quality.min_composite_quality = 50.0;
    config.server.free_models_quality.max_age_months = 0;
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=provider-a&view=full"
        ))
        .send()
        .await
        .expect("free models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("free models body");
    assert_eq!(body["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["data"][0]["model"]["name"], "variant-model");
    assert_eq!(body["data"][0]["model"]["effort_level"], "high");
}

#[tokio::test]
async fn auto_models_include_free_candidates_without_price_observations() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free-provider",
            &[CatalogRecord {
                model: "free-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    let mut benchmark = BenchmarkModel::fixture("free-model", 80.0, 80.0, 80.0, 0.0, 0.0);
    benchmark.reasoning_effort = Some("medium".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
        .expect("benchmarks");
    drop(store);

    let mut config = config_for(
        BTreeMap::from([(
            "free-provider".to_owned(),
            provider("https://example.com/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "free-provider".to_owned(),
            model: "free-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?view=full"))
        .send()
        .await
        .expect("auto models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("auto models body");
    assert_eq!(body["routes"]["free"]["primary"]["model"], "free-model");
    assert_eq!(body["routes"]["free"]["primary"]["pricing_eligible"], true);
    assert_eq!(
        body["routes"]["free"]["primary"]["expected_cost_microusd"],
        0
    );
    assert_eq!(
        body["routes"]["free"]["primary"]["price_per_million"]["input"],
        0.0
    );
    assert_eq!(
        body["routes"]["free"]["primary"]["price_per_million"]["source"],
        "provider_free"
    );

    let summary: Value = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models"))
        .send()
        .await
        .expect("auto models summary response")
        .json()
        .await
        .expect("auto models summary body");
    let primary = &summary["routes"]["free"]["primary"];
    assert_eq!(summary["view"], "summary");
    assert_eq!(
        primary["links"]["self"],
        format!("{gateway}/v1/catalog/models/free-provider/free-model")
    );
    assert_eq!(primary["reasoning_effort"], "medium");
    assert_eq!(
        primary
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect::<Vec<_>>()),
        Some(vec!["id", "links", "quality", "reasoning_effort"])
    );
    assert!(primary.get("price_per_million").is_none());
    assert!(primary.get("reference_price_per_million").is_none());
}

#[tokio::test]
async fn auto_free_uses_reference_cost_for_quota_limited_models() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "ollama-cloud",
            &[
                CatalogRecord {
                    model: "cheap-model".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.1),
                    output_price_per_million: Some(0.2),
                },
                CatalogRecord {
                    model: "expensive-model".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(2.0),
                },
            ],
        )
        .expect("catalog");
    let mut cheap = BenchmarkModel::fixture("cheap-model", 50.0, 50.0, 50.0, 0.1, 0.2);
    cheap.latency_seconds = Some(1.0);
    let mut expensive = BenchmarkModel::fixture("expensive-model", 52.0, 52.0, 52.0, 1.0, 2.0);
    expensive.latency_seconds = Some(1.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[cheap, expensive])
        .expect("benchmarks");
    drop(store);

    let mut ollama = provider("https://example.com/v1".to_owned());
    ollama.profile = Some(ProviderProfileId::OllamaCloud);
    let mut config = config_for(
        BTreeMap::from([("ollama-cloud".to_owned(), ollama)]),
        vec![TargetConfig {
            provider: "ollama-cloud".to_owned(),
            model: "cheap-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?route=free&view=full"))
        .send()
        .await
        .expect("auto models response");
    let body: Value = response.json().await.expect("auto models body");
    let primary = &body["routes"]["free"]["primary"];
    assert_eq!(primary["model"], "cheap-model");
    assert_eq!(primary["expected_cost_microusd"], 0);
    assert!(
        primary["reference_cost_microusd"]
            .as_u64()
            .is_some_and(|cost| cost > 0)
    );
    assert_eq!(primary["access"]["kind"], "quota_limited_free_tier");
}

#[tokio::test]
async fn auto_free_quota_cost_uses_request_token_estimates() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "ollama-cloud",
            &[
                CatalogRecord {
                    model: "input-cheap".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.1),
                    output_price_per_million: Some(2.0),
                },
                CatalogRecord {
                    model: "output-cheap".to_owned(),
                    access_kind: AccessKind::QuotaLimitedFreeTier,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(0.1),
                },
            ],
        )
        .expect("catalog");
    let mut input_cheap = BenchmarkModel::fixture("input-cheap", 50.0, 50.0, 50.0, 0.1, 2.0);
    input_cheap.latency_seconds = Some(1.0);
    let mut output_cheap = BenchmarkModel::fixture("output-cheap", 50.0, 50.0, 50.0, 1.0, 0.1);
    output_cheap.latency_seconds = Some(1.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[input_cheap, output_cheap])
        .expect("benchmarks");
    drop(store);

    let mut ollama = provider(format!("http://{upstream}/v1"));
    ollama.profile = Some(ProviderProfileId::OllamaCloud);
    let mut config = config_for(
        BTreeMap::from([("ollama-cloud".to_owned(), ollama)]),
        vec![TargetConfig {
            provider: "ollama-cloud".to_owned(),
            model: "input-cheap".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-free",
            "messages": [{"role": "user", "content": "x".repeat(10_000)}]
        }))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("auto-free body");
    assert_eq!(body["model"], "input-cheap");
}

#[tokio::test]
async fn auto_models_keep_base_and_pro_variants_in_separate_modes() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let catalog = |model: &str, input, output| CatalogRecord {
        model: model.to_owned(),
        access_kind: AccessKind::Paid,
        context_length: Some(128_000),
        supports_tools: Some(true),
        supports_vision: Some(false),
        supports_structured_output: Some(false),
        input_price_per_million: Some(input),
        output_price_per_million: Some(output),
    };
    store
        .replace_catalog(
            "paid-provider",
            &[
                catalog("mimo-v2.5", 0.14, 0.28),
                catalog("mimo-v2.5-pro", 0.43, 0.87),
            ],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("mimo-v2-5-0424", 37.2, 37.2, 37.2, 0.14, 0.28),
                BenchmarkModel::fixture("mimo-v2-5-pro", 43.0, 43.0, 43.0, 0.43, 0.87),
            ],
        )
        .expect("benchmarks");
    drop(store);

    let mut paid = provider("https://example.com/v1".to_owned());
    paid.billing_mode = BillingMode::Paid;
    paid.model_mappings
        .insert("mimo-v2.5".to_owned(), "mimo-v2-5-0424".to_owned());
    let mut config = config_for(
        BTreeMap::from([("paid-provider".to_owned(), paid)]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "mimo-v2.5".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?view=full"))
        .send()
        .await
        .expect("auto models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("auto models body");
    assert_eq!(body["routes"]["efficient"]["primary"]["model"], "mimo-v2.5");
    assert_eq!(
        body["routes"]["efficient"]["primary"]["benchmark_match"],
        "configured"
    );
    assert_eq!(
        body["routes"]["balanced"]["primary"]["model"],
        "mimo-v2.5-pro"
    );
    assert_eq!(
        body["routes"]["balanced"]["primary"]["benchmark_match"],
        "exact"
    );
}

#[tokio::test]
async fn auto_models_prefer_measured_costs_over_price_estimates() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let models = [
        ("efficient-a", 41.0, 5.0, 1.0),
        ("efficient-b", 40.0, 0.20, 2.0),
        ("efficient-c", 39.0, 0.30, 3.0),
        ("efficient-d", 38.0, 0.40, 4.0),
    ];
    store
        .replace_catalog(
            "paid-provider",
            &models
                .iter()
                .map(|(model, _, price, _)| CatalogRecord {
                    model: (*model).to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(*price),
                    output_price_per_million: Some(*price),
                })
                .collect::<Vec<_>>(),
        )
        .expect("catalog");
    let benchmarks = models
        .iter()
        .map(|(model, quality, price, latency)| {
            let mut benchmark =
                BenchmarkModel::fixture(model, *quality, *quality, *quality, *price, *price);
            benchmark.latency_seconds = Some(*latency);
            if *model == "efficient-a" {
                benchmark.cost_per_task_usd = Some(0.50);
            }
            benchmark
        })
        .collect::<Vec<_>>();
    store
        .replace_benchmarks("fixture", "Fixture", &benchmarks)
        .expect("benchmarks");
    drop(store);

    let mut paid = provider("https://example.com/v1".to_owned());
    paid.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("paid-provider".to_owned(), paid)]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "efficient-a".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/auto-models?route=efficient&view=full"
        ))
        .send()
        .await
        .expect("auto models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("auto models body");
    assert_eq!(
        body["routes"]["efficient"]["primary"]["model"],
        "efficient-a"
    );
    let fallbacks = body["routes"]["efficient"]["fallbacks"]
        .as_array()
        .expect("fallback array");
    assert_eq!(fallbacks.len(), 2);
    assert_eq!(fallbacks[0]["model"], "efficient-b");
    assert_eq!(fallbacks[1]["model"], "efficient-c");
}

#[tokio::test]
async fn providers_lists_available_secret_backed_providers_without_credentials() {
    unsafe {
        std::env::set_var("MODEL_GATEWAY_TEST_PROVIDER_KEY", "fixture-secret");
    }
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let mut available = provider("https://available.example/v1".to_owned());
    available.profile = Some(ProviderProfileId::OpenRouter);
    available.api_key_secret = Some("MODEL_GATEWAY_TEST_PROVIDER_KEY".to_owned());
    let mut unavailable = provider("https://unavailable.example/v1".to_owned());
    unavailable.profile = Some(ProviderProfileId::Groq);
    unavailable.api_key_secret = Some("MODEL_GATEWAY_MISSING_KEY".to_owned());
    let mut config = config_for(
        BTreeMap::from([
            ("available".to_owned(), available),
            ("unavailable".to_owned(), unavailable),
        ]),
        vec![TargetConfig {
            provider: "available".to_owned(),
            model: "available-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);

    let gateway = spawn_gateway(config).await;
    let response: Value = reqwest::get(format!("{gateway}/v1/providers"))
        .await
        .expect("providers response")
        .json()
        .await
        .expect("providers body");
    let providers = response["data"].as_array().expect("provider data");
    // Now lists ALL built-in providers, not just configured ones
    assert!(providers.len() > 2, "should list all built-in providers");
    let available_prov = providers
        .iter()
        .find(|p| p["id"] == "available")
        .expect("available provider");
    assert_eq!(available_prov["name"], "OpenRouter");
    assert_eq!(
        available_prov["api_key_secret"],
        "MODEL_GATEWAY_TEST_PROVIDER_KEY"
    );
    assert_eq!(available_prov["api_key_source"], "environment");
    assert_eq!(available_prov["available"], true);
    assert_eq!(available_prov["model_count"], 0);
    assert_eq!(available_prov["free_model_count"], 0);
    let unavailable_prov = providers
        .iter()
        .find(|p| p["id"] == "unavailable")
        .expect("unavailable provider");
    assert_eq!(unavailable_prov["available"], false);
    // Unconfigured providers should show available: false and api_key_source: "none"
    let unconfigured = providers
        .iter()
        .find(|p| p["id"] == "google-gemini")
        .expect("google-gemini");
    assert_eq!(unconfigured["available"], false);
    assert_eq!(unconfigured["api_key_source"], "none");

    let response: Value = reqwest::get(format!("{gateway}/v1/providers?available=false"))
        .await
        .expect("unavailable providers response")
        .json()
        .await
        .expect("unavailable providers body");
    let unavailable = response["data"].as_array().expect("unavailable data");
    // available=false returns configured-unavailable + all unconfigured providers
    let unavailable_prov = unavailable
        .iter()
        .find(|p| p["id"] == "unavailable")
        .expect("unavailable provider");
    assert_eq!(unavailable_prov["available"], false);

    unsafe {
        std::env::remove_var("MODEL_GATEWAY_TEST_PROVIDER_KEY");
    }
}

#[tokio::test]
async fn forwards_json_and_tools_without_rewriting_response_model() {
    let provider_address = spawn_provider(ProviderResponse::Success).await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "smoke",
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"type": "function", "function": {"name": "test"}}],
            "extra_body": {"preserve": true}
        }))
        .send()
        .await
        .expect("gateway response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-alias"], "smoke");
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
    assert_eq!(
        response.headers()["x-model-gateway-served-model"],
        "upstream-model"
    );
    let body: Value = response.json().await.expect("json response");
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "ok\n- Upstream: Model Default, local"
    );
}

#[tokio::test]
async fn streams_sse_and_falls_back_before_output() {
    let failing = spawn_provider(ProviderResponse::Failure(
        StatusCode::BAD_GATEWAY,
        "first failure",
    ))
    .await;
    let streaming_router = Router::new().route(
        "/v1/chat/completions",
        post(|Json(_body): Json<Value>| async { ProviderResponse::Stream.respond(json!({})) }),
    );
    let streaming = spawn_router(streaming_router).await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("first".to_owned(), provider(format!("http://{failing}/v1"))),
            (
                "second".to_owned(),
                provider(format!("http://{streaming}/v1")),
            ),
        ]),
        vec![
            TargetConfig {
                provider: "first".to_owned(),
                model: "first-model".to_owned(),
            },
            TargetConfig {
                provider: "second".to_owned(),
                model: "second-model".to_owned(),
            },
        ],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "smoke",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("stream response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-fallbacks"], "1");
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content type")
            .to_str()
            .expect("content type string")
            .starts_with("text/event-stream")
    );
    let body = response.text().await.expect("stream body");
    assert!(body.contains("- Second: Model Default, second"));
    assert!(body.contains("data: [DONE]"));
}

#[tokio::test]
async fn returns_last_fallback_error_body_and_metadata() {
    let first = spawn_provider(ProviderResponse::Failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "first failure",
    ))
    .await;
    let second = spawn_provider(ProviderResponse::Failure(
        StatusCode::TOO_MANY_REQUESTS,
        "last failure",
    ))
    .await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("first".to_owned(), provider(format!("http://{first}/v1"))),
            ("second".to_owned(), provider(format!("http://{second}/v1"))),
        ]),
        vec![
            TargetConfig {
                provider: "first".to_owned(),
                model: "first-model".to_owned(),
            },
            TargetConfig {
                provider: "second".to_owned(),
                model: "second-model".to_owned(),
            },
        ],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("fallback response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["x-model-gateway-alias"], "smoke");
    assert_eq!(response.headers()["x-model-gateway-provider"], "second");
    assert_eq!(response.headers()["x-model-gateway-fallbacks"], "1");
    let body: Value = response.json().await.expect("last error body");
    assert_eq!(body["error"]["message"], "last failure");
}

#[tokio::test]
async fn body_limits_and_stream_types_use_openai_errors() {
    let provider_address = spawn_provider(ProviderResponse::Success).await;
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    );
    config.server.max_body_bytes = 64;
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let oversized = client
        .post(format!("{gateway}/v1/chat/completions"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(format!(
            "{{\"model\":\"smoke\",\"messages\":[],\"padding\":\"{}\"}}",
            "x".repeat(128)
        ))
        .send()
        .await
        .expect("oversized response");
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = oversized.json().await.expect("oversized error");
    assert_eq!(body["error"]["code"], "body_too_large");

    let invalid_stream = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": "yes", "messages": []}))
        .send()
        .await
        .expect("invalid stream response");
    assert_eq!(invalid_stream.status(), StatusCode::BAD_REQUEST);
    let body: Value = invalid_stream.json().await.expect("stream error");
    assert_eq!(body["error"]["code"], "stream");

    let invalid_effort = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "reasoning_effort": "extreme", "messages": []}))
        .send()
        .await
        .expect("invalid reasoning effort response");
    assert_eq!(invalid_effort.status(), StatusCode::BAD_REQUEST);
    let body: Value = invalid_effort.json().await.expect("reasoning effort error");
    assert_eq!(body["error"]["code"], "reasoning_effort");
}

#[tokio::test]
async fn model_and_health_endpoints_are_detail_free() {
    let provider_address = spawn_provider(ProviderResponse::Success).await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let client = reqwest::Client::new();
    let models: Value = client
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .expect("models")
        .json()
        .await
        .expect("models json");
    assert_eq!(models["data"][0]["id"], "local");
    assert_eq!(models["data"][1]["id"], "auto-free");
    assert_eq!(models["data"][2]["id"], "auto-efficient");
    assert_eq!(models["data"][3]["id"], "auto-balanced");
    assert_eq!(models["data"][4]["id"], "auto-frontier");
    assert_eq!(models["data"][5]["id"], "smoke");
    let ready: Value = client
        .get(format!("{gateway}/health/ready"))
        .send()
        .await
        .expect("ready")
        .json()
        .await
        .expect("ready json");
    assert_eq!(ready, json!({"status": "ready"}));
}

#[tokio::test]
async fn disabled_frontier_route_is_hidden_and_rejected() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let mut config = config_for(
        BTreeMap::from([(
            "provider".to_owned(),
            provider(format!("http://{upstream}/v1")),
        )]),
        vec![TargetConfig {
            provider: "provider".to_owned(),
            model: "model".to_owned(),
        }],
    );
    config.server.auto_frontier_enabled = false;
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let models: Value = client
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .expect("models response")
        .json()
        .await
        .expect("models JSON");
    assert!(
        !models["data"]
            .as_array()
            .expect("models")
            .iter()
            .any(|model| model["id"] == "auto-frontier")
    );
    let response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("disabled route response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.expect("error JSON");
    assert_eq!(body["error"]["code"], "route_disabled");
}

#[tokio::test]
async fn local_route_discovers_the_only_loaded_model() {
    let local = spawn_local_provider(vec!["mtplx-7b"]).await;
    let configured = spawn_provider(ProviderResponse::Success).await;
    let mut config = config_for(
        BTreeMap::from([(
            "configured".to_owned(),
            provider(format!("http://{configured}/v1")),
        )]),
        vec![TargetConfig {
            provider: "configured".to_owned(),
            model: "configured-model".to_owned(),
        }],
    );
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "local", "messages": [{"role": "user", "content": "hello"}]}))
        .send()
        .await
        .expect("local response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-alias"], "local");
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
    let body: Value = response.json().await.expect("local json");
    assert_eq!(body["model"], "mtplx-7b");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "ok\n- MTPLX: 7b Default, Local"
    );
}

#[tokio::test]
async fn local_route_rejects_ambiguous_discovery() {
    let local = spawn_local_provider(vec!["first", "second"]).await;
    let configured = spawn_provider(ProviderResponse::Success).await;
    let mut config = config_for(
        BTreeMap::from([(
            "configured".to_owned(),
            provider(format!("http://{configured}/v1")),
        )]),
        vec![TargetConfig {
            provider: "configured".to_owned(),
            model: "configured-model".to_owned(),
        }],
    );
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "local", "messages": []}))
        .send()
        .await
        .expect("ambiguous local response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("local error json");
    assert_eq!(body["error"]["code"], "local_model_ambiguous");
}

#[tokio::test]
async fn local_route_rediscovers_after_model_not_found() {
    let (local, discoveries) = spawn_reloading_local_provider().await;
    let configured = spawn_provider(ProviderResponse::Success).await;
    let mut config = config_for(
        BTreeMap::from([(
            "configured".to_owned(),
            provider(format!("http://{configured}/v1")),
        )]),
        vec![TargetConfig {
            provider: "configured".to_owned(),
            model: "configured-model".to_owned(),
        }],
    );
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "local", "messages": []}))
        .send()
        .await
        .expect("reloaded local response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-fallbacks"], "1");
    let body: Value = response.json().await.expect("reloaded local json");
    assert_eq!(body["model"], "loaded-model");
    assert_eq!(discoveries.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn auto_free_selects_only_verified_free_models() {
    let free = spawn_provider(ProviderResponse::Success).await;
    let mut free_provider = provider(format!("http://{free}/v1"));
    free_provider.free_models = vec!["verified-free".to_owned()];
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([("free".to_owned(), free_provider)]),
        vec![TargetConfig {
            provider: "free".to_owned(),
            model: "verified-free".to_owned(),
        }],
    ))
    .await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "free");
    let body: Value = response.json().await.expect("auto-free json");
    assert_eq!(body["model"], "verified-free");
}

#[tokio::test]
async fn auto_free_filters_catalog_capability_mismatches() {
    let unsupported = spawn_provider(ProviderResponse::Success).await;
    let supported = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "unsupported",
            &[CatalogRecord {
                model: "no-tools".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(false),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("unsupported catalog");
    store
        .replace_catalog(
            "supported",
            &[CatalogRecord {
                model: "with-tools".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("supported catalog");
    drop(store);
    let mut unsupported_config = provider(format!("http://{unsupported}/v1"));
    unsupported_config.free_models = vec!["no-tools".to_owned()];
    let mut supported_config = provider(format!("http://{supported}/v1"));
    supported_config.free_models = vec!["with-tools".to_owned()];
    let mut config = config_for(
        BTreeMap::from([
            ("unsupported".to_owned(), unsupported_config),
            ("supported".to_owned(), supported_config),
        ]),
        vec![TargetConfig {
            provider: "unsupported".to_owned(),
            model: "advanced-only".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-free",
            "messages": [],
            "tools": [{"type": "function", "function": {"name": "fixture"}}]
        }))
        .send()
        .await
        .expect("capability response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "supported");
}

#[tokio::test]
async fn auto_free_falls_back_to_local_after_configured_quota() {
    let free = spawn_provider(ProviderResponse::Success).await;
    let local = spawn_local_provider(vec!["local-model"]).await;
    let mut free_provider = provider(format!("http://{free}/v1"));
    free_provider.free_models = vec!["limited-free".to_owned()];
    free_provider.quotas = vec![QuotaLimit {
        kind: QuotaKind::Requests,
        limit: 1,
        window_seconds: 3_600,
        boundary: QuotaBoundary::Rolling,
    }];
    let mut config = config_for(
        BTreeMap::from([("free".to_owned(), free_provider)]),
        vec![TargetConfig {
            provider: "free".to_owned(),
            model: "limited-free".to_owned(),
        }],
    );
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("first free response");
    assert_eq!(first.headers()["x-model-gateway-provider"], "free");

    let second = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("local fallback response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["x-model-gateway-provider"], "local");
    let body: Value = second.json().await.expect("fallback json");
    assert_eq!(body["model"], "local-model");
}

#[tokio::test]
async fn auto_free_ignores_provider_with_missing_key() {
    let keyed = spawn_provider(ProviderResponse::Success).await;
    let local = spawn_local_provider(vec!["local-model"]).await;
    let mut keyed_provider = provider(format!("http://{keyed}/v1"));
    keyed_provider.api_key_secret = Some("UNAVAILABLE_TEST_KEY".to_owned());
    keyed_provider.free_models = vec!["keyed-free".to_owned()];
    let mut config = config_for(
        BTreeMap::from([("keyed".to_owned(), keyed_provider)]),
        vec![TargetConfig {
            provider: "keyed".to_owned(),
            model: "keyed-free".to_owned(),
        }],
    );
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("missing-key fallback");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
}

#[tokio::test]
async fn auto_free_cools_down_a_rate_limited_model() {
    let throttled_calls = Arc::new(AtomicUsize::new(0));
    let calls = throttled_calls.clone();
    let throttled = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ProviderResponse::Failure(StatusCode::TOO_MANY_REQUESTS, "limited").respond(body)
            }
        }),
    ))
    .await;
    let healthy = spawn_provider(ProviderResponse::Success).await;
    let mut throttled_provider = provider(format!("http://{throttled}/v1"));
    throttled_provider.free_models = vec!["free-a".to_owned()];
    let mut healthy_provider = provider(format!("http://{healthy}/v1"));
    healthy_provider.free_models = vec!["free-b".to_owned()];
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("a-throttled".to_owned(), throttled_provider),
            ("b-healthy".to_owned(), healthy_provider),
        ]),
        vec![TargetConfig {
            provider: "a-throttled".to_owned(),
            model: "free-a".to_owned(),
        }],
    ))
    .await;
    let client = reqwest::Client::new();

    for _ in 0..2 {
        let response = client
            .post(format!("{gateway}/v1/chat/completions"))
            .json(&json!({"model": "auto-free", "messages": []}))
            .send()
            .await
            .expect("auto-free response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-model-gateway-provider"], "b-healthy");
    }
    assert_eq!(throttled_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auto_free_prefers_higher_quality_model() {
    let weak = spawn_provider(ProviderResponse::Success).await;
    let strong = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model) in [("weak", "weak-model"), ("strong", "strong-model")] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("weak-model", 40.0, 30.0, 20.0, 0.0, 0.0),
                BenchmarkModel::fixture("strong-model", 90.0, 85.0, 80.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("weak".to_owned(), provider(format!("http://{weak}/v1"))),
            ("strong".to_owned(), provider(format!("http://{strong}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "weak".to_owned(),
            model: "weak-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "strong");
}

#[tokio::test]
async fn auto_free_quality_bar_filters_low_quality() {
    let provider_address = spawn_provider(ProviderResponse::Success).await;
    let local = spawn_local_provider(vec!["local-model"]).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free",
            &[CatalogRecord {
                model: "low-quality-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(false),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "low-quality-model",
                10.0,
                10.0,
                10.0,
                0.0,
                0.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([(
            "free".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "free".to_owned(),
            model: "low-quality-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
}

#[tokio::test]
async fn auto_free_selects_highest_composite_quality_model() {
    let strong = spawn_provider(ProviderResponse::Success).await;
    let weak = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model) in [
        ("strong-provider", "strong-model"),
        ("weak-provider", "weak-model"),
    ] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("strong-model", 90.0, 80.0, 70.0, 0.0, 0.0),
                BenchmarkModel::fixture("weak-model", 60.0, 50.0, 40.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            (
                "strong-provider".to_owned(),
                provider(format!("http://{strong}/v1")),
            ),
            (
                "weak-provider".to_owned(),
                provider(format!("http://{weak}/v1")),
            ),
        ]),
        vec![TargetConfig {
            provider: "strong-provider".to_owned(),
            model: "strong-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    // Coding request picks highest composite quality
    let coding_response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-free",
            "messages": [{"role": "user", "content": "Implement a Rust service with concurrency."}]
        }))
        .send()
        .await
        .expect("coding auto-free response");
    assert_eq!(coding_response.status(), StatusCode::OK);
    assert_eq!(
        coding_response.headers()["x-model-gateway-provider"],
        "strong-provider"
    );

    // General request also picks highest composite quality (same model)
    let general_response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-free",
            "messages": [{"role": "user", "content": "Summarize this paragraph."}]
        }))
        .send()
        .await
        .expect("general auto-free response");
    assert_eq!(general_response.status(), StatusCode::OK);
    assert_eq!(
        general_response.headers()["x-model-gateway-provider"],
        "strong-provider"
    );
}

#[tokio::test]
async fn auto_free_emits_selection_headers() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free",
            &[CatalogRecord {
                model: "benchmarked-free".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "benchmarked-free",
                85.0,
                80.0,
                75.0,
                0.0,
                0.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([("free".to_owned(), provider(format!("http://{upstream}/v1")))]),
        vec![TargetConfig {
            provider: "free".to_owned(),
            model: "benchmarked-free".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-task"], "general");
    assert_eq!(response.headers()["x-model-gateway-quality"], "83.5");
    assert_eq!(response.headers()["x-model-gateway-complexity"], "simple");
    assert_eq!(response.headers()["x-model-gateway-classifier"], "rules-v1");
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-latency-observed"],
        "true"
    );
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-latency-seconds"],
        "1"
    );
}

#[tokio::test]
async fn auto_free_falls_back_through_multiple_providers() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let a_ref = calls_a.clone();
    let b_ref = calls_b.clone();
    let provider_a = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = a_ref.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ProviderResponse::Failure(StatusCode::TOO_MANY_REQUESTS, "limited").respond(body)
            }
        }),
    ))
    .await;
    let provider_b = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = b_ref.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ProviderResponse::Failure(StatusCode::TOO_MANY_REQUESTS, "limited").respond(body)
            }
        }),
    ))
    .await;
    let provider_c = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model) in [("a", "free-a"), ("b", "free-b"), ("c", "free-c")] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("free-a", 50.0, 50.0, 50.0, 0.0, 0.0),
                BenchmarkModel::fixture("free-b", 50.0, 50.0, 50.0, 0.0, 0.0),
                BenchmarkModel::fixture("free-c", 50.0, 50.0, 50.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    drop(store);
    let config = config_for(
        BTreeMap::from([
            ("a".to_owned(), provider(format!("http://{provider_a}/v1"))),
            ("b".to_owned(), provider(format!("http://{provider_b}/v1"))),
            ("c".to_owned(), provider(format!("http://{provider_c}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "a".to_owned(),
            model: "free-a".to_owned(),
        }],
    );
    let mut config = config;
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "c");
    assert!(calls_a.load(Ordering::SeqCst) >= 1);
    assert!(calls_b.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn auto_free_quality_bar_filters_by_context() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let local = spawn_local_provider(vec!["local-model"]).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free",
            &[CatalogRecord {
                model: "tiny-context".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(4_096),
                supports_tools: Some(false),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([("free".to_owned(), provider(format!("http://{upstream}/v1")))]),
        vec![TargetConfig {
            provider: "free".to_owned(),
            model: "tiny-context".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.local_base_url = format!("http://{local}/v1");
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
}

#[tokio::test]
async fn auto_free_does_not_invalidate_pin_on_rate_limit() {
    let throttled_calls = Arc::new(AtomicUsize::new(0));
    let calls = throttled_calls.clone();
    let throttled = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                ProviderResponse::Failure(StatusCode::TOO_MANY_REQUESTS, "limited").respond(body)
            }
        }),
    ))
    .await;
    let healthy = spawn_provider(ProviderResponse::Success).await;
    let mut throttled_provider = provider(format!("http://{throttled}/v1"));
    throttled_provider.free_models = vec!["free-a".to_owned()];
    let mut healthy_provider = provider(format!("http://{healthy}/v1"));
    healthy_provider.free_models = vec!["free-b".to_owned()];
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("a-throttled".to_owned(), throttled_provider),
            ("b-healthy".to_owned(), healthy_provider),
        ]),
        vec![TargetConfig {
            provider: "a-throttled".to_owned(),
            model: "free-a".to_owned(),
        }],
    ))
    .await;
    let client = reqwest::Client::new();

    // Pin is NOT invalidated on 429 (only on 401/403 auth failures).
    // Cooldown handles temporary routing; cooldown skips A on second request.
    for _ in 0..2 {
        let response = client
            .post(format!("{gateway}/v1/chat/completions"))
            .json(&json!({"model": "auto-free", "messages": []}))
            .send()
            .await
            .expect("auto-free response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-model-gateway-provider"], "b-healthy");
    }
    assert_eq!(throttled_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auto_free_abandons_pin_on_auth_failure() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count = call_count.clone();
    let provider_a = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let count = count.clone();
            async move {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ProviderResponse::Failure(StatusCode::UNAUTHORIZED, "bad key").respond(body)
                } else {
                    ProviderResponse::Success.respond(body)
                }
            }
        }),
    ))
    .await;
    let provider_b = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model, quality) in [("a", "model-a", 90.0), ("b", "model-b", 50.0)] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
        store
            .replace_benchmarks(
                "fixture",
                "Fixture",
                &[BenchmarkModel::fixture(
                    model, quality, quality, quality, 0.0, 0.0,
                )],
            )
            .expect("benchmarks");
    }
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("a".to_owned(), provider(format!("http://{provider_a}/v1"))),
            ("b".to_owned(), provider(format!("http://{provider_b}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "a".to_owned(),
            model: "model-a".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    // First request: A returns 401 (permanent auth failure), falls back to B
    let first = client
        .post(format!("{gateway}/v1/chat/completions"))
        .header("x-session-id", "auth-test")
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("first response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-model-gateway-provider"], "b");

    // Second request: pin was invalidated on 401, B has new pin
    let second = client
        .post(format!("{gateway}/v1/chat/completions"))
        .header("x-session-id", "auth-test")
        .json(&json!({"model": "auto-free", "messages": []}))
        .send()
        .await
        .expect("second response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["x-model-gateway-provider"], "b");
}

#[tokio::test]
async fn auto_free_prefers_fast_model_for_simple_task() {
    let fast = spawn_provider(ProviderResponse::Success).await;
    let slow = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model) in [("fast", "fast-model"), ("slow", "slow-model")] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    let mut fast_bench = BenchmarkModel::fixture("fast-model", 75.0, 75.0, 75.0, 0.0, 0.0);
    fast_bench.latency_seconds = Some(0.5);
    let mut slow_bench = BenchmarkModel::fixture("slow-model", 80.0, 75.0, 70.0, 0.0, 0.0);
    slow_bench.latency_seconds = Some(5.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[fast_bench, slow_bench])
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("fast".to_owned(), provider(format!("http://{fast}/v1"))),
            ("slow".to_owned(), provider(format!("http://{slow}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "fast".to_owned(),
            model: "fast-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": [{"role": "user", "content": "What is 2+2?"}]}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "fast");
}

#[tokio::test]
async fn auto_free_prefers_quality_model_for_complex_task() {
    let fast = spawn_provider(ProviderResponse::Success).await;
    let slow = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model) in [("fast", "fast-model"), ("slow", "slow-model")] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    let mut fast_bench = BenchmarkModel::fixture("fast-model", 50.0, 50.0, 50.0, 0.0, 0.0);
    fast_bench.latency_seconds = Some(0.5);
    let mut slow_bench = BenchmarkModel::fixture("slow-model", 80.0, 80.0, 80.0, 0.0, 0.0);
    slow_bench.latency_seconds = Some(5.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[fast_bench, slow_bench])
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("fast".to_owned(), provider(format!("http://{fast}/v1"))),
            ("slow".to_owned(), provider(format!("http://{slow}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "fast".to_owned(),
            model: "fast-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-free",
            "messages": [{"role": "user", "content": "Implement a complex multi-step refactoring with concurrency."}],
            "tools": [{"type": "function", "function": {"name": "edit_file", "parameters": {}}}]
        }))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    // The fast model is outside the quality-regret window, so quality wins.
    assert_eq!(response.headers()["x-model-gateway-provider"], "slow");
}

#[tokio::test]
async fn auto_free_quality_bar_filters_low_quality_composite() {
    let weak = spawn_provider(ProviderResponse::Success).await;
    let strong = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model) in [("weak", "weak-model"), ("strong", "strong-model")] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[
                BenchmarkModel::fixture("weak-model", 25.0, 25.0, 25.0, 0.0, 0.0),
                BenchmarkModel::fixture("strong-model", 70.0, 70.0, 70.0, 0.0, 0.0),
            ],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("weak".to_owned(), provider(format!("http://{weak}/v1"))),
            ("strong".to_owned(), provider(format!("http://{strong}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "weak".to_owned(),
            model: "weak-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": [{"role": "user", "content": "Implement a complex multi-step refactoring with tools."}]}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "strong");
}

#[tokio::test]
async fn auto_free_pareto_dominance_prunes_slow_models() {
    let provider_a = spawn_provider(ProviderResponse::Success).await;
    let provider_b = spawn_provider(ProviderResponse::Success).await;
    let provider_c = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (prov, model) in [("a", "model-a"), ("b", "model-b"), ("c", "model-c")] {
        store
            .replace_catalog(
                prov,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::ZeroPrice,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(false),
                    supports_structured_output: Some(false),
                    input_price_per_million: Some(0.0),
                    output_price_per_million: Some(0.0),
                }],
            )
            .expect("catalog");
    }
    let mut bench_a = BenchmarkModel::fixture("model-a", 70.0, 70.0, 70.0, 0.0, 0.0);
    bench_a.latency_seconds = Some(5.0);
    let mut bench_b = BenchmarkModel::fixture("model-b", 70.0, 70.0, 70.0, 0.0, 0.0);
    bench_b.latency_seconds = Some(1.0);
    let mut bench_c = BenchmarkModel::fixture("model-c", 65.0, 65.0, 65.0, 0.0, 0.0);
    bench_c.latency_seconds = Some(0.5);
    store
        .replace_benchmarks("fixture", "Fixture", &[bench_a, bench_b, bench_c])
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            ("a".to_owned(), provider(format!("http://{provider_a}/v1"))),
            ("b".to_owned(), provider(format!("http://{provider_b}/v1"))),
            ("c".to_owned(), provider(format!("http://{provider_c}/v1"))),
        ]),
        vec![TargetConfig {
            provider: "a".to_owned(),
            model: "model-a".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-free", "messages": [{"role": "user", "content": "Hello."}]}))
        .send()
        .await
        .expect("auto-free response");
    assert_eq!(response.status(), StatusCode::OK);
    let provider = response.headers()["x-model-gateway-provider"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        provider == "b" || provider == "c",
        "model-a (quality=70, latency=5s) should be dominated by model-b (quality=70, latency=1s); got {provider}"
    );
}

#[tokio::test]
async fn direct_alias_reports_missing_provider_key_in_openai_shape() {
    let keyed = spawn_provider(ProviderResponse::Success).await;
    let mut keyed_provider = provider(format!("http://{keyed}/v1"));
    keyed_provider.api_key_secret = Some("UNAVAILABLE_DIRECT_KEY".to_owned());
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([("keyed".to_owned(), keyed_provider)]),
        vec![TargetConfig {
            provider: "keyed".to_owned(),
            model: "keyed-model".to_owned(),
        }],
    ))
    .await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("missing direct key response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("OpenAI error body");
    assert_eq!(body["error"]["type"], "upstream_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("credential")
    );
}

#[tokio::test]
async fn auto_efficient_uses_cost_then_quality_floor() {
    let cheap = spawn_provider(ProviderResponse::Success).await;
    let strong = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "cheap",
            &[CatalogRecord {
                model: "cheap-model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("cheap catalog");
    store
        .replace_catalog(
            "strong",
            &[CatalogRecord {
                model: "strong-model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("strong catalog");
    store
        .replace_benchmarks(
            "fixture",
            "fixture attribution",
            &[
                BenchmarkModel::fixture("cheap-model", 55.0, 50.0, 45.0, 0.1, 0.2),
                BenchmarkModel::fixture("strong-model", 92.0, 95.0, 90.0, 5.0, 10.0),
            ],
        )
        .expect("benchmarks");
    drop(store);
    let mut cheap_provider = provider(format!("http://{cheap}/v1"));
    cheap_provider.billing_mode = BillingMode::Paid;
    let mut strong_provider = provider(format!("http://{strong}/v1"));
    strong_provider.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([
            ("cheap".to_owned(), cheap_provider),
            ("strong".to_owned(), strong_provider),
        ]),
        vec![TargetConfig {
            provider: "cheap".to_owned(),
            model: "advanced-only".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    let simple = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-efficient", "messages": [{"role": "user", "content": "Summarize this sentence."}]}))
        .send()
        .await
        .expect("simple response");
    assert_eq!(simple.status(), StatusCode::OK);
    assert_eq!(simple.headers()["x-model-gateway-provider"], "cheap");
    assert_eq!(simple.headers()["x-model-gateway-task"], "general");
    assert_eq!(simple.headers()["x-model-gateway-complexity"], "simple");
    assert_eq!(simple.headers()["x-model-gateway-classifier"], "rules-v1");

    // Both requests use cheap — composite quality floor (40) is the same for all tasks.
    // Pareto picks cheapest first among non-dominated models.
    let complex = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-efficient",
            "messages": [{"role": "user", "content": "Implement and debug a multi-step Rust service, write comprehensive tests, and reason about concurrency failures."}],
            "tools": [{"type": "function", "function": {"name": "edit"}}]
        }))
        .send()
        .await
        .expect("complex response");
    assert_eq!(complex.status(), StatusCode::OK);
    assert_eq!(complex.headers()["x-model-gateway-provider"], "cheap");
}

#[tokio::test]
async fn auto_efficient_honors_explicit_paid_authorization_and_spend_caps() {
    let paid = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid",
            &[CatalogRecord {
                model: "paid-model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "paid-model",
                90.0,
                90.0,
                90.0,
                1.0,
                1.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut paid_provider = provider(format!("http://{paid}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    paid_provider.quotas = vec![QuotaLimit {
        kind: QuotaKind::CostMicrousd,
        limit: 1_100,
        window_seconds: 86_400,
        boundary: QuotaBoundary::Rolling,
    }];
    let mut config = config_for(
        BTreeMap::from([("paid".to_owned(), paid_provider)]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "paid-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    let request = json!({
        "model": "auto-efficient",
        "messages": [{"role": "user", "content": "Implement a comprehensive multi-step production architecture with concurrency safeguards."}],
        "tools": [{"type": "function", "function": {"name": "edit"}}]
    });
    let first = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&request)
        .send()
        .await
        .expect("first paid response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-model-gateway-provider"], "paid");
}

#[tokio::test]
async fn auto_efficient_uses_canonical_mapping_and_reasoning_effort() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid",
            &[CatalogRecord {
                model: "provider/model-v1".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    let mut low = BenchmarkModel::fixture("canonical-model-low", 80.0, 80.0, 80.0, 1.0, 1.0);
    low.reasoning_effort = Some("low".to_owned());
    let mut high = BenchmarkModel::fixture("canonical-model", 95.0, 95.0, 95.0, 2.0, 2.0);
    high.reasoning_effort = Some("high".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[low, high])
        .expect("benchmarks");
    drop(store);
    let mut paid_provider = provider(format!("http://{upstream}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    paid_provider
        .model_mappings
        .insert("provider/model-v1".to_owned(), "canonical-model".to_owned());
    let mut config = config_for(
        BTreeMap::from([("paid".to_owned(), paid_provider)]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "provider/model-v1".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-efficient",
            "reasoning_effort": "high",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-reasoning-effort"],
        "High"
    );
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-match"],
        "configured"
    );
    assert_eq!(
        response.headers()["x-model-gateway-canonical-model"],
        "canonical-model"
    );
    let body: Value = response.json().await.expect("response JSON");
    assert!(
        body["choices"][0]["message"]["content"]
            .as_str()
            .expect("content")
            .contains("High")
    );
}

#[tokio::test]
async fn auto_efficient_falls_back_when_paid_models_are_unbenchmarked() {
    let paid = spawn_provider(ProviderResponse::Success).await;
    let free = spawn_provider(ProviderResponse::Success).await;
    let mut paid_provider = provider(format!("http://{paid}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    let mut free_provider = provider(format!("http://{free}/v1"));
    free_provider.free_models = vec!["free-model".to_owned()];
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("paid".to_owned(), paid_provider),
            ("free".to_owned(), free_provider),
        ]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "unbenchmarked-paid".to_owned(),
        }],
    ))
    .await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-efficient", "messages": []}))
        .send()
        .await
        .expect("fallback response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "free");
}

#[tokio::test]
async fn auto_frontier_keeps_effort_variants_as_distinct_candidates() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "cli-proxy",
            &[CatalogRecord {
                model: "gpt-5.6-sol".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: Some(5.0),
                output_price_per_million: Some(30.0),
            }],
        )
        .expect("catalog");
    let mut max = BenchmarkModel::fixture("gpt-5-6-sol", 90.0, 90.0, 90.0, 5.0, 30.0);
    max.reasoning_effort = Some("max".to_owned());
    max.latency_seconds = Some(100.0);
    max.end_to_end_response_seconds = Some(10.0);
    max.cost_per_task_usd = Some(0.50);
    let mut medium = BenchmarkModel::fixture("gpt-5-6-sol-medium", 80.0, 80.0, 80.0, 5.0, 30.0);
    medium.reasoning_effort = Some("medium".to_owned());
    medium.latency_seconds = Some(1.0);
    medium.end_to_end_response_seconds = Some(40.0);
    medium.cost_per_task_usd = Some(0.01);
    store
        .replace_benchmarks("fixture", "Fixture", &[max, medium])
        .expect("benchmarks");
    drop(store);

    let mut provider_config = provider(format!("http://{upstream}/v1"));
    provider_config.profile = Some(ProviderProfileId::CliProxyApi);
    provider_config.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("cli-proxy".to_owned(), provider_config)]),
        vec![TargetConfig {
            provider: "cli-proxy".to_owned(),
            model: "gpt-5.6-sol".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.frontier_quality_floor_single = 50.0;
    let gateway = spawn_gateway(config).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?route=frontier&view=full"))
        .send()
        .await
        .expect("auto models response")
        .json()
        .await
        .expect("auto models body");
    let primary = &body["routes"]["frontier"]["primary"];
    assert_eq!(primary["id"], "cli-proxy/gpt-5.6-sol");
    assert_eq!(primary["reasoning_effort"], "medium");
    assert_eq!(primary["benchmark_cost_per_task_usd"], 0.01);
    assert_eq!(primary["latency_seconds"], 40.0);
    assert_eq!(
        body["routes"]["frontier"]["selection_policy"]["strategy"],
        "latency_aware_pareto"
    );
    assert_eq!(
        body["routes"]["frontier"]["selection_policy"]["weights"]["latency"],
        0.25
    );

    let catalog: Value = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&provider=cli-proxy&task=general&limit=100&view=full&variants=all"
        ))
        .send()
        .await
        .expect("expanded catalog response")
        .json()
        .await
        .expect("expanded catalog body");
    assert_eq!(catalog["variants"], "all");
    assert_eq!(catalog["meta"]["total"], 2);
    assert_eq!(catalog["data"][0]["model"]["effort_level"], "max");
    assert_eq!(catalog["data"][0]["benchmarks"]["cost_per_task_usd"], 0.50);
    assert_eq!(catalog["data"][1]["model"]["effort_level"], "medium");
    assert_eq!(catalog["data"][1]["benchmarks"]["cost_per_task_usd"], 0.01);
}

#[tokio::test]
async fn pricing_refresh_flips_auto_efficient_selection() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for provider_name in ["provider-a", "provider-b"] {
        store
            .replace_catalog(
                provider_name,
                &[CatalogRecord {
                    model: "gpt-5.6-luna".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(true),
                    supports_structured_output: Some(true),
                    input_price_per_million: None,
                    output_price_per_million: None,
                }],
            )
            .expect("catalog");
    }
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "gpt-5-6-luna",
                40.0,
                40.0,
                40.0,
                0.0,
                0.0,
            )],
        )
        .expect("benchmarks");
    let observation = |provider: &str, input: f64, output: f64| PriceObservation {
        source: "models.dev".to_owned(),
        source_kind: PriceSourceKind::ModelsDev,
        scope: PriceScope::RuntimeProvider,
        provider_key: Some(provider.to_owned()),
        model_id: "gpt-5.6-luna".to_owned(),
        rates: PriceRates {
            input_price_per_million: Some(input),
            output_price_per_million: Some(output),
            ..PriceRates::default()
        },
        fetched_at: Some(100),
        as_of: None,
        valid_from: None,
        valid_until: None,
        attribution: None,
    };
    store
        .replace_pricing(
            "models.dev",
            PriceSourceKind::ModelsDev,
            "Models.dev (https://models.dev/)",
            &[
                observation("provider-a", 0.2, 1.2),
                observation("provider-b", 5.0, 30.0),
            ],
        )
        .expect("pricing v1");
    drop(store);

    let paid = |base: &str| {
        let mut config = provider(format!("http://{base}/v1"));
        config.billing_mode = BillingMode::Paid;
        config
    };
    let mut config = config_for(
        BTreeMap::from([
            ("provider-a".to_owned(), paid(&upstream.to_string())),
            ("provider-b".to_owned(), paid(&upstream.to_string())),
        ]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path.clone());
    let gateway = spawn_gateway(config).await;

    let primary = |gateway: &str| {
        let gateway = gateway.to_owned();
        async move {
            let body: Value = reqwest::Client::new()
                .get(format!("{gateway}/v1/auto-models?route=efficient"))
                .send()
                .await
                .expect("auto models response")
                .json()
                .await
                .expect("auto models body");
            body["routes"]["efficient"]["primary"]["id"]
                .as_str()
                .expect("primary id")
                .to_owned()
        }
    };

    // Phase 1: models.dev reports provider-a as cheaper; the route picks it.
    assert_eq!(primary(&gateway).await, "provider-a/gpt-5.6-luna");

    // Phase 2: models.dev revises Luna pricing (provider-b now cheaper). A
    // refresh of the active pricing snapshot must change the selection with
    // no hard-coded favorites or special cases.
    let store = RoutingStore::open(Some(&state_path)).expect("routing store reopen");
    store
        .replace_pricing(
            "models.dev",
            PriceSourceKind::ModelsDev,
            "Models.dev (https://models.dev/)",
            &[
                observation("provider-a", 10.0, 60.0),
                observation("provider-b", 0.2, 1.2),
            ],
        )
        .expect("pricing v2");
    drop(store);
    assert_eq!(primary(&gateway).await, "provider-b/gpt-5.6-luna");
}

#[tokio::test]
async fn auto_frontier_selects_cheapest_paid_model_above_floor() {
    let expensive = spawn_provider(ProviderResponse::Stream).await;
    let cheap = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model) in [("expensive", "premium-model"), ("cheap", "budget-model")] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(true),
                    supports_structured_output: Some(true),
                    input_price_per_million: None,
                    output_price_per_million: None,
                }],
            )
            .expect("catalog");
    }
    let premium = BenchmarkModel::fixture("premium-model", 90.0, 90.0, 90.0, 5.0, 10.0);
    let budget = BenchmarkModel::fixture("budget-model", 80.0, 80.0, 80.0, 1.0, 2.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[premium, budget])
        .expect("benchmarks");
    drop(store);
    let expensive_cfg = {
        let mut p = provider(format!("http://{expensive}/v1"));
        p.billing_mode = BillingMode::Paid;
        p
    };
    let cheap_cfg = {
        let mut p = provider(format!("http://{cheap}/v1"));
        p.billing_mode = BillingMode::Paid;
        p
    };
    let mut config = config_for(
        BTreeMap::from([
            ("expensive".to_owned(), expensive_cfg),
            ("cheap".to_owned(), cheap_cfg),
        ]),
        vec![TargetConfig {
            provider: "expensive".to_owned(),
            model: "premium-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-frontier",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("frontier response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "cheap");
}

#[tokio::test]
async fn auto_frontier_skips_free_models_and_free_providers() {
    let free_prov = spawn_provider(ProviderResponse::Success).await;
    let paid_prov = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free-provider",
            &[CatalogRecord {
                model: "free-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    store
        .replace_catalog(
            "paid-provider",
            &[CatalogRecord {
                model: "paid-model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "paid-model",
                70.0,
                70.0,
                70.0,
                1.0,
                1.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([
            (
                "free-provider".to_owned(),
                provider(format!("http://{free_prov}/v1")),
            ),
            ("paid-provider".to_owned(), {
                let mut p = provider(format!("http://{paid_prov}/v1"));
                p.billing_mode = BillingMode::Paid;
                p
            }),
        ]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "paid-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("frontier response");
    // Only paid provider's model should be selected, free provider is skipped
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-provider"],
        "paid-provider"
    );
}

#[tokio::test]
async fn auto_frontier_reroutes_same_canonical_model_before_output() {
    let exhausted = spawn_provider(ProviderResponse::Failure(
        StatusCode::TOO_MANY_REQUESTS,
        "exhausted",
    ))
    .await;
    let available = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for provider in ["a", "b"] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: "carrier-model".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(true),
                    supports_structured_output: Some(true),
                    input_price_per_million: None,
                    output_price_per_million: None,
                }],
            )
            .expect("catalog");
    }
    let mut benchmark = BenchmarkModel::fixture("gpt-canonical", 90.0, 90.0, 90.0, 1.0, 1.0);
    benchmark.creator = Some("OpenAI".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
        .expect("benchmarks");
    drop(store);
    let configured_provider = |base_url: String| {
        let mut configured = provider(base_url);
        configured.billing_mode = BillingMode::Paid;
        configured
            .model_mappings
            .insert("carrier-model".to_owned(), "gpt-canonical".to_owned());
        configured
    };
    let mut config = config_for(
        BTreeMap::from([
            (
                "a".to_owned(),
                configured_provider(format!("http://{exhausted}/v1")),
            ),
            (
                "b".to_owned(),
                configured_provider(format!("http://{available}/v1")),
            ),
        ]),
        vec![TargetConfig {
            provider: "a".to_owned(),
            model: "carrier-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("rerouted response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "b");
    assert_eq!(response.headers()["x-model-gateway-fallbacks"], "1");
}

#[tokio::test]
async fn auto_frontier_skips_free_billing_providers() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "frontier",
            &[CatalogRecord {
                model: "gpt-preview".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "gpt-preview",
                95.0,
                95.0,
                95.0,
                1.0,
                1.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([(
            "frontier".to_owned(),
            provider(format!("http://{upstream}/v1")),
        )]),
        vec![TargetConfig {
            provider: "frontier".to_owned(),
            model: "gpt-preview".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);

    // Free-billing provider with paid offering: skipped by frontier
    let gateway = spawn_gateway(config.clone()).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("frontier error");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    // Set to paid billing: should work
    config
        .providers
        .get_mut("frontier")
        .expect("provider")
        .billing_mode = BillingMode::Paid;
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("frontier response");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auto_frontier_reports_quality_and_capability_exclusions() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let catalog = |supports_tools| CatalogRecord {
        model: "gpt-frontier".to_owned(),
        access_kind: AccessKind::Paid,
        context_length: Some(128_000),
        supports_tools: Some(supports_tools),
        supports_vision: Some(true),
        supports_structured_output: Some(true),
        input_price_per_million: None,
        output_price_per_million: None,
    };
    store
        .replace_catalog("frontier", &[catalog(false)])
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "gpt-frontier",
                60.0,
                60.0,
                60.0,
                1.0,
                1.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut frontier_provider = provider(format!("http://{upstream}/v1"));
    frontier_provider.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("frontier".to_owned(), frontier_provider)]),
        vec![TargetConfig {
            provider: "frontier".to_owned(),
            model: "gpt-frontier".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path.clone());
    config.server.frontier_quality_floor_single = 70.0;
    let quality_gateway = spawn_gateway(config.clone()).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{quality_gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("quality error");
    // Quality floor 70 > model quality 60: no candidates
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    config.server.frontier_quality_floor_single = 50.0;
    let capability_gateway = spawn_gateway(config.clone()).await;
    let response = client
        .post(format!("{capability_gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-frontier",
            "messages": [],
            "tools": [{"type": "function", "function": {"name": "edit"}}]
        }))
        .send()
        .await
        .expect("capability error");
    // Capability mismatch: tools not supported
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    // With tools supported and normal quota, should succeed
    RoutingStore::open(Some(&state_path))
        .expect("routing store")
        .replace_catalog("frontier", &[catalog(true)])
        .expect("updated catalog");
    let ok_gateway = spawn_gateway(config).await;
    let response = client
        .post(format!("{ok_gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("frontier response");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admission_returns_retry_after_while_stream_holds_permit() {
    let provider_address = spawn_provider(ProviderResponse::HoldStream).await;
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    );
    config.server.max_in_flight = 1;
    config.server.admission_timeout_ms = 25;
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let first = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": true, "messages": []}))
        .send()
        .await
        .expect("first stream");
    assert_eq!(first.status(), StatusCode::OK);

    let overloaded = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("overload response");
    assert_eq!(overloaded.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(overloaded.headers()["retry-after"], "1");
    drop(first);
}

#[tokio::test]
async fn active_stream_has_no_total_response_header_deadline() {
    let provider_address = spawn_provider(ProviderResponse::TimedStream).await;
    let mut upstream = provider(format!("http://{provider_address}/v1"));
    upstream.response_header_timeout_seconds = 1;
    upstream.stream_idle_timeout_seconds = 2;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([("local".to_owned(), upstream)]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let body = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": true, "messages": []}))
        .send()
        .await
        .expect("stream response")
        .text()
        .await
        .expect("stream body");
    assert!(body.contains("data: [DONE]"));
}

#[tokio::test]
async fn preserves_multimodal_and_unknown_fields_for_each_target() {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(|Json(body): Json<Value>| async move {
            Json(json!({
                "model": body["model"],
                "echo": body,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
            }))
        }),
    );
    let provider_address = spawn_router(router).await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let response: Value = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "smoke",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,fixture"}}
                ]
            }],
            "vendor_extension": {"preserve": [1, 2, 3]}
        }))
        .send()
        .await
        .expect("multimodal response")
        .json()
        .await
        .expect("multimodal json");
    assert_eq!(response["model"], "upstream-model");
    assert_eq!(
        response["echo"]["messages"][0]["content"][1]["type"],
        "image_url"
    );
    assert_eq!(
        response["echo"]["vendor_extension"],
        json!({"preserve": [1, 2, 3]})
    );
}

#[tokio::test]
async fn transport_failure_does_not_fallback() {
    let unavailable = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("temporary bind")
        .local_addr()
        .expect("temporary address");
    let calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = calls.clone();
    let fallback = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let calls = fallback_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"unexpected": true}))
            }
        }),
    ))
    .await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            (
                "unavailable".to_owned(),
                provider(format!("http://{unavailable}/v1")),
            ),
            (
                "fallback".to_owned(),
                provider(format!("http://{fallback}/v1")),
            ),
        ]),
        vec![
            TargetConfig {
                provider: "unavailable".to_owned(),
                model: "first".to_owned(),
            },
            TargetConfig {
                provider: "fallback".to_owned(),
                model: "second".to_owned(),
            },
        ],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("transport response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers()["x-model-gateway-provider"],
        "unavailable"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn client_disconnect_releases_stream_permit() {
    let provider_address = spawn_provider(ProviderResponse::HoldStream).await;
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    );
    config.server.max_in_flight = 1;
    config.server.admission_timeout_ms = 500;
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let first = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": true, "messages": []}))
        .send()
        .await
        .expect("first stream");
    drop(first);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let second = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": true, "messages": []}))
        .send()
        .await
        .expect("second stream");
    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test]
async fn response_header_timeout_does_not_fallback() {
    let first = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Json(json!({"late": true}))
        }),
    ))
    .await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let calls = fallback_calls.clone();
    let second = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"unexpected": true}))
            }
        }),
    ))
    .await;
    let mut first_config = provider(format!("http://{first}/v1"));
    first_config.response_header_timeout_seconds = 1;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("first".to_owned(), first_config),
            ("second".to_owned(), provider(format!("http://{second}/v1"))),
        ]),
        vec![
            TargetConfig {
                provider: "first".to_owned(),
                model: "first-model".to_owned(),
            },
            TargetConfig {
                provider: "second".to_owned(),
                model: "second-model".to_owned(),
            },
        ],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("timeout response");
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(response.headers()["x-model-gateway-provider"], "first");
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stream_idle_timeout_ends_started_response_without_fallback() {
    let first = spawn_provider(ProviderResponse::HoldStream).await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let calls = fallback_calls.clone();
    let second = spawn_router(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"unexpected": true}))
            }
        }),
    ))
    .await;
    let mut first_config = provider(format!("http://{first}/v1"));
    first_config.stream_idle_timeout_seconds = 1;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([
            ("first".to_owned(), first_config),
            ("second".to_owned(), provider(format!("http://{second}/v1"))),
        ]),
        vec![
            TargetConfig {
                provider: "first".to_owned(),
                model: "first-model".to_owned(),
            },
            TargetConfig {
                provider: "second".to_owned(),
                model: "second-model".to_owned(),
            },
        ],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "smoke", "stream": true, "messages": []}))
        .send()
        .await
        .expect("stream timeout response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "first");
    let mut stream = response.bytes_stream();
    let first_chunk = stream
        .next()
        .await
        .expect("first stream chunk")
        .expect("first stream chunk bytes");
    assert!(first_chunk.starts_with(b"data: {\"choices\":[]}"));
    assert!(stream.next().await.expect("idle timeout chunk").is_err());
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provider_saturation_does_not_block_another_provider() {
    let held = spawn_provider(ProviderResponse::HoldStream).await;
    let available = spawn_provider(ProviderResponse::Success).await;
    let mut held_config = provider(format!("http://{held}/v1"));
    held_config.max_in_flight = Some(1);
    let mut config = Config {
        server: ServerConfig {
            max_in_flight: 4,
            admission_timeout_ms: 25,
            ..ServerConfig::default()
        },
        providers: BTreeMap::from([
            ("held".to_owned(), held_config),
            (
                "available".to_owned(),
                provider(format!("http://{available}/v1")),
            ),
        ]),
        models: BTreeMap::from([
            (
                "held-model".to_owned(),
                ModelConfig {
                    targets: vec![TargetConfig {
                        provider: "held".to_owned(),
                        model: "held-upstream".to_owned(),
                    }],
                },
            ),
            (
                "available-model".to_owned(),
                ModelConfig {
                    targets: vec![TargetConfig {
                        provider: "available".to_owned(),
                        model: "available-upstream".to_owned(),
                    }],
                },
            ),
        ]),
    };
    config.server.max_body_bytes = 1024 * 1024;
    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();
    let held_response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "held-model", "stream": true, "messages": []}))
        .send()
        .await
        .expect("held stream");
    assert_eq!(held_response.status(), StatusCode::OK);
    let available_response = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "available-model", "messages": []}))
        .send()
        .await
        .expect("available response");
    assert_eq!(available_response.status(), StatusCode::OK);
    drop(held_response);
}

#[tokio::test]
async fn caller_sensitive_headers_are_not_forwarded_upstream() {
    let (provider_address, sensitive_headers_seen) = spawn_header_echo_provider().await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .header(header::AUTHORIZATION, "Bearer caller-secret")
        .header(header::COOKIE, "session=caller-secret")
        .header("x-forwarded-for", "198.51.100.10")
        .json(&json!({"model": "smoke", "messages": []}))
        .send()
        .await
        .expect("header response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(sensitive_headers_seen.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_unknown_aliases_in_openai_shape() {
    let provider_address = spawn_provider(ProviderResponse::Success).await;
    let gateway = spawn_gateway(config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{provider_address}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "upstream-model".to_owned(),
        }],
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "missing", "messages": []}))
        .send()
        .await
        .expect("gateway response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.expect("json error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "model_not_found");
}

#[tokio::test]
async fn paid_models_lists_only_paid_provider_offerings() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let mut paid_provider = provider("https://paid.example/v1".to_owned());
    paid_provider.billing_mode = BillingMode::Paid;
    paid_provider.profile = Some(ProviderProfileId::OpenRouter);
    let mut free_provider = provider("https://free.example/v1".to_owned());
    free_provider.profile = Some(ProviderProfileId::Groq);
    let mut config = config_for(
        BTreeMap::from([
            ("paid".to_owned(), paid_provider),
            ("free".to_owned(), free_provider),
        ]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "paid-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path.clone());

    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid",
            &[CatalogRecord {
                model: "gpt-4o".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("paid catalog");
    store
        .replace_pricing(
            "models.dev",
            PriceSourceKind::ModelsDev,
            "Fixture pricing",
            &[PriceObservation {
                source: "models.dev".to_owned(),
                source_kind: PriceSourceKind::ModelsDev,
                scope: PriceScope::RuntimeProvider,
                provider_key: Some("paid".to_owned()),
                model_id: "gpt-4o".to_owned(),
                rates: PriceRates {
                    input_price_per_million: Some(2.5),
                    output_price_per_million: Some(10.0),
                    cache_read_price_per_million: Some(1.25),
                    cache_write_price_per_million: Some(3.75),
                    ..PriceRates::default()
                },
                fetched_at: None,
                as_of: None,
                valid_from: None,
                valid_until: None,
                attribution: None,
            }],
        )
        .expect("pricing");
    store
        .replace_catalog(
            "free",
            &[CatalogRecord {
                model: "gemini-free".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("free catalog");
    store
        .replace_pricing(
            "models.dev",
            PriceSourceKind::ModelsDev,
            "Models.dev fixture",
            &[PriceObservation {
                source: "models.dev".to_owned(),
                source_kind: PriceSourceKind::ModelsDev,
                scope: PriceScope::RuntimeProvider,
                provider_key: Some("paid".to_owned()),
                model_id: "gpt-4o".to_owned(),
                rates: PriceRates {
                    input_price_per_million: Some(2.5),
                    output_price_per_million: Some(10.0),
                    cache_read_price_per_million: Some(1.25),
                    cache_write_price_per_million: Some(3.75),
                    ..PriceRates::default()
                },
                fetched_at: None,
                as_of: None,
                valid_from: None,
                valid_until: None,
                attribution: Some("fixture".to_owned()),
            }],
        )
        .expect("pricing fixture");
    let mut benchmark = BenchmarkModel::fixture("gpt-4o", 82.0, 80.0, 78.0, 2.5, 10.0);
    benchmark.cost_per_task_usd = Some(0.042);
    benchmark.latency_seconds = Some(0.7);
    benchmark.time_to_first_answer_seconds = Some(1.8);
    benchmark.end_to_end_response_seconds = Some(3.6);
    benchmark.output_tokens_per_second = Some(125.0);
    benchmark.cache_read_price_per_million = Some(1.1);
    benchmark.cache_write_price_per_million = Some(3.3);
    store
        .replace_benchmarks("artificial-analysis", "Fixture benchmark", &[benchmark])
        .expect("benchmark fixture");
    let effective = store
        .effective_price("paid", Some("openrouter"), "gpt-4o", None, 604_800)
        .expect("effective fixture price")
        .expect("fixture price observation");
    assert_eq!(effective.cache_read_price_per_million, Some(1.25));
    assert_eq!(effective.cache_write_price_per_million, Some(3.75));

    let gateway = spawn_gateway(config).await;
    let client = reqwest::Client::new();

    let openapi: Value = client
        .get(format!("{gateway}/openapi.json"))
        .send()
        .await
        .expect("OpenAPI response")
        .json()
        .await
        .expect("OpenAPI JSON");
    assert_eq!(openapi["openapi"], "3.1.0");
    for path in [
        "/health/live",
        "/health/ready",
        "/openapi.json",
        "/v1/models",
        "/v1/providers",
        "/v1/auto-models",
        "/v1/rankings",
        "/v1/chat/completions",
        "/v1/catalog/models",
        "/v1/catalog/models/{provider}/{model}",
    ] {
        assert!(
            openapi["paths"][path].is_object(),
            "missing OpenAPI path: {path}"
        );
    }
    assert_eq!(
        openapi["paths"]["/v1/catalog/models"]["get"]["summary"],
        "List catalog model resources"
    );

    let response = client
        .get(format!("{gateway}/v1/catalog/models?access=paid&view=full"))
        .send()
        .await
        .expect("paid-models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["object"], "model.collection");
    assert_eq!(body["view"], "full");
    assert_eq!(body["meta"]["limit"], 25);
    // Only the paid provider's models should appear (free provider excluded)
    assert_eq!(
        body["data"].as_array().map(|a| a.len()),
        Some(1),
        "should only include the paid (non-free) model"
    );
    assert_eq!(body["data"][0]["model"]["provider"], "paid");
    assert_eq!(body["data"][0]["model"]["name"], "gpt-4o");
    assert_eq!(body["data"][0]["id"], "paid/gpt-4o");
    assert_eq!(
        body["data"][0]["links"]["self"],
        format!("{gateway}/v1/catalog/models/paid/gpt-4o")
    );
    assert_eq!(body["data"][0]["price_per_million"]["cache_read"], 1.25);
    assert_eq!(body["data"][0]["price_per_million"]["cache_write"], 3.75);
    assert_eq!(body["data"][0]["price_per_million"]["input"], 2.5);
    assert_eq!(body["data"][0]["benchmarks"]["cost_per_task_usd"], 0.042);
    assert_eq!(
        body["data"][0]["benchmarks"]["end_to_end_response_seconds"],
        3.6
    );
    assert_eq!(
        body["data"][0]["benchmarks"]["output_tokens_per_second"],
        125.0
    );
    assert_eq!(
        body["data"][0]["benchmarks"]["output_tokens_per_task"],
        1024
    );

    let detail: Value = client
        .get(format!("{gateway}/v1/catalog/models/paid/gpt-4o"))
        .send()
        .await
        .expect("model detail response")
        .json()
        .await
        .expect("model detail JSON");
    assert_eq!(detail["object"], "model");
    assert_eq!(detail["id"], "paid/gpt-4o");
    assert_eq!(detail["data"]["model"]["name"], "gpt-4o");
    assert_eq!(detail["data"]["price_per_million"]["cache_read"], 1.25);
    assert_eq!(detail["data"]["price_per_million"]["cache_write"], 3.75);
    assert_eq!(detail["data"]["benchmarks"]["cost_per_task_usd"], 0.042);
    assert_eq!(
        detail["data"]["benchmarks"]["time_to_first_answer_seconds"],
        1.8
    );

    let collection = client
        .get(format!("{gateway}/v1/catalog/models?access=all&limit=1"))
        .send()
        .await
        .expect("catalog collection response");
    assert_eq!(collection.status(), StatusCode::OK);
    assert!(collection.headers().get("etag").is_some());
    assert!(collection.headers().get("last-modified").is_some());
    let etag = collection
        .headers()
        .get("etag")
        .expect("etag")
        .to_str()
        .expect("etag value")
        .to_owned();
    let last_modified = collection
        .headers()
        .get("last-modified")
        .expect("last-modified")
        .to_str()
        .expect("last-modified value")
        .to_owned();
    let collection_body: Value = collection.json().await.expect("catalog collection JSON");
    assert_eq!(collection_body["object"], "model.collection");
    assert_eq!(collection_body["view"], "summary");
    assert_eq!(collection_body["meta"]["total"], 2);
    assert_eq!(collection_body["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        collection_body["data"][0]
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect::<Vec<_>>()),
        Some(vec!["id", "links", "quality", "reasoning_effort"])
    );
    assert!(collection_body["data"][0]["links"]["self"].is_string());
    assert!(collection_body["data"][0]["quality"].is_object());
    assert!(collection_body["data"][0].get("benchmarks").is_none());
    assert!(collection_body["links"]["next"].is_string());

    let full_bytes = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&limit=1&view=full"
        ))
        .send()
        .await
        .expect("full catalog response")
        .bytes()
        .await
        .expect("full catalog bytes");
    let selected_response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&limit=1&view=full&fields=id,links,benchmarks"
        ))
        .send()
        .await
        .expect("selected catalog response");
    let selected_etag = selected_response
        .headers()
        .get("etag")
        .expect("selected etag")
        .to_str()
        .expect("selected etag value")
        .to_owned();
    let selected_content_length = selected_response
        .headers()
        .get(header::CONTENT_LENGTH)
        .expect("selected content length")
        .to_str()
        .expect("selected content length value")
        .parse::<usize>()
        .expect("numeric selected content length");
    let selected_bytes = selected_response
        .bytes()
        .await
        .expect("selected catalog bytes");
    assert_eq!(selected_content_length, selected_bytes.len());
    assert!(
        full_bytes.len() > selected_bytes.len(),
        "full catalog response ({}) must exceed the selected projection ({})",
        full_bytes.len(),
        selected_bytes.len()
    );
    assert_ne!(etag, selected_etag, "ETag must identify the representation");
    let selected_body: Value = serde_json::from_slice(&selected_bytes).expect("selected JSON");
    assert_eq!(
        selected_body["meta"]["fields"],
        json!(["id", "links", "benchmarks"])
    );
    assert_eq!(
        selected_body["data"][0]
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect::<Vec<_>>()),
        Some(vec!["benchmarks", "id", "links"])
    );

    let reordered_response = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&limit=1&view=full&fields=benchmarks,id,links"
        ))
        .send()
        .await
        .expect("reordered selected catalog response");
    assert_eq!(
        reordered_response
            .headers()
            .get("etag")
            .expect("reordered etag")
            .to_str()
            .expect("reordered etag value"),
        selected_etag
    );
    let reordered_fields: Value = reordered_response
        .json()
        .await
        .expect("reordered selected catalog JSON");
    assert_eq!(
        reordered_fields["meta"]["fields"],
        selected_body["meta"]["fields"]
    );
    assert_eq!(reordered_fields["data"], selected_body["data"]);
    assert!(
        reordered_fields["links"]["self"]
            .as_str()
            .is_some_and(|link| link.contains("fields=id%2Clinks%2Cbenchmarks")),
        "pagination links must use canonical field ordering"
    );

    let invalid_fields: Value = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&fields=id,not_a_field"
        ))
        .send()
        .await
        .expect("invalid fields response")
        .json()
        .await
        .expect("invalid fields JSON");
    assert_eq!(invalid_fields["error"]["code"], "invalid_fields");

    let next_link = collection_body["links"]["next"]
        .as_str()
        .expect("next link");
    let cursor = next_link.split("cursor=").nth(1).expect("cursor");
    let stale_cursor = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&limit=1&cursor={cursor}"
        ))
        .send()
        .await
        .expect("stale cursor response");
    assert_eq!(stale_cursor.status(), StatusCode::CONFLICT);

    let snapshot = collection_body["meta"]["snapshot"]
        .as_str()
        .expect("snapshot token");
    let past_end: Value = client
        .get(format!(
            "{gateway}/v1/catalog/models?access=all&limit=1&cursor={snapshot}:999"
        ))
        .send()
        .await
        .expect("past-end cursor response")
        .json()
        .await
        .expect("past-end cursor JSON");
    assert_eq!(past_end["error"]["code"], "invalid_cursor");

    let not_modified = client
        .get(format!("{gateway}/v1/catalog/models?access=all&limit=1"))
        .header("if-none-match", etag.clone())
        .send()
        .await
        .expect("conditional catalog response");
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert!(not_modified.bytes().await.expect("304 body").is_empty());

    let weak_not_modified = client
        .get(format!("{gateway}/v1/catalog/models?access=all&limit=1"))
        .header("if-none-match", format!("W/{etag}"))
        .send()
        .await
        .expect("weak conditional catalog response");
    assert_eq!(weak_not_modified.status(), StatusCode::NOT_MODIFIED);

    let wildcard_not_modified = client
        .get(format!("{gateway}/v1/catalog/models?access=all&limit=1"))
        .header("if-none-match", "*")
        .send()
        .await
        .expect("wildcard conditional catalog response");
    assert_eq!(wildcard_not_modified.status(), StatusCode::NOT_MODIFIED);

    let not_modified_since = client
        .get(format!("{gateway}/v1/catalog/models?access=all&limit=1"))
        .header("if-modified-since", last_modified)
        .send()
        .await
        .expect("if-modified-since catalog response");
    assert_eq!(not_modified_since.status(), StatusCode::NOT_MODIFIED);

    let free_detail = client
        .get(format!("{gateway}/v1/catalog/models/free/gemini-free"))
        .send()
        .await
        .expect("free model detail response");
    assert_eq!(free_detail.status(), StatusCode::OK);
    let free_detail_body: Value = free_detail.json().await.expect("free model detail JSON");
    assert_eq!(free_detail_body["data"]["id"], "free/gemini-free");

    let legacy_detail = client
        .get(format!("{gateway}/v1/models/paid/gpt-4o"))
        .send()
        .await
        .expect("legacy model detail response");
    assert_eq!(legacy_detail.status(), StatusCode::NOT_FOUND);

    for path in ["/v1/free-models", "/v1/paid-models"] {
        let legacy_collection = client
            .get(format!("{gateway}{path}"))
            .send()
            .await
            .expect("legacy collection response");
        assert_eq!(legacy_collection.status(), StatusCode::NOT_FOUND);
    }

    let invalid_query = client
        .get(format!("{gateway}/v1/catalog/models?access=invalid"))
        .send()
        .await
        .expect("invalid catalog query response");
    assert_eq!(invalid_query.status(), StatusCode::BAD_REQUEST);
    let invalid_query_body: Value = invalid_query.json().await.expect("invalid query JSON");
    assert_eq!(invalid_query_body["error"]["code"], "invalid_query");

    for limit in ["0", "101"] {
        let invalid_limit: Value = client
            .get(format!("{gateway}/v1/catalog/models?limit={limit}"))
            .send()
            .await
            .expect("invalid limit response")
            .json()
            .await
            .expect("invalid limit JSON");
        assert_eq!(invalid_limit["error"]["code"], "invalid_limit");
    }
}

#[tokio::test]
async fn subscription_models_report_zero_effective_and_reference_prices() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "cli-proxy",
            &[CatalogRecord {
                model: "gpt-subscription".to_owned(),
                access_kind: AccessKind::SubscriptionIncluded,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: Some(1.25),
                output_price_per_million: Some(10.0),
            }],
        )
        .expect("catalog");
    let mut benchmark = BenchmarkModel::fixture("gpt-subscription", 60.0, 60.0, 60.0, 1.25, 10.0);
    benchmark.latency_seconds = Some(1.0);
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
        .expect("benchmarks");
    drop(store);

    let mut cli_proxy = provider(format!("http://{upstream}/v1"));
    cli_proxy.profile = Some(ProviderProfileId::CliProxyApi);
    cli_proxy.billing_mode = BillingMode::Subscription;
    let mut config = config_for(
        BTreeMap::from([("cli-proxy".to_owned(), cli_proxy)]),
        vec![TargetConfig {
            provider: "cli-proxy".to_owned(),
            model: "gpt-subscription".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=paid&provider=cli-proxy&view=full"
        ))
        .send()
        .await
        .expect("paid models response");
    let body: Value = response.json().await.expect("paid models body");
    let model = &body["data"][0];
    assert_eq!(model["access"]["kind"], "subscription_included");
    assert_eq!(model["access"]["overage"], "subscription_limited");
    assert_eq!(model["price_per_million"]["input"], 0.0);
    assert_eq!(model["price_per_million"]["source"], "subscription");
    assert_eq!(model["reference_price_per_million"]["input"], 1.25);
    assert_eq!(model["reference_price_per_million"]["output"], 10.0);

    let response = reqwest::Client::new()
        .get(format!(
            "{gateway}/v1/catalog/models?access=free&provider=cli-proxy&view=full"
        ))
        .send()
        .await
        .expect("free models response");
    let body: Value = response.json().await.expect("free models body");
    assert!(body["data"].as_array().is_some_and(Vec::is_empty));

    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?route=frontier&view=full"))
        .send()
        .await
        .expect("auto models response");
    let body: Value = response.json().await.expect("auto models body");
    let primary = &body["routes"]["frontier"]["primary"];
    assert_eq!(primary["access"]["kind"], "subscription_included");
    assert_eq!(primary["expected_cost_microusd"], 0);
    assert!(primary["reference_cost_microusd"].is_null());
    assert!(primary["estimated_cost_microusd"].as_u64().is_some());
    assert_eq!(primary["cost_source"], "token_price_scenario");

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-frontier",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("subscription completion");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-expected-cost-microusd"],
        "0"
    );
}

#[tokio::test]
async fn auto_balanced_selects_mid_range_model() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid",
            &[CatalogRecord {
                model: "balanced-model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: Some(2.0),
                output_price_per_million: Some(4.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "balanced-model",
                75.0,
                70.0,
                65.0,
                2.0,
                4.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    let mut paid_provider = provider(format!("http://{upstream}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("paid".to_owned(), paid_provider)]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "balanced-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(
            &json!({"model": "auto-balanced", "messages": [{"role": "user", "content": "Hello."}]}),
        )
        .send()
        .await
        .expect("auto-balanced response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "paid");
}

#[tokio::test]
async fn auto_balanced_disabled_when_config_says_so() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid",
            &[CatalogRecord {
                model: "model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: Some(2.0),
                output_price_per_million: Some(4.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture("model", 75.0, 70.0, 65.0, 2.0, 4.0)],
        )
        .expect("benchmarks");
    drop(store);
    let mut paid_provider = provider(format!("http://{upstream}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("paid".to_owned(), paid_provider)]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.auto_balanced_enabled = false;
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(
            &json!({"model": "auto-balanced", "messages": [{"role": "user", "content": "Hello."}]}),
        )
        .send()
        .await
        .expect("auto-balanced response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["error"]["code"], "route_disabled");
}

#[tokio::test]
async fn auto_balanced_appears_in_model_listing() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let _store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider(format!("http://{upstream}/v1")),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "test".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.auto_balanced_enabled = true;
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .expect("models response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"auto-balanced"));
    assert!(ids.contains(&"auto-efficient"));
    assert!(ids.contains(&"auto-frontier"));
    assert!(ids.contains(&"auto-free"));
}

#[tokio::test]
async fn auto_balanced_falls_back_to_auto_free() {
    let free = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free-prov",
            &[CatalogRecord {
                model: "free-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([(
            "free-prov".to_owned(),
            provider(format!("http://{free}/v1")),
        )]),
        vec![TargetConfig {
            provider: "free-prov".to_owned(),
            model: "free-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(
            &json!({"model": "auto-balanced", "messages": [{"role": "user", "content": "Hello."}]}),
        )
        .send()
        .await
        .expect("auto-balanced response");
    assert_eq!(response.status(), StatusCode::OK);
    // No paid benchmarks available, so balanced falls back to auto-free
    assert_eq!(response.headers()["x-model-gateway-provider"], "free-prov");
}

#[tokio::test]
async fn auto_balanced_with_no_benchmarks_returns_error() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let _store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider("http://127.0.0.1:1/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "test".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.auto_free_enabled = false;
    config.server.local_base_url = "http://127.0.0.1:1/v1".to_owned();
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-balanced", "messages": []}))
        .send()
        .await
        .expect("auto-balanced response");
    // No benchmarks, no free models, no local → error
    assert!(response.status().is_client_error() || response.status().is_server_error());
}

#[tokio::test]
async fn runtime_rejects_suggestions_until_mapping_is_approved() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid-provider",
            &[CatalogRecord {
                model: "model-family".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(2.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "model-family-2025",
                50.0,
                50.0,
                50.0,
                1.0,
                2.0,
            )],
        )
        .expect("benchmarks");

    let mut paid = provider(format!("http://{upstream}/v1"));
    paid.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([("paid-provider".to_owned(), paid)]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "model-family".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.auto_free_enabled = false;
    config.server.local_base_url = "http://127.0.0.1:1/v1".to_owned();
    let gateway = spawn_gateway(config).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-balanced",
            "messages": [{"role": "user", "content": "before approval"}]
        }))
        .send()
        .await
        .expect("unapproved response");
    assert!(!response.status().is_success());

    store
        .approve_model_mapping("paid-provider", "model-family", "model-family-2025")
        .expect("approve mapping");
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-balanced",
            "messages": [{"role": "user", "content": "after approval"}]
        }))
        .send()
        .await
        .expect("approved response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-match"],
        "approved"
    );
}

#[tokio::test]
async fn canonical_entity_link_propagates_to_exact_provider_alias() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "paid-provider",
            &[CatalogRecord {
                model: "Vendor/Canonical-Model".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(1.0),
                output_price_per_million: Some(2.0),
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "canonical-benchmark",
                50.0,
                50.0,
                50.0,
                1.0,
                2.0,
            )],
        )
        .expect("benchmarks");
    let entity_id = "hf:vendor/canonical-model";
    store
        .replace_identity_source(&IdentityImport {
            source: "models.dev".to_owned(),
            attribution: "Fixture".to_owned(),
            entities: vec![IdentityEntityRecord {
                id: entity_id.to_owned(),
                creator: Some("vendor".to_owned()),
                family: Some("canonical".to_owned()),
                version: None,
                variant: None,
                release_date: None,
                hugging_face_id: Some("Vendor/Canonical-Model".to_owned()),
            }],
            aliases: vec![IdentityAliasRecord {
                source: "models.dev".to_owned(),
                provider_key: "paid-profile".to_owned(),
                provider_model_id: "Vendor/Canonical-Model".to_owned(),
                entity_id: entity_id.to_owned(),
                confidence: IdentityConfidence::CanonicalReference,
                provenance_url: "fixture".to_owned(),
                observed_at: 100,
            }],
        })
        .expect("identities");
    store
        .approve_benchmark_identity_link(entity_id, "canonical-benchmark", "fixture")
        .expect("entity link");

    let mut paid = provider(format!("http://{upstream}/v1"));
    paid.billing_mode = BillingMode::Paid;
    paid.pricing_profile = Some("paid-profile".to_owned());
    let mut config = config_for(
        BTreeMap::from([("paid-provider".to_owned(), paid)]),
        vec![TargetConfig {
            provider: "paid-provider".to_owned(),
            model: "Vendor/Canonical-Model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.auto_free_enabled = false;
    config.server.local_base_url = "http://127.0.0.1:1/v1".to_owned();
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-balanced",
            "messages": [{"role": "user", "content": "canonical entity"}]
        }))
        .send()
        .await
        .expect("entity-linked response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-match"],
        "approved"
    );
}

#[tokio::test]
async fn auto_efficient_falls_back_to_auto_free() {
    let free = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "free-prov",
            &[CatalogRecord {
                model: "free-model".to_owned(),
                access_kind: AccessKind::ZeroPrice,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(false),
                supports_structured_output: Some(false),
                input_price_per_million: Some(0.0),
                output_price_per_million: Some(0.0),
            }],
        )
        .expect("catalog");
    drop(store);
    let mut config = config_for(
        BTreeMap::from([(
            "free-prov".to_owned(),
            provider(format!("http://{free}/v1")),
        )]),
        vec![TargetConfig {
            provider: "free-prov".to_owned(),
            model: "free-model".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-efficient", "messages": [{"role": "user", "content": "Hello."}]}))
        .send()
        .await
        .expect("auto-efficient response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "free-prov");
}

#[tokio::test]
async fn reserved_alias_auto_balanced_is_rejected() {
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let _store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let mut config = config_for(
        BTreeMap::from([(
            "local".to_owned(),
            provider("http://127.0.0.1:1/v1".to_owned()),
        )]),
        vec![TargetConfig {
            provider: "local".to_owned(),
            model: "test".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.models.insert(
        "auto-balanced".to_owned(),
        ModelConfig {
            targets: vec![TargetConfig {
                provider: "local".to_owned(),
                model: "test".to_owned(),
            }],
        },
    );
    let result = config.validate_structure();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("reserved"),
        "expected reserved error, got: {err}"
    );
}

#[tokio::test]
async fn health_diagnostics_reports_credentials_and_catalog_separately() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "keyless",
            &[CatalogRecord {
                model: "model-a".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: None,
                supports_tools: None,
                supports_vision: None,
                supports_structured_output: None,
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    drop(store);
    let mut locked = provider(format!("http://{upstream}/v1"));
    locked.api_key_secret = Some("MISSING_GATEWAY_KEY".to_owned());
    let mut config = config_for(
        BTreeMap::from([
            (
                "keyless".to_owned(),
                provider(format!("http://{upstream}/v1")),
            ),
            ("locked".to_owned(), locked),
        ]),
        vec![TargetConfig {
            provider: "keyless".to_owned(),
            model: "model-a".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway}/health/diagnostics"))
        .send()
        .await
        .expect("diagnostics response")
        .json()
        .await
        .expect("diagnostics body");
    // The gateway itself is healthy even though one provider has no credential
    // and the other has no catalog: readiness and provider data are separate.
    assert_eq!(body["status"], "ready");
    assert_eq!(body["gateway"]["providers_configured"], 2);
    assert_eq!(body["gateway"]["providers_with_credentials"], 0);
    assert_eq!(body["provider_catalogs"]["status"], "partial");
    let providers = body["providers"].as_array().expect("providers array");
    assert!(
        providers
            .iter()
            .all(|provider| provider["id"] != "\u{0}local")
    );
    let locked = providers
        .iter()
        .find(|provider| provider["id"] == "locked")
        .expect("locked provider");
    assert_eq!(locked["credential"], "missing");
    assert_eq!(locked["credential_source"], Value::Null);
    assert_eq!(locked["catalog"], "missing");
    assert_eq!(locked["available"], false);
    let keyless = providers
        .iter()
        .find(|provider| provider["id"] == "keyless")
        .expect("keyless provider");
    assert_eq!(keyless["credential"], "not_required");
    assert_eq!(keyless["catalog"], "fresh");
    assert_eq!(keyless["available"], true);
    let raw = body.to_string();
    assert!(!raw.contains("MISSING_GATEWAY_KEY"));
    assert!(!raw.contains("sk-"));
}

#[tokio::test]
async fn health_diagnostics_distinguishes_healthy_gateway_from_stale_catalogs() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "fixture",
            &[CatalogRecord {
                model: "model-a".to_owned(),
                access_kind: AccessKind::Paid,
                context_length: None,
                supports_tools: None,
                supports_vision: None,
                supports_structured_output: None,
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    store
        .replace_benchmarks(
            "fixture",
            "Fixture",
            &[BenchmarkModel::fixture(
                "model-a", 50.0, 50.0, 50.0, 1.0, 2.0,
            )],
        )
        .expect("benchmarks");
    drop(store);
    // Age the snapshots past their freshness window deterministically:
    // `refreshed_at` is stored with whole-second precision, so a 2s wait with
    // a 1s window guarantees staleness.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut config = config_for(
        BTreeMap::from([(
            "fixture".to_owned(),
            provider(format!("http://{upstream}/v1")),
        )]),
        vec![TargetConfig {
            provider: "fixture".to_owned(),
            model: "model-a".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.catalog_max_age_seconds = 1;
    config.server.benchmark_max_age_seconds = 1;
    config.server.pricing_max_age_seconds = 1;
    let gateway = spawn_gateway(config).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway}/health/diagnostics"))
        .send()
        .await
        .expect("diagnostics response")
        .json()
        .await
        .expect("diagnostics body");
    // A healthy gateway with stale provider data: readiness stays green while
    // the provider catalog and benchmark snapshot report unavailability.
    assert_eq!(body["status"], "ready");
    assert_eq!(body["gateway"]["status"], "ready");
    assert_eq!(body["provider_catalogs"]["status"], "unavailable");
    assert_eq!(body["provider_catalogs"]["fresh"], 0);
    assert_eq!(body["benchmarks"]["status"], "stale");
    assert_eq!(body["benchmarks"]["snapshots"], 1);
    assert_eq!(body["providers"][0]["catalog"], "stale");
}

#[tokio::test]
async fn frontier_without_benchmark_latency_reports_missing_and_uses_cost_order() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider_name, input, output) in [("provider-a", 0.2, 1.2), ("provider-b", 5.0, 30.0)] {
        store
            .replace_catalog(
                provider_name,
                &[CatalogRecord {
                    model: "gpt-5.6-luna".to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(true),
                    supports_structured_output: Some(true),
                    input_price_per_million: Some(input),
                    output_price_per_million: Some(output),
                }],
            )
            .expect("catalog");
    }
    let mut benchmark = BenchmarkModel::fixture("gpt-5-6-luna", 60.0, 60.0, 60.0, 0.2, 1.2);
    benchmark.latency_seconds = None;
    benchmark.end_to_end_response_seconds = None;
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
        .expect("benchmarks");
    drop(store);
    let paid = |base: &str| {
        let mut config = provider(format!("http://{base}/v1"));
        config.billing_mode = BillingMode::Paid;
        config
    };
    let mut config = config_for(
        BTreeMap::from([
            ("provider-a".to_owned(), paid(&upstream.to_string())),
            ("provider-b".to_owned(), paid(&upstream.to_string())),
        ]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.frontier_quality_floor_single = 50.0;
    let gateway = spawn_gateway(config).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?route=frontier&view=full"))
        .send()
        .await
        .expect("auto models response")
        .json()
        .await
        .expect("auto models body");
    let route = &body["routes"]["frontier"];
    // Missing latency is reported explicitly, never invented: no candidate had
    // a measured latency, so the latency weight is inert and the cheaper
    // provider wins on cost alone.
    assert_eq!(route["latency_observed"], false);
    assert_eq!(route["selection_policy"]["weights"]["latency"], 0.25);
    let primary = &route["primary"];
    assert_eq!(primary["id"], "provider-a/gpt-5.6-luna");
    assert_eq!(primary["latency_seconds"], Value::Null);
    assert_eq!(primary["latency_available"], false);

    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-frontier",
            "messages": [{"role": "user", "content": "latency diagnostic"}]
        }))
        .send()
        .await
        .expect("frontier response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-gateway-benchmark-latency-observed"],
        "false"
    );
    assert!(
        response
            .headers()
            .get("x-model-gateway-benchmark-latency-seconds")
            .is_none()
    );
}

#[tokio::test]
async fn frontier_observed_latency_outranks_missing_latency_at_equal_cost() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider_name, model) in [
        ("provider-a", "gpt-5.6-luna"),
        ("provider-b", "gpt-5.6-sol"),
    ] {
        store
            .replace_catalog(
                provider_name,
                &[CatalogRecord {
                    model: model.to_owned(),
                    access_kind: AccessKind::Paid,
                    context_length: Some(128_000),
                    supports_tools: Some(true),
                    supports_vision: Some(true),
                    supports_structured_output: Some(true),
                    input_price_per_million: Some(5.0),
                    output_price_per_million: Some(30.0),
                }],
            )
            .expect("catalog");
    }
    let mut observed = BenchmarkModel::fixture("gpt-5-6-luna", 60.0, 60.0, 60.0, 5.0, 30.0);
    observed.latency_seconds = Some(1.0);
    let mut missing = BenchmarkModel::fixture("gpt-5-6-sol", 60.0, 60.0, 60.0, 5.0, 30.0);
    missing.latency_seconds = None;
    missing.end_to_end_response_seconds = None;
    store
        .replace_benchmarks("fixture", "Fixture", &[observed, missing])
        .expect("benchmarks");
    drop(store);
    let paid = |base: &str| {
        let mut config = provider(format!("http://{base}/v1"));
        config.billing_mode = BillingMode::Paid;
        config
    };
    let mut config = config_for(
        BTreeMap::from([
            ("provider-a".to_owned(), paid(&upstream.to_string())),
            ("provider-b".to_owned(), paid(&upstream.to_string())),
        ]),
        vec![TargetConfig {
            provider: "provider-a".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    config.server.frontier_quality_floor_single = 50.0;
    let gateway = spawn_gateway(config).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{gateway}/v1/auto-models?route=frontier&view=full"))
        .send()
        .await
        .expect("auto models response")
        .json()
        .await
        .expect("auto models body");
    let route = &body["routes"]["frontier"];
    assert_eq!(route["latency_observed"], true);
    let primary = &route["primary"];
    // Equal quality and cost: the observed-latency candidate earns the latency
    // weight; the missing-latency candidate earns nothing and cannot win.
    assert_eq!(primary["id"], "provider-a/gpt-5.6-luna");
    assert_eq!(primary["latency_available"], true);
    assert_eq!(primary["latency_seconds"], 1.0);
    let fallback = &route["fallbacks"][0];
    assert_eq!(fallback["id"], "provider-b/gpt-5.6-sol");
    assert_eq!(fallback["latency_available"], false);
    assert_eq!(fallback["latency_seconds"], Value::Null);
}
