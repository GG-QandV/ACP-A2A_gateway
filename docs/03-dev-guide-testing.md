# Дев-гайд: тестирование

## Обзор по модулям

| Модуль | Что тестировать | Как (без сети/процессов или с ними) |
|---|---|---|
| `protocol` | (Де)сериализация типов ACP/A2A | Unit, без внешних зависимостей |
| `core::reply` | Match на оба варианта `Reply<T,U>` | Unit, тривиально |
| `core::lease` | Конкурентный acquire, timeout, RAII-release | Unit + `tokio::test`, без сети |
| `core::task_store` | save/load roundtrip, path traversal, atomic write | Unit + `tempfile`, без сети |
| `core::convert` | Маппинг ContentBlock↔Part, TaskState↔StopReason | Unit с mock `AcpAgent`/`A2aAgent` |
| `core::stdio_agent` | Реальный spawn, dead-process detection | Интеграционный, требует бинарник-стаб |
| `core::http_agent` | HTTP JSON-RPC клиент | Интеграционный, требует mock HTTP-сервер |
| `gatewayd::registry` | check_token, lookup | Unit, без сети |
| `gatewayd::transport_*` | End-to-end через реальный TCP/HTTP | Интеграционный, требует запущенный gatewayd |

## 1. protocol — тестирование типов

```bash
cargo test -p protocol
```

Пример теста (добавить в `protocol/src/acp.rs` или отдельный `tests/`):

```rust
#[test]
fn content_block_text_roundtrip() {
    let original = ContentBlock::Text { text: "hello".into() };
    let json = serde_json::to_string(&original).unwrap();
    let restored: ContentBlock = serde_json::from_str(&json).unwrap();
    assert!(matches!(restored, ContentBlock::Text { text } if text == "hello"));
}
```

Приоритет: проверить, что реальные JSON-примеры из спеки ACP/A2A
парсятся без ошибок (взять примеры из agentclientprotocol.com/protocol/v1/schema
и a2a-protocol.org/latest/specification — вставить как fixture-строки).

## 2. core::lease — конкурентность

```bash
cargo test -p core --lib lease
```

Уже есть тест-паттерн в `test_turn_lease.py` у Hermes (см. предыдущий
разбор) — переносим идею на Rust:

```rust
#[tokio::test]
async fn concurrent_acquire_serializes() {
    let lease = TurnLease::default();
    let session = SessionId("s1".into());

    let guard1 = lease.acquire(&session, Duration::from_secs(1)).await.unwrap();
    let start = std::time::Instant::now();

    // Второй acquire на ТУ ЖЕ сессию должен ждать, а не пройти сразу
    let lease2 = &lease;
    let session2 = session.clone();
    let handle = tokio::spawn(async move {
        lease2.acquire(&session2, Duration::from_secs(5)).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(guard1); // освобождаем первый лиз

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
    assert!(result.is_err()); // TurnLeaseTimeoutError, не панику и не тихий проход
}
```

## 3. core::task_store — уже покрыт в исходном файле

Тесты `save_then_load_roundtrip`, `load_missing_task_errors_cleanly`,
`path_traversal_id_is_sanitized` уже написаны в `core/src/task_store.rs`
(см. `#[cfg(test)] mod tests`). Запуск:

```bash
cargo test -p core --lib task_store
```

## 4. core::convert — тестирование с mock-агентами

Ключевая идея: не поднимать реальный процесс/HTTP для проверки маппинга —
реализовать `AcpAgent`/`A2aAgent` заглушками прямо в тесте.

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

    let task = Task { /* с status.message заполненным */ };
    let result = adapter.send_task(task).await.unwrap();

    match result {
        Reply::Complete(t) => assert_eq!(t.status.state, TaskState::Completed),
        _ => panic!("expected Complete"),
    }
}
```

Обязательные кейсы для покрытия (соответствуют местам, где маппинг
не биективен — см. архитектурный гайд):

- `TaskState::InputRequired` → `task_state_to_stop_reason` возвращает `Err`, не паникует.
- `ContentBlock::ResourceLink` → `Part::Text` (деградация, не потеря).
- Конкурентные `send_task` на одну сессию → второй блокируется `TurnLease`, не гонка.

## 5. core::stdio_agent — интеграционный тест с реальным процессом

Нужен минимальный ACP-совместимый скрипт-стаб (не полноценный агент),
чтобы не зависеть от внешних бинарников в тестах:

```bash
# tests/fixtures/echo_acp_agent.py — минимальный ACP-эхо для тестов
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
    // "false" завершается немедленно с кодом 1
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = agent.initialize(InitializeRequest { /* ... */ }).await;
    assert!(result.is_err()); // не должно висеть до 60s таймаута
}
```

## 6. core::http_agent — mock HTTP-сервер

Используем `wiremock` (добавить как dev-dependency) вместо реального
внешнего A2A-агента:

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
    // проверить Reply::Complete с ожидаемым Task
}
```

## 7. gatewayd::registry — тесты уже написаны

`valid_token_passes`, `invalid_token_denied`, `agent_lookup_by_id` — см.
`#[cfg(test)] mod tests` в `gatewayd/src/registry.rs`.

```bash
cargo test -p gatewayd --lib registry
```

## 8. End-to-end (весь gatewayd целиком)

Единственный уровень, где нужен реально запущенный процесс gatewayd
плюс реальный (или стаб) агент.

```bash
# Терминал 1: поднять gateway с тестовым конфигом
cargo run -p gatewayd -- tests/fixtures/e2e_config.yaml

# Терминал 2: направление 1 (ACP-клиент -> ACP-агент, passthrough)
echo '{"token":"t-test","agent_id":"echo-agent"}' | nc localhost 8347
# затем вручную отправить {"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}

# Терминал 2: направление 4 (A2A-клиент -> ACP-агент)
curl -X POST http://localhost:8348/agents/echo-agent/rpc \
  -H "Authorization: Bearer t-test" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}]}}}'
```

Автоматизация (Rust-интеграционный тест в `gatewayd/tests/e2e.rs`):
спавнить `gatewayd` как subprocess внутри теста, дождаться готовности
порта, выполнить запросы через `reqwest`/`tokio::net::TcpStream`,
проверить ответы, убить процесс в конце теста (`Drop`).

## 9. Критерии приёмки (сведены к проверяемому)

Соответствуют §9 исходного ТЗ и §6 SPEC v2, адаптированы под то, что
реализовано:

1. `cargo test --workspace` — все тесты зелёные.
2. `cargo clippy --workspace -- -D warnings` — без предупреждений.
3. Неверный/отсутствующий токен → отказ на TCP и HTTP до чтения payload
   (тест `invalid_token_denied` + интеграционный на `transport_tcp`).
4. Два конкурентных `session/prompt`/`send_task` на одну сессию не
   происходят одновременно (тест `concurrent_acquire_serializes`).
5. Мёртвый процесс агента → явная ошибка, не таймаут 60s
   (тест `dead_process_returns_error_not_hang`).
6. `get_task` после `send_task` возвращает сохранённый результат
   (тест `save_then_load_roundtrip` + end-to-end).
