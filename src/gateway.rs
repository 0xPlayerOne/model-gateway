use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::config::{Config, ProviderConfig, TargetConfig};
use crate::secrets::{SecretError, SecretResolver};

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Debug, Error)]
pub enum GatewayBuildError {
    #[error("configuration error: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("provider '{provider}' client could not be built: {message}")]
    Client { provider: String, message: String },
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    providers: Arc<BTreeMap<String, ProviderRuntime>>,
    global_permits: Arc<Semaphore>,
}

struct ProviderRuntime {
    config: ProviderConfig,
    api_key: Option<String>,
    client: Client,
    permits: Arc<Semaphore>,
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
            .user_agent("model-gateway/0.1")
            .build()
            .map_err(|error| GatewayBuildError::Client {
                provider: name.clone(),
                message: error.to_string(),
            })?;
        let api_key = match provider.api_key_secret.as_deref() {
            Some(name) => secrets.get(name)?,
            None => None,
        };
        providers.insert(
            name.clone(),
            ProviderRuntime {
                config: provider.clone(),
                api_key,
                client,
                permits: Arc::new(Semaphore::new(
                    provider
                        .max_in_flight
                        .unwrap_or(config.server.max_in_flight),
                )),
            },
        );
    }
    let state = AppState {
        global_permits: Arc::new(Semaphore::new(config.server.max_in_flight)),
        config: Arc::new(config),
        providers: Arc::new(providers),
    };
    Ok(Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .layer(DefaultBodyLimit::max(state.config.server.max_body_bytes))
        .with_state(state))
}

pub async fn run_server(
    config: Config,
    secrets: &SecretResolver,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind: std::net::SocketAddr = config.server.bind.parse()?;
    let shutdown_grace = Duration::from_secs(config.server.shutdown_grace_seconds);
    let app = build_app(config, secrets)?;
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
    let _ = state;
    Json(json!({"status": "ready"}))
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let data = state
        .config
        .models
        .keys()
        .map(|id| json!({"id": id, "object": "model", "owned_by": "model-gateway"}))
        .collect::<Vec<_>>();
    Json(json!({"object": "list", "data": data}))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let request_id = request_id(&headers);
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                request_id,
                "request body exceeded the configured limit",
                "invalid_request_error",
                Some("body_too_large"),
            );
        }
        Err(_) => {
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
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "request body must be an object",
                "invalid_request_error",
                Some("invalid_request"),
            );
        }
        Err(_) => {
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
            return error_response(
                StatusCode::BAD_REQUEST,
                request_id,
                "field 'stream' must be a boolean",
                "invalid_request_error",
                Some("stream"),
            );
        }
    };
    let targets = match resolve_targets(&state, &model) {
        Ok(targets) => targets,
        Err((status, message, code)) => {
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
            return admission_error(
                request_id,
                "gateway is at capacity",
                state.config.server.admission_timeout_ms,
            );
        }
    };
    let mut attempts = 0usize;
    let mut last_error = None;
    for target in targets {
        attempts += 1;
        let mut target_request = request.clone();
        target_request["model"] = Value::String(target.model.clone());
        let Some(provider) = state.providers.get(&target.provider) else {
            last_error = Some((
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                Bytes::new(),
                target.provider.clone(),
            ));
            continue;
        };
        let provider_permit = match timeout(
            Duration::from_millis(state.config.server.admission_timeout_ms),
            provider.permits.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
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
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    request_id,
                    "upstream request failed",
                    "upstream_error",
                    None,
                );
            }
            Err(_) => {
                drop(provider_permit);
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    request_id,
                    "upstream response headers timed out",
                    "upstream_error",
                    None,
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
                    attempts,
                    idle_timeout_seconds: provider.config.stream_idle_timeout_seconds,
                    is_stream,
                    global_permit,
                    provider_permit,
                },
            );
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
        if !is_fallback_status(status) {
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
        last_error = Some((
            status,
            response_headers,
            response_body,
            target.provider.clone(),
        ));
    }
    match last_error {
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
    }
}

#[allow(clippy::single_match)]
fn resolve_targets(
    state: &AppState,
    model: &str,
) -> Result<Vec<TargetConfig>, (StatusCode, String, &'static str)> {
    if let Some(config) = state.config.models.get(model) {
        return Ok(config.targets.clone());
    }
    match model.split_once('/') {
        Some((provider_name, upstream_model)) => match state.config.providers.get(provider_name) {
            Some(provider) if provider.allow_model_passthrough => {
                return Ok(vec![TargetConfig {
                    provider: provider_name.to_owned(),
                    model: upstream_model.to_owned(),
                }]);
            }
            _ => {}
        },
        None => {}
    }
    Err((
        StatusCode::NOT_FOUND,
        format!("model '{model}' is not configured"),
        "model_not_found",
    ))
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
    attempts: usize,
    idle_timeout_seconds: u64,
    is_stream: bool,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    provider_permit: tokio::sync::OwnedSemaphorePermit,
}

fn relay_response(
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
        attempts,
        is_stream,
        global_permit,
        provider_permit,
        ..
    } = context;
    let mut upstream = response.bytes_stream();
    let stream = async_stream::stream! {
        loop {
            match timeout(idle_timeout, upstream.next()).await {
                Ok(Some(Ok(chunk))) => yield Ok::<Bytes, std::io::Error>(chunk),
                Ok(Some(Err(error))) => {
                    yield Err(std::io::Error::other(error));
                    break;
                }
                Ok(None) => break,
                Err(_) => {
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
    if is_stream {
        response
            .headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    response
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
        "content-length",
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

#[cfg(test)]
mod tests {
    use super::{is_fallback_status, request_id};
    use axum::http::{HeaderMap, StatusCode};

    #[test]
    fn fallback_statuses_are_explicit() {
        assert!(is_fallback_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_fallback_status(StatusCode::BAD_GATEWAY));
        assert!(!is_fallback_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn request_id_is_generated_or_preserved() {
        let empty = HeaderMap::new();
        assert!(request_id(&empty).starts_with("mg-"));
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "client-request".parse().expect("header"));
        assert_eq!(request_id(&headers), "client-request");
    }
}
