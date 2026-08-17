//! gatewayd/src/bin/mock_acp_agent.rs
//! Тестовый ACP-агент для интеграционных тестов gatewayd (E2E-харнес).
//!
//! Говорит по ACP (JSON-RPC 2.0 over stdio) ровно настолько, насколько
//! нужно шлюзу: initialize, session/new, session/prompt. Ответы собираются
//! по схеме protocol/src/acp.rs (camelCase, "type" как дискриминатор
//! ContentBlock, stopReason "end_turn"). Любой другой запрос с id
//! получает -32601 method_not_found.
//!
//! Режимы через окружение (задаются в Registry/AgentEntry теста):
//!   MOCK_AGENT_PROMPT_TEXT        — текст ответа (по умолчанию "pong")
//!   MOCK_AGENT_EXIT_AFTER_PROMPTS — после N успешных session/prompt
//!                                   ответить и выйти с кодом 0. Нужно для
//!                                   теста ContextLost: процесс умирает,
//!                                   супервизор перезапускает его (поколение
//!                                   растёт), и разговор старого поколения
//!                                   помечается потерянным.

use std::io::{BufRead, Write};

fn main() {
    let prompt_text = std::env::var("MOCK_AGENT_PROMPT_TEXT").unwrap_or_else(|_| "pong".to_string());
    let exit_after_prompts: u64 = std::env::var("MOCK_AGENT_EXIT_AFTER_PROMPTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut prompts_served: u64 = 0;
    let mut session_counter: u64 = 0;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        // Notification (без id): session/cancel и прочее — отвечать нечего.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg.get("method").and_then(serde_json::Value::as_str).unwrap_or("");

        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": {},
                "agentInfo": { "name": "mock-acp-agent", "version": "0.0.1" },
                "authMethods": []
            }),
            "session/new" => {
                session_counter += 1;
                serde_json::json!({ "sessionId": format!("sess-{session_counter}") })
            }
            "session/prompt" => {
                prompts_served += 1;
                // Отвечаем на промпт, затем, если достигнут лимит — умираем.
                let resp = serde_json::json!({
                    "stopReason": "end_turn",
                    "content": [{
                        "type": "text",
                        "text": prompt_text,
                    }],
                });
                write_response(&stdout, &id, Ok(resp));
                if exit_after_prompts > 0 && prompts_served >= exit_after_prompts {
                    return;
                }
                continue;
            }
            _ => {
                write_response(
                    &stdout,
                    &id,
                    Err(serde_json::json!({
                        "code": -32601,
                        "message": format!("method_not_found: {method}"),
                    })),
                );
                return;
            }
        };

        write_response(&stdout, &id, Ok(result));
    }
}

fn write_response(stdout: &std::io::Stdout, id: &serde_json::Value, result: Result<serde_json::Value, serde_json::Value>) {
    let body = match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    };
    let mut line = match serde_json::to_vec(&body) {
        Ok(v) => v,
        Err(_) => return,
    };
    line.push(b'\n');
    let mut out = stdout.lock();
    let _ = out.write_all(&line);
    let _ = out.flush();
}
