# Dev guide: testing

## Module overview

| Module | What to test | How (without/with network and processes) |
|---|---|---|
| `protocol` | (De)serialization of ACP/A2A types | Unit, no external dependencies |
| `core::reply` | Match on both `Reply<T,U>` variants | Unit, trivial |
| `core::lease` | Concurrent acquire, timeout, RAII release | Unit + `tokio::test`, no network |
| `core::task_store` | save/load roundtrip, path traversal, atomic write | Unit + `tempfile`, no network |
| `core::convert` | Mapping ContentBlock↔Part, TaskState↔StopReason | Unit with mock `AcpAgent`/`A2aAgent` |
| `core::stdio_agent` | Real spawn, dead-process detection | Integration, requires a stub binary |
| `core::http_agent` | HTTP JSON-RPC client | Integration, requires a mock HTTP server |
| `gatewayd::registry` | check_token, lookup | Unit, no network |
| `gatewayd::transport_*` | End-to-end over real TCP/HTTP | Integration, requires a running gatewayd |

## 1. protocol — type testing

```bash
cargo test -p protocol
```

Example test (add to `protocol/src/acp.rs` or a separate `tests/`):

```rust
#[test]
fn content_block_text_roundtrip() {
    let original = ContentBlock::Text { text: "hello".into() };
    let json = serde_json::to_string(&original).unwrap();
    let restored: ContentBlock = serde_json::from_str(&json).unwrap();
    assert!(matches!(restored, ContentBlock::Text { text } if text == "hello"));
}
```

Priority: check that real JSON examples from the ACP/A2A spec
parse without errors (take examples from agentclientprotocol.com/protocol/v1/schema
and a2a-protocol.org/latest/specification — paste them as fixture strings).

## 2. core::lease — concurrency

```bash
cargo test -p core --lib lease
```

There is already a test pattern in Hermes' `test_turn_lease.py` (see the previous
review) — porting the idea to Rust:

```rust
#[tokio::test]
async fn concurrent_acquire_serializes() {
    let lease = TurnLease::default();
    let session = SessionId("s1".into());

    let guard1 = lease.acquire(&session, Duration::from_secs(1)).await.unwrap();
    let start = std::time::Instant::now();

    // The second acquire on the SAME session must wait, not pass immediately
    let lease2 = &lease;
    let session2 = session.clone();
    let handle = tokio::spawn(async move {
        lease2.acquire(&session2, Duration::from_secs(5)).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(guard1); // release the first lease

    let guard2 = handle.await.unwrap().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(100));
    drop(guard2);
}

#[tokio::test]
async fn acquire_timeout_is_fail_closed() {
    let lease = TurnLease::default();
    let session = SessionId("s1".into());
    let _guard1 = lease.acquire(&session, Duration::from_secs(10)).await.unwrap();

    let result = lease.acquire(&session, Duration::from_millis(50)).await;
    assert!(result.is_err()); // TurnLeaseTimeoutError, not a panic and not a silent pass
}
```

## 3. core::task_store — already covered in the source file

The tests `save_then_load_roundtrip`, `load_missing_task_errors_cleanly`,
`path_traversal_id_is_sanitized` are already written in `core/src/task_store.rs`
(see `#[cfg(test)] mod tests`). Run:

```bash
cargo test -p core --lib task_store
```

## 4. core::convert — testing with mock agents

The key idea: don't spin up a real process/HTTP just to check the mapping —
implement `AcpAgent`/`A2aAgent` with stubs right in the test.

```rust
struct MockAcpAgent {
    fixed_response: PromptResponse,
}

#[async_trait]
impl AcpAgent for MockAcpAgent {
    async fn initialize(&self, _: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        Ok(InitializeResponse { /* ... */ })
    }
    async fn new_session(&self, _: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
        Ok(NewSessionResponse { session_id: SessionId("mock-session".into()) })
    }
    async fn prompt(&self, _: PromptRequest) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        Ok(Reply::Complete(self.fixed_response.clone()))
    }
    async fn cancel(&self, _: SessionId) -> anyhow::Result<()> { Ok(()) }
}

#[tokio::test]
async fn acp_as_a2a_maps_end_turn_to_completed() {
    let mock = MockAcpAgent { fixed_response: PromptResponse { stop_reason: StopReason::EndTurn } };
    let adapter = AcpAsA2a::new(mock, "/tmp".into(), tempfile::tempdir().unwrap().path(), Duration::from_secs(5));

    let task = Task { /* with status.message populated */ };
    let result = adapter.send_task(task).await.unwrap();

    match result {
        Reply::Complete(t) => assert_eq!(t.status.state, TaskState::Completed),
        _ => panic!("expected Complete"),
    }
}
```

