# ТЗ: Диалекты A2A (SDK/Spec) в шлюзе и адаптере + общий диалект-зонд

- **Статус:** черновик (на утверждение владельца). Код не меняется.
- **Дата:** 2026-08-17
- **Продукты:** `ACP-A2A_gateway` (шлюз), `agent-connector` (адаптер-коннектор).
- **Объединяет:** `ACP-A2A_gateway/docs/TZ-add-adapterd-wire-format.md` (Раздел 1),
  `agent-connector/docs/design/TZ-driver-a2a-wire-format.md` (Раздел 2),
  и диалект-зонд из `A2A-protocol-strategy-2026.md` §9.2 (Раздел 3).
- **Цель:** базовый диалект A2A — **SDK (v1.0/ProtoJSON)**, fallback — **Spec (pre-1.0)**,
  работают оба продукта; клиент, говорящий на любом из них, корректно определяется
  общим запросом-зондом.

---

# Раздел 1. Шлюз `ACP-A2A_gateway`: приём SDK wire-формата (adapterd)

## 1.1 Контекст

`driver-a2a-client` (в `agent-connector`) написан под wire-формат **JSON-RPC слоя
SDK `a2a-rs`** (метод `SendMessage`, proto-сериализация полей, обёртка `{task: ...}`).
Шлюз сейчас отвечает только в своём семантическом формате (`message/send`,
плоский Task, lowercase). Чтобы `adapterd` ↔ шлюз работали «из коробки», шлюзу
нужно принимать/отдавать SDK-формат **параллельно** со своим (по выбору клиента).

## 1.2 Что добавить

### 1.2.1 Вход: принять метод `SendMessage` (camelCase) на том же `/rpc`

Сейчас `dispatch_a2a_method` матчит `"message/send"`, `"tasks/get"`,
`"tasks/cancel"`. Добавить алиасы SDK-имён:

| SDK-метод (a2a-rs) | Аналог шлюза   |
| ------------------ | -------------- |
| `SendMessage`      | `message/send` |
| `GetTask`          | `tasks/get`    |
| `CancelTask`       | `tasks/cancel` |

Плюс — возможно — `ListTasks`, `SubscribeToTask` (если нужно для совместимости;
в MVP — только первые три, зеркально текущему).

**Источник имён:** `a2a/src/jsonrpc.rs:138-148` (SDK, `methods`).

### 1.2.2 Вход: десериализовать параметры `SendMessage` в proto-формате

SDK-клиент шлёт `message` в proto-виде:

```json
{ "message": { "role": "ROLE_USER", "parts": [ {"text": "..."} ] } }
```

Шлюз сейчас ожидает `role: "user"`, part `{"kind":"text",...}`. Нужна нормализация
**на входе**: распознать оба варианта и свести к внутреннему `protocol::a2a::Message`:

- `role`: `ROLE_USER`/`user` → `User`; `ROLE_AGENT`/`agent` → `Agent`.
- part:
  - SDK `{"text": "..."}` → внутренний `Part::Text`
  - SDK `{"raw": <base64>}` / `{"url": "..."}` → `Part::File` (или `Data`)
  - шлюзовый `{"kind":"text","text":"..."}` → как сейчас
- SDK может не слать `kind` — protojson-формат.

> Реализация: `fn normalize_message(Value) -> protocol::a2a::Message`,
> пробуем SDK-раскладку, при неудаче — текущую.

### 1.2.3 Выход: отдать Task в `{task: ...}` + `TASK_STATE_*` + proto parts

