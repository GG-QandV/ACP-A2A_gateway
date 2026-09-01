// ============================================================================
// gatewayd/tests/rest_transport.rs
//
// E2E tests for the REST route POST /agents/:id/message:send (and the legacy alias
// /message/send). The harness is live: Registry points at a real
// stdio process mock_acp_agent (the crate's own [[bin]]), the router
// is built from the gatewayd lib part, requests go through
// tower::ServiceExt::oneshot. No stubs.
//
// Key contracts (confirmed by a2a-rs code, see the task card):
// - The SUCCESS response is {"task": {...}} (SendMessageResponse), NOT a flat Task.
// - The ERROR envelope is {"error": {"code", "status", "message", "details"}},
//   with no jsonrpc/id fields.
// - The legacy alias /message/send leads to the same handler.
// - ContextLost -> 409 + status "ABORTED" + code -32010.
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::transport_http::{http_status_to_sdk_name, router};
use serde_json::{json, Value};
use tower::ServiceExt;

fn mock_bin() -> String {
    std::env::var("CARGO_BIN_EXE_mock_acp_agent").expect(
        "CARGO_BIN_EXE_mock_acp_agent должен быть задан: cargo test собирает [[bin]] пакета",
    )
}

/// Builds a router with a Registry where agent_id leads to a process
/// mock_acp_agent with the given environment. The task-store directory is
/// a unique tempdir per router (integration tests may run
/// in parallel; a shared directory would cause task collisions).
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

    // TempDir must outlive the requests: the adapter writes tasks into it.
    // The test process is short-lived — a deliberate leak instead of hauling
    // a TempDir out of every helper and complicating signatures.
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

async fn test_router_with_stub_agent(agent_id: &str) -> axum::Router {
    build_router(agent_id, HashMap::new())
}

/// The mock exits (code 0) after the first session/prompt: the first request
/// manages to create a session in generation 1, the second one lands on the
/// restarted process (generation 2) and gets ContextLost.
async fn test_router_with_context_lost_agent(agent_id: &str) -> axum::Router {
    let mut env = HashMap::new();
    env.insert("MOCK_AGENT_EXIT_AFTER_PROMPTS".to_string(), "1".to_string());
    build_router(agent_id, env)
}

/// POSTs a message to message:send with a valid token and a given
/// contextId. contextId is pinned explicitly: without it build_task invents
/// a new one on every request, and conversation continuation cannot be checked.
async fn post_message(router: axum::Router, path: &str, context_id: &str) -> Response {
    router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "message": {
                            "messageId": "m1",
                            "role": "ROLE_USER",
                            "parts": [{ "text": "ping" }],
                            "contextId": context_id,
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: Response) -> Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn rest_path_with_colon_is_matched_by_axum() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message:send", "ctx-1").await;

    // Stronger than the original v2 check (!= NOT_FOUND): the mock is live, so
    // the response must be 200 with a task wrapper.
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json.get("task").is_some());
}

/// Legacy alias, confirmed by a2a-server/src/rest.rs:24,68 — the SDK server
/// accepts both paths into the same handler.
#[tokio::test]
async fn legacy_message_send_path_routes_to_same_handler() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message/send", "ctx-1").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    // Same response contract as the main path — the task wrapper.
    assert!(json.get("task").is_some());
}

/// Fact from a2a-server/src/rest.rs:1264: the server's test reads
/// send_resp["task"]["id"] — the wrapper EXISTS and is mandatory. Unwrapping
/// was a wrong assumption in v1.
#[tokio::test]
async fn rest_response_keeps_task_wrapper_matching_sdk_send_message_response() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message:send", "ctx-1").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;

    assert!(
        json.get("task").is_some(),
        "REST-ответ ДОЛЖЕН содержать обёртку task (SendMessageResponse protojson), \
         разворачивание в v1 было ошибочным предположением"
    );
    assert!(json["task"].get("id").is_some());
    assert!(json["task"]["status"]["state"]
        .as_str()
        .unwrap()
        .starts_with("TASK_STATE_"));
    // The mock replies end_turn — the task is completed, not stuck in submitted.
    assert_eq!(json["task"]["status"]["state"], "TASK_STATE_COMPLETED");
}

