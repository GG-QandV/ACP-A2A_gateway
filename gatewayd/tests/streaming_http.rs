//! gatewayd/tests/streaming_http.rs
//!
//! T3 (Streaming roadmap Part 1): a real HTTP SSE client receives
//! several A2aEvents before the terminal one (final: true). Uses the same
//! in-process harness as rest_transport.rs: Registry -> real
//! mock_acp_agent stdio process (with MOCK_AGENT_STREAM_CHUNKS), the router is
//! from the gatewayd lib part, the request goes through tower::ServiceExt::oneshot.
//!
//! The mock agent sends 3 agent_message_chunk chunks with a delay, then
//! a final PromptResponse. Through convert.rs (direction 4) each
//! chunk becomes A2aEvent::TaskStatusUpdate(final:false), the terminal one —
//! TaskStatusUpdate(final:true). The SSE client should see several
//! events before the final marker.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::transport_http::router;
use serde_json::{json, Value};
use tower::ServiceExt;

fn mock_bin() -> String {
    std::env::var("CARGO_BIN_EXE_mock_acp_agent").expect(
        "CARGO_BIN_EXE_mock_acp_agent должен быть задан: cargo test собирает [[bin]] пакета",
    )
}

fn build_router(agent_id: &str, env: HashMap<String, String>) -> axum::Router {
    let tokens: HashSet<String> = ["test-token".to_string()].into_iter().collect();
    let mut agents = HashMap::new();
    agents.insert(
        agent_id.to_string(),
        AgentEntry::new(
            Transport::Stdio {
                command: vec![mock_bin()],
                cwd: None,
                env,
            },
            4,
            Duration::from_secs(15),
            Duration::from_secs(120),
        ),
    );
    let registry = std::sync::Arc::new(Registry::new(tokens, agents));

    let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));

    router(
        registry,
        dir.path().to_path_buf(),
        Duration::from_secs(5),
        Duration::from_secs(10),
        "http://localhost:8348".to_string(),
        None,
    )
}

async fn request_body(app: &axum::Router, body: Value) -> Response {
    request_body_for(app, "hermes-stream", body).await
}

async fn request_body_for(app: &axum::Router, agent_id: &str, body: Value) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/agents/{agent_id}/rpc"))
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .expect("request completes")
}

/// T3: a real SSE client receives several events before final: true.
#[tokio::test]
async fn sse_client_receives_multiple_events_before_final() {
    let mut env = HashMap::new();
    env.insert("MOCK_AGENT_STREAM_CHUNKS".to_string(), "3".to_string());
    env.insert("MOCK_AGENT_CHUNK_DELAY_MS".to_string(), "30".to_string());
    let app = build_router("hermes-stream", env);

    let resp = request_body(
        &app,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": { "role": "user", "parts": [{ "kind": "text", "text": "hello" }] } }
        }),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "streaming message/send должен быть 200"
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.contains("text/event-stream") {
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("body");
        panic!(
            "должен быть SSE, got content-type: {content_type}, body: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("SSE тело читается");

    let text = String::from_utf8_lossy(&body).to_string();
    // Each event is "data: {json}\n\n". Count the A2aEvent events.
    let events: Vec<&str> = text
        .split("data: ")
        .filter(|s| s.starts_with('{'))
        .collect();
    assert!(
        events.len() >= 4,
        "должно быть >=4 событий (3 чанка + terminal), got {}: {text}",
        events.len()
    );

    let mut non_final = 0usize;
    for raw in &events {
        let json_part = raw.split("\n\n").next().unwrap_or("");
        if let Ok(v) = serde_json::from_str::<Value>(json_part) {
            // The SSE frame contains an A2aEvent TaskStatusUpdate
            if v.get("status").is_some() {
                let is_final = v.get("final").and_then(Value::as_bool).unwrap_or(false);
                if !is_final {
                    non_final += 1;
                }
            }
        }
    }
    assert!(
        non_final >= 3,
        "должно быть минимум 3 не-терминальных события (чанка), got {non_final}"
    );
    let has_terminal = text.contains("\"final\":true");
    assert!(has_terminal, "должен быть терминальный final:true: {text}");
}

/// P-24: a nonexistent agent_id returns 404 (UnknownAgent from lookup),
/// not 503 (StreamCapacityExhausted from try_acquire_stream) — the
/// "lookup before permit" order must not regress: if someone in the future
/// swaps the order, this test catches the regression.
#[tokio::test]
async fn unknown_agent_returns_404_not_503() {
    let app = build_router("hermes-stream", HashMap::new());

    let resp = request_body_for(
        &app,
        "no-such-agent",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": { "role": "user", "parts": [{ "kind": "text", "text": "hello" }] } }
        }),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "несуществующий агент должен дать 404, а не 503 (лимит стримов не должен срабатывать до lookup)"
    );
}
