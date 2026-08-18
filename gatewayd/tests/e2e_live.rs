//! gatewayd/tests/e2e_live.rs
//!
//! Живой E2E шлюза (ТЗ §2.6 п.4, направление 2): реальный HTTP-запрос к
//! поднятому gatewayd с агентом hermes-main (hermes acp). В отличие от
//! rest_transport.rs (внутрипроцессный харнес на mock_acp_agent) — идёт
//! через сеть к живому процессу шлюза и реальному агенту.
//!
//! Требует поднятого шлюза:
//!   gatewayd /tmp/gateway-e2e/config.yaml
//! (конфиг: agents.hermes-main = [hermes, acp], токен t-e2e-001,
//! http_listen 127.0.0.1:8348).
//!
//! Запуск:
//!   cargo test -p gatewayd --test e2e_live -- --ignored --nocapture
//!
//! E2E_GATEWAY_URL / E2E_TOKEN / E2E_AGENT переопределяют базовый URL,
//! токен и agent_id (по умолчанию http://127.0.0.1:8348, t-e2e-001,
//! hermes-main).

use serde_json::json;
use serde_json::Value;

fn base_url() -> String {
    std::env::var("E2E_GATEWAY_URL").unwrap_or_else(|_| "http://127.0.0.1:8348".into())
}

fn token() -> String {
    std::env::var("E2E_TOKEN").unwrap_or_else(|_| "t-e2e-001".into())
}

fn agent_id() -> String {
    std::env::var("E2E_AGENT").unwrap_or_else(|_| "hermes-main".into())
}

fn rpc_url() -> String {
    format!("{}/agents/{}/rpc", base_url(), agent_id())
}

async fn post_json(url: &str, token: &str, body: Value) -> reqwest::Response {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap()
        .post(url)
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .expect("request to live gateway must reach it")
}

/// Живой E2E по spec-wire: message/send доходит до hermes и возвращает
/// Completed с текстом (плоский Task, lowercase state).
#[tokio::test]
#[ignore]
async fn e2e_live_spec_wire_message_send_completes() {
    let resp = post_json(
        &rpc_url(),
        &token(),
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": { "role": "user", "parts": [{ "kind": "text", "text": "Reply with exactly: GW_SPEC_OK" }] } }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200, "spec message/send must be 200");
    let body: Value = resp.json().await.expect("valid JSON-RPC response");
    assert!(
        body.get("error").is_none(),
        "no error expected, got: {body}"
    );

    let result = body.get("result").expect("result present");
    let state = result.pointer("/status/state").and_then(Value::as_str);
    assert_eq!(
        state,
        Some("completed"),
        "state must be completed, got: {state:?}"
    );
    let text = extract_text(result);
    assert!(
        text.contains("GW_SPEC_OK"),
        "hermes must echo GW_SPEC_OK, got: {text:?}"
    );
    println!("E2E spec OK: text={text:?}");
}

/// Живой E2E по SDK-wire: SendMessage через шлюз доходит до hermes и
/// возвращает TASK_STATE_COMPLETED в обёртке {task}.
#[tokio::test]
#[ignore]
async fn e2e_live_sdk_wire_send_message_completes() {
    let resp = post_json(
        &rpc_url(),
        &token(),
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
            "params": { "message": { "role": "ROLE_USER", "parts": [{ "text": "Reply with exactly: GW_SDK_OK" }] } }
        }),
    )
    .await;
    assert_eq!(resp.status(), 200, "SDK SendMessage must be 200");
    let body: Value = resp.json().await.expect("valid JSON-RPC response");
    assert!(
        body.get("error").is_none(),
        "no error expected, got: {body}"
    );

    let task = body
        .pointer("/result/task")
        .expect("SDK result.task wrapper");
    let state = task.pointer("/status/state").and_then(Value::as_str);
    assert_eq!(
        state,
        Some("TASK_STATE_COMPLETED"),
        "SDK state must be TASK_STATE_COMPLETED, got: {state:?}"
    );
    let text = extract_text(task);
    assert!(
        text.contains("GW_SDK_OK"),
        "hermes must echo GW_SDK_OK, got: {text:?}"
    );
    println!("E2E SDK OK: text={text:?}");
}

/// Живой E2E: agent-card доступен по токену и отдаёт url rpc-эндпоинта.
#[tokio::test]
#[ignore]
async fn e2e_live_agent_card_available() {
    let url = format!(
        "{}/agents/{}/.well-known/agent.json",
        base_url(),
        agent_id()
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .header("authorization", format!("Bearer {}", token()))
        .send()
        .await
        .expect("card request must reach gateway");
    assert_eq!(resp.status(), 200);
    let card: Value = resp.json().await.expect("valid JSON card");
    let card_url = card.get("url").and_then(Value::as_str).unwrap_or("");
    assert!(
        card_url.contains(&agent_id()),
        "card url must reference the agent, got: {card_url}"
    );
    println!("E2E card OK: url={card_url}");
}

/// Собирает текст ответа из частей артефактов результата. И spec, и SDK
/// несут текст в поле "text" (spec: {"kind":"text","text":...}, sdk:
/// {"text":...}).
fn extract_text(result: &Value) -> String {
    let mut out = String::new();
    if let Some(artifacts) = result.get("artifacts").and_then(Value::as_array) {
        for artifact in artifacts {
            if let Some(parts) = artifact.get("parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    out
}
