# ТЗ: Универсальный ACP Gateway + конвертер A2A ↔ ACP


> **Язык:** русский · [English version](./SPEC-acp-a2a-gateway-legacy.md)
**Статус:** черновик
**Версия:** 0.1
**Дата:** 2026-08-07

---

## 1. Цель

Создать универсальный сетевой слой поверх протокола ACP (Agent Client
Protocol), который решает две задачи:

1. **ACP Gateway** — любой ACP-клиент (Zed, Neovim, VS Code, JetBrains и
   т.п.) может подключиться к любому ACP-агенту по сети, а не только к
   локальному stdio-процессу. Доступ ограничен токеном.
2. **A2A ↔ ACP конвертер** — двунаправленный мост между экосистемами
   Google A2A (Agent-to-Agent, HTTP JSON-RPC) и ACP (JSON-RPC над stdio):
   - A2A-клиент → ACP-агент;
   - ACP-клиент → A2A-агент.

Оба компонента — **агностичны**: не привязаны к конкретному агенту.
Любой клиент с поддержкой ACP может коннектиться к любому агенту с ACP;
любой агент A2A общается с любым агентом ACP и наоборот.

---

## 2. Термины и стандарты

| Термин                 | Определение                                                                                                                                   |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| **ACP**                | Agent Client Protocol (https://agentclientprotocol.com). JSON-RPC 2.0, newline-delimited, поверх stdio. Клиент спавнит агента как subprocess. |
| **A2A**                | Google Agent-to-Agent protocol. JSON-RPC 2.0 over HTTP(S), discovery через `/.well-known/agent.json` (AgentCard), стриминг через SSE.         |
| **Gateway**            | Сервер, принимающий сетевые ACP-соединения и проксирующий их к целевым агентам.                                                               |
| **Конвертер (bridge)** | Компонент, транслирующий запросы/события ACP ↔ A2A.                                                                                           |
| **Агент**              | Любая ACP- или A2A-сторона, исполняющая промпты.                                                                                              |
| **Клиент**             | Любая сторона, отправляющая промпты агенту.                                                                                                   |
| **Токен**              | Секрет (Bearer), ограничивающий доступ к gateway/конвертеру.                                                                                  |

### 2.1 Методы ACP (релевантные)

| Метод                        | Направление   | Описание                                  |
| ---------------------------- | ------------- | ----------------------------------------- |
| `initialize`                 | C → A         | Capability negotiation, версия протокола  |
| `authenticate`               | C → A         | Auth (у claurst — no-op, локальные креды) |
| `session/new`                | C → A         | Создать сессию (cwd, MCP-ростер)          |
| `session/load`               | C → A         | Загрузить существующую сессию             |
| `session/prompt`             | C → A         | Выполнить turn; стримит `session/update`  |
| `session/cancel`             | C → A (notif) | Отменить текущий turn                     |
| `session/update`             | A → C         | Стрим текст/tool deltas                   |
| `session/request_permission` | A → C         | Запрос одобрения tool-вызова              |
| `session/update_step`        | A → C         | Обновление шага (опционально)             |

### 2.2 Методы A2A (релевантные)

| Метод                  | Направление | Описание                                    |
| ---------------------- | ----------- | ------------------------------------------- |
| `agent/getCard`        | C → A       | Метаданные агента (deprecated, → discovery) |
| `message/send`         | C → A       | Отправить сообщение, получить полный ответ  |
| `message/reply`        | C → A       | Ответ в существующей сессии                 |
| `message/stream`       | C → A       | То же, но SSE-стриминг                      |
| `task/send`            | C → A       | Создать задачу; сервер воркает асинхронно   |
| `task/get`             | C → A       | Получить состояние/артефакты задачи         |
| `task/cancel`          | C → A       | Отменить задачу                             |
| `task/resubscribe`     | C → A       | Подписка на обновления задачи               |
| `task/status-update`   | A → C       | Push-уведомление о статусе                  |
| `task/artifact-update` | A → C       | Push-уведомление об артефактах              |
| `message/update`       | A → C       | Push-обновление сообщения                   |

A2A типы: `AgentCard`, `Message` (parts: text/file/data/audio/video/image),
`Task` (id, sessionId, status: submitted/working/input-required/completed/
canceled/failed/unknown, artifacts), `Part`.

### 2.3 Семантический маппинг (целевой)

| ACP                               | A2A                                           |
| --------------------------------- | --------------------------------------------- |
| сессия (`session_id`)             | `sessionId`                                   |
| turn (`session/prompt`)           | `task` / `message`                            |
| текст (`TextContent`)             | `TextPart`                                    |
| tool-call (`ToolCallContent`)     | `DataPart` (структурированный JSON)           |
| tool-result (`ToolResultContent`) | `DataPart` (JSON)                             |
| изображение (`ImageContent`)      | `ImagePart`                                   |
| аудио (`AudioContent`)            | `AudioPart`                                   |
| `session/update`                  | SSE `message/update` / `task/artifact-update` |
| `session/request_permission`      | `input-required` (запрос на вход)             |
| `session/cancel`                  | `task/cancel`                                 |

---

## 3. Архитектура

```
                ┌────────────────────────────────────────────────┐
                │                ACP GATEWAY (ядро)              │
                │                                                │
  ACP-клиент ──▶│  Transport A: TCP/WS/HTTP-SSE + stdio-обёртка  │──▶ ACP-агент A (stdio)
  (Zed и др.)   │  Auth: Bearer-токен                            │──▶ ACP-агент B (stdio)
                │  Реестр агентов: токен → агент                 │──▶ ACP-агент C (сетевой)
                │                                                │
                └────────────────────────────────────────────────┘

                ┌────────────────────────────────────────────────┐
                │             A2A ↔ ACP КОНВЕРТЕР                │
                │                                                │
  A2A-клиент ──▶│  Endpoint A2A: HTTP JSON-RPC + SSE             │──▶ ACP-агент (spawn/сеть)
                │  Auth: Bearer-токен                            │
                │                                                │
  ACP-клиент ──▶│  Endpoint ACP: stdio/TCP + обёртка             │──▶ A2A-агент (HTTP)
                └────────────────────────────────────────────────┘
```

Оба компонента разделяют **реестр агентов** — единый конфиг, описывающий
доступные агенты и их транспорт. Ядро конвертера построено на интерфейсах
`AcpAgent` и `A2aAgent`, что даёт универсальность (см. §6).

---

## 4. Компонент 1: ACP Gateway

### 4.1 Назначение

Позволить ACP-клиенту работать с агентом, который не является локальным
stdio-процессом клиента (удалённый хост, общий пул агентов, другой
пользователь), с авторизацией по токену.

### 4.2 Транспорты входящих соединений

Реализуются **все** перечисленные, за выбор отвечает конфиг:

1. **TCP** — newline-delimited JSON-RPC 2.0 (чистый ACP-канон поверх
   сокета). Порт + TLS-опция.
2. **WebSocket** — тот же JSON-RPC, кадры WS. Удобен для браузерных
   клиентов и прокси.
3. **HTTP + SSE** — JSON-RPC-запросы через POST `/rpc`, ответы и события
   через SSE. Удобен для ACP-клиентов, поддерживающих HTTP-транспорт.
4. **stdio-обёртка** (`acp-gateway` как CLI) — клиент запускает
   `acp-gateway --token <T>` вместо агента; обёртка принимает ACP по
   stdio и проксирует его на gateway по одному из сетевых транспортов.
   Это ключ к совместимости: **любой** редактор, умеющий спавнить ACP-
   агента, получает сетевой доступ без изменений.

### 4.3 Аутентификация

- Bearer-токен в заголовке/первом сообщении рукопожатия.
- Для TCP/stdio-обёртки: токен передаётся аргументом
  (`--token`) или env (`ACP_GATEWAY_TOKEN`).
- Для WS/HTTP: `Authorization: Bearer <token>`.
- `authenticate` из ACP-канона остаётся no-op: фактическая проверка —
  на уровне транспорта при подключении.
- Токен в реестре привязан к **одному или нескольким** целевым агентам.

### 4.4 Реестр агентов

Конфиг-файл (JSON/YAML), секция `agents`:

```yaml
gateway:
  bind: 0.0.0.0:8347
  tls: false
  tokens:
    - token: "t-zed-prod"
      agents: ["claurst-main", "claude-cc"]
    - token: "t-ops"
      agents: ["any"]
agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/srv/workspaces/main"
    env: { OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}" }
  claude-cc:
    transport: stdio
    command: ["claude", "acp"]
  remote-gw:
    transport: acp-gateway
    url: "wss://gw2.example:8347"
    token: "t-upstream"
```

Правила роутинга: при подключении с токеном клиент выбирает целевого
агента (явно в `initialize`/первом сообщении, либо дефолтный из токена).

### 4.5 Проксирование

Gateway транслирует запросы/ответы **1:1** (без семантического
преобразования):

- `initialize`, `authenticate`, `session/new`, `session/load`,
  `session/prompt`, `session/cancel` — проксируются целевому агенту.
- `session/update`, `session/request_permission`,
  `session/update_step` — ретранслируются обратно клиенту.
- Маппинг `session_id`: gateway ведёт таблицу `внешний_id ↔
  внутренний_id`, т.к. разные агенты могут генерировать коллизии.
- Таймауты: подключение, idle, turn. Настраиваются per-agent.

### 4.6 Управление жизненным циклом агента

- **stdio-агенты**: lazy-spawn при первой сессии, kill при завершении
  последней сессии (или keep-alive по конфигу). Одна сессия → один
  процесс; сессия закрепляется за процессом.
- **сетевые агенты**: пул постоянных соединений с переподключением.
- **cancel/oversubscribe**: при `session/cancel` клиенту сразу
  возвращается подтверждение, отмена пробрасывается агенту.

### 4.7 Безопасность

- TLS для всех сетевых транспортов (опция).
- Rate limiting на уровне токена.
- Запрет на выполнение агента с произвольным `cwd` из сети, если не
  разрешено конфигом.
- Логирование подключений и токенов — без самих токенов.

---

## 5. Компонент 2: A2A ↔ ACP конвертер

### 5.1 Направление A2A-клиент → ACP-агент

Конвертер поднимает HTTP-эндпоинт A2A (`/.well-known/agent.json` +
`/rpc`), для входящих вызовов спавнит/подключается к ACP-агенту.

**Маппинг запросов:**

| A2A                      | ACP                              | Детали                                                   |
| ------------------------ | -------------------------------- | -------------------------------------------------------- |
| `agent/getCard`          | `initialize`                     | Card строится из capabilities агента                     |
| `message/send`           | `session/new` + `session/prompt` | Каждое сообщение — новый turn в (созданной ранее) сессии |
| `message/reply`          | `session/prompt`                 | Та же ACP-сессия                                         |
| `task/send`              | `session/new` + `session/prompt` | A2A `task.id` ↔ ACP `session_id`                         |
| `task/get`               | транскрипт сессии                | Агрегировать `session/update` события                    |
| `task/cancel`            | `session/cancel`                 | Notif, ответ сразу                                       |
| `task/resubscribe` / SSE | `session/update`                 | Перенаправление стрима                                   |

**Маппинг контента:**

| A2A Part          | ACP ContentBlock                                                                  |
| ----------------- | --------------------------------------------------------------------------------- |
| `TextPart`        | `TextContent`                                                                     |
| `DataPart` (JSON) | `ToolCallContent` / `ToolResultContent` (по схеме, если агент отдаёт tool-вызовы) |
| `ImagePart`       | `ImageContent`                                                                    |
| `AudioPart`       | `AudioContent`                                                                    |
| A2A `Artifact`    | агрегированный текст сессии                                                       |

**Пермишены:** ACP `session/request_permission` конвертер переводит в
A2A статус `input-required` (если A2A-клиент поддерживает input) либо
применяет политику из конфига (`allow`/`deny`/`ask`).

### 5.2 Направление ACP-клиент → A2A-агент

Конвертер принимает ACP-соединение (stdio-обёртка или сетевой транспорт,
как в §4.2) и ходит к A2A-агенту по HTTP.

**Маппинг запросов:**

| ACP                          | A2A                                           | Детали                                                                              |
| ---------------------------- | --------------------------------------------- | ----------------------------------------------------------------------------------- |
| `initialize`                 | `agent/getCard`                               | Capabilities из карточки агента                                                     |
| `session/new`                | —                                             | Создаётся внутренняя сессия; A2A `sessionId` генерируется лениво при первом промпте |
| `session/prompt`             | `task/send`                                   | `session_id` ↔ A2A `sessionId`                                                      |
| `session/cancel`             | `task/cancel`                                 | —                                                                                   |
| `session/update`             | SSE `message/update` / `task/artifact-update` | Стрим от агента                                                                     |
| `session/request_permission` | —                                             | Не поддерживается A2A напрямую; A2A-агент сам решает                                |

**Маппинг контента:** обратный к §5.1.

### 5.3 Сессии

- Таблица `acp_session_id ↔ a2a_session_id ↔ task_id`.
- Один A2A `task` = один ACP turn (возможно несколько `session/update`
  сообщений в рамках одного turn).
- При `session/new` с новым `cwd` — новая внутренняя сессия, старые
  задачи не наследуют контекст (в v1).

### 5.4 Стриминг

- A2A → ACP: `message/stream`/`task/resubscribe` (SSE) читаются
  построчно, каждое `task/artifact-update`/`message/update`
  транслируется в `session/update`.
- ACP → A2A: `session/update` агрегируется в артефакты; если A2A-клиент
  запросил stream — события пушатся по SSE.
- Backpressure: ограничение очереди событий на соединение.

### 5.5 Ошибки и edge cases

- ACP `session/load` не поддерживается A2A-агентом → честный
  `method_not_found`.
- A2A `input-required` (нет permission на стороне агента) → конвертер
  отвечает ошибкой или ждёт ввода по политике.
- Таймаут A2A-агента → `session/update` с ошибкой + корректный
  `stop_reason`.
- Идемпотентность `task/send` по `id`.

---

## 6. Универсальность (требование к интерфейсам)

Ядро конвертера и gateway не должно знать о конкретном агенте. Для этого
вводятся два trait-интерфейса:

```rust
trait AcpAgent {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId>;
    async fn prompt(&self, session: SessionId, prompt: Prompt) -> Result<PromptResponse>;
    async fn cancel(&self, session: SessionId) -> Result<()>;
    fn events(&self) -> UnboundedReceiver<SessionUpdate>;
}

trait A2aAgent {
    async fn card(&self) -> Result<AgentCard>;
    async fn send_message(&self, msg: Message) -> Result<Message>;
    async fn send_task(&self, task: Task) -> Result<Task>;
    async fn get_task(&self, id: TaskId) -> Result<Task>;
    async fn cancel_task(&self, id: TaskId) -> Result<Task>;
    async fn stream(&self, id: TaskId) -> UnboundedReceiver<A2aEvent>;
}
```

Конкретные реализации:

- `StdioAcpAgent` (spawn `claurst acp` / `claude acp` и т.п.);
- `GatewayAcpAgent` (клиент удалённого ACP-gateway);
- `HttpA2aAgent` (клиент удалённого A2A-сервера);
- конвертеры реализуют один trait через другой (адаптеры),
  что автоматически даёт «любой ↔ любой».

---

## 7. Нефункциональные требования

1. **Надёжность:** переподключение к агенту с экспоненциальным backoff;
   корректная передача состояния после реконнекта; отсутствие потери
   событий в пределах bounded-очереди.
2. **Производительность:** стриминг в реальном времени (задержка
   события < 200 мс на локальной сети); минимальный оверхед
   проксирования (допустим ~1 мс на сообщение).
3. **Безопасность:** токены не логируются; TLS по умолчанию для
   сетевых транспортов; rate limiting; валидация id (path traversal).
4. **Наблюдаемость:** structured logs (tracing), метрики соединений
   (кол-во активных сессий, ошибок, латентность), health-endpoint.
5. **Конфигурируемость:** всё через конфиг-файл + env (`{env:VAR}`
   подстановка, как в settings.json claurst).
6. **Тестируемость:** мок-агенты обоих протоколов; E2E-тесты
   «реальный ACP-клиент → gateway → реальный claurst».

---

## 8. Этапы реализации

1. **Этап 0 — dotenv в claurst** (предусловие): загрузка `.env` из
   `~/.claurst/.env`/`$CLAURST_ENV_FILE` при старте, чтобы
   `{env:...}` резолвился во всех режимах.
2. **Этап 1 — ACP Gateway MVP:** TCP-транспорт + токен + реестр +
   проксирование 1:1 к одному stdio-агенту (`claurst acp`).
   Критерий: Zed/`acp_e2e.py` работает через `acp-gateway` с токеном.
3. **Этап 2 — transports:** stdio-обёртка, WS, HTTP+SSE; TLS; rate limit.
4. **Этап 3 — A2A ↔ ACP конвертер:** направление A2A→ACP на
   реализациях `StdioAcpAgent`; тест через A2A-тест-клиент.
5. **Этап 4 — обратное направление** ACP→A2A на `HttpA2aAgent`.
6. **Этап 5 — универсальность и стабильность:** интерфейсы §6,
   reconnection, метрики, health, полировка edge cases.

---

## 9. Критерии приёмки

1. `acp_e2e.py` (или Zed) через gateway с токеном получает `PONG` от
   `claurst acp`.
2. Два разных ACP-агента в реестре, выбор по токену.
3. A2A-клиент (`curl`/Python) шлёт `task/send` → получает текст от
   `claurst acp`; `task/get` возвращает артефакты; `task/cancel` стопает.
4. ACP-клиент шлёт `session/prompt` → конвертер создаёт `task/send` к
   мок-A2A-агенту; `session/update` стримится.
5. Неверный/отсутствующий токен → отказ на уровне транспорта.
6. Стриминг: задержка первого события < 200 мс; нет потери событий
   при bounded-очереди.
7. Все crate'ы проходят `cargo check --workspace` и clippy без warnings.

---

## 10. Открытые вопросы

1. Transport по умолчанию для gateway (TCP vs WS vs HTTP+SSE) — канон
   ACP не определяет сетевой слой; нужно зафиксировать в спеке.
2. Маппинг tool-call ACP → A2A: A2A не имеет нативного tool-call
   контракта. Использовать `DataPart` с JSON-схемой — нужно утвердить
   схему.
3. `session/request_permission` в A2A-направлении: политика по умолчанию
   (`allow`/`deny`/`ask`), и как передать «input» обратно A2A-агенту.
4. Схема реестра агентов (YAML vs JSON, где хранить секреты).
5. Нужен ли отдельный crate `claurst-acp-gateway` или расширить
   `agent-acp`.

---

## 11. Связанные материалы

- ACP-сервер claurst: `crates/acp/` (реализация), `crates/agent-acp/`
  (headless-бинарник).
- ACP-спека: https://agentclientprotocol.com
- A2A-спека: https://google.github.io/A2A/
- Remote Control bridge (не путать): `crates/bridge/` — это мост к
  claude.ai web UI, а не ACP-gateway.
