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
        client: reqwest::Client::new(),
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

    let target_url = format!("{}/{}", url.trim_end_matches('/'), path);
    let method = request.method().clone();

    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("body read error: {e}")).into_response(),
    };

    let mut upstream_req = state.client.request(method, &target_url).body(body_bytes);

    if let Some(ct) = headers.get("content-type") {
        upstream_req = upstream_req.header("content-type", ct);
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
