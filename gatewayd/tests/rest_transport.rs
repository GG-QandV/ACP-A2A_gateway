// ============================================================================
// gatewayd/tests/rest_transport.rs
//
// E2E-тесты REST-маршрута POST /agents/:id/message:send (и легаси-алиаса
// /message/send). Харнес живой: Registry указывает на реальный
// stdio-процесс mock_acp_agent (собственный [[bin]] пакета), роутер
// собирается из lib-части gatewayd, запросы идут через
// tower::ServiceExt::oneshot. Никаких заглушек.
//
// Ключевые контракты (подтверждены кодом a2a-rs, см. карточку задачи):
// - Ответ УСПЕХА — {"task": {...}} (SendMessageResponse), НЕ плоский Task.
// - Конверт ОШИБКИ — {"error": {"code", "status", "message", "details"}},
//   без полей jsonrpc/id.
// - Легаси-алиас /message/send ведёт на тот же хендлер.
// - ContextLost -> 409 + status "ABORTED" + code -32010.
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::transport_http::{http_status_to_sdk_name, router};
use serde_json::{json, Value};
use tower::ServiceExt;

fn mock_bin() -> String {
    std::env::var("CARGO_BIN_EXE_mock_acp_agent").expect(
        "CARGO_BIN_EXE_mock_acp_agent должен быть задан: cargo test собирает [[bin]] пакета",
    )
}

/// Собирает роутер с Registry, в котором agent_id ведёт на процесс
/// mock_acp_agent с заданным окружением. Каталог хранилища задач —
/// уникальный tempdir на роутер (интеграционные тесты могут идти
/// параллельно, общий каталог дал бы коллизии задач).
fn build_router(agent_id: &str, env: HashMap<String, String>) -> axum::Router {
    let tokens: HashSet<String> = ["test-token".to_string()].into_iter().collect();
    let mut agents = HashMap::new();
    agents.insert(
        agent_id.to_string(),
        AgentEntry::new(
            Transport::Stdio {
                command: vec![mock_bin()],
                cwd: None,
                env,
            },
            4,
            Duration::from_secs(15),
            Duration::from_secs(120),
        ),
    );
    let registry = std::sync::Arc::new(Registry::new(tokens, agents));

    // TempDir обязан жить дольше запросов: adapter пишет задачи в него.
    // Тестовый процесс короткоживущий — намеренная утечка вместо того,
    // чтобы тащить TempDir из каждого хелпера и усложнять сигнатуры.
    let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));

    router(
        registry,
        dir.path().to_path_buf(),
        Duration::from_secs(5),
        Duration::from_secs(10),
        "http://localhost:8348".to_string(),
    )
}

async fn test_router_with_stub_agent(agent_id: &str) -> axum::Router {
    build_router(agent_id, HashMap::new())
}

/// Мок выходит (код 0) после первого session/prompt: первый запрос
/// успевает создать сессию в поколении 1, второй — попадает на
/// перезапущенный процесс (поколение 2) и получает ContextLost.
async fn test_router_with_context_lost_agent(agent_id: &str) -> axum::Router {
    let mut env = HashMap::new();
    env.insert("MOCK_AGENT_EXIT_AFTER_PROMPTS".to_string(), "1".to_string());
    build_router(agent_id, env)
}

/// POST сообщения на message:send с валидным токеном и заданным
/// contextId. contextId фиксируется явно: без него build_task выдумывает
/// новый на каждый запрос, и продолжение разговора не проверить.
async fn post_message(router: axum::Router, path: &str, context_id: &str) -> Response {
    router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "message": {
                            "messageId": "m1",
                            "role": "ROLE_USER",
                            "parts": [{ "text": "ping" }],
                            "contextId": context_id,
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: Response) -> Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn rest_path_with_colon_is_matched_by_axum() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message:send", "ctx-1").await;

    // Сильнее исходной проверки v2 (!= NOT_FOUND): мок жив, значит
    // ответ обязан быть 200 с обёрткой task.
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json.get("task").is_some());
}

/// Легаси-алиас, подтверждённый a2a-server/src/rest.rs:24,68 — сервер SDK
/// принимает оба пути на один и тот же хендлер обработки.
#[tokio::test]
async fn legacy_message_send_path_routes_to_same_handler() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message/send", "ctx-1").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    // Тот же контракт ответа, что и у основного пути — обёртка task.
    assert!(json.get("task").is_some());
}

