# Стратегія A2A-протоколу 2026 — SDK-first (v1.0) з fallback на Spec (pre-1.0) та ACP

**Обґрунтування архітектурного рішення для продуктів `ACP-A2A_gateway` та `agent-connector`**

- **Статус:** прийнято до виконання (оформлено для презентації в remote-репозиторіях на GitHub).
- **Дата:** 2026-08-17
- **Продукти:** `ACP-A2A_gateway` (шлюз) та `agent-connector` (адаптер-конектор).
- **Рішення в один рядок:** базовий діалект A2A — **SDK (v1.0, ProtoJSON)**; **Spec (pre-1.0)** підтримується як fallback для сумісності зі старими клієнтами; **ACP** — як deep fallback для успадкованих інсталяцій. ANP (W3C DID) — поза scope (окрема ніша).

---

## 1. TL;DR

1. Екосистема агентних протоколів **консолідувалася у 2026 році**: A2A — переможець горизонтального шару (агент↔агент), MCP — вертикального (агент↔інструменти), ACP згорнуто та влито в A2A.
2. A2A досяг **v1.0** (березень 2026, Linux Foundation, TSC з 8 вендорів, 150+ організацій) — стабільний production-стандарт.
3. У v1.0 стався **breaking change**: канонічним wire став **ProtoJSON** — PascalCase-методи (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE`-енуми, єдиний тип `Part`. Це «SDK-діалект».
4. Старий JSON-RPC binding (`message/send`, `tasks/get`, lowercase) — **pre-1.0 wire**, офіційно позначений як legacy. Це «Spec-діалект».
5. **Наші продукти** говорять/приймають: **база = SDK (v1.0)**; **fallback = Spec (pre-1.0)** — щоб працювали клієнти, які говорять лише старим діалектом; **deep fallback = ACP** — для успадкованих інсталяцій.
6. **ANP (W3C DID)** — окрема ніша для відкритого вебу (децентралізована ідентичність), не замінює A2A в нашому scope.
7. Політика підтримки: **старі діалекти підтримуємо визначений період**, розуміючи, що міграція/рефакторинг продуктів завжди має вартість (див. §6).

---

## 2. Контекст: консолідація екосистеми агентних протоколів (2026)

### 2.1 A2A — переможець горизонтального шару

A2A створено Google у квітні 2025 і передано Linux Foundation:

> "Agent2Agent (A2A) is an open protocol enabling communication and interoperability between opaque agentic applications."
> — [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)

> "A2A is an open protocol created by Google for secure agent-to-agent communication and collaboration... with growing support from more than 100 leading technology companies."
> — [Linux Foundation: Launch of the Agent2Agent Protocol Project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)

Управління на v1.0 перейшло до нейтрального TSC:

> "The A2A Technical Steering Committee includes representatives from AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, and ServiceNow."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "A2A has reached v1.0 with cryptographically signed Agent Cards, gRPC bindings, and production deployments inside Azure AI Foundry, Amazon Bedrock AgentCore, and Google Agent Engine."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 2.2 ACP — згорнуто та влито в A2A

IBM Research запустила ACP у березні 2025 як конкурента A2A. У серпні 2025 протокол офіційно злився з A2A, репозиторій заархівовано, активну розробку припинено:

> "Today, we're excited to share that ACP is officially merging with the A2A under the Linux Foundation umbrella... As part of this transition, the ACP team will be winding down active development and will begin contributing its technology and expertise directly to A2A."
> — [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)

> "The repo was archived on August 27, 2025 and set to read-only. In other words, ACP as a standalone protocol was over."
> — [DEV Community: Mapping MCP, A2A, and ACP (2026-06-28)](https://dev.to/kanywst/mapping-mcp-a2a-and-acp-telling-ai-agent-protocols-apart-in-2026-1hha)

> "The ecosystem consolidated on A2A, with ACP's REST-first instincts absorbed into the surviving standard rather than thrown away."
> — [Oshri Cohen: ACP: The Agent Protocol That Merged Into A2A (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

### 2.3 MCP і A2A — два комплементарні шари (не конкуренти)

Стандарт чітко розділяє ролі:

> "MCP is for application-to-tool interaction; A2A is for agent-to-agent."
> — [Tyk.io: Agent protocols — MCP, A2A, ACP (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

Обидва протоколи тепер під єдиним управлінням Linux Foundation через Agentic AI Foundation (AAIF):

> "The Linux Foundation... today announced the formation of the Agentic AI Foundation (AAIF), and founding contributions of three leading projects... Anthropic's Model Context Protocol (MCP), Block's goose, and OpenAI's AGENTS.md."
> — [Linux Foundation: Formation of AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)

### 2.4 Висновок для нашої стратегії

- Третього діалекту в горизонті A2A **не з'явиться** — конкуренція протоколів завершена, стандарт один.
- «Діалекти» SDK/Spec у нашому коді — це **не два протоколи, а дві епохи одного A2A**: v1.0 (SDK/ProtoJSON) і pre-1.0 (Spec/старий JSON-RPC binding).
- Отже, правильна стратегія — **базуватися на v1.0 (SDK)**, а старий wire підтримувати як сумісність.

---

## 3. Чому база = A2A SDK (v1.0 / ProtoJSON)

### 3.1 v1.0 — стабільний production-стандарт

> "A2A Protocol Ships v1.0: Production-Ready Standard for Agent-to-Agent Communication... The v1.0 release emphasizes maturity rather than reinvention."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "The v1.0 release in early 2026 added four capabilities that moved A2A from prototype to production-grade."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 3.2 ProtoJSON: єдиний канонічний wire

На v1.0 протокол став proto-first, серіалізація — ProtoJSON (ADR-001). Методи — PascalCase, що збігаються з gRPC RPC:

> "Method names are **PascalCase, matching the gRPC RPCs** — `SendMessage`, `GetTask`, `CancelTask`, etc.... `message/send` / `tasks/get` were the **pre-1.0** JSON-RPC binding."
> — [a2aproject/a2a-rs, issue #35: Wire incompatibility (відповідь мейнтейнера)](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "Enums are **SCREAMING_SNAKE_CASE** — `"role": "ROLE_USER"`, `"state": "TASK_STATE_COMPLETED"`. ADR-001 calls the casing change out explicitly as a breaking change from the old lowercase form."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "The big one: `text`, `file`, and `data` are unified into a single `Part` type — no more separate `TextPart`/`FilePart`/`DataPart`, and no `kind` discriminator to carry around."
> — [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

Канонічний набір методів (SDK-діалект):

| Операція | JSON-RPC метод (v1.0) | REST (v1.0) |
|---|---|---|
| Надіслати повідомлення | `SendMessage` | `POST /v1/message:send` |
| Стрімінг | `SendStreamingMessage` | `POST /v1/message:stream` |
| Отримати задачу | `GetTask` | `GET /v1/tasks/{id}` |
| Список задач | `ListTasks` | `GET /v1/tasks` |
| Скасування | `CancelTask` | `POST /v1/tasks/{id}:cancel` |
| Підписка | `SubscribeToTask` | `GET /v1/tasks/{id}:subscribe` |

> Джерело: [a2a-rs-core на docs.rs](https://docs.rs/crate/a2a-rs-core/latest) (JSON-RPC at `POST /v1/rpc`, REST at `/v1/message:send`, `/v1/tasks/*`) та [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs).

### 3.3 Вбудоване версіювання та сумісність

v1.0 вирішив проблему «який діалект у сервера» **на рівні стандарту**:

- **Agent Card** несе `protocolVersion` (`"0.3"`, `"1.0"` тощо) — клієнт обирає формат за ним (без probe).
- **`A2A-Version` header** — клієнт оголошує версію, сервер відповідає `VersionNotSupportedError` при розбіжності.
- Офіційні SDK містять **opt-in сумісність із pre-1.0** — саме той «перекладач», який ми обговорювали:

> "A v1.0 server can transparently accept v0.3 clients (and a v1.0 client can transparently talk to v0.3 servers) by opting into the compat layer with `legacyCompat: { enabled: true }`."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js)

> "Client Agents that require latest features of the protocol should be configured to request specific versions and avoid automatic fallback to older versions, to prevent silently losing functionality."
> — [A2A Protocol Specification v1.0, §3.6 Versioning](https://a2a-protocol.org/latest/specification/)

### 3.4 Офіційні SDK та транспорти

Референсна реалізація (`a2a-rs`, workspace під A2A v1) підтримує: JSON-RPC 2.0 over HTTP, REST/HTTP+JSON, gRPC (tonic), SLIMRPC, SSE. Клієнт сам веде переговори про транспорт за Agent Card:

> "The CLI resolves the public agent card from a base URL, negotiates JSON-RPC or HTTP+JSON..."
> — [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs)

**Висновок:** SDK (v1.0) — єдиний діалект, за яким майбутнє: офіційні SDK, підписані картки, OAuth 2.0 (PKCE, Device flow), gRPC, вбудована версійна негодація. Це наша база.

---

## 4. Чому fallback = A2A Spec (pre-1.0)

Попри v1.0, **старий wire ще живий**: ним говорять
- клієнти/агенти, закріплені на pre-1.0 binding (`message/send`, `tasks/get`, lowercase);
- Python `a2a-sdk` та інші SDK, які ще не мігрували:

> "the `message/send` + lowercase-`user` expectation is the **pre-1.0** A2A wire (the older Google spec, or an SDK still on it such as the Python `a2a-sdk`)."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

Сам стандарт зберігає зворотну сумісність через **legacy-аліаси**:

> "The v1.0 SDKs... an opt-in compatibility layer for v0.3 peers" / "`message/send`-style slash aliases + lowercase enum deserialization... gated behind a feature/flag, as long as the default stays v1.0.0."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js) та [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

Тому **Spec (pre-1.0) залишається в наших продуктах як fallback**: шлюз і адаптер розуміють і `SendMessage`, і `message/send` — щоб клієнти, які говорять лише старим діалектом, продовжували працювати без переробки. Це вимога бізнес-сумісності, а не технічної необхідності.

---

## 5. Чому deep fallback = ACP

ACP як окремий протокол мертвий (див. §2.2), але:

- існують **успадковані інсталяції** BeeAI/ACP-агентів;
- міграція потребує часу, і доки клієнти не перейшли — вони мають працювати;

> "If you built on ACP, you weren't wrong... when the merge came, migrating to A2A was a port, not a rewrite, because the concepts mapped almost one to one."
> — [Oshri Cohen (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

Офіційна позиція: **нові проєкти мають цілитися в A2A**, ACP — лише історична сумісність:

> "New projects should use A2A; ACP is now historical context."
> — [MegaOneAI: MCP vs A2A vs ACP (2026-06-01)](https://megaoneai.com/analysis/mcp-vs-a2a-vs-acp-ai-agent-protocols/)

> "Migrate to A2A" — офіційна вказівка IBM: [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)

**Висновок:** ACP у наших продуктах — **deep fallback** (остання сходинка), лише для успадкованих клієнтів. Нові підключення — на A2A (SDK або Spec).

---

## 6. Політика підтримки версій

Ми **підтримуємо старші діалекти визначений період** (Spec pre-1.0 та ACP) паралельно з базовим SDK (v1.0), тому що:

1. Перехід/рефакторинг софту та продуктів — це завжди вартість (час, ризики, тестування).
2. Частина клієнтів фізично не може перейти одразу — вони на заморожених середовищах, внутрішніх інсталяціях, успадкованому коді.
3. Стандарт сам гарантує сумісність — ми лише слідуємо його механізму (compat-шари, legacy-аліаси).

Порядок підтримки (строгий пріоритет на вхідних запитах):

| Пріоритет | Діалект | Статус | Роль |
|---|---|---|---|
| 1 | **A2A SDK (v1.0, ProtoJSON)** | актуальний, розвивається | **база** |
| 2 | **A2A Spec (pre-1.0)** | legacy, без нових фіч | fallback |
| 3 | **ACP** | заморожено, згорнуто в A2A | deep fallback |
| — | **ANP (W3C DID)** | окрема ніша | поза scope (§7) |

Політика виведення з підтримки: діалект прибирається не раніше, ніж через один major-цикл після підтвердження (за логами шлюзу), що на ньому не залишилося активних підключень.

---

## 7. ANP (W3C DID) — окрема ніша, поза нашим scope

ANP (Agent Network Protocol) — це інший шар задачі: **децентралізована ідентичність і відкрита веб-інфраструктура** для агентів (DID, `did:wba`, WNS-хендли, discovery, E2E-меседжинг), а не конкуруючий JSON-RPC діалект для делегування задач.

> "ANP aims to become the HTTP of the Agentic Web era: a protocol suite for identity, naming, discovery, negotiation, secure messaging, and application-level collaboration."
> — [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)

> "ANP... based on the W3C DID standard... ensuring that any two agents can securely verify each other's identity and establish private, reliable encrypted communication channels without central authority intervention."
> — [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)

Екосистема бачить A2A та ANP як **доповнювальні**, а не замінні:

> "Together with Anthropic's Model Context Protocol (MCP), these specifications now form a coherent layered stack: MCP handles vertical tool access, A2A handles horizontal agent-to-agent delegation, and ANP extends that federation to the open web."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

**Рішення:** ANP не додаємо як діалект. Якщо знадобиться децентралізована ідентичність — це окремий проєкт поверх A2A (наприклад, `did:wba` як метод аутентифікації у шлюзі), не зміна wire-шару. Стандарти: [W3C DID Core](https://www.w3.org/TR/did-core/), [W3C DID v1.1](https://www.w3.org/TR/did-1.1/).

---

## 8. Посилання на офіційні документи

### A2A / стандарт
- [A2A Protocol Specification v1.0](https://a2a-protocol.org/latest/specification/)
- [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)
- [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)
- [ADR-001: ProtoJSON serialization](https://github.com/a2aproject/A2A/blob/main/adrs/adr-001-protojson-serialization.md)
- [Commit ae6a562: стандартизація імен операцій v1.0](https://github.com/a2aproject/A2A/commit/ae6a562d5d972f2c4b184f748bb32e1fa9aa7bf2)

### SDK
- [a2aproject/a2a-rs (Rust)](https://github.com/a2aproject/a2a-rs)
- [a2aproject/a2a-js (TypeScript) — incl. legacyCompat](https://github.com/google-a2a/a2a-js)
- [a2a-rs-core на docs.rs (методи/транспорти)](https://docs.rs/crate/a2a-rs-core/latest)
- [a2a-rs, issue #18: method-нейми JSON-RPC](https://github.com/a2aproject/a2a-rs/issues/18)
- [a2a-rs, issue #35: wire-несумісність pre-1.0 vs v1.0](https://github.com/EmilLindfors/a2a-rs/issues/35)

### Екосистема / консолідація
- [Linux Foundation: Launch of A2A project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)
- [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)
- [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)
- [Linux Foundation: Formation of Agentic AI Foundation / AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)
- [Anthropic: Donating MCP and establishing AAIF](https://www.anthropic.com/news/donating-the-model-context-protocol-and-establishing-of-the-agentic-ai-foundation)
- [OpenAI: co-founding AAIF](https://openai.com/index/agentic-ai-foundation/)
- [Zylos Research: A2A, ACP, ANP in Production (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)
- [Tyk.io: Agent protocols guide (MCP, A2A, ACP) (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)
- [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

### ANP / DID (окрема ніша)
- [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)
- [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)
- [did:wba Method Specification (ANP-03)](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/03-did-wba-method-design-specification.md)
- [W3C DID Core v1.0](https://www.w3.org/TR/did-core/)
- [W3C DID v1.1 (Candidate Recommendation)](https://www.w3.org/TR/did-1.1/)

---

## 9. ТЗ: корекція/рефакторинг продуктів під стратегію

Мета: **привести wire-шар шлюзу та адаптера до пріоритетів SDK → Spec → ACP**, з автоматичним визначенням діалекту клієнта.

> Детальне уніфіковане ТЗ — у `docs/TZ-a2a-dialects-gateway-adapter.md` (Розділ 1 — шлюз, Розділ 2 — адаптер, Розділ 3 — спільний діалект-зонд).

### 9.1 Цільова схема конекції

| Сходинка | Роль | Діалект | Компоненти |
|---|---|---|---|
| **1 (база)** | основний inbound/outbound | **A2A SDK (v1.0)** | шлюз: `SendMessage`/`GetTask`/`CancelTask`; адаптер: `driver-a2a-client` (`wire_format: sdk`) |
| **2 (fallback)** | сумісність зі старими клієнтами | **A2A Spec (pre-1.0)** | шлюз: `message/send`/`tasks/get`/`tasks/cancel`; адаптер: `wire_format: spec` |
| **3 (deep fallback)** | успадковані інсталяції | **ACP** | адаптер: `driver-acp-client` + `protocol-acp-mapper` (лише для відомих старих агентів) |

Правила:
1. **Нові підключення за замовчуванням — SDK (v1.0)**. Spec — лише якщо діалект клієнта визначено як pre-1.0. ACP — лише явна конфігурація legacy-агента.
2. Шлюз продовжує приймати **обидва A2A-діалекти на вході** (`/rpc`): за ім'ям методу (`SendMessage` vs `message/send`) обирається парсер/рендерер (вже частково реалізовано в `transport_http.rs:381-417`).
3. Адаптер на виході (у бік агентів) — база SDK; на вході (A2A-сервер, `protocol-a2a-server`) — також приймати Spec (поточна база `a2a-server` розуміє лише SDK-методи).

### 9.2 Ключова підзадача: діалект-зонд (короткий первинний запит)

**Мета:** за одним коротким запитом у стилі A2A-SDK одразу зрозуміти, **на якому діалекті вміє/може комунікувати клієнт** (SDK / Spec / ACP / невідомий).

**Принцип:** зонд має бути **ідемпотентним** — не створювати задач і не мати побічних ефектів. Використовуємо `GetTask`/`tasks/get` із заздалегідь неіснуючим `task_id` (випадковий UUID), а не `SendMessage`/`message/send` (ті створюють реальну задачу).

**Алгоритм (серверний детект на вході, аналог в обох продуктах):**

```
1. Прийняти перший запит до агента.
2. Визначити діалект за ім'ям методу:
     SendMessage | GetTask | CancelTask | ListTasks → SDK (v1.0)
     message/send | tasks/get | tasks/cancel       → Spec (pre-1.0)
     інакше                                         → ACP/інший → див. крок 5
3. Якщо метод розпізнано — відповісти тим самим діалектом (парсер/рендерер за методом).
4. Додатково для клієнтів, які ще не зробили жодного виклику:
   GET /.well-known/agent.json → protocolVersion ("1.0" → SDK, "0.x" → Spec).
   Це пріоритетний канал визначення (без probe).
5. Якщо метод не розпізнано жодним діалектом → повернути method_not_found
   з підказкою про відомі діалекти (SDK/Spec) та посиланням на стратегію.
```

**Зонд (клієнтська сторона, якщо Agent Card недоступний):**

```
POST /agents/:id/rpc
{ "jsonrpc": "2.0", "id": 1, "method": "GetTask",
  "params": { "name": "tasks/<uuid>" } }            # SDK-стиль
```

Інтерпретація відповіді:

| Відповідь | Вердикт |
|---|---|
| `result` (або помилка «task not found» без `method_not_found`) | сервер розуміє **SDK** → працюємо на SDK |
| `-32601` / `-32000` + `method_not_found:` | не SDK → пробуємо Spec: |
|   `POST ... { "method": "tasks/get", "params": { "id": "<uuid>" } }` | |
|   помилка «task not found» | сервер розуміє **Spec** → працюємо на Spec |
|   `method_not_found` і для `tasks/get` | не A2A → пробуємо ACP (інший інтерфейс) |
|   і ACP не розпізнав | явна помилка: «діалект клієнта не визначено» |

Кешування: результат детекту зберігається **на ендпоінт** (один зонд на перший контакт), повторні запити зонд не викликають.

**DoD підзадачі:**
- [ ] зонд не створює задач (тільки `GetTask`/`tasks/get` з неіснуючим id);
- [ ] детект за Agent Card (`protocolVersion`) — пріоритетніший за зонд;
- [ ] кеш діалекту на ендпоінт;
- [ ] пріоритет SDK при неоднозначності;
- [ ] зрозуміла помилка з переліком підтримуваних діалектів, якщо жоден не визначено.

### 9.3 Обсяг і межі

- **Шлюз (`ACP-A2A_gateway`):** вже приймає SDK+Spec на вході (`transport_http.rs`). Додати: детект за Agent Card/`protocolVersion`; вивід SDK-формату для SDK-запитів.
- **Адаптер (`agent-connector`):** `driver-a2a-client` — додати `wire_format: auto` (зонд + кеш) з пріоритетом SDK; `protocol-a2a-server` — прийом Spec на вході.
- **ACP** — не розширювати, лише зберегти наявний `driver-acp-client` для явно сконфігурованих legacy-агентів.
- **Поза scope:** ANP, DID-аутентифікація, новий транспорт (gRPC) — окремі задачі.

### P.S. MCP як альтернатива

MCP — **не альтернатива A2A**, а комплементарний шар: A2A — зв'язок «агент↔агент», MCP — «агент↔інструменти» (§2.3). У `agent-connector` MCP вже є як `driver-mcp` (tools → skills). Використовувати MCP **замість** A2A не можна без втрати горизонтального делегування. Правильний патерн: оркестратор спілкується по A2A, а інструменти викликає по MCP.

> "A high-level orchestrator agent uses A2A to manage workflows and delegate to specialist agents; each specialist agent then uses MCP internally to call its own tools."
> — [Tyk.io (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

---

## 10. Резюме рішення

1. **База — A2A SDK (v1.0/ProtoJSON)**: майбутнє стандарту, офіційні SDK, production-фічі.
2. **Fallback — A2A Spec (pre-1.0)**: сумісність зі старими клієнтами (Python a2a-sdk та інші).
3. **Deep fallback — ACP**: лише успадковані інсталяції, без розвитку.
4. **ANP/W3C DID — поза scope**: окрема ніша відкритого вебу.
5. **MCP — не альтернатива, а вертикальний шар** поруч/зверху.
6. **Підтримка старих діалектів — на визначений період**, з поваги до вартості міграції у клієнтів.