Когда клиент вызвал SDK-метод (`SendMessage`) — ответ должен быть в SDK-формате,
чтобы `driver-a2a-client` (ждёт `result.task`) распарсил:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "task": {
      "id": "task-...",
      "contextId": "ctx-...",
      "status": {
        "state": "TASK_STATE_COMPLETED",
        "message": { "messageId": "...", "role": "ROLE_AGENT", "parts": [ {"text":"..."} ] },
        "timestamp": "..."
      },
      "artifacts": [
        { "artifact_id": "...", "name": "response", "description": null,
          "parts": [ {"text":"..."} ], "metadata": null }
      ]
    }
  }
}
```

Преобразование (из внутреннего `Task`):

- `id` → `id` (строка остаётся `task-...`, SDK TaskId — строка).
- `context_id` → `contextId` (camelCase).
- `status.state` → `TASK_STATE_<UPPER>` (смотреть `a2a/src/types.rs` serde).
- `message.message_id` → `messageId`.
- `message.role` → `ROLE_AGENT` / `ROLE_USER`.
- part `{kind:"text",text}` → `{text}`; `{kind:"file",file}` → `{url|raw}`;
  `{kind:"data",data}` → `{data}`.
- Обёртка `{ "task": ... }` — **обязательно** (SDK-клиент ждёт её).
- `artifacts` — поля `artifact_id` (SDK ждёт `artifact_id`, как в шлюзе).

> **Важно:** SDK-формат на выходе — только для SDK-запросов. Для запросов
> `message/send` (свой формат шлюза) ответ остаётся плоским, чтобы не ломать
> существующих клиентов шлюза.

### 1.2.4 Как отличить формат клиента

По **имени метода запроса**:

- `SendMessage` / `GetTask` / `CancelTask` → SDK-формат (вход нормализуем,
  выход в `{task:...}` + `TASK_STATE_*`).
- `message/send` / `tasks/get` / `tasks/cancel` → текущий семантический формат
  (без изменений).

Это детерминировано: клиент не может «переключить» формат mid-session.

## 1.3 Схема внутренней нормализации

```
POST /agents/:id/rpc
  │
  ├─ method == "SendMessage" ──► normalize SDK-params → protocol::a2a::Message
  │                               → adapter.send_task_as(...)
  │                               → render Task → SDK-формат ({task, TASK_STATE_*})
  │
  ├─ method == "message/send" ─► (текущий путь, без изменений)
  │
  ├─ GetTask / CancelTask  ───► алиасы → tasks/get, tasks/cancel (SDK-ответ)
  │
  └─ иначе ──────────────────► method_not_found
