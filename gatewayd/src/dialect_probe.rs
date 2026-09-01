//! gatewayd/src/dialect_probe.rs
//!
//! Dialect probe for Direction 2 (A2A client -> A2A agent, passthrough).
//! Implements §3 of the spec (SPEC-a2a-dialects-gateway-adapter.md): the gateway itself goes to
//! third-party A2A agents via transport_a2a_passthrough (Transport::Http{url,
//! push_token}) and must know which dialect (SDK/Spec) the agent answers in
//! BEFORE proxying the first real client request.
//!
//! Principle (§3.1 of the spec): the probe is idempotent — GetTask/tasks/get with a nonexistent
//! task_id (random UUID), NOT SendMessage/message/send (those create a task).
//! The result is cached on agent_id — one probe per first contact.
//!
//! "method not found" recognition — by code -32601 (standard JSON-RPC
//! 2.0) OR by normalized text with several known
//! wordings ("method not found", "method_not_found", "unknown
//! method") — NOT by one exact substring of a particular gateway. The same
//! fix as applied in the client-side
//! driver-a2a-client/src/dialect_probe.rs (D1): a standard server
//! answers -32601 "Method not found" without a colon, the exact substring
//! "method_not_found:" misses this.

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

    /// Invalidation of the cached dialect (D3): called when a real
    /// (non-probe) proxied request returned MethodNotFound — a sign
    /// that the cached dialect is wrong for this endpoint.
    /// The next request to agent_id runs the probe again.
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

/// FIXED (same D1 principle as in driver-a2a-client): checks code
/// -32601 (standard JSON-RPC "method not found") OR normalized
/// text with several wordings, not one exact substring.
fn interpret_probe_response(body: &Value) -> bool {
    !response_indicates_method_not_found(body)
}

/// Recognizes MethodNotFound in a JSON-RPC response (shared by the probe and by
/// the D3 cache invalidation). True = the server did not recognize the method — by code -32601
/// (JSON-RPC 2.0 standard) OR normalized text ("method not found",
/// "method_not_found", "unknown method"). False = the server understood the method.
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