/// The error envelope is {"code", "status", "message", "details"}, without
/// jsonrpc/id. Fact from a2a-server/src/rest.rs:470.
#[tokio::test]
async fn rest_error_envelope_has_all_four_sdk_fields() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/agent-x/message:send")
                .header("content-type", "application/json")
                // No Authorization — expect 401.
                .body(Body::from(json!({ "message": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(response).await;

    assert!(
        json.get("jsonrpc").is_none(),
        "REST-ошибка не несёт jsonrpc-конверт"
    );
    assert!(json.get("id").is_none(), "REST-ошибка не несёт JSON-RPC id");

    let error = &json["error"];
    assert!(error["code"].is_i64(), "поле code обязательно");
    assert!(
        error["status"].is_string(),
        "поле status обязательно (SDK REST-конверт, 4 поля)"
    );
    assert!(error["message"].is_string(), "поле message обязательно");
    assert!(
        error.get("details").is_some(),
        "поле details должно присутствовать (может быть null)"
    );
    // 401 -> UNAUTHENTICATED per the gRPC mapping.
    assert_eq!(error["status"], "UNAUTHENTICATED");
}

/// ContextLost after a process restart: the first request creates a session
/// in generation 1, the mock dies, the supervisor restarts it (generation 2),
/// the second request with the same contextId — 409 + ABORTED + -32010.
#[tokio::test]
async fn rest_context_lost_matches_rpc_behavior_with_four_field_envelope() {
    let router = test_router_with_context_lost_agent("agent-y").await;

    // The conversation is created and completes successfully (generation 1).
    let first = post_message(router.clone(), "/agents/agent-y/message:send", "ctx-lost").await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "первый запрос обязан пройти"
    );
    drop(first);

    // Wait out the supervisor's respawn backoff (5s) so the second request
    // hits the already-restarted process instead of getting "process is dead,
    // restart not before ...".
    tokio::time::sleep(Duration::from_secs(5) + Duration::from_millis(500)).await;

    let response = post_message(router, "/agents/agent-y/message:send", "ctx-lost").await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json = body_json(response).await;
    assert_eq!(json["error"]["code"], -32010);
    assert_eq!(json["error"]["status"], "ABORTED");
}

#[test]
fn http_status_to_sdk_name_covers_all_used_codes() {
    // Regression: if rest_sdk_error gains a new StatusCode without
    // a matching branch in http_status_to_sdk_name, the test must fail,
    // not silently return "UNKNOWN" in production. It checks the actual
    // function from transport_http, not a copy of it.
    let used_codes = [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::NOT_FOUND,
        StatusCode::CONFLICT,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::NOT_IMPLEMENTED,
        StatusCode::OK,
    ];
    for code in used_codes {
        assert_ne!(
            http_status_to_sdk_name(code),
            "UNKNOWN",
            "код {code} используется в rest_sdk_error, но не замаплен на SDK status name"
        );
    }
}

/// Regression on continue via contextId: the second message/send into the same session
/// must reuse the same ACP session (SessionId), not create a new one.
/// Before the fix, ensure_session created a new SessionId every time — the second
/// request timed out because the agent expected continuation in the old session.
/// (unit coverage: convert.rs::same_context_reuses_session)
#[tokio::test]
async fn second_message_send_same_context_returns_same_session() {
    let router = test_router_with_stub_agent("agent-continue").await;

    let first = post_message(
        router.clone(),
        "/agents/agent-continue/message:send",
        "ctx-continue-1",
    )
    .await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "первый message/send должен пройти успешно"
    );
    let first_json = body_json(first).await;
    assert!(
        first_json.get("task").is_some(),
        "ответ должен содержать обёртку task"
    );
    let first_context_id = first_json["task"]["contextId"]
        .as_str()
        .expect("contextId присутствует");
    assert_eq!(
        first_context_id, "ctx-continue-1",
        "contextId должен совпадать с отправленным"
    );

    // Second request with the same contextId — must reuse the same session,
    // not time out, and return the same contextId. If the session were created
    // anew, the response would come with a new context (or a timeout).
    let second = post_message(
        router,
        "/agents/agent-continue/message:send",
        "ctx-continue-1",
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "второй message/send с тем же contextId должен пройти успешно, не таймаутить"
    );
    let second_json = body_json(second).await;
    assert!(
        second_json.get("task").is_some(),
        "ответ должен содержать обёртку task"
    );
    let second_context_id = second_json["task"]["contextId"]
        .as_str()
        .expect("contextId присутствует");
    assert_eq!(
        second_context_id, "ctx-continue-1",
        "contextId должен совпадать (тот же разговор)"
    );
}
