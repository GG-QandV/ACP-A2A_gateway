//! gatewayd/tests/streaming_tcp.rs
//!
//! T4 (Часть 3 роадмапа стриминга): TCP-клиент направления 3 получает
//! построчные session/update-нотификации от A2A-агента, отвечающего SSE.
//!
//! Цепочка: TCP-клиент -> gatewayd (transport_tcp, A2aAsAcp) ->
//! HttpA2aAgent (SSE-клиент, core/src/http_agent.rs) -> mock A2A-сервер
//! (text/event-stream). HttpA2aAgent::send_task возвращает Reply::Streaming,
//! A2aAsAcp::prompt транслирует A2aEvent -> SessionUpdate, transport_tcp
//! пишет построчно в TCP-сокет как session/update нотификации.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use protocol::a2a::{A2aEvent, Message, MessageRole, Part, TaskId, TaskState, TaskStatus};

/// Mock A2A-сервер: message/send отвечает SSE-потоком из двух A2aEvent
/// (working, затем completed). ВАЖНО: события сериализуются через
/// serde_json::to_string(&A2aEvent) — ТОЧНО так же, как это делает
/// прод stream_to_sse (gatewayd/src/transport_http.rs). Никакого ручного
/// JSON-хардкода: формат (snake_case task_id, kebab-case state, lowercase
/// role) берётся из serde-атрибутов реальных типов и не может
/// рассинхронизироваться.
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

/// Поднимает transport_tcp::serve с Http-агентом (ведущим на mock SSE-сервер).
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

    // Узнаём свободный порт: bind на 0, читаем адрес, освобождаем.
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

    // Даём шлюзу мгновение на bind.
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

    // Handshake: ACP-клиент представляется.
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

    // Читаем ответ session/new — берём настоящий sessionId.
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

    // session/prompt с реальным sessionId.
    let prompt = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/prompt\",\"params\":{{\"sessionId\":\"{session_id}\",\"prompt\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}}}}\n"
    );
    reader.get_mut().write_all(prompt.as_bytes()).await.unwrap();

    let mut notifications = 0usize;
    let mut lines = 0usize;
    // Читаем, пока не получим минимум 1 session/update и не наступит
    // пауза в данных (стрим завершился после терминала). Стрим-путь
    // направления 3 не шлёт финальный PromptResponse — только нотификации,
    // затем тишина (канал закрыт, соединение продолжает обслуживать
    // следующие запросы клиента).
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
