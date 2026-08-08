# Гайд по гатевею: что это, как запустить, как подключать агентов

Репо: `GG-QandV/ASP-A2A_gateway` (Rust workspace: `protocol`, `gateway-core`, `gatewayd`).

Гатевей — прослойка между ACP-агентами (claurst, hermes, opencode) и A2A-клиентами
(внешние сервисы, другие агенты). Поднимает два порта, обслуживает четыре
направления, раздаёт токены, изолирует разговоры между клиентами.

## 1. Направления (какой клиент → какой агент)

| # | Входящая сторона | Агент | Транспорт | Порт |
|---|---|---|---|---|
| 1 | ACP-клиент | ACP (stdio) | TCP passthrough | `listen` |
| 2 | ACP-клиент | A2A (HTTP) | TCP, конвертер ACP→A2A | `listen` |
| 3 | A2A-клиент | ACP (stdio) | TCP, конвертер A2A→ACP | `http_listen` |
| 4 | A2A-клиент | ACP (stdio) | HTTP, конвертер A2A→ACP | `http_listen` |

- `listen` (по умолч. `0.0.0.0:8347`) — TCP: первая строка — handshake JSON.
- `http_listen` (по умолч. `0.0.0.0:8348`) — HTTP: Bearer-токен в `Authorization`.

## 2. Сборка

```bash
rustc --version   # нужно 1.80+ (зависимости: openssl, native-tls)
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 36 тестов (после P1-2)
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` из окружения сборки **не коммитить** (зависимости искусственно
понижены под старый компилятор в чужих артефактах).

## 3. Конфиг (`config.yaml`)

```yaml
listen: "0.0.0.0:8347"          # TCP
http_listen: "0.0.0.0:8348"     # HTTP
tokens: [ "t-dev-local-001" ]   # действительные токены клиентов

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/srv/workspaces/main"
    env: { OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}" }  # {env:X} — из окружения
  hermes-main:
    transport: stdio
    command: ["hermes", "acp"]
    cwd: "/tmp"
  ops-agent:
    transport: http
    url: "https://ops.internal/a2a"
    push_token: "{env:OPS_AGENT_PUSH_TOKEN}"

task_store_dir: "/tmp/gateway/tasks"
turn_lease_timeout_secs: 30     # ожидание лизы на одну сессию
agent_call_timeout_secs: 120    # таймаут одного JSON-RPC к stdio-агенту (аудит P2-11)
```

Особенности конфига (валидируются на старте, см. аудит P1-10):
- `{env:VAR}` с отсутствующей переменной → **ошибка старта** (не пустая строка).
- Пустой токен в `tokens` → **ошибка старта**.

## 4. Как подключать реальных агентов

ACP-режим у агентов включается **подкомандой** (проверено на живых бинарях):

| Агент | Команда | Проверено |
|---|---|---|
| claurst 0.1.7 | `claurst acp` | да |
| Hermes Agent | `hermes acp` | да |
| opencode | `opencode acp` | (по доке) |

Важно:
- **Не** `--bare`, **не** `--print`, не stdin-JSON-RPC при обычном запуске — ACP-сервер
  стартует только через подкоманду `acp`.
- ACP-агенту нужен **настоящий pipe** на stdout: перенаправление в файл
  (`> out.log`) роняет Hermes с `ValueError: Pipe transport is only for pipes`.
  Гатевей запускает агента через `Stdio::piped()` — корректно.
- claurst: `prompt` в `session/prompt` — это **sequence** ContentBlock'ов, не строка
  (строка → `-32602 invalid type ... expected a sequence`). Блоки пишутся с полем
  `type` (не `kind`): `{"type":"text","text":"..."}`.
- claurst env: `CLAURST_DISABLE_MODELS_FETCH=1` (не ходить за списком моделей),
  `CLAURST_SHARE_NO_OPEN=1` (не открывать браузер).

## 5. Протокол: ACP-клиент → гатевей (направления 1 и 2, TCP)

Первая строка — handshake:

```json
{"token":"t-dev-local-001","agent_id":"claurst-main"}
```

