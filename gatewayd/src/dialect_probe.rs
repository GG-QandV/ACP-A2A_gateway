//! gatewayd/src/dialect_probe.rs
//!
//! Диалект-зонд для Направления 2 (A2A-клиент -> A2A-агент, passthrough).
//! Реализует §3 тЗ (TZ-a2a-dialects-gateway-adapter.md): шлюз сам ходит к
//! сторонним A2A-агентам через transport_a2a_passthrough (Transport::Http{url,
//! push_token}) и должен знать, на каком диалекте (SDK/Spec) агент отвечает
//! ДО того, как проксировать первый реальный запрос клиента.
//!
//! Принцип (§3.1 тЗ): зонд идемпотентен — GetTask/tasks/get с несуществующим
//! task_id (случайный UUID), НЕ SendMessage/message/send (те создают задачу).
//! Результат кэшируется на agent_id — один зонд на первый контакт.
//!
//! Распознавание "метод не найден" — по коду -32601 (стандартный JSON-RPC
//! 2.0) ИЛИ по нормализованному тексту с несколькими известными
//! формулировками ("method not found", "method_not_found", "unknown
//! method") — НЕ по одной точной подстроке конкретного шлюза. Тот же
//! фикс, что применён в клиентском
//! driver-a2a-client/src/dialect_probe.rs (D1): стандартный сервер
//! отвечает -32601 "Method not found" без двоеточия, точная подстрока
//! "method_not_found:" это пропускает.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A2aDialect {
    Sdk,
    Spec,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("probe request failed: {0}")]
    Http(String),
    #[error(
        "agent responded with unrecognized dialect (neither SDK nor Spec JSON-RPC methods matched)"
    )]
    Unrecognized,
}

#[derive(Clone)]
pub struct DialectCache {
    cache: Arc<DashMap<String, A2aDialect>>,
}

impl DialectCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
        }
    }

    pub fn get(&self, agent_id: &str) -> Option<A2aDialect> {
        self.cache.get(agent_id).map(|entry| *entry.value())
    }

    pub fn set(&self, agent_id: &str, dialect: A2aDialect) {
        self.cache.insert(agent_id.to_string(), dialect);
    }

    /// Инвалидация кэшированного диалекта (D3): вызывается, когда реальный
    /// (не зондовый) проксируемый запрос вернул MethodNotFound — признак
    /// того, что закэшированный диалект неверен для этого endpoint.
    /// Следующий запрос к agent_id выполнит зонд заново.
    pub fn remove(&self, agent_id: &str) {
        self.cache.remove(agent_id);
    }
}

impl Default for DialectCache {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn probe_dialect(
    client: &reqwest::Client,
    base_url: &str,
    push_token: Option<&str>,
) -> Result<A2aDialect, ProbeError> {
    let probe_task_id = Uuid::new_v4().to_string();

    let sdk_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "GetTask",
        "params": { "name": format!("tasks/{probe_task_id}") }
    });

    if let Some(dialect) =
        try_probe_request(client, base_url, push_token, &sdk_payload, A2aDialect::Sdk).await?
    {
        return Ok(dialect);
    }

    let spec_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/get",
        "params": { "id": probe_task_id }
    });

    if let Some(dialect) = try_probe_request(
        client,
        base_url,
        push_token,
        &spec_payload,
        A2aDialect::Spec,
    )
    .await?
    {
        return Ok(dialect);
    }

    Err(ProbeError::Unrecognized)
}

async fn try_probe_request(
    client: &reqwest::Client,
    base_url: &str,
    push_token: Option<&str>,
    payload: &Value,
    candidate: A2aDialect,
) -> Result<Option<A2aDialect>, ProbeError> {
    let mut req = client
        .post(base_url)
        .timeout(Duration::from_secs(10))
        .json(payload);
    if let Some(token) = push_token {
        req = req.bearer_auth(token);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| ProbeError::Http(e.to_string()))?;
    let body: Value = resp
        .json()
        .await
        .map_err(|e| ProbeError::Http(e.to_string()))?;

    Ok(interpret_probe_response(&body).then_some(candidate))
}

/// ИСПРАВЛЕНО (тот же D1-принцип, что в driver-a2a-client): проверяет код
/// -32601 (стандартный JSON-RPC "method not found") ИЛИ нормализованный
/// текст с несколькими формулировками, а не одну точную подстроку.
fn interpret_probe_response(body: &Value) -> bool {
    !response_indicates_method_not_found(body)
}

/// Распознаёт MethodNotFound в JSON-RPC-ответе (общая для зонда и для
/// инвалидации кэша D3). True = сервер не распознал метод — по коду -32601
/// (стандарт JSON-RPC 2.0) ИЛИ нормализованному тексту ("method not found",
/// "method_not_found", "unknown method"). False = сервер понял метод.
pub fn response_indicates_method_not_found(body: &Value) -> bool {
    const JSONRPC_STANDARD_METHOD_NOT_FOUND: i64 = -32601;

    let Some(error) = body.get("error") else {
        return false;
    };
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    let normalized = message.to_lowercase();
    code == JSONRPC_STANDARD_METHOD_NOT_FOUND
        || normalized.contains("method not found")
        || normalized.contains("method_not_found")
        || normalized.contains("unknown method")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_standard_jsonrpc_method_not_found_by_code() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        });
        assert!(!interpret_probe_response(&body));
    }

    #[test]
    fn recognizes_our_gateway_marker_text() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32000, "message": "method_not_found: GetTask" }
        });
        assert!(!interpret_probe_response(&body));
    }

    #[test]
    fn task_not_found_error_is_recognized_as_dialect_match() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32001, "message": "task not found: task-deadbeef" }
        });
        assert!(interpret_probe_response(&body));
    }

    #[test]
    fn successful_result_is_recognized_as_dialect_match() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "task": { "id": "task-x", "status": { "state": "TASK_STATE_UNSPECIFIED" } } }
        });
        assert!(interpret_probe_response(&body));
    }

    #[test]
    fn dialect_cache_stores_and_retrieves_result() {
        let cache = DialectCache::new();
        cache.set("hermes", A2aDialect::Spec);
        assert_eq!(cache.get("hermes"), Some(A2aDialect::Spec));
    }

    #[test]
    fn dialect_cache_overwrites_on_second_set() {
        let cache = DialectCache::new();
        cache.set("hermes", A2aDialect::Sdk);
        cache.set("hermes", A2aDialect::Spec);
        assert_eq!(cache.get("hermes"), Some(A2aDialect::Spec));
    }

    #[test]
    fn response_indicates_mnf_by_standard_jsonrpc_code() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        });
        assert!(response_indicates_method_not_found(&body));
    }

    #[test]
    fn response_indicates_mnf_by_gateway_marker_text() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32000, "message": "method_not_found: message/send" }
        });
        assert!(response_indicates_method_not_found(&body));
    }

    #[test]
    fn response_does_not_indicate_mnf_for_task_not_found() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32001, "message": "task not found: task-x" }
        });
        assert!(!response_indicates_method_not_found(&body));
    }

    #[test]
    fn response_does_not_indicate_mnf_for_success() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "task": { "id": "x" } }
        });
        assert!(!response_indicates_method_not_found(&body));
    }

    #[test]
    fn dialect_cache_remove_invalidates_entry() {
        let cache = DialectCache::new();
        cache.set("hermes", A2aDialect::Sdk);
        cache.remove("hermes");
        assert_eq!(cache.get("hermes"), None);
    }
}
