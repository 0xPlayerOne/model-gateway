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
    BillingMode, Config, ModelConfig, ProviderConfig, QuotaBoundary, QuotaKind, QuotaLimit,
    ServerConfig, TargetConfig,
};
use model_gateway::gateway::build_app;
use model_gateway::routing::{CatalogRecord, RoutingStore};
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
    assert_eq!(models["data"][3]["id"], "auto-frontier");
    assert_eq!(models["data"][4]["id"], "smoke");
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
                is_free: true,
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
                is_free: true,
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
    let mut config = config_for(
        BTreeMap::from([
            (
                "unsupported".to_owned(),
                provider(format!("http://{unsupported}/v1")),
            ),
            (
                "supported".to_owned(),
                provider(format!("http://{supported}/v1")),
            ),
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
                is_free: false,
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
                is_free: false,
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
    assert_eq!(complex.headers()["x-model-gateway-provider"], "strong");
}

#[tokio::test]
async fn auto_efficient_honors_explicit_paid_authorization_and_spend_caps() {
    let paid = spawn_provider(ProviderResponse::Success).await;
    let free = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model, is_free) in [("paid", "paid-model", false), ("free", "free-model", true)]
    {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    is_free,
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
            &[
                BenchmarkModel::fixture("paid-model", 90.0, 90.0, 90.0, 1.0, 1.0),
                BenchmarkModel::fixture("free-model", 50.0, 50.0, 50.0, 0.0, 0.0),
            ],
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
    let mut free_provider = provider(format!("http://{free}/v1"));
    free_provider.free_models = vec!["free-model".to_owned()];
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
    let second = client
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&request)
        .send()
        .await
        .expect("spend-capped fallback response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["x-model-gateway-provider"], "paid");
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
                is_free: false,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    let mut low = BenchmarkModel::fixture("canonical-model", 80.0, 80.0, 80.0, 1.0, 1.0);
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
async fn auto_frontier_selects_only_openai_or_anthropic_canonical_creators() {
    let anthropic = spawn_provider(ProviderResponse::Stream).await;
    let other = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model) in [("anthropic", "claude"), ("other", "other-model")] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    is_free: false,
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
    let mut claude = BenchmarkModel::fixture("claude", 90.0, 90.0, 90.0, 2.0, 4.0);
    claude.creator = Some("Anthropic".to_owned());
    let mut cheaper = BenchmarkModel::fixture("other-model", 99.0, 99.0, 99.0, 0.1, 0.1);
    cheaper.creator = Some("Other Labs".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[claude, cheaper])
        .expect("benchmarks");
    drop(store);
    let mut anthropic_provider = provider(format!("http://{anthropic}/v1"));
    anthropic_provider.billing_mode = BillingMode::Paid;
    let mut other_provider = provider(format!("http://{other}/v1"));
    other_provider.billing_mode = BillingMode::Paid;
    let mut config = config_for(
        BTreeMap::from([
            ("anthropic".to_owned(), anthropic_provider),
            ("other".to_owned(), other_provider),
        ]),
        vec![TargetConfig {
            provider: "anthropic".to_owned(),
            model: "claude".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({
            "model": "auto-frontier",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("frontier response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-model-gateway-provider"], "anthropic");
    let body = response.text().await.expect("frontier stream");
    assert!(body.contains("- Claude:"));
    assert!(body.contains("data: [DONE]"));
}

#[tokio::test]
async fn auto_frontier_returns_explicit_error_without_free_or_local_fallback() {
    let paid = spawn_provider(ProviderResponse::Success).await;
    let free = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    for (provider, model, is_free) in [
        ("paid", "non-frontier", false),
        ("free", "free-model", true),
    ] {
        store
            .replace_catalog(
                provider,
                &[CatalogRecord {
                    model: model.to_owned(),
                    is_free,
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
    let mut benchmark = BenchmarkModel::fixture("non-frontier", 99.0, 99.0, 99.0, 0.1, 0.1);
    benchmark.creator = Some("Other Labs".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
        .expect("benchmarks");
    drop(store);
    let mut paid_provider = provider(format!("http://{paid}/v1"));
    paid_provider.billing_mode = BillingMode::Paid;
    let mut free_provider = provider(format!("http://{free}/v1"));
    free_provider.free_models = vec!["free-model".to_owned()];
    let mut config = config_for(
        BTreeMap::from([
            ("paid".to_owned(), paid_provider),
            ("free".to_owned(), free_provider),
        ]),
        vec![TargetConfig {
            provider: "paid".to_owned(),
            model: "non-frontier".to_owned(),
        }],
    );
    config.server.state_path = Some(state_path);
    let gateway = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("frontier error");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("error JSON");
    assert_eq!(body["error"]["code"], "frontier_access_unconfigured");
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
                    is_free: false,
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
async fn auto_frontier_requires_explicit_billing_and_preview_authorization() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    store
        .replace_catalog(
            "frontier",
            &[CatalogRecord {
                model: "gpt-preview".to_owned(),
                is_free: false,
                context_length: Some(128_000),
                supports_tools: Some(true),
                supports_vision: Some(true),
                supports_structured_output: Some(true),
                input_price_per_million: None,
                output_price_per_million: None,
            }],
        )
        .expect("catalog");
    let mut benchmark = BenchmarkModel::fixture("gpt-preview", 95.0, 95.0, 95.0, 1.0, 1.0);
    benchmark.creator = Some("OpenAI".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
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
    let unauthorized = spawn_gateway(config.clone()).await;
    let response = reqwest::Client::new()
        .post(format!("{unauthorized}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("billing error");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("billing error JSON");
    assert_eq!(body["error"]["code"], "frontier_billing_not_authorized");

    let frontier = config.providers.get_mut("frontier").expect("provider");
    frontier.billing_mode = BillingMode::Paid;
    let preview_blocked = spawn_gateway(config.clone()).await;
    let response = reqwest::Client::new()
        .post(format!("{preview_blocked}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("preview error");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("preview error JSON");
    assert_eq!(body["error"]["code"], "frontier_preview_not_authorized");

    config
        .providers
        .get_mut("frontier")
        .expect("provider")
        .allow_preview_models = true;
    let preview_allowed = spawn_gateway(config).await;
    let response = reqwest::Client::new()
        .post(format!("{preview_allowed}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("preview response");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auto_frontier_reports_quality_capability_and_spend_exclusions() {
    let upstream = spawn_provider(ProviderResponse::Success).await;
    let directory = tempfile::tempdir().expect("state directory");
    let state_path = directory.path().join("routing.sqlite3");
    let store = RoutingStore::open(Some(&state_path)).expect("routing store");
    let catalog = |supports_tools| CatalogRecord {
        model: "gpt-frontier".to_owned(),
        is_free: false,
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
    let mut benchmark = BenchmarkModel::fixture("gpt-frontier", 60.0, 60.0, 60.0, 1.0, 1.0);
    benchmark.creator = Some("OpenAI".to_owned());
    store
        .replace_benchmarks("fixture", "Fixture", &[benchmark])
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
    config.server.frontier_quality_floor_simple = 70.0;
    let quality_gateway = spawn_gateway(config.clone()).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{quality_gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("quality error");
    let body: Value = response.json().await.expect("quality error JSON");
    assert_eq!(body["error"]["code"], "frontier_quality_floor_not_met");

    config.server.frontier_quality_floor_simple = 50.0;
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
    let body: Value = response.json().await.expect("capability error JSON");
    assert_eq!(body["error"]["code"], "frontier_capability_mismatch");

    RoutingStore::open(Some(&state_path))
        .expect("routing store")
        .replace_catalog("frontier", &[catalog(true)])
        .expect("updated catalog");
    config
        .providers
        .get_mut("frontier")
        .expect("provider")
        .quotas = vec![QuotaLimit {
        kind: QuotaKind::CostMicrousd,
        limit: 1,
        window_seconds: 86_400,
        boundary: QuotaBoundary::Rolling,
    }];
    let spend_gateway = spawn_gateway(config).await;
    let response = client
        .post(format!("{spend_gateway}/v1/chat/completions"))
        .json(&json!({"model": "auto-frontier", "messages": []}))
        .send()
        .await
        .expect("spend error");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("spend error JSON");
    assert_eq!(body["error"]["code"], "frontier_spend_cap_reached");
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
