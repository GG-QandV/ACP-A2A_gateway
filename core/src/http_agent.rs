//! core/src/http_agent.rs
//!
//! HttpA2aAgent — implementation of the A2aAgent trait over a real HTTP JSON-RPC
//! client to an external A2A agent.

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

// FIXED (found by the compiler, E0277): it used to be derive(Deserialize),
// although the struct is used as OUTGOING params in call<P: Serialize>.
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
                // ADDED (T4): blocking=false for requests capable of
                // returning an SSE stream (otherwise the server returns a single JSON, not
                // an event stream).
                blocking: false,
                history_length: None,
            }),
        };

        let request_id = uuid_stub();
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "message/send",
            "params": params,
        });
        let mut req = self.client.post(self.rpc_endpoint()).json(&body);
        if let Some(token) = &self.push_token {
            req = req.bearer_auth(token);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("A2A HTTP request failed (message/send): {e}"))?;

        // ADDED (T4): if the server returned SSE — stream. Otherwise — the previous
        // behavior (full JSON Task).
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("text/event-stream") {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<protocol::a2a::A2aEvent>();
            tokio::spawn(sse_to_a2a_events(resp, tx));
            Ok(Reply::Streaming(rx))
        } else {
            let envelope: JsonRpcResponse<Task> = resp
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("A2A response не парсится как JSON-RPC: {e}"))?;
            match envelope {
                JsonRpcResponse::Ok { result, .. } => Ok(Reply::Complete(result)),
                JsonRpcResponse::Err { error, .. } => {
                    anyhow::bail!("A2A error message/send: [{}] {}", error.code, error.message)
                }
            }
        }
    }

    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.call("tasks/get", GetTaskParams { id: &id.0 }).await
    }

    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.call("tasks/cancel", GetTaskParams { id: &id.0 }).await
    }
}

/// ADDED (T4): reads the A2A agent's SSE response (a `data: {json}\n\n` stream)
/// and sends each A2aEvent to the channel. Ends when the stream closes or on a
/// read error. SSE frame: "data: {json}\n\n" — each data: line is a
/// separate event.
async fn sse_to_a2a_events(
    resp: reqwest::Response,
    tx: tokio::sync::mpsc::UnboundedSender<protocol::a2a::A2aEvent>,
) {
    use futures_util::StreamExt;
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "SSE-стрим A2A-агента: ошибка чтения, поток закрыт");
                break;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // SSE: events are separated by a blank line. Collect the data: lines.
        while let Some(pos) = buffer.find("\n\n") {
            let frame = buffer[..pos].to_string();
            buffer.drain(..pos + 2);
            let data_line = frame
                .lines()
                .find(|l| l.starts_with("data:"))
                .map(|l| l.trim_start_matches("data:").trim())
                .unwrap_or("");
            if data_line.is_empty() {
                continue;
            }
            match serde_json::from_str::<protocol::a2a::A2aEvent>(data_line) {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        // Receiver disconnected — end the stream.
                        return;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, data = %data_line, "SSE-фрейм не распознан как A2aEvent — пропущен");
                }
            }
        }
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

    /// T4: send_task with Content-Type: text/event-stream returns
    /// Reply::Streaming with A2aEvents from SSE frames. The full TCP path
    /// (gateway transport) is covered in gatewayd/tests/streaming_tcp.rs.
    #[tokio::test]
    async fn send_task_returns_streaming_on_sse_response() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        // Mock server: responds with an SSE stream of two A2aEvent events.
        // IMPORTANT: events are serialized via serde_json::to_string(&A2aEvent)
        // — EXACTLY like the prod stream_to_sse. No manual JSON hardcoding:
        // the format comes from the serde attributes of the real types.
        let app = Router::new().route("/a2a", post(|| async {
            let working = protocol::a2a::A2aEvent::TaskStatusUpdate {
                task_id: TaskId("t-1".into()),
                status: protocol::a2a::TaskStatus {
                    state: protocol::a2a::TaskState::Working,
                    message: None,
                    timestamp: None,
                },
                r#final: false,
            };
            let completed = protocol::a2a::A2aEvent::TaskStatusUpdate {
                task_id: TaskId("t-1".into()),
                status: protocol::a2a::TaskStatus {
                    state: protocol::a2a::TaskState::Completed,
                    message: None,
                    timestamp: None,
                },
                r#final: true,
            };
            let frame = |ev: &protocol::a2a::A2aEvent| {
                format!("data: {}\n\n", serde_json::to_string(ev).unwrap())
            };
            let sse = format!("{}{}", frame(&working), frame(&completed));
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                sse,
            )
                .into_response()
        }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let agent = HttpA2aAgent::new(format!("http://{addr}/a2a"), None);
        let task = Task {
            id: TaskId("t-1".into()),
            context_id: protocol::a2a::ContextId("ctx-1".into()),
            status: protocol::a2a::TaskStatus {
                state: protocol::a2a::TaskState::Submitted,
                message: Some(protocol::a2a::Message {
                    role: protocol::a2a::MessageRole::User,
                    parts: vec![protocol::a2a::Part::Text {
                        text: "hi".into(),
                    }],
                    message_id: None,
                }),
                timestamp: None,
            },
            history: None,
            artifacts: None,
            metadata: None,
        };

        let reply = agent.send_task(task).await.expect("send_task работает");
        let Reply::Streaming(mut rx) = reply else {
            panic!("при text/event-stream должен вернуться Reply::Streaming");
        };

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("первое событие приходит")
            .expect("канал не закрыт");
        assert!(matches!(first, protocol::a2a::A2aEvent::TaskStatusUpdate { r#final: false, .. }));

        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("второе событие приходит")
            .expect("канал не закрыт");
        assert!(matches!(second, protocol::a2a::A2aEvent::TaskStatusUpdate { r#final: true, .. }));
    }
}
