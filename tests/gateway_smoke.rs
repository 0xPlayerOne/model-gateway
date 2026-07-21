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
    Failure(StatusCode),
    Stream,
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
            Self::Failure(status) => (status, Json(json!({"error": {"message": "upstream failure"}}))).into_response(),
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
    assert_eq!(response.headers()["x-model-gateway-provider"], "local");
    let body: Value = response.json().await.expect("json response");
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn streams_sse_and_falls_back_before_output() {
    let failing = spawn_provider(ProviderResponse::Failure(StatusCode::BAD_GATEWAY)).await;
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
