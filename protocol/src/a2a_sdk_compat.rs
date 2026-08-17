//! protocol/src/a2a_sdk_compat.rs
//!
//! Совместимость с JSON-RPC слоем официального SDK a2a-rs. Два направления:
//! 1. Вход: params.message в SDK-форме (role: ROLE_USER, part: {text}) нужно
//!    привести к protocol::a2a::Message (role: user, part: {kind:text,text}),
//!    прежде чем отдать в build_task_from_send_params — иначе
//!    serde_json::from_value::<Message> упадёт на неизвестном варианте role.
//! 2. Выход: готовый protocol::a2a::Task нужно отрендерить в SDK-форму
//!    ({task:{...}}, TASK_STATE_*, ROLE_*, {text} без kind) для ответа на
//!    SendMessage/GetTask/CancelTask.
//!
//! Отдельный модуль, а не правка a2a.rs: сохраняет неизменной текущую
//! сериализацию Task/Message для семантических клиентов (регрессионный риск
//! правки прямо в a2a.rs — иначе message/send начинает отвечать иначе).

use crate::a2a::{
    Artifact, FilePart, Message, MessageRole, Part, Task, TaskState,
};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum SdkCompatError {
    #[error("sdk message: 'role' field missing or unrecognized")]
    UnknownRole,
    #[error("sdk message: part has neither 'text' nor 'url'/'raw' nor SDK-shaped kind")]
    UnknownPart,
}

/// Пробует распознать SDK-форму role (`"ROLE_USER"`/`"ROLE_AGENT"`), при
/// неудаче падает на текущую семантическую (`"user"`/`"agent"`), которую
/// умеет разобрать сам serde на MessageRole. Возвращает уже нормализованное
/// значение поля role в формате, ожидаемом protocol::a2a::MessageRole
/// (lowercase), не трогая исходный Value.
fn normalize_role(raw: &Value) -> Result<MessageRole, SdkCompatError> {
    match raw.as_str() {
        Some("ROLE_USER") | Some("user") => Ok(MessageRole::User),
        Some("ROLE_AGENT") | Some("agent") => Ok(MessageRole::Agent),
        _ => Err(SdkCompatError::UnknownRole),
    }
}

/// Пробует SDK-форму part (`{"text": "..."}` без поля kind) и падает на
/// текущую семантическую (`{"kind":"text","text":"..."}`), которую разберёт
/// сам serde через protocol::a2a::Part. SDK file-part (`{"url":...}` /
/// `{"raw": base64}`) отдельно маппится на Part::File.
fn normalize_part(raw: &Value) -> Result<Part, SdkCompatError> {
    // Явный SDK-тег отсутствует — a2a-rs SDK protojson не эмитит "kind".
    // Если "kind" присутствует — это уже семантическая форма, разбираем как есть.
    if raw.get("kind").is_some() {
        return serde_json::from_value(raw.clone()).map_err(|_| SdkCompatError::UnknownPart);
    }

    if let Some(text) = raw.get("text").and_then(Value::as_str) {
        return Ok(Part::Text { text: text.to_string() });
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
            file: FilePart { uri, bytes, mime_type },
        });
    }

    Err(SdkCompatError::UnknownPart)
}

/// Нормализует произвольный входной `message` (SDK или семантический) в
/// `protocol::a2a::Message`. Вызывается ДО существующего
/// `serde_json::from_value::<Message>` в build_task_from_send_params —
/// заменяет его, а не дополняет, чтобы не парсить дважды.
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

    Ok(Message { role, parts, message_id })
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
        TaskState::Canceled => "TASK_STATE_CANCELLED", // SDK proto: "CANCELLED" (2 L), сверено с a2a-rs types.rs в TZ
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

/// Рендерит Task в SDK-форму: обёртка `{"task": {...}}`, camelCase
/// contextId/messageId, TASK_STATE_*, ROLE_*, части без "kind".
/// Обязательное требование SDK-клиента a2a-rs (ждёт result.task).
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
        assert!(matches!(normalize_message(&raw), Err(SdkCompatError::UnknownRole)));
    }

    #[test]
    fn render_task_sdk_uses_task_state_upper_snake_with_double_l_cancelled() {
        let task = Task {
            id: TaskId("task-1".into()),
            context_id: ContextId("ctx-1".into()),
            status: TaskStatus { state: TaskState::Canceled, message: None, timestamp: None },
            history: None,
            artifacts: None,
            metadata: None,
        };
        let rendered = render_task_sdk(&task);
        assert_eq!(rendered["task"]["status"]["state"], "TASK_STATE_CANCELLED");
        assert_eq!(rendered["task"]["contextId"], "ctx-1");
        // Обёртка {task:...} обязательна для SDK-клиента.
        assert!(rendered.get("task").is_some());
    }

    #[test]
    fn render_task_sdk_completed_roundtrips_artifact_text() {
        let task = Task {
            id: TaskId("task-2".into()),
            context_id: ContextId("ctx-2".into()),
            status: TaskStatus { state: TaskState::Completed, message: None, timestamp: None },
            history: None,
            artifacts: Some(vec![Artifact {
                artifact_id: "art-1".into(),
                name: Some("response".into()),
                description: None,
                parts: vec![Part::Text { text: "pong".into() }],
                metadata: None,
            }]),
            metadata: None,
        };
        let rendered = render_task_sdk(&task);
        assert_eq!(rendered["task"]["status"]["state"], "TASK_STATE_COMPLETED");
        assert_eq!(rendered["task"]["artifacts"][0]["parts"][0]["text"], "pong");
        // SDK part не должен содержать "kind".
        assert!(rendered["task"]["artifacts"][0]["parts"][0].get("kind").is_none());
    }
}