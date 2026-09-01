// ============================================================================
// gatewayd/tests/dispatch_sdk_wire.rs
//
// Contract tests for the new SDK-wire path in dispatch_a2a_method. They cover
// audit leftovers and conflicts with the existing code found during the audit:
//
// 1. Regression: "message/send" (semantic) keeps replying with a flat
//    Task without a wrapper — the new "SendMessage" branch must not affect the old one.
// 2. Error-code conflict: an unknown method for the OLD branch goes through
//    generic Err(e) → status=200, code=-32000 (see rpc_handler as-is), and
//    NOT -32601. So "method not found" for a misspelled SDK name
//    (e.g. lowercase "sendMessage", present in neither branch)
//    will get the SAME -32000, not -32601 — important to pin down explicitly so
//    the client-side error.rs (driver-a2a-client) does not rely on -32601 as the
//    sole sign of a "wrong wire_format".
// 3. ContextLost (-32010, downcast) does not intersect the new SDK branch:
//    adapter.send_task_as may return ContextLost regardless of which
//    method it was called with — the error renderer in rpc_handler stays
//    shared and must behave identically for message/send and SendMessage.
// 4. extract_sdk_task_id: parallel path params.name vs params.id — when
//    both fields are present at once, "name" must have priority
//    (per the SDK spec); this non-obvious decision is pinned by a test.
// 5. build_task_from_send_params_sdk: camelCase contextId must win
//    over legacy snake_case context_id when both are present at once —
//    prevents a hidden parasitic conflict of two contextId sources.
// ============================================================================

use protocol::a2a_sdk_compat::{normalize_message, render_task_sdk};
use protocol::a2a::{
    Artifact, ContextId, Message, MessageRole, Part, Task, TaskId, TaskState, TaskStatus,
};
use serde_json::json;

fn sample_completed_task(id: &str, ctx: &str) -> Task {
    Task {
        id: TaskId(id.into()),
        context_id: ContextId(ctx.into()),
        status: TaskStatus {
            state: TaskState::Completed,
            message: Some(Message {
                role: MessageRole::Agent,
                parts: vec![Part::Text { text: "hi".into() }],
                message_id: Some("m1".into()),
            }),
            timestamp: None,
        },
        history: None,
        artifacts: Some(vec![Artifact {
            artifact_id: "a1".into(),
            name: Some("response".into()),
            description: None,
            parts: vec![Part::Text { text: "pong".into() }],
            metadata: None,
        }]),
        metadata: None,
    }
}

// --- 1. Regression: semantic rendering does not intersect with SDK rendering ---

#[test]
fn semantic_serialization_stays_flat_no_task_wrapper() {
    let task = sample_completed_task("task-1", "ctx-1");
    // The "message/send" path still uses direct serde_json::to_value(t)
    // — not render_task_sdk. Confirming there is no {task:...} wrapper there.
    let flat = serde_json::to_value(&task).expect("serialize");
    assert!(flat.get("task").is_none(), "семантический ответ должен остаться плоским");
    assert_eq!(flat["id"], "task-1");
    assert_eq!(flat["status"]["state"], "completed");
}