Дальше обычный ACP JSON-RPC построчно (`\n`-delimited), ответы приходят теми же
строками:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"acp-...","prompt":[{"type":"text","text":"hi"}],"contexts":[],"files":[]}}
```

Направление 1 (ACP→ACP) — полный passthrough: каждый `session/new` агента создаёт
новую сессию в **том же процессе** (проверено: claurst и hermes возвращают разные
`sessionId` на два подряд `session/new`).

## 6. Протокол: A2A-клиент → гатевей (направления 3 и 4, HTTP)

Карточка агента:

```
GET http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json
Authorization: Bearer <token>
```

RPC:

```
POST http://127.0.0.1:8348/agents/<agent_id>/rpc
Authorization: Bearer <token>
Content-Type: application/json
```

`message/send` — начать разговор:

```json
{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"привет"}]}
}}
```

Ответ:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "context_id":"ctx-...",
  "id":"task-...",
  "status":{"state":"completed","message":{...}},
  "artifacts":[{"name":"response","parts":[{"kind":"text","text":"..."}]}]
}}
```

### Продолжение разговора (contextId)

`contextId` передаётся **в поле `message.contextId`** (или `params.contextId`),
**не** в `configuration`:

```json
{"jsonrpc":"2.0","id":2,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"продолжай"}],"contextId":"ctx-..."}
}}
```

- Разговор закреплён за токеном: чужой существующий `contextId` отклоняется
  (`contextId принадлежит другому клиенту`) — IDOR закрыт (P1-1, P1-2).
- Без `contextId` гатевей заводит новый разговор и возвращает свежий `context_id`.
- Несуществующий `contextId` **не отклоняется**, а заводится заново — так задумано.
- Сессия живёт 24ч простоя (`DEFAULT_SESSION_TTL`), потолок
  `MAX_SESSIONS_PER_AGENT = 256` на агента.
- Атрибуция задачи переживает выселение сессии и рестарт шлюза (P1-2,
  `StoredTask { owner, task }` в TaskStore).

## 7. Известные ограничения и открытые проблемы

| # | Проблема | Статус |
|---|---|---|
| 1 | **continue по contextId таймаутит** (направление 4): второй `message/send` в ту же сессию не получает ответа до `agent_call_timeout`. Воспроизводится на claurst **и** hermes; прямой `session/prompt` к claurst в ту же сессию отвечает за ~3с. Значит дефект в конвертере/адаптере шлюза, не в агентах | 🔴 открыто, требует диагностики |
| 2 | Стриминг (`Reply::Streaming`) в конвертере A2A→ACP не реализован: падает с ошибкой «Фаза 1: стриминг не реализован» | ⏳ Фаза 2 |
| 3 | `tasks/get`, `tasks/cancel` — покрыты юнит-тестами, но на живом стенде через HTTP не прогонялись | ⏳ |
| 4 | Хеш токена — `std::hash::DefaultHasher`, не криптографический. Для сравнения на равенство достаточно; при модели угроз с подбором — менять на HMAC | ⏳ |
| 5 | `TurnLease::forget` вызывается при выселении сессии (P1-1) — утечек не найдено | ✅ |
| 6 | Кэш адаптеров не инвалидируется при смерти процесса агента (P2-10, следующий по риску) | 🔴 открыто |

### Репро #1 (continue-таймаут)

```bash
# 1) поднять шлюз с реальным агентом (claurst-main или hermes-main)
# 2) первый message/send без contextId -> OK, запомнить context_id
# 3) второй message/send с message.contextId=<тот же> -> "timed out"
```

## 8. Где что лежит

| Файл | Назначение |
|---|---|
| `gatewayd/src/transport_tcp.rs` | TCP: направления 1 и 2, stdio passthrough |
| `gatewayd/src/transport_http.rs` | HTTP: направления 3 и 4, A2A→ACP, парсинг contextId |
| `gatewayd/src/main.rs` | Конфиг, Registry, `{env:...}`, валидация |
| `core/src/convert.rs` | `AcpAsA2a` (A2A→ACP) и `A2aAsAcp` (ACP→A2A), сессии, владелец |
| `core/src/owner.rs` | `Owner` (Token hash / Anonymous) — P1-2 |
| `core/src/task_store.rs` | `TaskStore` + `StoredTask{owner, task}` — P1-2 |
| `core/src/stdio_agent.rs` | Процесс ACP-агента, JSON-RPC по id, session/update |
| `core/src/lease.rs` | `TurnLease` — сериализация prompt'ов на сессию |
| `core/src/http_agent.rs` | A2A-агент по HTTP (ops-agent) |
