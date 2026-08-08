# Итоговая структура: минимальный, надёжный, расширяемый gateway (Rust)

Синтез всех решений по треду: универсальный конвертер вместо двух бриджей,
токен как чистый allow/deny, seam-подход через `Reply<T,U>` для будущего
стриминга, `TurnLease` и паттерны модульности из `hermes-agent/gateway/`.

---

## 1. Дерево crate'ов (3 крейта — минимум без потери расширяемости)

```
gateway/
├── Cargo.toml                  # workspace
├── config.example.yaml
│
├── protocol/                   # ТИПЫ. Ничего не знает о Reply/lease/dispatch.
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── acp.rs               # SessionId, Prompt, PromptResponse, SessionUpdate
│       └── a2a.rs               # TaskId, Task, AgentCard, A2aEvent
│
├── core/                        # ЯДРО. Единственное место с реальной сложностью.
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── reply.rs              # enum Reply<T, U> — seam для стриминга (Фаза 2)
│       ├── agent.rs               # trait AcpAgent, trait A2aAgent
│       ├── convert.rs             # AcpAsA2a, A2aAsAcp — универсальный конвертер
│       ├── lease.rs               # TurnLease — сериализация promptов на сессию
│       └── stdio_agent.rs         # StdioAcpAgent — spawn + framing (Фаза 1.1)
│
└── gatewayd/                    # БИНАРНИК. Только проводка, без бизнес-логики.
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── registry.rs            # плоский token-set + agent map (id -> transport)
        ├── dispatch.rs             # token check -> registry lookup -> lease -> convert
        └── transport_tcp.rs        # newline-delimited JSON-RPC приёма соединений
```

