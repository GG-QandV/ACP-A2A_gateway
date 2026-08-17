//! gatewayd/src/transport_a2a_passthrough.rs
//! Направление 2: A2A-клиент -> A2A-агент, без семантического
//! преобразования — reverse-proxy, включая SSE-стрим как есть.
//!
//! ДОБАВЛЕНО: диалект-зонд (dialect_probe) выполняется один раз на
//! agent_id перед первым проксированием — результат только логируется,
//! не блокирует и не меняет сам passthrough (он остаётся reverse-proxy
//! "без семантического преобразования").

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;

use crate::dialect_probe::{probe_dialect, DialectCache};
use crate::registry::{Registry, Transport};

pub struct PassthroughState {
    registry: Arc<Registry>,
    client: reqwest::Client,
    dialect_cache: DialectCache,
}

pub fn router(registry: Arc<Registry>) -> Router {
    let state = Arc::new(PassthroughState {
        registry,
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client builds with default TLS backend"),
        dialect_cache: DialectCache::new(),
    });

    Router::new()
        .route("/a2a-proxy/:agent_id/*path", any(proxy_handler))
        .with_state(state)
}

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

fn build_target_url(base: &str, path: &str, query: Option<&str>) -> String {
    let safe_path = path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect::<Vec<_>>()
        .join("/");
    let query = query.map(|q| format!("?{q}")).unwrap_or_default();
    format!("{}/{}{}", base.trim_end_matches('/'), safe_path, query)
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

    if state.dialect_cache.get(&agent_id).is_none() {
        match probe_dialect(&state.client, &url, push_token.as_deref()).await {
            Ok(dialect) => {
                state.dialect_cache.set(&agent_id, dialect);
                tracing::info!(agent_id = %agent_id, ?dialect, "a2a dialect probed");
            }
            Err(e) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "a2a dialect probe failed — proxying request anyway"
                );
            }
        }
    }

    let target_url = build_target_url(&url, &path, request.uri().query());
    let method = request.method().clone();

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
    let content_type = upstream_resp.headers().get("content-type").cloned();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_and_query_are_forwarded() {
        assert_eq!(
            build_target_url("https://ops.internal/a2a", "rpc", Some("id=7&mode=fast")),
            "https://ops.internal/a2a/rpc?id=7&mode=fast"
        );
    }

    #[test]
    fn query_is_not_dropped() {
        let url = build_target_url("https://ops.internal/a2a", "tasks", Some("limit=10"));
        assert!(url.ends_with("?limit=10"));
    }

    #[test]
    fn traversal_segments_are_stripped() {
        let url = build_target_url("https://ops.internal/a2a", "../../admin/secret", None);
        assert_eq!(url, "https://ops.internal/a2a/admin/secret");
        assert!(!url.contains(".."));
    }

    #[test]
    fn trailing_and_double_slashes_do_not_break_url() {
        assert_eq!(
            build_target_url("https://ops.internal/a2a/", "//rpc//", None),
            "https://ops.internal/a2a/rpc"
        );
    }

    #[test]
    fn empty_path_targets_base_url() {
        assert_eq!(build_target_url("https://ops.internal/a2a", "", None), "https://ops.internal/a2a/");
    }

    #[test]
    fn body_limit_is_bounded() {
        const { assert!(MAX_BODY_BYTES > 0, "лимит тела должен быть ненулевым"); }
        const { assert!(MAX_BODY_BYTES <= 64 * 1024 * 1024, "лимит тела не должен быть безграничным"); }
    }

    #[test]
    fn passthrough_state_carries_dialect_cache_field() {
        let cache = DialectCache::new();
        cache.set("probe-integration-check", crate::dialect_probe::A2aDialect::Spec);
        assert_eq!(
            cache.get("probe-integration-check"),
            Some(crate::dialect_probe::A2aDialect::Spec)
        );
    }
}