```

Два рендерера Task:

- `render_task_semantic(Task) -> Value` (текущий, плоский).
- `render_task_sdk(Task) -> Value` (`{task:{...}}` + `TASK_STATE_*` + proto parts).

## 1.4 Изменяемые файлы (в репо шлюза)

| Файл                             | Правка                                                                                            |
| -------------------------------- | ------------------------------------------------------------------------------------------------- |
| `gatewayd/src/transport_http.rs` | добавить `SendMessage`/`GetTask`/`CancelTask` в `dispatch_a2a_method`; выбрать рендерер по методу |
| `gatewayd/src/transport_http.rs` | `build_task_from_send_params` — нормализация SDK/семантического message                           |
| `protocol/src/a2a.rs`            | (опц.) helpers `role_to_sdk`, `part_to_sdk`, `state_to_sdk` — или в `transport_http.rs`           |
| `gatewayd/src/transport_http.rs` | `render_task_sdk` (обёртка `{task}` + `TASK_STATE_*` + proto parts)                               |

Ни `protocol-acp`, ни `core` не меняются — SDK-формат касается только
A2A-границы (вход/выход HTTP).

---

# Раздел 2. Адаптер `agent-connector`: `driver-a2a-client` с двумя wire-форматами

## 2.1 Контекст

`driver-a2a-client` написан под **один** wire-формат — JSON-RPC слой официального
SDK `a2a-rs` (метод `SendMessage`, proto-поля). Живая проверка показала, что
шлюз `ACP-A2A_gateway` реализует **другой** wire-формат (метод `message/send`,
плоский Task, lowercase-состояния), который драйвер не понимает.

### Ключевой факт: «стандарта» на wire два

Официальный SDK `a2a-rs` сам предоставляет **два** wire-представления:

| Слой SDK | Wire                             | Источник                                                                                                                                 |
| -------- | -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| REST     | путь `/message:send`             | `a2a-client/src/rest.rs:17` (`REST_SEND_MESSAGE_PATH`)                                                                                   |
| JSON-RPC | метод `SendMessage` + proto-поля | `a2a/src/jsonrpc.rs:138` (`methods::SEND_MESSAGE`) + `a2a-server/src/jsonrpc.rs:73` (`protojson_conv::from_value::<SendMessageRequest>`) |

Шлюз реализует третий вид — **семантический** JSON-RPC по A2A-спеке (метод
`message/send`): `gatewayd/src/transport_http.rs:254`, свои типы в
`protocol/src/a2a.rs`.

Итог — **два** wire-вида релевантны для драйвера:

1. **A2aSdkJsonRpc** — метод `SendMessage`, proto-поля (`TASK_STATE_*`, `{text}`,
   `ROLE_USER`). Это формат нашего `protocol-a2a-server` (собран на том же SDK).
2. **A2aSpecJsonRpc** — метод `message/send`, семантические поля
   (`completed`, `{kind: text}`, `user`, плоский Task). Это формат шлюза.

Драйвер должен уметь работать с обоими — по выбору на endpoint-уровне.

## 2.2 Цель

Сделать `driver-a2a-client` **wire-формат-нейтральным**: уметь отправлять запрос
и разбирать ответ в любом из двух форматов, выбираемом конфигурацией агента.
Это позволит одному драйверу подключаться и к нашему `adapterd` (SDK-формат),
и к шлюзу (spec-формат) — без дублирования кода.

## 2.3 Точное сравнение двух форматов (по коду)

### 2.3.1 Метод запроса

|                 | SDK-формат               | Spec-формат                                          |
| --------------- | ------------------------ | ---------------------------------------------------- |
| JSON-RPC method | `"SendMessage"`          | `"message/send"`                                     |
| Источник        | `a2a/src/jsonrpc.rs:138` | `ACP-A2A_gateway/gatewayd/src/transport_http.rs:254` |

> **Почему нельзя «угадать» на отправку:** сервер принимает ровно одно имя
> метода. `SendMessage` → наш adapterd; `message/send` → шлюз. Неверный выбор =
> `method not found` (подтверждено живым тестом: adapterd вернул `-32601
> METHOD_NOT_FOUND` на `message/send`).

### 2.3.2 Параметры запроса `message`

Обе стороны принимают объект `{ message: { role, parts } }` на верхнем уровне.
Различие — внутри `part` и `role`:

| Поле          | SDK-формат                                                                        | Spec-формат                                           |
| ------------- | --------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `role`        | `"ROLE_USER"`                                                                     | `"user"`                                              |
| part text     | `{ "text": "..." }`                                                               | `{ "kind": "text", "text": "..." }`                   |
| part resource | `{ "url": ..., "media_type": ... }`?                                              | `{ "kind": "resource", "uri": ..., "mimeType": ... }` |
| Источник      | protojson (SDK десериализация: `unknown field \`kind\`` на `{kind}` — живой тест) | `protocol/src/a2a.rs` (`Message`, `Part` с `kind`)    |

> Живое подтверждение несовместимости SDK-стороны: adapterd на part
> `{ "kind": "text" }` вернул `-32700 PARSE_ERROR: unknown field \`kind\``.
> Наоборот, шлюз ожидает именно `{ kind, text }`.

### 2.3.3 Ответ `SendMessageResponse` / Task

| Аспект       | SDK-формат                                             | Spec-формат                               |
| ------------ | ------------------------------------------------------ | ----------------------------------------- |
| Обёртка      | `{ "task": { ... } }`                                  | плоский `{ id, context_id, status, ... }` |
| Состояние    | `"TASK_STATE_COMPLETED"`                               | `"completed"`                             |
| message.role | `"ROLE_AGENT"`                                         | `"agent"`                                 |
| part         | `{ "text": ... }`                                      | `{ "kind": "text", "text": ... }`         |
| Источник     | `a2a/src/types.rs` (`TaskState` serde: `TASK_STATE_*`) | `transport_http.rs` (плоский Task)        |

> Живое подтверждение: драйвер (ищет `result.task`) получил от шлюза плоский
> Task в `result` → ошибка `A2A response missing task: no task in result`.
> И наоборот, `a2a::TaskState` не распарсит `"completed"` (ожидает
> `"TASK_STATE_COMPLETED"`).