**Почему 3, а не 5 и не 1**: `protocol` отделён, потому что типы переиспользуются
и тестируются независимо от логики. `core` отделён от `gatewayd`, потому что
ядро (trait'ы, конвертер, lease) — единственное, что стоит покрывать unit-тестами
изолированно от сети; `gatewayd` — это только I/O-обвязка, которая меняется
при добавлении транспортов, но не должна тянуть за собой пересборку тестов ядра.

---

## 2. Поток запроса (что происходит на каждый входящий байт)

```
TCP-соединение
   │
   ▼
gatewayd::transport_tcp   — читает newline-delimited JSON-RPC
   │
   ▼
gatewayd::dispatch::handle_connection
   │
   ├─▶ registry.check_token(token)?          // allow/deny, без знания об агентах
   │        │ deny → закрыть соединение, до чтения payload
   │        ▼ allow
   ├─▶ registry.lookup(agent_id)              // id -> AgentEntry{transport}
   │
   ├─▶ lease.acquire(session_id).await?       // сериализация turn'ов на сессию
   │        │ timeout → TurnLeaseTimeoutError, отказ клиенту
   │        ▼ acquired
   ├─▶ core::convert::{AcpAsA2a|A2aAsAcp|identity}.prompt(...)
   │        │
   │        └─▶ match Reply<T,U> {
   │               Complete(resp)   => отдать клиенту   (Фаза 1: единственный путь)
   │               Streaming(rx)    => unreachable!()   (Фаза 2: pump в клиента)
   │            }
   │
   └─▶ lease.release(session_id)
```

Единственная реальная логика — конвертер и lease. Всё остальное — passthrough
байтов.

---

## 3. Ключевые типы (финальная версия)

```rust
// core/src/reply.rs — seam для будущего стриминга, без изменений когда он появится
pub enum Reply<T, U> {
    Complete(T),
    Streaming(tokio::sync::mpsc::UnboundedReceiver<U>),
}

// core/src/lease.rs — надёжность: не даёт двум promptам в одну сессию сталкиваться
pub struct TurnLease {
    locks: tokio::sync::Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
}

impl TurnLease {
    pub async fn acquire(&self, session: &SessionId, timeout: Duration)
        -> Result<TurnGuard, TurnLeaseTimeoutError> { /* fail-closed */ }
}

// core/src/agent.rs — оба протокола за одним контрактом
#[async_trait]
pub trait AcpAgent: Send + Sync {
    async fn prompt(&self, s: SessionId, p: Prompt) -> Result<Reply<PromptResponse, SessionUpdate>>;
    async fn cancel(&self, s: SessionId) -> Result<()>;
}

#[async_trait]
pub trait A2aAgent: Send + Sync {
    async fn send_task(&self, t: Task) -> Result<Reply<Task, A2aEvent>>;
    async fn cancel_task(&self, id: TaskId) -> Result<Task>;
}

// gatewayd/src/registry.rs — токен НЕ знает про агентов, агент НЕ знает про токен
pub struct Registry {
    tokens: HashSet<String>,               // allow/deny на вход, и только
    agents: HashMap<String, AgentEntry>,   // id -> {transport} (protocol выводится из transport)
}
```

---

## 4. Что делает решение надёжным уже в MVP (не отложено на "потом")

| Риск                                                                 | Механизм в MVP                                                                                                                           |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Два promptа одновременно в одну ACP-сессию портят stdio-поток агента | `TurnLease` — fail-closed, `TurnLeaseTimeoutError` вместо тихого зависания                                                               |
| Невалидный токен доходит до парсинга JSON-RPC                        | Проверка токена — первая операция после accept(), до чтения payload                                                                      |
| Процесс агента умер, а gateway продолжает слать ему запросы          | `StdioAcpAgent` держит `Child` и проверяет `try_wait()` перед каждым `prompt` — если процесс мёртв, `Result::Err` вместо тихого таймаута |
| Один зависший клиент блокирует весь сервис                           | Каждое соединение — свой `tokio::spawn`, `TurnLease` блокирует только на уровне сессии, не глобально                                     |

---

## 5. Точки расширения (без изменения существующих файлов)

| Что добавляется                      | Куда                                                                                                                   | Что НЕ трогается                                             |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Стриминг                             | `core/convert.rs`: заменить `unreachable!()` на реальный маппинг; `gatewayd/dispatch.rs`: обработать `Streaming` ветку | Сигнатуры `AcpAgent`/`A2aAgent`, `Reply<T,U>`, `registry.rs` |
| WS/HTTP+SSE транспорт                | Новый файл `gatewayd/src/transport_ws.rs`, вызывающий тот же `dispatch::handle_connection`                             | `core/`, `protocol/`, `transport_tcp.rs`                     |
| TLS                                  | Обёртка вокруг `TcpListener` в `transport_tcp.rs` (rustls)                                                             | Всё выше транспортного слоя                                  |
| Rate limiting                        | Новый `gatewayd/src/rate_limit.rs`, подключается перед `registry.check_token`                                          | `dispatch.rs` логика после токена                            |
| Множественные агенты на один токен   | Просто расширение `agents:` в YAML — `Registry` уже поддерживает                                                       | Код не меняется совсем                                       |
| `HttpA2aAgent` (реальный A2A-клиент) | Новый файл `core/src/http_agent.rs`, реализует `A2aAgent`                                                              | `convert.rs` работает с любой реализацией трейта уже сейчас  |

---

## 6. Оценка (сведено с учётом всех упрощений по треду)

| Часть                                                             | Дни         |
| ----------------------------------------------------------------- | ----------- |
| `protocol` (типы, без логики)                                     | 1           |
| `core`: `Reply`, trait'ы, `TurnLease`                             | 1.5         |
| `core`: `convert.rs` (AcpAsA2a + A2aAsAcp, синхронный путь)       | 2.5         |
| `core`: `StdioAcpAgent` (spawn, framing, dead-process check)      | 1           |
| `gatewayd`: registry + dispatch + TCP-транспорт                   | 1.5         |
| Тесты (lease concurrency, token deny, оба направления конвертера) | 1.5         |
| **Итого MVP**                                                     | **~9 дней** |

Сверху (модули из §5, по мере необходимости): стриминг +3-4 дня, WS/HTTP+SSE
+1-2 дня каждый, TLS +1 день, rate limiting +0.5 дня.