Required coverage cases (they correspond to the places where the mapping
is not bijective — see the architecture guide):

- `TaskState::InputRequired` → `task_state_to_stop_reason` returns `Err`, does not panic.
- `ContentBlock::ResourceLink` → `Part::Text` (degradation, not loss).
- Concurrent `send_task` calls on one session → the second is blocked by `TurnLease`, no race.

## 5. core::stdio_agent — integration test with a real process

A minimal ACP-compatible stub script is needed (not a full agent),
so the tests don't depend on external binaries:

```bash
# tests/fixtures/echo_acp_agent.py — minimal ACP echo for tests
#!/usr/bin/env python3
import sys, json
for line in sys.stdin:
    req = json.loads(line)
    if req["method"] == "initialize":
        resp = {"jsonrpc": "2.0", "id": req["id"], "result": {"protocolVersion": "1"}}
    elif req["method"] == "session/prompt":
        resp = {"jsonrpc": "2.0", "id": req["id"], "result": {"stopReason": "end_turn"}}
    else:
        resp = {"jsonrpc": "2.0", "id": req["id"], "result": {}}
    print(json.dumps(resp), flush=True)
```

```rust
#[tokio::test]
async fn stdio_agent_survives_roundtrip() {
    let agent = StdioAcpAgent::spawn(
        &["python3".into(), "tests/fixtures/echo_acp_agent.py".into()],
        &None,
        &HashMap::new(),
    ).await.unwrap();

    let resp = agent.initialize(InitializeRequest { /* ... */ }).await.unwrap();
    assert_eq!(resp.protocol_version, "1");
}

#[tokio::test]
async fn dead_process_returns_error_not_hang() {
    let agent = StdioAcpAgent::spawn(&["false".into()], &None, &HashMap::new()).await.unwrap();
    // "false" exits immediately with code 1
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = agent.initialize(InitializeRequest { /* ... */ }).await;
    assert!(result.is_err()); // must not hang until the 60s timeout
}
```

## 6. core::http_agent — mock HTTP server

Use `wiremock` (add as a dev-dependency) instead of a real
external A2A agent:

```toml
[dev-dependencies]
wiremock = "0.6"
```

```rust
#[tokio::test]
async fn http_agent_send_task_parses_response() {
    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": "req-1",
            "result": { "id": "task-1", "contextId": "ctx-1",
                        "status": { "state": "completed" } }
        })))
        .mount(&mock_server)
        .await;

    let agent = HttpA2aAgent::new(mock_server.uri(), None);
    let task = Task { /* ... */ };
    let result = agent.send_task(task).await.unwrap();
    // check Reply::Complete with the expected Task
}
```

## 7. gatewayd::registry — tests already written

`valid_token_passes`, `invalid_token_denied`, `agent_lookup_by_id` — see
`#[cfg(test)] mod tests` in `gatewayd/src/registry.rs`.

```bash
cargo test -p gatewayd --lib registry
```

## 8. End-to-end (the whole gatewayd)

The only level that requires an actually running gatewayd process
plus a real (or stub) agent.

```bash
# Terminal 1: start the gateway with a test config
cargo run -p gatewayd -- tests/fixtures/e2e_config.yaml

# Terminal 2: direction 1 (ACP client -> ACP agent, passthrough)
echo '{"token":"t-test","agent_id":"echo-agent"}' | nc localhost 8347
# then manually send {"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}

# Terminal 2: direction 4 (A2A client -> ACP agent)
curl -X POST http://localhost:8348/agents/echo-agent/rpc \
  -H "Authorization: Bearer t-test" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}]}}}'
```

Automation (a Rust integration test in `gatewayd/tests/e2e.rs`):
spawn `gatewayd` as a subprocess inside the test, wait for the port to be
ready, run requests via `reqwest`/`tokio::net::TcpStream`,
check the responses, kill the process at the end of the test (`Drop`).

## 9. Acceptance criteria (reduced to what is checkable)

Match §9 of the original spec and §6 of SPEC v2, adapted to what is
implemented:

1. `cargo test --workspace` — all tests green.
2. `cargo clippy --workspace -- -D warnings` — no warnings.
3. Invalid/missing token → refusal on TCP and HTTP before reading the payload
   (test `invalid_token_denied` + an integration test on `transport_tcp`).
4. Two concurrent `session/prompt`/`send_task` calls on one session do not
   happen simultaneously (test `concurrent_acquire_serializes`).
5. Dead agent process → explicit error, not a 60s timeout
   (test `dead_process_returns_error_not_hang`).
6. `get_task` after `send_task` returns the saved result
   (test `save_then_load_roundtrip` + end-to-end).