## 2.4 Проектирование

### 2.4.1 Конфигурация: новое поле `wire_format`

В `A2aClientConfig` (crates/driver-a2a-client) добавить:

```rust
/// Wire-формат A2A-агента.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum A2aWireFormat {
    /// Официальный a2a-rs SDK JSON-RPC слой: метод SendMessage, proto-поля.
    #[default]
    Sdk,
    /// Семантический A2A JSON-RPC (шлюз ACP-A2A_gateway): метод message/send.
    Spec,
}
```

В `adapterd-config` вариант `A2aClient` — поле:

```yaml
- id: hermes
  driver: a2a-client
  endpoint: https://agentmesh-labs.mnemostroma.com/agents/hermes/rpc
  wire_format: spec          # sdk (default) | spec
```

Десериализация: `#[serde(default)]` + enum строк (`sdk`/`spec`). Отсутствие
поля → `sdk` (обратная совместимость, текущее поведение).

### 2.4.2 Внутренняя структура: два wire-модуля

Без общих веток `if wire_format` по всему коду:

```
crates/driver-a2a-client/src/
├── lib.rs            # AgentDriver impl, dispatch по wire_format
├── wire/mod.rs       # trait A2aWire { method(); build_message(); parse_task() }
├── wire/sdk.rs       # A2aSdkWire  — SendMessage + protojson + a2a::Task
└── wire/spec.rs      # A2aSpecWire — message/send + семантические поля
```

```rust
trait A2aWire: Send + Sync {
    fn jsonrpc_method(&self) -> &'static str;
    fn build_message(&self, input: &[Part], task_id: Option<&TaskId>) -> Value;
    /// Возвращает нормализованный Task (единый внутренний тип), либо ошибку.
    fn parse_response(&self, result: &Value) -> Result<NormalizedTask, A2aClientError>;
}
```

`A2aClientDriver` держит `wire: Box<dyn A2aWire>` (или enum), выбирается в
`A2aClientDriver::new(config)`.

### 2.4.3 Единый внутренний тип `NormalizedTask`

```rust
struct NormalizedTask {
    id: String,
    state: NormalizedState,      // Working | InputRequired | Completed | Failed | Cancelled
    message: String,
    output_parts: Vec<Part>,
}
```

- `A2aSdkWire::parse_response` — десериализует `a2a::Task` (типизированно),
  маппит `a2a::TaskState` → `NormalizedState`.
- `A2aSpecWire::parse_response` — парсит плоский Value вручную
  (`completed`/`failed`/`canceled`/`inputRequired` lowercase),
  части `{kind,text}` → `Part`.

`invoke()` работает только с `NormalizedTask` → `DriverEvent`.

> **Обоснование единого типа:** `AgentDriver::invoke` обязан выдавать
> `DriverEvent` — один набор событий. Формат wire — деталь транспорта,
> не должна протекать в логику драйвера.

### 2.4.4 cancel / provide_input

- `cancel` — локальный `CancellationToken` (формат-независим) + best-effort
  HTTP-cancel. Для SDK-формата сигнал отмены — отдельный вызов (`tasks/cancel`).
- `provide_input` — повторный `message/send` с тем же `task_id`/`taskId` через
  выбранный wire. Имена полей зависят от формата (`task_id` vs `taskId`),
  поэтому `build_message` уже принимает `task_id` и сам решает, куда его класть.

## 2.5 Маппинг ошибок

| Ситуация                                                                   | Результат                                                                          |
| -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| сервер вернул `error` (JSON-RPC)                                           | `DriverEvent::Failed` c кодом `a2a_remote_error`                                   |
| `result` нет / нет task                                                    | `DriverEvent::Failed` `a2a_no_task`                                                |
| неподдерживаемый wire (будущие форматы)                                    | ошибка в `new()`, агент не стартует                                                |
| несовпадение формата (отправлен `SendMessage`, сервер ждал `message/send`) | `-32601 METHOD_NOT_FOUND` → `DriverEvent::Failed`; в логе — hint про `wire_format` |

## 2.6 Тесты (Раздел 2)

