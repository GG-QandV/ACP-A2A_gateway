# Описание A2A-шлюза `ACP-A2A_gateway`


> **Язык:** русский · [English version](./a2a-gateway-overview.md)
**Репозиторий:** `GG-QandV/ACP-A2A_gateway`
**Развёртывание:** адрес хоста, имя systemd-сервиса и публичный домен — в приватных
заметках развёртывания, не в этом репозитории.
**Роль:** посредник A2A-клиент → ACP-агенты (hermes, claurst).

## 1. Архитектура

```
A2A-клиент (curl / adapterd / любой)
   │  HTTP JSON-RPC (см. ниже)
   ▼
gatewayd (transport_http.rs)
   ├─ GET  /agents/:id/.well-known/agent.json   → карточка агента
   ├─ POST /agents/:id/rpc                        → JSON-RPC (методы ниже)
   │     └─ dispatch_a2a_method → AcpAsA2a → SupervisedStdioAgent → child (hermes acp)
   └─ (TCP 127.0.0.1:8347 — чистый ACP-прокси, наружу не публикуется)
```

- Агенты **спавнятся лениво** при первом обращении к их `:id`, живут до падения.
- Аутентификация: `Authorization: Bearer <token>` (токены из `/srv/gateway/env`,
  проверяется до спавна агента).

## 2. Wire-формат (что шлюз отдаёт и принимает)

### 2.1 Маршруты

| Путь | Метод | Что возвращает |
|---|---|---|
| `/agents/{id}/.well-known/agent.json` | GET | карточка агента (AgentCard) |
| `/agents/{id}/rpc` | POST | JSON-RPC 2.0 ответ |

### 2.2 JSON-RPC методы (POST /rpc)

| Метод | Params | Ответ `result` |
|---|---|---|
| `message/send` | `{ message: { role, parts, contextId? } }` | **плоский Task** (без обёртки `task`) |
| `tasks/get` | `{ id: "<task_id>" }` | плоский Task |
| `tasks/cancel` | `{ id: "<task_id>" }` | плоский Task |
| (всё остальное) | — | `error: method_not_found` |

### 2.3 Формат Task (плоский, не `{task: ...}`)

```json
{
  "id": "task-<hex>",
  "context_id": "ctx-<hex>",
  "status": {
    "state": "completed",            // lowercase: submitted|working|input_required|auth_required|completed|failed|canceled|rejected
    "message": { "role": "agent", "parts": [...] , "message_id": null },
    "timestamp": "ISO8601"
  },
  "history": null,
  "artifacts": [
    { "artifact_id": "...", "name": "response", "description": null,
      "parts": [ {"kind":"text","text":"..."} ], "metadata": null }
  ],
  "metadata": null
}
```

- **TaskId / ContextId** — обычные строки `task-...` / `ctx-...` (не UUID).
- **state** — `#[serde(rename_all = "kebab-case")]` lowercase: `completed`,
  `failed`, `canceled` и т.д. (из `protocol/src/a2a.rs:43-44`).

### 2.4 Формат Message / Part

```json
{ "role": "user", "parts": [ {"kind":"text","text":"..."} ] }
```

- `role` — lowercase `user` / `agent` (`#[serde(rename_all="lowercase")]`).
- `part` — тег `kind`, lowercase: `{"kind":"text","text":...}`,
  `{"kind":"file","file":{uri,bytes,mime_type}}`, `{"kind":"data","data":...}`
  (`protocol/src/a2a.rs:97-118`).

### 2.5 Карточка агента (agent.json)

```json
{
  "name": "hermes-agent",
  "description": null,
  "version": "1",
  "url": "https://gateway.example.com/agents/hermes/rpc",
  "capabilities": { "streaming": false, "push_notifications": false },
  "skills": []
}
```

### 2.6 Ошибки

| Код | Смысл |
|---|---|
| `-32010` + HTTP 409 | контекст потерян (перезапуск агента) — клиент должен начать заново |
| `-32000` | прочая ошибка приложения |
| `-32601` | method not found |

### 2.7 Подтверждено живыми запросами (2026-08-17)

- `message/send` → `result` = плоский Task с `state: "completed"`, артефакт
  `parts: [{"kind":"text","text":"Hello"}, ...]` (hermes ответил).
- `tasks/get` → тот же плоский Task (артефакт с текстом ответа).
- Карточка: `{name, description:null, version, url, capabilities, skills:[]}`.

## 3. Известные ограничения

- Только 3 метода (`message/send`, `tasks/get`, `tasks/cancel`).
- Нет `SendMessage`/`GetTask` (camelCase), нет proto-полей `TASK_STATE_*`,
  нет `{task: ...}` обёртки.
- Стриминг (`SendStreamingMessage` / SSE) не реализован («Фаза 1»).
- Multi-turn: второй `message/send` в ту же сессию виснет до
  `agent_call_timeout_secs` (известный баг upstream, `TECH_DEBT-ru.md`).
- Хеш токена — не криптографический (`DefaultHasher`).
