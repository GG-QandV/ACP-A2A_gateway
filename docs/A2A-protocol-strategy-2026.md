# A2A Protocol Strategy 2026 — SDK-first (v1.0) с fallback на Spec (pre-1.0) и ACP

**Обоснование архитектурного решения для продуктов `ACP-A2A_gateway` и `agent-connector`**

- **Статус:** принято к исполнению (для презентации в remote-репозиториях на GitHub).
- **Дата:** 2026-08-17
- **Продукты:** `ACP-A2A_gateway` (шлюз) и `agent-connector` (адаптер-коннектор).
- **Решение в одну строку:** базовый диалект A2A — **SDK (v1.0, ProtoJSON)**; поддерживается **Spec (pre-1.0)** как fallback для совместимости со старыми клиентами; **ACP** — как deep fallback для унаследованных инсталляций. ANP (W3C DID) — вне scope (отдельная ниша).

---

## 1. TL;DR

1. Экосистема агентных протоколов **консолидировалась в 2026 году**: A2A — победитель горизонтального слоя (агент↔агент), MCP — вертикального (агент↔инструменты), ACP свернут и влит в A2A.
2. A2A достиг **v1.0** (март 2026, Linux Foundation, TSC из 8 вендоров, 150+ организаций) — стабильный production-стандарт.
3. В v1.0 произошёл **breaking change**: каноническим wire стал **ProtoJSON** — PascalCase-методы (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE`-энмы, единый тип `Part`. Это и есть «SDK-диалект».
4. Старый JSON-RPC binding (`message/send`, `tasks/get`, lowercase) — **pre-1.0 wire**, официально обозначен как legacy. Это «Spec-диалект».
5. **Наши продукты** говорят/принимают: **база = SDK (v1.0)**; **fallback = Spec (pre-1.0)** — чтобы работали клиенты, которые говорят только на старом диалекте; **deep fallback = ACP** — для устаревших инсталляций.
6. **ANP (W3C DID)** — отдельная ниша для открытого веба (децентрализованная идентичность), не заменяет A2A в нашем scope.
7. Политика поддержки: **старые диалекты поддерживаем определённый период**, понимая, что миграция/рефакторинг продуктов — это всегда стоимость (см. §6).

---

## 2. Контекст: консолидация экосистемы агентных протоколов (2026)

### 2.1 A2A — победитель горизонтального слоя

A2A создан Google в апреле 2025 и передан Linux Foundation:

> "Agent2Agent (A2A) is an open protocol enabling communication and interoperability between opaque agentic applications."
> — [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)

> "A2A is an open protocol created by Google for secure agent-to-agent communication and collaboration... with growing support from more than 100 leading technology companies."
> — [Linux Foundation: Launch of the Agent2Agent Protocol Project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)

В v1.0 управление перешло к нейтральному TSC:

> "The A2A Technical Steering Committee includes representatives from AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, and ServiceNow."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "A2A has reached v1.0 with cryptographically signed Agent Cards, gRPC bindings, and production deployments inside Azure AI Foundry, Amazon Bedrock AgentCore, and Google Agent Engine."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 2.2 ACP — свёрнут и влит в A2A

IBM Research запустила ACP в марте 2025 как конкурента A2A. В августе 2025 протокол официально слился с A2A, репозиторий заархивирован, активная разработка прекращена:

> "Today, we're excited to share that ACP is officially merging with the A2A under the Linux Foundation umbrella... As part of this transition, the ACP team will be winding down active development and will begin contributing its technology and expertise directly to A2A."
> — [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)

> "The repo was archived on August 27, 2025 and set to read-only. In other words, ACP as a standalone protocol was over."
> — [DEV Community: Mapping MCP, A2A, and ACP (2026-06-28)](https://dev.to/kanywst/mapping-mcp-a2a-and-acp-telling-ai-agent-protocols-apart-in-2026-1hha)

> "The ecosystem consolidated on A2A, with ACP's REST-first instincts absorbed into the surviving standard rather than thrown away."
> — [Oshri Cohen: ACP: The Agent Protocol That Merged Into A2A (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

### 2.3 MCP и A2A — два комплементарных слоя (не конкуренты)

Стандарт явно разделяет роли:

> "MCP is for application-to-tool interaction; A2A is for agent-to-agent."
> — [Tyk.io: Agent protocols — MCP, A2A, ACP (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

Оба протокола теперь под единым управлением Linux Foundation через Agentic AI Foundation (AAIF):

> "The Linux Foundation... today announced the formation of the Agentic AI Foundation (AAIF), and founding contributions of three leading projects... Anthropic's Model Context Protocol (MCP), Block's goose, and OpenAI's AGENTS.md."
> — [Linux Foundation: Formation of AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)

### 2.4 Вывод для нашей стратегии

- Третьего диалекта в горизонте A2A **не появится** — конкуренция протоколов завершена, стандарт один.
- «Диалекты» SDK/Spec в нашем коде — это **не два протокола, а две эпохи одного A2A**: v1.0 (SDK/ProtoJSON) и pre-1.0 (Spec/старый JSON-RPC binding).
- Значит, правильная стратегия — **базироваться на v1.0 (SDK)**, а старый wire поддерживать как совместимость.

---

## 3. Почему база = A2A SDK (v1.0 / ProtoJSON)

### 3.1 v1.0 — стабильный production-стандарт

> "A2A Protocol Ships v1.0: Production-Ready Standard for Agent-to-Agent Communication... The v1.0 release emphasizes maturity rather than reinvention."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "The v1.0 release in early 2026 added four capabilities that moved A2A from prototype to production-grade."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 3.2 ProtoJSON: единый канонический wire

В v1.0 протокол стал proto-first, сериализация — ProtoJSON (ADR-001). Методы — PascalCase, совпадающие с gRPC RPC:

> "Method names are **PascalCase, matching the gRPC RPCs** — `SendMessage`, `GetTask`, `CancelTask`, etc.... `message/send` / `tasks/get` were the **pre-1.0** JSON-RPC binding."
> — [a2aproject/a2a-rs, issue #35: Wire incompatibility (ответ мейнтейнера)](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "Enums are **SCREAMING_SNAKE_CASE** — `"role": "ROLE_USER"`, `"state": "TASK_STATE_COMPLETED"`. ADR-001 calls the casing change out explicitly as a breaking change from the old lowercase form."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "The big one: `text`, `file`, and `data` are unified into a single `Part` type — no more separate `TextPart`/`FilePart`/`DataPart`, and no `kind` discriminator to carry around."
> — [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

Канонический набор методов (SDK-диалект):

| Операция | JSON-RPC метод (v1.0) | REST (v1.0) |
|---|---|---|
| Отправить сообщение | `SendMessage` | `POST /v1/message:send` |
| Стриминг | `SendStreamingMessage` | `POST /v1/message:stream` |
| Получить задачу | `GetTask` | `GET /v1/tasks/{id}` |
| Список задач | `ListTasks` | `GET /v1/tasks` |
| Отмена | `CancelTask` | `POST /v1/tasks/{id}:cancel` |
| Подписка | `SubscribeToTask` | `GET /v1/tasks/{id}:subscribe` |

> Источник: [a2a-rs-core на docs.rs](https://docs.rs/crate/a2a-rs-core/latest) (JSON-RPC at `POST /v1/rpc`, REST at `/v1/message:send`, `/v1/tasks/*`) и [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs).

### 3.3 Встроенное версионирование и совместимость

v1.0 решил проблему «какой диалект у сервера» **на уровне стандарта**:

- **Agent Card** несёт `protocolVersion` (`"0.3"`, `"1.0"` и т.д.) — клиент по нему выбирает формат (без probe-зондов).
- **`A2A-Version` header** — клиент объявляет версию, сервер отвечает `VersionNotSupportedError` при несовпадении.
- Официальные SDK включают **opt-in совместимость с pre-1.0** — ровно тот «переводчик», который мы обсуждали:

> "A v1.0 server can transparently accept v0.3 clients (and a v1.0 client can transparently talk to v0.3 servers) by opting into the compat layer with `legacyCompat: { enabled: true }`."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js)

> "Client Agents that require latest features of the protocol should be configured to request specific versions and avoid automatic fallback to older versions, to prevent silently losing functionality."
> — [A2A Protocol Specification v1.0, §3.6 Versioning](https://a2a-protocol.org/latest/specification/)

### 3.4 Официальные SDK и транспорты

Ссылочная реализация (`a2a-rs`, workspace под A2A v1) поддерживает: JSON-RPC 2.0 over HTTP, REST/HTTP+JSON, gRPC (tonic), SLIMRPC, SSE. Клиент **сам ведёт переговоры о транспорте по Agent Card**:

> "The CLI resolves the public agent card from a base URL, negotiates JSON-RPC or HTTP+JSON..."
> — [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs)

**Вывод:** SDK (v1.0) — единственный диалект, за которым стоит будущее: официальные SDK, подписанные карточки, OAuth 2.0 (PKCE, Device flow), gRPC, встроенная версия-негодиация. Это и есть наша база.

---

## 4. Почему fallback = A2A Spec (pre-1.0)

Несмотря на v1.0, **старый wire ещё жив**: им говорят
- клиенты/агенты, закреплённые на pre-1.0 binding (`message/send`, `tasks/get`, lowercase);
- Python `a2a-sdk` и другие SDK, которые ещё не мигрировали:
> "the `message/send` + lowercase-`user` expectation is the **pre-1.0** A2A wire (the older Google spec, or an SDK still on it such as the Python `a2a-sdk`)."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

Сам стандарт сохраняет обратную совместимость через **legacy-алиасы**:

> "The v1.0 SDKs... an opt-in compatibility layer for v0.3 peers" / "`message/send`-style slash aliases + lowercase enum deserialization... gated behind a feature/flag, as long as the default stays v1.0.0."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js) и [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

Поэтому **Spec (pre-1.0) остаётся в наших продуктах как fallback**: шлюз и адаптер понимают и `SendMessage`, и `message/send` — чтобы клиенты, которые говорят только на старом диалекте, продолжали работать без переделки. Это требование бизнес-совместимости, а не технической необходимости.

---

## 5. Почему deep fallback = ACP

ACP как отдельный протокол мёртв (см. §2.2), но:

- существуют **унаследованные инсталляции** BeeAI/ACP-агентов;
- миграция требует времени, и пока клиенты не перешли — они должны работать;

> "If you built on ACP, you weren't wrong... when the merge came, migrating to A2A was a port, not a rewrite, because the concepts mapped almost one to one."
> — [Oshri Cohen (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

Официальная позиция: **новые проекты должны целиться в A2A**, а ACP — только историческая совместимость:

> "New projects should use A2A; ACP is now historical context."
> — [MegaOneAI: MCP vs A2A vs ACP (2026-06-01)](https://megaoneai.com/analysis/mcp-vs-a2a-vs-acp-ai-agent-protocols/)

> "Migrate to A2A" — официальное указание IBM: [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)

**Вывод:** ACP в наших продуктах — **deep fallback** (последняя ступень), только для унаследованных клиентов. Новые подключения — на A2A (SDK или Spec).

---

## 6. Политика поддержки версий

Мы **поддерживаем старшие диалекты определённый период** (Spec pre-1.0 и ACP) параллельно с базовым SDK (v1.0), потому что:

1. Переход/рефакторинг софта и продуктов — это всегда стоимость (время, риски, тестирование).
2. Часть клиентов физически не может перейти сразу — они на замороженных средах, внутренних инсталляциях, унаследованном коде.
3. Стандарт сам гарантирует совместимость — мы лишь следуем его механизму (compat-слои, legacy-алиасы).

Порядок поддержки (строгий приоритет на входящих запросах):

| Приоритет | Диалект | Статус | Роль |
|---|---|---|---|
| 1 | **A2A SDK (v1.0, ProtoJSON)** | актуальный, развивается | **база** |
| 2 | **A2A Spec (pre-1.0)** | legacy, без новых фич | fallback |
| 3 | **ACP** | заморожен, свернут в A2A | deep fallback |
| — | **ANP (W3C DID)** | отдельная ниша | вне scope (§7) |

Политика вывода из поддержки: диалект убирается не ранее, чем через один major-цикл после подтверждения, что на нём не осталось активных подключений (измеряется по логам шлюза).

---

## 7. ANP (W3C DID) — отдельная ниша, вне нашего scope

ANP (Agent Network Protocol) — это другой слой задачи: **децентрализованная идентичность и открытая веб-инфраструктура** для агентов (DID, `did:wba`, WNS-хэндлы, discovery, E2E-мессенджинг), а не конкурирующий JSON-RPC диалект для делегирования задач.

> "ANP aims to become the HTTP of the Agentic Web era: a protocol suite for identity, naming, discovery, negotiation, secure messaging, and application-level collaboration."
> — [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)

> "ANP... based on the W3C DID standard... ensuring that any two agents can securely verify each other's identity and establish private, reliable encrypted communication channels without central authority intervention."
> — [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)

Экосистема видит A2A и ANP как **дополняющие**, а не заменяющие:

> "Together with Anthropic's Model Context Protocol (MCP), these specifications now form a coherent layered stack: MCP handles vertical tool access, A2A handles horizontal agent-to-agent delegation, and ANP extends that federation to the open web."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

**Решение:** ANP не добавляем как диалект. Если потребуется децентрализованная идентичность — это отдельный проект поверх A2A (например, `did:wba` как метод аутентификации в шлюзе), не изменение wire-слоя. Стандарты: [W3C DID Core](https://www.w3.org/TR/did-core/), [W3C DID v1.1](https://www.w3.org/TR/did-1.1/).

---

## 8. Ссылки на официальные документы

### A2A / стандарт
- [A2A Protocol Specification v1.0](https://a2a-protocol.org/latest/specification/)
- [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)
- [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)
- [ADR-001: ProtoJSON serialization](https://github.com/a2aproject/A2A/blob/main/adrs/adr-001-protojson-serialization.md)
- [Commit ae6a562: стандартизация имён операций v1.0](https://github.com/a2aproject/A2A/commit/ae6a562d5d972f2c4b184f748bb32e1fa9aa7bf2)

### SDK
- [a2aproject/a2a-rs (Rust)](https://github.com/a2aproject/a2a-rs)
- [a2aproject/a2a-js (TypeScript) — incl. legacyCompat](https://github.com/google-a2a/a2a-js)
- [a2a-rs-core на docs.rs (методы/транспорты)](https://docs.rs/crate/a2a-rs-core/latest)
- [a2a-rs, issue #18: метод-неймы JSON-RPC](https://github.com/a2aproject/a2a-rs/issues/18)
- [a2a-rs, issue #35: wire-несовместимость pre-1.0 vs v1.0](https://github.com/EmilLindfors/a2a-rs/issues/35)

### Экосистема / консолидация
- [Linux Foundation: Launch of A2A project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)
- [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)
- [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)
- [Linux Foundation: Formation of Agentic AI Foundation / AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)
- [Anthropic: Donating MCP and establishing AAIF](https://www.anthropic.com/news/donating-the-model-context-protocol-and-establishing-of-the-agentic-ai-foundation)
- [OpenAI: co-founding AAIF](https://openai.com/index/agentic-ai-foundation/)
- [Zylos Research: A2A, ACP, ANP in Production (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)
- [Tyk.io: Agent protocols guide (MCP, A2A, ACP) (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)
- [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

### ANP / DID (отдельная ниша)
- [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)
- [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)
- [did:wba Method Specification (ANP-03)](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/03-did-wba-method-design-specification.md)
- [W3C DID Core v1.0](https://www.w3.org/TR/did-core/)
- [W3C DID v1.1 (Candidate Recommendation)](https://www.w3.org/TR/did-1.1/)

---

## 9. ТЗ: коррекция/рефакторинг продуктов под стратегию

Цель: **привести wire-слой шлюза и адаптера к приоритетам SDK → Spec → ACP**, с автоматическим определением диалекта клиента.

### 9.1 Целевая схема коннекции

| Ступень | Роль | Диалект | Компоненты |
|---|---|---|---|
| **1 (база)** | основной inbound/outbound | **A2A SDK (v1.0)** | шлюз: `SendMessage`/`GetTask`/`CancelTask`; адаптер: `driver-a2a-client` (`wire_format: sdk`) |
| **2 (fallback)** | совместимость со старыми клиентами | **A2A Spec (pre-1.0)** | шлюз: `message/send`/`tasks/get`/`tasks/cancel`; адаптер: `wire_format: spec` |
| **3 (deep fallback)** | унаследованные инсталляции | **ACP** | адаптер: `driver-acp-client` + `protocol-acp-mapper` (только для известных старых агентов) |

Правила:
1. **Новые подключения по умолчанию — SDK (v1.0)**. Spec — только если определён диалект клиента как pre-1.0. ACP — только явная конфигурация legacy-агента.
2. Шлюз продолжает принимать **оба A2A-диалекта на входе** (`/rpc`): по имени метода (`SendMessage` vs `message/send`) выбирается парсер/рендерер (уже частично реализовано в `transport_http.rs:381-417`).
3. Адаптер на выходе (в сторону агентов) — база SDK; на входе (A2A-сервер, `protocol-a2a-server`) — также уметь принимать Spec (текущая база `a2a-server` понимает только SDK-методы).

### 9.2 Ключевая подзадача: диалект-зонд (короткий первичный запрос)

**Цель:** по одному короткому запросу в стиле A2A-SDK сразу понять, **на каком диалекте умеет/может коммуницировать клиент** (SDK / Spec / ACP / неизвестен).

**Принцип:** зонд должен быть **идемпотентным** — не создавать задач и не иметь побочных эффектов. Для этого используем `GetTask`/`tasks/get` с заведомо несуществующим `task_id` (случайный UUID), а не `SendMessage`/`message/send` (те создают реальную задачу).

**Алгоритм (серверный детект на входе, аналог в обоих продуктах):**

```
1. Принять первый запрос к агенту.
2. Определить диалект по имени метода:
     SendMessage | GetTask | CancelTask | ListTasks → SDK (v1.0)
     message/send | tasks/get | tasks/cancel       → Spec (pre-1.0)
     иначе                                          → ACP/иной → см. шаг 5
3. Если метод распознан — ответить в том же диалекте (парсер/рендерер по методу).
4. Дополнительно для клиентов, которые ещё не сделали ни одного вызова:
   GET /.well-known/agent.json → protocolVersion ("1.0" → SDK, "0.x" → Spec).
   Это предпочтительный канал определения (без probe).
5. Если метод не распознан ни одним диалектом → вернуть method_not_found
   с подсказкой об известных диалектах (SDK/Spec) и ссылкой на стратегию.
```

**Зонд (клиентская сторона, если Agent Card недоступен):**

```
POST /agents/:id/rpc
{ "jsonrpc": "2.0", "id": 1, "method": "GetTask",
  "params": { "name": "tasks/<uuid>" } }            # SDK-стиль
```

Интерпретация ответа:

| Ответ | Вердикт |
|---|---|
| `result` (или ошибка «task not found» без `method_not_found`) | сервер понимает **SDK** → работаем на SDK |
| `-32601` / `-32000` + `method_not_found:` | не SDK → пробуем Spec: |
|   `POST ... { "method": "tasks/get", "params": { "id": "<uuid>" } }` | |
|   ошибка «task not found» | сервер понимает **Spec** → работаем на Spec |
|   `method_not_found` и для `tasks/get` | не A2A → пробуем ACP (иной интерфейс) |
|   и ACP не распознал | явная ошибка: «диалект клиента не определён» |

Кэширование: результат детекта хранится **на эндпоинт** (один зонд на первый контакт), повторные запросы зонд не вызывают.

**DoD подзадачи:**
- [ ] зонд не создаёт задач (только `GetTask`/`tasks/get` с несуществующим id);
- [ ] детект по Agent Card (`protocolVersion`) — приоритетнее зонда;
- [ ] кэш диалекта на эндпоинт;
- [ ] приоритет SDK при неоднозначности;
- [ ] понятная ошибка с перечислением поддерживаемых диалектов, если ни один не определён.

### 9.3 Объём и границы

- **Шлюз (`ACP-A2A_gateway`):** уже принимает SDK+Spec на входе (`transport_http.rs`). Добавить: детект по Agent Card/`protocolVersion`; вывод SDK-формата для SDK-запросов (частично в ТЗ `TZ-add-adapterd-wire-format.md`).
- **Адаптер (`agent-connector`):** `driver-a2a-client` — добавить `wire_format: auto` (зонд + кэш) с приоритетом SDK; `protocol-a2a-server` — приём Spec на входе.
- **ACP** — не расширять, только сохранить существующий `driver-acp-client` для явно сконфигурированных legacy-агентов.
- **Не в scope:** ANP, DID-аутентификация, новый транспорт (gRPC) — отдельные задачи.

### P.S. MCP как альтернатива

MCP — **не альтернатива A2A**, а комплементарный слой: A2A — связь «агент↔агент», MCP — «агент↔инструменты» (§2.3). В `agent-connector` MCP уже есть как `driver-mcp` (tools → skills). Использовать MCP **вместо** A2A нельзя без потери горизонтальной делегации. Правильный паттерн: оркестратор общается по A2A, а инструменты вызывает по MCP.

> "A high-level orchestrator agent uses A2A to manage workflows and delegate to specialist agents; each specialist agent then uses MCP internally to call its own tools."
> — [Tyk.io (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

---

## 10. Резюме решения

1. **База — A2A SDK (v1.0/ProtoJSON)**: будущее стандарта, официальные SDK, production-фичи.
2. **Fallback — A2A Spec (pre-1.0)**: совместимость со старыми клиентами (Python a2a-sdk и др.).
3. **Deep fallback — ACP**: только унаследованные инсталляции, без развития.
4. **ANP/W3C DID — вне scope**: отдельная ниша открытого веба.
5. **MCP — не альтернатива, а вертикальный слой** поверх/рядом.
6. **Поддержка старых диалектов — на определённый период**, из уважения к стоимости миграции у клиентов.