//! core/src/http_agent.rs
//!
//! HttpA2aAgent — реализация trait A2aAgent поверх реального HTTP JSON-RPC
//! клиента к внешнему A2A-агенту.

use std::time::Duration;

use async_trait::async_trait;
use protocol::a2a::{AgentCard, SendMessageParams, Task, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::A2aAgent;
use crate::reply::Reply;

pub struct HttpA2aAgent {
    client: reqwest::Client,
    base_url: String,
    push_token: Option<String>,
}

impl HttpA2aAgent {
    pub fn new(base_url: impl Into<String>, push_token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client builds with default TLS backend"),
            base_url: base_url.into(),
            push_token,
        }
    }

    fn rpc_endpoint(&self) -> String {
        self.base_url.clone()
    }

    fn agent_card_url(&self) -> String {
        format!("{}/.well-known/agent.json", self.base_url.trim_end_matches('/'))
    }

    async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        let request_id = uuid_stub();
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });

        let mut req = self.client.post(self.rpc_endpoint()).json(&body);
        if let Some(token) = &self.push_token {
            req = req.bearer_auth(token);
        }

        let resp: JsonRpcResponse<R> = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("A2A HTTP request failed ({method}): {e}"))?
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("A2A response не парсится как JSON-RPC ({method}): {e}"))?;

        match resp {
            JsonRpcResponse::Ok { result, .. } => Ok(result),
            JsonRpcResponse::Err { error, .. } => {
                anyhow::bail!("A2A error {method}: [{}] {}", error.code, error.message)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonRpcResponse<R> {
    Ok { #[allow(dead_code)] jsonrpc: String, result: R },
    Err { #[allow(dead_code)] jsonrpc: String, error: JsonRpcError },
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ИСПРАВЛЕНО (найдено компилятором, E0277): был derive(Deserialize),
// хотя структура используется как ИСХОДЯЩИЕ params в call<P: Serialize>.
#[derive(Debug, Serialize)]
struct GetTaskParams<'a> {
    id: &'a str,
}

#[async_trait]
impl A2aAgent for HttpA2aAgent {
    async fn card(&self) -> anyhow::Result<AgentCard> {
        let resp = self
            .client
            .get(self.agent_card_url())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("не удалось получить AgentCard: {e}"))?;

        resp.json::<AgentCard>()
            .await
            .map_err(|e| anyhow::anyhow!("AgentCard не парсится: {e}"))
    }

    async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, protocol::a2a::A2aEvent>> {
        let message = task
            .status
            .message
            .clone()
            .ok_or_else(|| anyhow::anyhow!("task.status.message обязателен для send_task"))?;

        let params = SendMessageParams {
            message,
            configuration: Some(protocol::a2a::MessageSendConfiguration {
                blocking: true,
                history_length: None,
            }),
        };

        let result_task: Task = self.call("message/send", params).await?;
        Ok(Reply::Complete(result_task))
    }

    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.call("tasks/get", GetTaskParams { id: &id.0 }).await
    }

    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.call("tasks/cancel", GetTaskParams { id: &id.0 }).await
    }
}

fn uuid_stub() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!("req-{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_endpoint_uses_base_url_as_is() {
        let agent = HttpA2aAgent::new("https://ops.internal/a2a", None);
        assert_eq!(agent.rpc_endpoint(), "https://ops.internal/a2a");
    }

    #[test]
    fn agent_card_url_appends_wellknown_path() {
        let agent = HttpA2aAgent::new("https://ops.internal/a2a/", None);
        assert_eq!(
            agent.agent_card_url(),
            "https://ops.internal/a2a/.well-known/agent.json"
        );
    }
}
