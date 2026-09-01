//! gatewayd/tests/streaming_tcp.rs
//!
//! T4 (Part 3 of the streaming roadmap): a TCP client of direction 3 receives
//! line-by-line session/update notifications from an A2A agent answering with SSE.
//!
//! Chain: TCP client -> gatewayd (transport_tcp, A2aAsAcp) ->
//! HttpA2aAgent (SSE client, core/src/http_agent.rs) -> mock A2A server
//! (text/event-stream). HttpA2aAgent::send_task returns Reply::Streaming,
//! A2aAsAcp::prompt translates A2aEvent -> SessionUpdate, transport_tcp
//! writes line by line to the TCP socket as session/update notifications.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use protocol::a2a::{A2aEvent, Message, MessageRole, Part, TaskId, TaskState, TaskStatus};

/// Mock A2A server: message/send replies with an SSE stream of two A2aEvent
/// items (working, then completed). IMPORTANT: events are serialized via
/// serde_json::to_string(&A2aEvent) — EXACTLY the same way production
/// stream_to_sse does (gatewayd/src/transport_http.rs). No manual
/// JSON hardcoding: the format (snake_case task_id, kebab-case state, lowercase
/// role) comes from the serde attributes of the real types and cannot
/// drift out of sync.
async fn spawn_mock_sse_server() -> String {
    let app = Router::new().route(
        "/a2a",
        post(|| async {
            let working = A2aEvent::TaskStatusUpdate {
                task_id: TaskId("t-1".into()),
                status: TaskStatus {
                    state: TaskState::Working,
                    message: Some(Message {
                        role: MessageRole::Agent,
                        parts: vec![Part::Text {
                            text: "чанк-1".into(),
                        }],
                        message_id: None,
                    }),
                    timestamp: None,
                },
                r#final: false,
            };
            let completed = A2aEvent::TaskStatusUpdate {
                task_id: TaskId("t-1".into()),
                status: TaskStatus {
                    state: TaskState::Completed,
                    message: None,
                    timestamp: None,
                },
                r#final: true,
            };
            let frame = |ev: &A2aEvent| format!("data: {}\n\n", serde_json::to_string(ev).unwrap());
            let sse = format!("{}{}", frame(&working), frame(&completed));
            (StatusCode::OK, [("content-type", "text/event-stream")], sse).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/a2a")
}

/// Spins up transport_tcp::serve with an Http agent (pointing at the mock SSE server).
async fn spawn_tcp_gateway(a2a_url: String) -> String {
    let tokens: HashSet<String> = ["t-test".to_string()].into_iter().collect();
    let mut agents = HashMap::new();
    agents.insert(
        "a2a-stream".to_string(),
        AgentEntry::new(
            Transport::Http {
                url: a2a_url,
                push_token: None,
            },
            4,
            Duration::from_secs(15),
            Duration::from_secs(120),
        ),
    );
    let registry = std::sync::Arc::new(Registry::new(tokens, agents));

    // Learn a free port: bind on 0, read the address, release.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);

    let serve_registry = registry.clone();
    let task_store_dir = tempfile::tempdir().unwrap().path().to_path_buf();
    let serve_addr = addr.clone();

    tokio::spawn(async move {
        let _ = gatewayd::transport_tcp::serve(
            &serve_addr,
            serve_registry,
            task_store_dir,
            Duration::from_secs(5),
        )
        .await;
    });

    // Give the gateway a moment to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn tcp_client_receives_session_update_notifications() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let a2a_url = spawn_mock_sse_server().await;
    let gateway_addr = spawn_tcp_gateway(a2a_url).await;

    let mut socket = tokio::net::TcpStream::connect(&gateway_addr)
        .await
        .expect("TCP-шлюз принимает");
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Handshake: the ACP client introduces itself.
    socket
        .write_all(b"{\"token\":\"t-test\",\"agent_id\":\"a2a-stream\"}\n")
        .await
        .unwrap();
    // session/new
    socket
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/new\",\"params\":{\"cwd\":\"/tmp\"}}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(socket);

    // Read the session/new response — take the real sessionId.
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("ответ session/new в пределах 5с")
        .unwrap_or(0);
    assert!(n > 0, "шлюз должен ответить на session/new");
    let session_new: serde_json::Value = serde_json::from_str(line.trim()).expect("валидный JSON");
    assert!(
        session_new.get("error").is_none(),
        "session/new не должен быть ошибкой: {session_new}"
    );
    let session_id = session_new["result"]["sessionId"]
        .as_str()
        .expect("sessionId присутствует")
        .to_string();

    // session/prompt with the real sessionId.
    let prompt = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/prompt\",\"params\":{{\"sessionId\":\"{session_id}\",\"prompt\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}}}}\n"
    );
    reader.get_mut().write_all(prompt.as_bytes()).await.unwrap();

    let mut notifications = 0usize;
    let mut lines = 0usize;
    // Keep reading until we get at least 1 session/update and a
    // pause in data occurs (the stream ended after the terminal). The stream path
    // of direction 3 does not send a final PromptResponse — only notifications,
    // then silence (the channel is closed, the connection keeps serving
    // the client's next requests).
    loop {
        let n = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("чтение не должно паниковать")
            .unwrap_or(0);
        if n == 0 && notifications >= 1 {
            break;
        }
        if n == 0 && notifications == 0 {
            panic!("шлюз закрыл соединение без единой нотификации");
        }
        lines += 1;
        if line.contains("\"method\":\"session/update\"") {
            notifications += 1;
        }
        if notifications >= 1 {
            break;
        }
    }

    assert!(
        notifications >= 1,
        "TCP-клиент должен получить минимум 1 session/update-нотификацию, got {notifications} из {lines} строк"
    );
}
