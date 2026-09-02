//! protocol/src/a2a_sdk_compat.rs
//!
//! Compatibility with the JSON-RPC layer of the official a2a-rs SDK. Two directions:
//! 1. Input: params.message in SDK shape (role: ROLE_USER, part: {text}) must be
//!    converted to protocol::a2a::Message (role: user, part: {kind:text,text})
//!    before passing it to build_task_from_send_params — otherwise
//!    serde_json::from_value::<Message> would fail on an unknown role variant.
//! 2. Output: a finished protocol::a2a::Task must be rendered into SDK shape
//!    ({task:{...}}, TASK_STATE_*, ROLE_*, {text} without kind) to answer
//!    SendMessage/GetTask/CancelTask.
//!
//! A separate module rather than an edit of a2a.rs: keeps the current
//! Task/Message serialization unchanged for semantic clients (regression risk
//! of editing a2a.rs directly — otherwise message/send starts replying differently).

use crate::a2a::{Artifact, FilePart, Message, MessageRole, Part, Task, TaskState};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum SdkCompatError {
    #[error("sdk message: 'role' field missing or unrecognized")]
    UnknownRole,
    #[error("sdk message: part has neither 'text' nor 'url'/'raw' nor SDK-shaped kind")]
    UnknownPart,
}

/// Tries to recognize the SDK role shape (`"ROLE_USER"`/`"ROLE_AGENT"`); on
/// failure falls back to the current semantic shape (`"user"`/`"agent"`),
/// which serde itself can parse into MessageRole. Returns the already
/// normalized role field value in the format expected by
/// protocol::a2a::MessageRole (lowercase), without touching the original Value.
fn normalize_role(raw: &Value) -> Result<MessageRole, SdkCompatError> {
    match raw.as_str() {
        Some("ROLE_USER") | Some("user") => Ok(MessageRole::User),
        Some("ROLE_AGENT") | Some("agent") => Ok(MessageRole::Agent),
        _ => Err(SdkCompatError::UnknownRole),
    }
}

/// Tries the SDK part shape (`{"text": "..."}` without a kind field) and falls
/// back to the current semantic shape (`{"kind":"text","text":"..."}`), which
/// serde itself parses via protocol::a2a::Part. The SDK file-part
/// (`{"url":...}` / `{"raw": base64}`) is mapped onto Part::File separately.
fn normalize_part(raw: &Value) -> Result<Part, SdkCompatError> {
    // Explicit SDK tag is absent — the a2a-rs SDK protojson does not emit "kind".
    // If "kind" is present — this is already the semantic shape, parse it as-is.
    if raw.get("kind").is_some() {
        return serde_json::from_value(raw.clone()).map_err(|_| SdkCompatError::UnknownPart);
    }

    if let Some(text) = raw.get("text").and_then(Value::as_str) {
        return Ok(Part::Text {
            text: text.to_string(),
        });
    }

    if raw.get("url").is_some() || raw.get("raw").is_some() {
        let uri = raw.get("url").and_then(Value::as_str).map(str::to_string);
        let bytes = raw.get("raw").and_then(Value::as_str).map(str::to_string);
        let mime_type = raw
            .get("media_type")
            .or_else(|| raw.get("mediaType"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Ok(Part::File {
            file: FilePart {
                uri,
                bytes,
                mime_type,
            },
        });
    }

    Err(SdkCompatError::UnknownPart)
}

/// Normalizes an arbitrary incoming `message` (SDK or semantic) into
/// `protocol::a2a::Message`. Called BEFORE the existing
/// `serde_json::from_value::<Message>` in build_task_from_send_params —
/// replaces it rather than supplements it, to avoid parsing twice.
pub fn normalize_message(raw: &Value) -> Result<Message, SdkCompatError> {
    let role_raw = raw.get("role").ok_or(SdkCompatError::UnknownRole)?;
    let role = normalize_role(role_raw)?;

    let parts_raw = raw
        .get("parts")
        .and_then(Value::as_array)
        .ok_or(SdkCompatError::UnknownPart)?;

    let parts: Result<Vec<Part>, SdkCompatError> = parts_raw.iter().map(normalize_part).collect();
    let parts = parts?;

    let message_id = raw
        .get("messageId")
        .or_else(|| raw.get("message_id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(Message {
        role,
        parts,
        message_id,
    })
}

fn task_state_to_sdk(state: TaskState) -> &'static str {
    match state {
        TaskState::Unspecified => "TASK_STATE_UNSPECIFIED",
        TaskState::Submitted => "TASK_STATE_SUBMITTED",
        TaskState::Working => "TASK_STATE_WORKING",
        TaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
        TaskState::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
        TaskState::Completed => "TASK_STATE_COMPLETED",
        TaskState::Failed => "TASK_STATE_FAILED",
        TaskState::Canceled => "TASK_STATE_CANCELLED", // SDK proto: "CANCELLED" (2 L), verified against a2a-rs types.rs in TZ
        TaskState::Rejected => "TASK_STATE_REJECTED",
    }
}

fn role_to_sdk(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "ROLE_USER",
        MessageRole::Agent => "ROLE_AGENT",
    }
}

