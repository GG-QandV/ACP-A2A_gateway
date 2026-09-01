# ТЗ: добавить в шлюз `ACP-A2A_gateway` wire-формат adapterd (SDK a2a-rs)


> **Язык:** русский · [English version](./SPEC-add-adapterd-wire-format.md)
> **ЗАМЕНЕНО:** объединено в единое ТЗ
> `docs/SPEC-a2a-dialects-gateway-adapter-ru.md` → Раздел 1. Этот файл сохранён как
> исходник, правки — в объединённом документе.

- **Статус:** заменено (см. выше). Код не менялся.
- **Дата:** 2026-08-17
- **Цель:** шлюз должен понимать формат, который использует `agent-connector`
  `adapterd` (JSON-RPC слой официального SDK `a2a-rs`), чтобы `adapterd`
  мог вызывать агентов шлюза через существующий `driver-a2a-client` без
  изменения драйвера.

---

## 1. Контекст

`driver-a2a-client` (в `agent-connector`) написан под wire-формат **JSON-RPC
слоя SDK `a2a-rs`** (метод `SendMessage`, proto-сериализация полей,
обёртка `{task: ...}`). Шлюз `ACP-A2A_gateway` сейчас отвечает только в своём
семантическом формате (`message/send`, плоский Task, lowercase). Чтобы
`adapterd` ↔ шлюз работали «из коробки», шлюзу нужно принимать/отдавать
SDK-формат **параллельно** со своим (по выбору клиента).

> Связанный документ: `agent-connector/docs/design/TZ-driver-a2a-wire-format.md`
> (там же — сравнение двух форматов по коду обеих сторон).

---

## 2. Что именно нужно добавить

### 2.1 Вход: принять метод `SendMessage` (camelCase) на том же `/rpc`

Сейчас `dispatch_a2a_method` матчит `"message/send"`, `"tasks/get"`,
`"tasks/cancel"`. Добавить алиасы SDK-имен:

| SDK-метод (a2a-rs) | Аналог шлюза |
|---|---|
| `SendMessage` | `message/send` |
| `GetTask` | `tasks/get` |
| `CancelTask` | `tasks/cancel` |

Плюс — возможно — `ListTasks`, `SubscribeToTask` (если нужно для совместимости;
в ТЗ на MVP — только первые три, зеркально текущему).

**Источник имён:** `a2a/src/jsonrpc.rs:138-148` (SDK, `methods`).

### 2.2 Вход: десериализовать параметры `SendMessage` в proto-формате

SDK-клиент шлёт `message` в proto-виде:

```json
{ "message": { "role": "ROLE_USER", "parts": [ {"text": "..."} ] } }
```

Шлюз сейчас ожидает `role: "user"`, part `{"kind":"text",...}`. Нужна
нормализация **на входе**: распознать оба варианта и свести к внутреннему
`protocol::a2a::Message`:

- `role`: `ROLE_USER`/`user` → `User`; `ROLE_AGENT`/`agent` → `Agent`.
- part:
  - SDK `{"text": "..."}` → внутренний `Part::Text`
  - SDK `{"raw": <base64>}` / `{"url": "..."}` → `Part::File` (или `Data`)
  - шлюзовый `{"kind":"text","text":"..."}` → как сейчас
- SDK может не слать `kind` — protojson-формат.

> Реализация: `fn normalize_message(Value) -> protocol::a2a::Message`,
> пробуем SDK-раскладку, при неудаче — текущую.

### 2.3 Выход: отдать Task в `{task: ...}` + `TASK_STATE_*` + proto parts

Когда клиент вызвал SDK-метод (`SendMessage`) — ответ должен быть в
SDK-формате, чтобы `driver-a2a-client` (ждёт `result.task`) распарсил:

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

### 2.4 Как отличить формат клиента

По **имени метода запроса**:
- `SendMessage` / `GetTask` / `CancelTask` → SDK-формат (вход нормализуем,
  выход в `{task:...}` + `TASK_STATE_*`).
- `message/send` / `tasks/get` / `tasks/cancel` → текущий семантический формат
  (без изменений).

Это детерминировано: клиент не может «переключить» формат mid-session.

---

## 3. Схема внутренней нормализации

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

---

## 4. Изменяемые файлы (в репо шлюза)

| Файл | Правка |
|---|---|
| `gatewayd/src/transport_http.rs` | добавить `SendMessage`/`GetTask`/`CancelTask` в `dispatch_a2a_method`; выбрать рендерер по методу |
| `gatewayd/src/transport_http.rs` | `build_task_from_send_params` — нормализация SDK/семантического message |
| `protocol/src/a2a.rs` | (опц.) helpers `role_to_sdk`, `part_to_sdk`, `state_to_sdk` — или в `transport_http.rs` |
| `gatewayd/src/transport_http.rs` | `render_task_sdk` (обёртка `{task}` + `TASK_STATE_*` + proto parts) |

Ни `protocol-acp`, ни `core` не меняются — SDK-формат касается только
A2A-границы (вход/выход HTTP).

---

## 5. Тесты

1. **Unit:** `normalize_message` — SDK params (`ROLE_USER`, `{text}`) и
   семантические (`user`, `{kind,text}`) → один `protocol::a2a::Message`.
2. **Unit:** `render_task_sdk` — внутренний `Task` с `Completed` →
   `{task:{status:{state:"TASK_STATE_COMPLETED"}, message:{role:"ROLE_AGENT", parts:[{text}]}}}`.
3. **Contract:** POST `/rpc` с `SendMessage` (SDK-тело) → `result.task` с
   `TASK_STATE_COMPLETED`; и `message/send` → плоский Task (регрессия текущего).
4. **Живой E2E:** `adapterd` (driver-a2a-client, `wire_format: sdk`) →
   шлюз → hermes: `invoke` → `Completed` (текст hermes).

DoD шлюза: `cargo test` в `ACP-A2A_gateway`, регрессия текущих клиентов
(семантический формат не тронут).

---

## 6. Объём

- Мидл, ~0.5–1 день. Только A2A-граница шлюза, ядро (`core`) не трогается.
- Не требует изменения `driver-a2a-client` (он остаётся SDK-форматным).
- Параллельный документ `agent-connector/docs/design/TZ-driver-a2a-wire-format.md`
  описывает обратную опцию (адаптация драйвера) — на случай, если решено
  менять драйвер, а не шлюз. Решение — за владельцем.