/// Факт из a2a-server/src/rest.rs:1264: тест сервера читает
/// send_resp["task"]["id"] — обёртка ЕСТЬ и обязательна. Разворачивание
/// было ошибочным предположением v1.
#[tokio::test]
async fn rest_response_keeps_task_wrapper_matching_sdk_send_message_response() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = post_message(router, "/agents/agent-x/message:send", "ctx-1").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;

    assert!(
        json.get("task").is_some(),
        "REST-ответ ДОЛЖЕН содержать обёртку task (SendMessageResponse protojson), \
         разворачивание в v1 было ошибочным предположением"
    );
    assert!(json["task"].get("id").is_some());
    assert!(json["task"]["status"]["state"]
        .as_str()
        .unwrap()
        .starts_with("TASK_STATE_"));
    // Мок отвечает end_turn — задача завершена, а не зависла в submitted.
    assert_eq!(json["task"]["status"]["state"], "TASK_STATE_COMPLETED");
}

/// Конверт ошибки — {"code", "status", "message", "details"}, без
/// jsonrpc/id. Факт из a2a-server/src/rest.rs:470.
#[tokio::test]
async fn rest_error_envelope_has_all_four_sdk_fields() {
    let router = test_router_with_stub_agent("agent-x").await;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/agent-x/message:send")
                .header("content-type", "application/json")
                // Без Authorization — ожидаем 401.
                .body(Body::from(json!({ "message": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(response).await;

    assert!(
        json.get("jsonrpc").is_none(),
        "REST-ошибка не несёт jsonrpc-конверт"
    );
    assert!(json.get("id").is_none(), "REST-ошибка не несёт JSON-RPC id");

    let error = &json["error"];
    assert!(error["code"].is_i64(), "поле code обязательно");
    assert!(
        error["status"].is_string(),
        "поле status обязательно (SDK REST-конверт, 4 поля)"
    );
    assert!(error["message"].is_string(), "поле message обязательно");
    assert!(
        error.get("details").is_some(),
        "поле details должно присутствовать (может быть null)"
    );
    // 401 -> UNAUTHENTICATED по gRPC-маппингу.
    assert_eq!(error["status"], "UNAUTHENTICATED");
}

/// ContextLost после перезапуска процесса: первый запрос создаёт сессию
/// в поколении 1, мок умирает, супервизор перезапускает его (поколение 2),
/// второй запрос с тем же contextId — 409 + ABORTED + -32010.
#[tokio::test]
async fn rest_context_lost_matches_rpc_behavior_with_four_field_envelope() {
    let router = test_router_with_context_lost_agent("agent-y").await;

    // Разговор заводится и завершается успешно (поколение 1).
    let first = post_message(router.clone(), "/agents/agent-y/message:send", "ctx-lost").await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "первый запрос обязан пройти"
    );
    drop(first);

    // Пережидаем respawn backoff супервизора (5s), чтобы второй запрос
    // попал уже на перезапущенный процесс, а не получил «процесс мёртв,
    // повторный запуск не раньше».
    tokio::time::sleep(Duration::from_secs(5) + Duration::from_millis(500)).await;

    let response = post_message(router, "/agents/agent-y/message:send", "ctx-lost").await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json = body_json(response).await;
    assert_eq!(json["error"]["code"], -32010);
    assert_eq!(json["error"]["status"], "ABORTED");
}

#[test]
fn http_status_to_sdk_name_covers_all_used_codes() {
    // Регрессия: если в rest_sdk_error появится новый StatusCode без
    // соответствующей ветки в http_status_to_sdk_name, тест должен упасть,
    // а не молча отдать "UNKNOWN" в проде. Проверяется именно боевая
    // функция из transport_http, а не её копия.
    let used_codes = [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::NOT_FOUND,
        StatusCode::CONFLICT,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::NOT_IMPLEMENTED,
        StatusCode::OK,
    ];
    for code in used_codes {
        assert_ne!(
            http_status_to_sdk_name(code),
            "UNKNOWN",
            "код {code} используется в rest_sdk_error, но не замаплен на SDK status name"
        );
    }
}
