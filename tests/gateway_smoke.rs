use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Router, serve};
use futures_util::stream;
use model_gateway::config::{Config, ModelConfig, ProviderConfig, ServerConfig, TargetConfig};
use model_gateway::gateway::build_app;
use model_gateway::secrets::SecretResolver;
use serde_json::{Value, json};
use tokio::net::TcpListener;

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
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: one\n\n"));
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: two\n\n"));
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
    let body: Value = response.json().await.expect("json response");
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
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
    assert_eq!(models["data"][0]["id"], "smoke");
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
