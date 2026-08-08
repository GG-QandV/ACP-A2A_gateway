//! gatewayd/src/transport_a2a_passthrough.rs
//! Направление 2: A2A-клиент -> A2A-агент, без семантического
//! преобразования — reverse-proxy, включая SSE-стрим как есть.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;

use crate::registry::{Registry, Transport};

pub struct PassthroughState {
    registry: Arc<Registry>,
    client: reqwest::Client,
}

pub fn router(registry: Arc<Registry>) -> Router {
    let state = Arc::new(PassthroughState {
        registry,
        // ИСПРАВЛЕНО (аудит P1-7): без таймаута зависший upstream держал
        // клиентское соединение бесконечно.
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client builds with default TLS backend"),
    });

    Router::new()
        .route("/a2a-proxy/:agent_id/*path", any(proxy_handler))
        .with_state(state)
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string)
}

async fn proxy_handler(
    State(state): State<Arc<PassthroughState>>,
    AxumPath((agent_id, path)): AxumPath<(String, String)>,
    headers: HeaderMap,
    request: Request<Body>,
) -> impl IntoResponse {
    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "missing token").into_response(),
    };
    if !state.registry.check_token(&token) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }

    let entry = match state.registry.lookup(&agent_id) {
        Some(e) => e.clone(),
        None => return (StatusCode::NOT_FOUND, "unknown agent_id").into_response(),
    };

    let Transport::Http { url, push_token } = entry.transport else {
        return (
            StatusCode::BAD_REQUEST,
            "agent_id is not an A2A/http agent (use TCP transport for ACP targets)",
        )
            .into_response();
    };

    // ИСПРАВЛЕНО (аудит P1-8, часть 1): path из wildcard подставлялся в
    // URL как есть — сегменты ".." выводили запрос за пределы адреса
    // агента. Теперь нормализуем.
    let safe_path = path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect::<Vec<_>>()
        .join("/");
    // ИСПРАВЛЕНО (аудит P1-8, часть 2): query-строка терялась целиком.
    let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let target_url = format!("{}/{}{}", url.trim_end_matches('/'), safe_path, query);
    let method = request.method().clone();

    // ИСПРАВЛЕНО (аудит P1-4): usize::MAX = OOM одним запросом.
    const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
    let body_bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, format!("body read error: {e}")).into_response()
        }
    };

    let mut upstream_req = state.client.request(method, &target_url).body(body_bytes);

    if let Some(ct) = headers.get("content-type") {
        upstream_req = upstream_req.header("content-type", ct);
    }
    // ИСПРАВЛЕНО (аудит P1-8): без Accept upstream не отдаёт
    // text/event-stream, и SSE-режим (ради которого здесь bytes_stream)
    // не работал вовсе.
    if let Some(accept) = headers.get("accept") {
        upstream_req = upstream_req.header("accept", accept);
    }
    if let Some(pt) = &push_token {
        upstream_req = upstream_req.bearer_auth(pt);
    }

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    };

    let status = upstream_resp.status();
    let content_type = upstream_resp
        .headers()
        .get("content-type")
        .cloned();

    let stream = upstream_resp.bytes_stream();
    let mut response = Response::builder().status(status.as_u16());
    if let Some(ct) = content_type {
        response = response.header("content-type", ct);
    }

    match response.body(Body::from_stream(stream)) {
        Ok(r) => r.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("response build error: {e}")).into_response(),
    }
}