fn part_to_sdk(part: &Part) -> Value {
    match part {
        Part::Text { text } => json!({ "text": text }),
        Part::File { file } => {
            let mut obj = serde_json::Map::new();
            if let Some(uri) = &file.uri {
                obj.insert("url".to_string(), json!(uri));
            }
            if let Some(bytes) = &file.bytes {
                obj.insert("raw".to_string(), json!(bytes));
            }
            if let Some(mime_type) = &file.mime_type {
                obj.insert("media_type".to_string(), json!(mime_type));
            }
            Value::Object(obj)
        }
        Part::Data { data } => json!({ "data": data }),
    }
}

fn message_to_sdk(message: &Message) -> Value {
    let parts: Vec<Value> = message.parts.iter().map(part_to_sdk).collect();
    let mut obj = serde_json::Map::new();
    obj.insert("role".to_string(), json!(role_to_sdk(message.role)));
    obj.insert("parts".to_string(), json!(parts));
    if let Some(mid) = &message.message_id {
        obj.insert("messageId".to_string(), json!(mid));
    }
    Value::Object(obj)
}

fn artifact_to_sdk(artifact: &Artifact) -> Value {
    let parts: Vec<Value> = artifact.parts.iter().map(part_to_sdk).collect();
    json!({
        "artifact_id": artifact.artifact_id,
        "name": artifact.name,
        "description": artifact.description,
        "parts": parts,
        "metadata": artifact.metadata,
    })
}

/// Renders Task into SDK shape: `{"task": {...}}` wrapper, camelCase
/// contextId/messageId, TASK_STATE_*, ROLE_*, parts without "kind".
/// Mandatory requirement of the a2a-rs SDK client (it expects result.task).
pub fn render_task_sdk(task: &Task) -> Value {
    let status = json!({
        "state": task_state_to_sdk(task.status.state),
        "message": task.status.message.as_ref().map(message_to_sdk),
        "timestamp": task.status.timestamp,
    });

    let artifacts: Option<Vec<Value>> = task
        .artifacts
        .as_ref()
        .map(|arts| arts.iter().map(artifact_to_sdk).collect());

    json!({
        "task": {
            "id": task.id.0,
            "contextId": task.context_id.0,
            "status": status,
            "artifacts": artifacts,
            "metadata": task.metadata,
        }
    })
}

#[cfg(test)]
mod compat_tests {
    use super::*;
    use crate::a2a::{ContextId, TaskId, TaskStatus};

    #[test]
    fn normalize_message_accepts_sdk_shape() {
        let raw = json!({
            "role": "ROLE_USER",
            "parts": [{ "text": "ping" }]
        });
        let msg = normalize_message(&raw).expect("must normalize sdk shape");
        assert!(matches!(msg.role, MessageRole::User));
        assert!(matches!(&msg.parts[0], Part::Text { text } if text == "ping"));
    }

    #[test]
    fn normalize_message_accepts_semantic_shape() {
        let raw = json!({
            "role": "user",
            "parts": [{ "kind": "text", "text": "ping" }]
        });
        let msg = normalize_message(&raw).expect("must normalize semantic shape");
        assert!(matches!(msg.role, MessageRole::User));
        assert!(matches!(&msg.parts[0], Part::Text { text } if text == "ping"));
    }

    #[test]
    fn normalize_message_rejects_unknown_role() {
        let raw = json!({ "role": "typo", "parts": [] });
        assert!(matches!(
            normalize_message(&raw),
            Err(SdkCompatError::UnknownRole)
        ));
    }

    #[test]
    fn render_task_sdk_uses_task_state_upper_snake_with_double_l_cancelled() {
        let task = Task {
            id: TaskId("task-1".into()),
            context_id: ContextId("ctx-1".into()),
            status: TaskStatus {
                state: TaskState::Canceled,
                message: None,
                timestamp: None,
            },
            history: None,
            artifacts: None,
            metadata: None,
        };
        let rendered = render_task_sdk(&task);
        assert_eq!(rendered["task"]["status"]["state"], "TASK_STATE_CANCELLED");
        assert_eq!(rendered["task"]["contextId"], "ctx-1");
        // The {task:...} wrapper is mandatory for the SDK client.
        assert!(rendered.get("task").is_some());
    }

    #[test]
    fn render_task_sdk_completed_roundtrips_artifact_text() {
        let task = Task {
            id: TaskId("task-2".into()),
            context_id: ContextId("ctx-2".into()),
            status: TaskStatus {
                state: TaskState::Completed,
                message: None,
                timestamp: None,
            },
            history: None,
            artifacts: Some(vec![Artifact {
                artifact_id: "art-1".into(),
                name: Some("response".into()),
                description: None,
                parts: vec![Part::Text {
                    text: "pong".into(),
                }],
                metadata: None,
            }]),
            metadata: None,
        };
        let rendered = render_task_sdk(&task);
        assert_eq!(rendered["task"]["status"]["state"], "TASK_STATE_COMPLETED");
        assert_eq!(rendered["task"]["artifacts"][0]["parts"][0]["text"], "pong");
        // The SDK part must not contain "kind".
        assert!(rendered["task"]["artifacts"][0]["parts"][0]
            .get("kind")
            .is_none());
    }
}