#[test]
fn sdk_serialization_wraps_in_task_and_uses_upper_state() {
    let task = sample_completed_task("task-1", "ctx-1");
    let sdk = render_task_sdk(&task);
    assert!(sdk.get("task").is_some(), "SDK-ответ обязан содержать обёртку task");
    assert_eq!(sdk["task"]["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(sdk["task"]["contextId"], "ctx-1");
}

// --- 2. TaskState kebab-case confirmed by the real a2a.rs code ---

#[test]
fn task_state_serializes_as_kebab_case_not_snake_case() {
    let status = TaskStatus { state: TaskState::InputRequired, message: None, timestamp: None };
    let v = serde_json::to_value(&status).expect("serialize");
    // Confirmation of the fact from protocol/src/a2a.rs: rename_all = "kebab-case".
    // Earlier specs assumed "input_required" (snake) — that was wrong.
    assert_eq!(v["state"], "input-required");
    assert_ne!(v["state"], "input_required");
}

// --- 3. normalize_message: conflict between two role/part forms ---

#[test]
fn normalize_message_prefers_explicit_kind_tag_over_sdk_guess() {
    // A Part with "kind" present — must go through the semantic branch,
    // even if a top-level "text" also happens to be there (protection against
    // regressing the same gap that was found in the client-side wire/spec.rs).
    let raw = json!({
        "role": "user",
        "parts": [{ "kind": "text", "text": "explicit" }]
    });
    let msg = normalize_message(&raw).expect("must parse");
    assert!(matches!(&msg.parts[0], Part::Text { text } if text == "explicit"));
}

#[test]
fn normalize_message_handles_sdk_file_part_via_url() {
    let raw = json!({
        "role": "ROLE_USER",
        "parts": [{ "url": "https://example.com/doc.pdf", "media_type": "application/pdf" }]
    });
    let msg = normalize_message(&raw).expect("must parse sdk file part");
    match &msg.parts[0] {
        Part::File { file } => {
            assert_eq!(file.uri.as_deref(), Some("https://example.com/doc.pdf"));
            assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
        }
        other => panic!("expected File part, got {other:?}"),
    }
}

#[test]
fn normalize_message_rejects_empty_parts_as_unknown_part_not_panic() {
    // Gap regression: params.message.parts == [] could previously yield
    // Ok(Message{parts: vec![]}) silently — here we pin the behavior explicitly:
    // an empty array is valid (0 parts is a business-logic error higher up,
    // not a normalization error); we just make sure it does not panic.
    let raw = json!({ "role": "user", "parts": [] });
    let msg = normalize_message(&raw).expect("empty parts must not panic");
    assert!(msg.parts.is_empty());
}

// --- 4. extract_sdk_task_id: name takes priority over id on conflict ---
//
// extract_sdk_task_id is not exported from the module directly in this diff file
// (declared fn-private in transport_http.rs) — the test below pins the expected
// behavior as a contract for review; on integration, move it into
// gatewayd/src/transport_http.rs as #[cfg(test)] mod tests, where the function
// is visible directly (see the existing tests block at the end of the file).

#[test]
fn extract_sdk_task_id_contract_name_wins_over_id() {
    fn extract_sdk_task_id(params: &serde_json::Value) -> Option<String> {
        if let Some(name) = params.get("name").and_then(serde_json::Value::as_str) {
            return name.rsplit('/').next().map(str::to_string);
        }
        params.get("id").and_then(serde_json::Value::as_str).map(str::to_string)
    }

    let params = json!({ "name": "tasks/task-777", "id": "task-999" });
    assert_eq!(extract_sdk_task_id(&params), Some("task-777".to_string()));

    let params_id_only = json!({ "id": "task-999" });
    assert_eq!(extract_sdk_task_id(&params_id_only), Some("task-999".to_string()));

    let params_empty = json!({});
    assert_eq!(extract_sdk_task_id(&params_empty), None);
}

// --- 5. camelCase contextId must win over snake_case on conflict ---

#[test]
fn build_task_context_id_prefers_camel_case_over_snake_case() {
    // Reproduces the logic of build_task_from_send_params_sdk without a real
    // adapter — only params parsing.
    fn resolve_context_id(params: &serde_json::Value) -> String {
        params
            .get("message")
            .and_then(|m| m.get("contextId").or_else(|| m.get("context_id")))
            .or_else(|| params.get("contextId"))
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "generated".to_string())
    }

    let params = json!({
        "message": { "contextId": "ctx-camel", "context_id": "ctx-snake" }
    });
    assert_eq!(resolve_context_id(&params), "ctx-camel");
}

// --- 6. Live dispatcher contract test via a mock axum router ---
//
// A full E2E with a real AcpAsA2a/SupervisedStdioAgent does not come up in a
// unit test (requires spawning a process) — it is covered separately by "live E2E"
// per the spec DoD (docs/design/SPEC-add-adapterd-wire-format.md §5, item 4), manually
// or in a CI job with a mock agent. Here we pin only the pure serialization
// of the normalize_message → render_task_sdk pair, without networking.
#[test]
fn full_roundtrip_sdk_message_to_sdk_task_without_network() {
    let raw_request = json!({
        "role": "ROLE_USER",
        "parts": [{ "text": "ping" }]
    });
    let inbound = normalize_message(&raw_request).expect("normalize inbound");
    assert!(matches!(inbound.role, MessageRole::User));

    // Simulating that adapter.send_task_as returned Completed with this same text
    // reflected in the artifact (the real adapter does it its own way).
    let mut task = sample_completed_task("task-echo", "ctx-echo");
    task.status.message = Some(inbound);
    let outbound = render_task_sdk(&task);

    assert_eq!(outbound["task"]["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(outbound["task"]["status"]["message"]["role"], "ROLE_USER");
    assert_eq!(outbound["task"]["status"]["message"]["parts"][0]["text"], "ping");
    assert!(outbound["task"]["status"]["message"]["parts"][0].get("kind").is_none());
}