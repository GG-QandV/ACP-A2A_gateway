# SPEC: acp-gateway (минимальная версия ТЗ v0.1)

Базируется на `tz-acp-a2a-gateway.md`. Цель — сократить ТЗ до
реализуемого за одну итерацию MVP на Rust, без потери критериев приёмки
из §9 исходного документа.

---

## 0. Что сознательно вырезано из ТЗ для MVP

| Пункт ТЗ                                    | Решение для MVP                                  |
| -------------------------------------------- | ------------------------------------------------- |
| 3 транспорта (TCP/WS/HTTP+SSE)                | Только **TCP**. Остальное — v1.1.                 |
| Multi-agent реестр, роутинг по токену         | 1 статический токен → 1 агент. Реестр — потом.    |
| Обратное направление ACP→A2A (bridge #2)      | Не входит в MVP, отдельный milestone.             |
| TLS, rate limiting                            | Заглушки/TODO, не блокируют приёмку.               |
| Reconnect с backoff, health-endpoint, метрики | Отложено в НФТ v1.1.                               |
| `session/load`, permission policy (allow/deny/ask) | Заглушка: всегда `allow`.                     |
| session_id remapping (несколько сессий/процесс) | Не нужен: 1 сессия = 1 процесс на MVP.          |

Это даёт два независимых, отдельно сдаваемых куска: **(A) ACP Gateway
MVP** и **(B) A2A→ACP bridge MVP** — вместо одного большого релиза.

---

## 1. Crate layout

```
acp-gateway/
├── Cargo.toml                # workspace
├── crates/
│   ├── acp-proto/            # JSON-RPC 2.0 типы ACP, (de)serialize
│   ├── acp-core/             # trait AcpAgent + доменные типы
│   ├── acp-stdio-agent/      # StdioAcpAgent: spawn + framing по stdio
│   ├── acp-gateway/          # bin: TCP-сервер, token-auth, proxy loop
│   └── acp-a2a-bridge/       # bin: HTTP(A2A) endpoint -> AcpAgent
└── config.example.yaml
```

## 2. Ключевой trait (единственный, нужный для MVP)

```rust
#[async_trait::async_trait]
pub trait AcpAgent: Send + Sync {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId>;
    async fn prompt(&self, session: SessionId, prompt: Prompt) -> Result<PromptResponse>;
    async fn cancel(&self, session: SessionId) -> Result<()>;
    fn subscribe(&self, session: SessionId) -> tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>;
}
```

`A2aAgent` trait из §6 исходного ТЗ откладывается до второго
направления bridge — для MVP он не нужен.

## 3. Конфиг (минимум)

```yaml
gateway:
  bind: "0.0.0.0:8347"
agent:
  command: ["claurst", "acp"]
  cwd: "/srv/workspace"
token: "t-static-dev"
```

Без секции `agents:` (мультиагентный реестр) и без `tokens:` списка —
один токен, один агент.

## 4. Транспорт

Только TCP, newline-delimited JSON-RPC 2.0 (канон ACP как есть, без
адаптации под WS/SSE). Хендшейк: первая строка от клиента —
`{"token": "<T>"}`; при несовпадении — закрыть соединение с кодом ошибки,
до этого момента ACP-сообщения не принимаются и не форвардятся агенту.

## 5. Проксирование

1:1 форвардинг между TCP-сокетом и stdin/stdout дочернего процесса
(`StdioAcpAgent`), без ремаппинга `session_id` (не нужен при 1
сессии/процесс). Lazy-spawn при первом `session/new`, kill при закрытии
соединения.

## 6. Критерии приёмки MVP (сведены к проверяемому минимуму из §9 ТЗ)

1. `acp_e2e.py` через `acp-gateway --config config.yaml` получает PONG от `claurst acp`.
2. Неверный/отсутствующий токен → отказ на уровне транспорта.
3. `cargo check --workspace` и `clippy` без warnings.

(Пункты 2, 4, 6 из §9 — multi-agent, обратный bridge, стриминг-латентность — вне MVP, идут в v1.1/v2.)

## 7. Оценка трудозатрат

| Этап                                              | Дни     |
| -------------------------------------------------- | ------- |
| Workspace skeleton + `acp-proto` (JSON-RPC типы)    | 1       |
| `AcpAgent` trait + `StdioAcpAgent` (spawn + framing) | 1.5     |
| TCP-транспорт + token-auth + proxy loop             | 1.5     |
| Тесты: mock-agent unit + e2e с реальным `claurst acp` | 1     |
| **Итого: ACP Gateway MVP**                          | **5**   |
| A2A HTTP endpoint (`/rpc`, `/.well-known/agent.json`), только A2A→ACP | 3–4 |
| **Итого: Gateway MVP + однонаправленный bridge**    | **8–9** |

Полный объём исходного ТЗ (оба направления bridge, 3 транспорта, TLS,
rate limiting, reconnect+backoff, метрики/health, multi-agent реестр,
тесты edge-cases из §5.5) — ориентировочно ещё **+10–15 дней** сверху.

**Итого весь ТЗ целиком: ≈ 18–24 человеко-дня (3.5–5 недель одного Rust-разработчика уровня middle+/senior).**
