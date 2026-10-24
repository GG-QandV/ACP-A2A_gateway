//! gatewayd/tests/detached_send.rs
//!
//! Non-blocking message/send (`configuration.blocking: false`): the gateway
//! answers at once with the task in state `working`, the turn runs in the
//! background, tasks/get later serves the final task with the agent's text.
//! Same in-process harness as streaming_http.rs: Registry -> real
//! mock_acp_agent stdio process, router from the gatewayd lib part.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::transport_http::router;
use serde_json::{json, Value};
use tower::ServiceExt;

const AGENT: &str = "slow-agent";

fn build_router(env: HashMap<String, String>) -> axum::Router {
    let tokens: HashSet<String> = ["test-token".to_string()].into_iter().collect();
    let mut agents = HashMap::new();
    agents.insert(
        AGENT.to_string(),
        AgentEntry::new(
            Transport::Stdio {
                command: vec![std::env::var("CARGO_BIN_EXE_mock_acp_agent")
                    .expect("cargo test builds the mock_acp_agent bin")],
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

async fn rpc(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/agents/{AGENT}/rpc"))
                .header("content-type", "application/json")
                .header("authorization", "Bearer test-token")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .expect("request completes");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "JSON body expected, got: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, value)
}

fn send(blocking: bool) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "message/send",
        "params": {
            "message": { "role": "user", "parts": [{ "kind": "text", "text": "hello" }] },
            "configuration": { "blocking": blocking }
        }
    })
}

/// The agent is silent for 2 x 600 ms before its reply; the non-blocking send
/// must return long before that, and tasks/get must end with the full text.
#[tokio::test]
async fn non_blocking_send_returns_at_once_and_tasks_get_serves_the_result() {
    let mut env = HashMap::new();
    env.insert("MOCK_AGENT_STREAM_CHUNKS".to_string(), "2".to_string());
    env.insert("MOCK_AGENT_CHUNK_DELAY_MS".to_string(), "600".to_string());
    let app = build_router(env);

    let started = Instant::now();
    let (status, body) = rpc(&app, send(false)).await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        elapsed < Duration::from_millis(500),
        "non-blocking send must not wait for the agent, took {elapsed:?}"
    );
    let result = &body["result"];
    assert_eq!(result["status"]["state"], "working", "body: {body}");
    let task_id = result["id"].as_str().expect("task id").to_string();

    let deadline = Instant::now() + Duration::from_secs(20);
    let final_task = loop {
        let (_, got) = rpc(
            &app,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tasks/get", "params": { "id": task_id } }),
        )
        .await;
        let state = got["result"]["status"]["state"].as_str().unwrap_or("");
        if state != "working" {
            break got;
        }
        assert!(Instant::now() < deadline, "task still working: {got}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let result = &final_task["result"];
    assert_eq!(result["status"]["state"], "completed", "task: {final_task}");
    let text = result["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("");
    for expected in ["chunk-0", "chunk-1", "pong"] {
        assert!(
            text.contains(expected),
            "final text must contain {expected}: {final_task}"
        );
    }
}

/// `blocking: true` (and an absent flag) keep the old behavior: the response
/// carries the agent's reply, not a `working` task.
#[tokio::test]
async fn blocking_send_keeps_the_old_behavior() {
    let app = build_router(HashMap::new());
    let (status, body) = rpc(&app, send(true)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_ne!(
        body["result"]["status"]["state"], "working",
        "blocking send must wait for the turn: {body}"
    );
}
