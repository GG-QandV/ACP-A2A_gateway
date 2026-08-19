//! gatewayd/tests/streaming_load.rs
//!
//! T10 (Gate 4 роадмапа стриминга): нагрузочный прогон — 5 мок-агентов,
//! 20 параллельных стримов на агента (100 одновременных SSE-стримов через
//! один роутер). Критерии Gate 4:
//!   - каждый запрос завершается 200 OK (не 503 StreamCapacityExhausted);
//!   - каждый стрим доходит до terminal (final: true) — permit на агента
//!     корректно возвращаются, иначе 20-й стрим упал бы в 503.
//!
//! Полный 10-минутный прогон на устойчивость памяти — ручной (см.
//! docs/streaming-roadmap-checklist.md, Gate 4); здесь автоматическая
//! быстрая версия того же сценария.
//!
//! Mock-агент шлёт 3 чанка agent_message_chunk (задержка 1ms), затем
//! финальный PromptResponse. Один процесс mock-агента обрабатывает запросы
//! последовательно, поэтому параллельность здесь — нагрузка на шлюз:
//! сессия открывается/закрывается для каждого из 100 стримов.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::future::join_all;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::transport_http::router;
use serde_json::json;
use tower::ServiceExt;

const AGENT_COUNT: usize = 5;
const STREAMS_PER_AGENT: usize = 20;

fn mock_bin() -> String {
    std::env::var("CARGO_BIN_EXE_mock_acp_agent").expect(
        "CARGO_BIN_EXE_mock_acp_agent должен быть задан: cargo test собирает [[bin]] пакета",
    )
}

fn build_router() -> axum::Router {
    let tokens: HashSet<String> = ["load-token".to_string()].into_iter().collect();
    let mut agents = HashMap::new();
    for i in 0..AGENT_COUNT {
        let mut env = HashMap::new();
        env.insert("MOCK_AGENT_STREAM_CHUNKS".to_string(), "3".to_string());
        env.insert("MOCK_AGENT_CHUNK_DELAY_MS".to_string(), "1".to_string());
        agents.insert(
            format!("load-agent-{i}"),
            AgentEntry::new(
                Transport::Stdio {
                    command: vec![mock_bin()],
                    cwd: None,
                    env,
                },
                STREAMS_PER_AGENT,
                Duration::from_secs(15),
                Duration::from_secs(120),
            ),
        );
    }
    let registry = std::sync::Arc::new(Registry::new(tokens, agents));

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

fn stream_request(agent_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/agents/{agent_id}/rpc"))
        .header("content-type", "application/json")
        .header("authorization", "Bearer load-token")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "message/send",
                "params": { "message": { "role": "user", "parts": [{ "kind": "text", "text": "load" }] } }
            })
            .to_string(),
        ))
        .unwrap()
}

/// Gate 4 / T10: 100 параллельных стримов, все 200 + final:true.
#[tokio::test]
async fn parallel_load_20_streams_per_agent_all_succeed() {
    let app = build_router();

    let mut futures = Vec::new();
    for i in 0..AGENT_COUNT {
        let agent_id = format!("load-agent-{i}");
        for _ in 0..STREAMS_PER_AGENT {
            let app = app.clone();
            let agent_id = agent_id.clone();
            futures.push(async move {
                let resp = app
                    .oneshot(stream_request(&agent_id))
                    .await
                    .expect("request completes");
                let status = resp.status();
                let body = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
                    .await
                    .expect("SSE тело читается")
                    .to_vec();
                (agent_id, status, body)
            });
        }
    }

    let results = join_all(futures).await;
    assert_eq!(
        results.len(),
        AGENT_COUNT * STREAMS_PER_AGENT,
        "все 100 стримов должны выполниться"
    );

    for (idx, (agent_id, status, body)) in results.into_iter().enumerate() {
        assert_eq!(
            status,
            StatusCode::OK,
            "стрим #{idx} ({agent_id}) должен быть 200 — permit на агента возвращаются корректно"
        );
        let text = String::from_utf8_lossy(&body).to_string();
        assert!(
            text.contains("\"final\":true"),
            "стрим #{idx} ({agent_id}) должен дойти до terminal final:true, got: {}",
            text.chars().take(300).collect::<String>()
        );
    }
}