1. **Unit: sdk-wire** — `build_message` даёт `SendMessage` + `ROLE_USER` +
   part `{text}`; `parse_response` из `{task:{...}, TASK_STATE_COMPLETED}` → `NormalizedTask`.
2. **Unit: spec-wire** — `build_message` даёт `message/send` + `user` +
   part `{kind,text}`; `parse_response` из плоского `{completed}` → `NormalizedTask`.
3. **Contract (mock axum server)** — два mock-сервера (SDK-формат и spec-формат),
   полный `invoke` → `Completed` через каждый wire.
4. **Живой E2E** (после сборки, вручную/скриптом):
   - наш `adapterd` (SDK-формат) ← `driver-a2a-client` c `wire_format: sdk`
   - шлюз hermes (spec-формат) ← `driver-a2a-client` c `wire_format: spec`

DoD: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`.

---

# Раздел 3. Общий диалект-зонд: запрос определения протокола и диалекта

Общая ключевая подзадача для обоих разделов: **короткий первичный запрос,
который сразу показывает, на каком протоколе/диалекте умеет коммуницировать
клиент** (SDK / Spec / ACP / неизвестен).

## 3.1 Принцип

Зонд должен быть **идемпотентным** — не создавать задач и не иметь побочных
эффектов. Используем `GetTask`/`tasks/get` с заведомо несуществующим `task_id`
(случайный UUID), а **не** `SendMessage`/`message/send` (те создают реальную задачу).

## 3.2 Определение на входе (серверная сторона, оба продукта)

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

## 3.3 Зонд (клиентская сторона, если Agent Card недоступен)

```
POST /agents/:id/rpc
{ "jsonrpc": "2.0", "id": 1, "method": "GetTask",
  "params": { "name": "tasks/<uuid>" } }            # SDK-стиль
```

Интерпретация ответа:

| Ответ                                                              | Вердикт                                      |
| ------------------------------------------------------------------ | -------------------------------------------- |
| `result` (или ошибка «task not found» без `method_not_found`)      | сервер понимает **SDK** → работаем на SDK    |
| `-32601` / `-32000` + `method_not_found:`                          | не SDK → пробуем Spec:                       |
| `POST ... { "method": "tasks/get", "params": { "id": "<uuid>" } }` |                                              |
| ошибка «task not found»                                            | сервер понимает **Spec** → работаем на Spec  |
| `method_not_found` и для `tasks/get`                               | не A2A → пробуем ACP (иной интерфейс)        |
| и ACP не распознал                                                 | явная ошибка: «диалект клиента не определён» |

Кэширование: результат детекта хранится **на эндпоинт** (один зонд на первый
контакт), повторные запросы зонд не вызывают.

## 3.4 Применение в продуктах

- **Шлюз (Раздел 1):** определение диалекта уже детерминировано именем метода
  (§1.2.4). Зонд не нужен на входе — он нужен для *своих* исходящих вызовов,
  если шлюз сам ходит в сторонних агентов (клиентская сторона, §3.3).
- **Адаптер (Раздел 2):** `wire_format: auto` (новое значение enum) — при первом
  контакте с endpoint выполняется зонд (§3.3), результат кэшируется, выбор
  `A2aSdkWire`/`A2aSpecWire` производится по нему. Приоритет при неоднозначности —
  **SDK**.

## 3.5 DoD зонда

- [ ] зонд не создаёт задач (только `GetTask`/`tasks/get` с несуществующим id);
- [ ] детект по Agent Card (`protocolVersion`) — приоритетнее зонда;
- [ ] кэш диалекта на эндпоинт;
- [ ] приоритет SDK при неоднозначности;
- [ ] понятная ошибка с перечислением поддерживаемых диалектов, если ни один не определён.

---

# Общие критерии приёмки

1. `cargo test --workspace` (оба репо) — зелёный.
2. Живой E2E: `adapterd` (driver-a2a-client) → шлюз → hermes: `invoke` →
   `Completed` (текст hermes) — через SDK и через Spec.
3. Регрессия: существующие клиенты шлюза (семантический формат) не тронуты.
4. Зонд не оставляет задач на сервере и не имеет побочных эффектов.