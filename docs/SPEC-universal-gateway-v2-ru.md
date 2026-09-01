# SPEC v2: универсальный ACP/A2A gateway (упрощённая архитектура)


> **Язык:** русский · [English version](./SPEC-universal-gateway-v2.md)
Пересмотр по замечанию: убрана искусственная граница "Gateway" vs
"Bridge" как двух разных бинарников. Один процесс, одна точка входа,
логика ветвится по совпадению протоколов клиента и агента.

---

## 1. Идея в одну фразу

Токен пускает или не пускает **к гейтвею вообще** — не более того.
Дальше: протокол клиента == протокол агента → голый проксинг без
преобразования. Протоколы разные → универсальный конвертер (один и
тот же код для обоих направлений A2A→ACP и ACP→A2A, за счёт
адаптеров одного trait'а через другой).

```
                 ┌───────────────────────────────────────────┐
                 │              acp-a2a-gateway               │
  ACP-клиент ───▶│  1. token check (пускать/не пускать)       │
  A2A-клиент ───▶│  2. lookup target agent (id → протокол)    │
                 │  3. client.proto == agent.proto ?          │
                 │       да  → passthrough (raw proxy)        │──▶ ACP-агент
                 │       нет → universal converter            │──▶ A2A-агент
                 └───────────────────────────────────────────┘
```

Никакого отдельного `acp-gateway` бинарника и отдельного `bridge`
бинарника — один сервис, один конфиг, один порт (или набор портов на
транспорт, см. §5).

---

## 2. Token

- Токен — это **allow/deny на вход в гейтвей**, точка. Не привязан к
  конкретным агентам, не порождает ACL-матрицу "токен → список агентов".
- Список валидных токенов — плоский набор в конфиге (или один shared
  secret на первую итерацию).
- Выбор целевого агента — отдельный параметр запроса (id агента в
  `session/new`/`initialize` для ACP, путь `/agents/{id}/rpc` для A2A),
  никак не завязан на токен.
- Невалидный/отсутствующий токен → разрыв соединения на транспортном
  уровне, до разбора ACP/A2A payload.

```rust
fn check_token(token: &str, valid: &HashSet<String>) -> bool {
    valid.contains(token)
}
```

Это всё, что нужно от auth-слоя для MVP.

---

## 3. Passthrough (одинаковые протоколы, без преобразования)

Когда клиент и целевой агент говорят на одном протоколе (ACP↔ACP или
A2A↔A2A), гейтвей **не парсит семантику** JSON-RPC методов — только
перекладывает кадры:

- ACP↔ACP: newline-delimited JSON-RPC уже одинаков что у клиента
  (сетевой транспорт), что у агента (stdio) — просто readline-цикл в
  обе стороны, без структур `InitializeRequest`/`Prompt`/и т.п.
- A2A↔A2A: HTTP-запрос форвардится как reverse-proxy (включая SSE-стрим
  как есть).

**Важная оговорка**: "без преобразования" — это про отсутствие
*семантического* маппинга методов. Транспортная разница всё равно есть
(TCP-кадр клиента ≠ stdin/stdout байты агента) и её нужно перегонять —
но это тривиальный byte/line copy, а не парсинг типов.

```rust
trait Passthrough {
    async fn pump(self, client: impl AsyncRead + AsyncWrite,
                         agent:  impl AsyncRead + AsyncWrite) -> Result<()>;
}
```

Один generic pump и для ACP↔ACP, и для A2A↔A2A (разница только в
транспортных обвязках: TCP-сокет vs stdio-канал vs HTTP-стрим).

---

## 4. Universal converter (разные протоколы)

Когда протоколы не совпадают — один и тот же конвертер работает в обе
стороны за счёт двух трейтов и адаптеров друг через друга (как в §6
исходного ТЗ, оставляем как ядро — это единственное место, где
реально нужна сложность):

```rust
#[async_trait]
trait AcpAgent {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId>;
    async fn prompt(&self, session: SessionId, prompt: Prompt) -> Result<PromptResponse>;
    async fn cancel(&self, session: SessionId) -> Result<()>;
    fn subscribe(&self, session: SessionId) -> UnboundedReceiver<SessionUpdate>;
}

#[async_trait]
trait A2aAgent {
    async fn card(&self) -> Result<AgentCard>;
    async fn send_task(&self, task: Task) -> Result<Task>;
    async fn get_task(&self, id: TaskId) -> Result<Task>;
    async fn cancel_task(&self, id: TaskId) -> Result<Task>;
    async fn stream(&self, id: TaskId) -> UnboundedReceiver<A2aEvent>;
}

// один универсальный адаптер в каждую сторону — это и есть "конвертер"
struct AcpAsA2a<T: AcpAgent>(T);   // A2A-клиент видит ACP-агента как A2aAgent
impl<T: AcpAgent> A2aAgent for AcpAsA2a<T> { /* маппинг из §5.1 ТЗ */ }

struct A2aAsAcp<T: A2aAgent>(T);   // ACP-клиент видит A2A-агента как AcpAgent
impl<T: A2aAgent> AcpAgent for A2aAsAcp<T> { /* маппинг из §5.2 ТЗ */ }
```

Гейтвею не важно, кто снаружи (ACP или A2A клиент) и кто внутри (ACP
или A2A агент) — если протоколы не совпали, он берёт нужный адаптер и
отдаёт клиенту интерфейс его родного протокола. Это и есть "универсальный
преобразователь а2а/асп/а2а" одним куском кода, а не двумя разными
бинарниками-бриджами.

---

## 5. Crate layout

```
gateway/
├── proto-acp/       # типы + framing ACP (JSON-RPC over stdio/TCP)
├── proto-a2a/       # типы + framing A2A (JSON-RPC over HTTP/SSE)
├── core/            # trait AcpAgent, trait A2aAgent, AcpAsA2a, A2aAsAcp
├── passthrough/     # generic pump byte-copy для одинаковых протоколов
└── gatewayd/         # bin: token check → agent lookup → passthrough | converter
```

Реестр агентов — плоский конфиг `id → {protocol, transport, endpoint}`,
без токен-специфичных списков доступа:

```yaml
listen: "0.0.0.0:8347"
tokens: ["t-dev-1", "t-dev-2"]
agents:
  claurst-main: { protocol: acp, transport: stdio, command: ["claurst", "acp"] }
  ops-agent:    { protocol: a2a, transport: http,  url: "https://ops.internal/a2a" }
```

---

## 6. Критерии приёмки

1. ACP-клиент → `claurst-main` (ACP-агент): passthrough, PONG проходит без изменений полей.
2. A2A-клиент → `ops-agent` (A2A-агент): reverse-proxy, включая SSE-стрим.
3. A2A-клиент → `claurst-main` (ACP-агент): через `AcpAsA2a`, `task/send` доходит как `session/prompt`, ответ маппится обратно.
4. ACP-клиент → `ops-agent` (A2A-агент): через `A2aAsAcp`, `session/prompt` доходит как `task/send`.
5. Неверный токен → разрыв на любом из четырёх сценариев выше, до чтения payload.
6. `cargo check --workspace` + clippy без warnings.

---

## 7. Оценка

| Часть                                                    | Дни     |
| ---------------------------------------------------------- | ------- |
| `proto-acp` + `proto-a2a` (типы, framing)                   | 1.5     |
| Token-check + agent registry + dispatch (passthrough vs converter) | 1     |
| Passthrough pump (ACP↔ACP, A2A↔A2A)                         | 1.5     |
| Универсальный конвертер: `AcpAsA2a` + `A2aAsAcp` (маппинги §5.1/5.2 исходного ТЗ) | 4–5 |
| Тесты: 4 сценария из §6 приёмки + мок-агенты                | 2       |
| **Итого MVP (один бинарник, оба направления, TCP+HTTP)**    | **10–11** |

Сверху (не входит в MVP, но было в исходном ТЗ): WS-транспорт, TLS,
rate limiting, reconnect+backoff, метрики/health, `session/load`,
permission-policy `allow/deny/ask` — ориентировочно **+6–8 дней**.

**Итого весь объём: ≈16–19 человеко-дней** — меньше прошлой оценки за
счёт того, что один универсальный конвертер закрывает оба направления
сразу, а не два отдельных бридж-бинарника.
