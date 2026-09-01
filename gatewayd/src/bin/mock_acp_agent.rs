//! gatewayd/src/bin/mock_acp_agent.rs
//! Test ACP agent for gatewayd integration tests (E2E harness).
//!
//! Speaks ACP (JSON-RPC 2.0 over stdio) exactly as much as
//! the gateway needs: initialize, session/new, session/prompt. Responses are built
//! per the protocol/src/acp.rs schema (camelCase, "type" as the
//! ContentBlock discriminator, stopReason "end_turn"). Any other request with an id
//! gets -32601 method_not_found.
//!
//! Modes via environment variables (set in Registry/AgentEntry of the test):
//!   MOCK_AGENT_PROMPT_TEXT        — response text (default "pong")
//!   MOCK_AGENT_EXIT_AFTER_PROMPTS — after N successful session/prompt
//!                                   calls, respond and exit with code 0. Needed for
//!                                   the ContextLost test: the process dies,
//!                                   the supervisor restarts it (the generation
//!                                   counter grows), and the old-generation conversation
//!                                   is marked as lost.

use std::io::{BufRead, Write};
use std::thread;
use std::time::Duration;

fn main() {
    let prompt_text =
        std::env::var("MOCK_AGENT_PROMPT_TEXT").unwrap_or_else(|_| "pong".to_string());
    let exit_after_prompts: u64 = std::env::var("MOCK_AGENT_EXIT_AFTER_PROMPTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let stream_chunks: u64 = std::env::var("MOCK_AGENT_STREAM_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let chunk_delay_ms: u64 = std::env::var("MOCK_AGENT_CHUNK_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let final_delay_ms: u64 = std::env::var("MOCK_AGENT_FINAL_DELAY_MS")
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

        // Notification (no id): session/cancel and the like — nothing to answer.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

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
                // ADDED (streaming tests): before the final response
                // agent_message_chunk chunks are emitted, each with its own
                // delay — so the test catches them one by one, not as a batch.
                if stream_chunks > 0 {
                    let session_id = msg
                        .pointer("/params/sessionId")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("sess-1")
                        .to_string();
                    for i in 0..stream_chunks {
                        if chunk_delay_ms > 0 {
                            thread::sleep(Duration::from_millis(chunk_delay_ms));
                        }
                        let chunk = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "messageId": format!("m-{i}"),
                                    "content": {
                                        "type": "text",
                                        "text": format!("chunk-{i}"),
                                    }
                                }
                            }
                        });
                        let mut line = match serde_json::to_vec(&chunk) {
                            Ok(v) => v,
                            Err(_) => return,
                        };
                        line.push(b'\n');
                        let mut out = stdout.lock();
                        let _ = out.write_all(&line);
                        let _ = out.flush();
                    }
                }
                // Respond to the prompt, then, if the limit is reached — die.
                // Delay before the final response — for the idle_chunk_timeout test.
                if final_delay_ms > 0 {
                    thread::sleep(Duration::from_millis(final_delay_ms));
                }
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

fn write_response(
    stdout: &std::io::Stdout,
    id: &serde_json::Value,
    result: Result<serde_json::Value, serde_json::Value>,
) {
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
