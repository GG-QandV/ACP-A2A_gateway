# Гайд по гатевею: что это, как запустить, как подключать агентов

> **English version**: [docs/06-gateway-guide-en.md](06-gateway-guide-en.md) ·
> **Українська версія**: [docs/06-gateway-guide-uk.md](06-gateway-guide-uk.md)

Репо: `GG-QandV/ACP-A2A_gateway` (Rust workspace: `protocol`, `gateway-core`, `gatewayd`).

Гатевей — прослойка между ACP-агентами (claurst, hermes, opencode) и A2A-клиентами
(внешние сервисы, другие агенты). Поднимает два порта, обслуживает четыре
направления, раздаёт токены, изолирует разговоры между клиентами.

## 1. Направления (какой клиент → какой агент)

| # | Входящая сторона | Агент | Транспорт | Порт/путь |
|---|---|---|---|---|
| 1 | ACP-клиент | ACP (stdio) | TCP passthrough | `listen` |
| 2 | A2A-клиент | A2A (HTTP) | HTTP reverse-proxy, без конвертера | `http_listen` → `/a2a-proxy/:id/*path` |
| 3 | ACP-клиент | A2A (HTTP) | TCP, конвертер ACP→A2A | `listen` |
| 4 | A2A-клиент | ACP (stdio) | HTTP, конвертер A2A→ACP | `http_listen` → `/agents/:id/rpc` |

- `listen` (по умолч. `0.0.0.0:8347`) — TCP: первая строка — handshake JSON.
- `http_listen` (по умолч. `0.0.0.0:8348`) — HTTP: Bearer-токен в `Authorization`.

## 2. Сборка

```bash
rustc --version   # нужно 1.80+ (зависимости: openssl, native-tls)
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 151 тест (Фазы 1–7, включая CLI/журнал/апрувы)
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` из окружения сборки **не коммитить** (зависимости искусственно
понижены под старый компилятор в чужих артефактах).

## 3. Конфиг (`config.yaml`)

```yaml
listen: "0.0.0.0:8347"          # TCP
http_listen: "0.0.0.0:8348"     # HTTP
public_url: "https://gateway.example.com"  # внешний адрес → AgentCard.url (P2-12)
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
task_retention_days: 7          # сколько дней хранить завершённые задачи (уборка фоном раз в час)
turn_lease_timeout_secs: 30     # ожидание лизы на одну сессию
agent_call_timeout_secs: 120    # таймаут одного JSON-RPC к stdio-агенту (аудит P2-11)
```

Особенности конфига (валидируются на старте, см. аудит P1-10):
- `{env:VAR}` с отсутствующей переменной → **ошибка старта** (не пустая строка).
- Пустой токен в `tokens` → **ошибка старта**.
- `public_url` — это адрес, по которому шлюз виден клиентам снаружи (за
  reverse-proxy — домен прокси), а не адрес привязки. Уходит в
  `AgentCard.url` (`.well-known/agent.json`), иначе карточка невалидна по
  A2A-спеке. По умолчанию `http://localhost:8348`.
- `task_retention_days` (по умолч. 7): завершённые задачи старше этого
  срока фоновая задача убирает раз в час (`TASK_SWEEP_INTERVAL`), ходя по
  каталогам на диске. Раньше `TaskStore::delete` не вызывался ниоткуда —
  файлы копились бесконечно.

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

## 4a. CLI-команды (Rust-модули, без внешнего sqlite3)

```bash
gatewayd config.yaml                     # запуск гейтвея

gatewayd --journal [--limit N] [--level info|warn|error] [--category NAME] \
         [--since 10m|6h|1d|2w|1mo] [--db PATH]
# просмотр durable-журнала ASCII-таблицей (алерты, обрывы, апрувы), время в UTC

gatewayd --approvals                     # статусы агентов + фингерпринты
gatewayd --approve <name>                # одобрить агента (обслуживается после рестарта)
gatewayd --reject <name>                 # отклонить агента
# неодобренный агент не обслуживается (HTTP 404 unknown agent_id) и пишется
# в журнал (category "approval")

gatewayd --setup                         # интерактивный мастер генерации конфига
```

## 5. Протокол: ACP-клиент → гатевей (направления 1 и 3, TCP)

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

## 6. Протокол: A2A-клиент → гатевей (направления 2 и 4, HTTP)

Карточка агента:

```
GET http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json
Authorization: Bearer <token>
```

`AgentCard.url` теперь заполняется из `config.public_url` + `agent_id`
(аудит P2-12): `https://gateway.example.com/agents/<agent_id>/rpc` — раньше
был пустым, карточка была невалидна.

RPC (направление 4 — A2A-клиент → ACP-агент):

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

### Направление 2: A2A-клиент → A2A-агент (reverse-proxy)

```
POST http://127.0.0.1:8348/a2a-proxy/<agent_id>/<path>?<query>
Authorization: Bearer <token>
Content-Type: application/json
Accept: application/json        # или text/event-stream для SSE
```

- Агент должен быть `transport: http` — иначе `400 agent_id is not an
  A2A/http agent`.
- Запрос проксируется как есть, без семантического преобразования, вместе
  с query-строкой (раньше терялась, P1-8) и SSE-стримом (`text/event-stream`).
- Путь нормализуется: `..`, `.`, двойные слеши вырезаются (P1-8) — запрос
  не выходит за пределы адреса агента.
- Лимит тела 32 MiB (P1-4), таймауты 300с/10с (P1-7).
- `push_token` агента уходит в `Authorization: Bearer` к upstream.

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
- Смерть агента: супервизор переспавнивает процесс (5с backoff); обращение к
  разговору, чья сессия была в прошлом поколении процесса, даёт `ContextLost`
  (`-32010` / HTTP 409); повторный вызов с тем же `contextId` заводит свежую
  сессию и работает (пометка одноразовая). Чужой contextId после рестарта —
  отказ по владельцу, не `ContextLost` (посторонний не узнаёт о существовании
  чужого контекста).

## 7. Известные ограничения и открытые проблемы

| # | Проблема | Статус |
|---|---|---|
| 1 | **continue по contextId таймаутит** (направление 4): второй `message/send` в ту же сессию не получает ответа до `agent_call_timeout`. Воспроизводится на claurst **и** hermes; прямой `session/prompt` к claurst в ту же сессию отвечает за ~3с. Значит дефект в конвертере/адаптере шлюза, не в агентах | 🔴 открыто, требует диагностики |
| 2 | Стриминг (`Reply::Streaming`) в конвертере A2A→ACP не реализован: падает с ошибкой «Фаза 1: стриминг не реализован» | ⏳ Фаза 2 |
| 3 | `tasks/get`, `tasks/cancel` — покрыты юнит-тестами, но на живом стенде через HTTP не прогонялись | ⏳ |
| 4 | Хеш токена — `std::hash::DefaultHasher`, не криптографический. Для сравнения на равенство достаточно; при модели угроз с подбором — менять на HMAC | ⏳ |
| 5 | `TurnLease::forget` вызывается при выселении сессии (P1-1) — утечек не найдено | ✅ |
| 6 | Смерть процесса агента | ✅ закрыто (P2-10): `SupervisedStdioAgent` переспавнивает (backoff 5с), старое поколение сессии → `ContextLost` → JSON-RPC `-32010` / HTTP 409. `is_alive` ловит смерть, но не зависание живого агента — висящий упрётся в `agent_call_timeout_secs` |
| 7 | Сессии без `session/new` — любой sessionId добавлял запись в HashMap навсегда (утечка) | ✅ закрыто (P2-8): сессия только через `session/new`, `prompt` отклоняет неизвестный sessionId ДО acquire, `cancel` освобождает лиз, TTL-выселение, потолок `MAX_SESSIONS_PER_CONNECTION = 256`. Live: 300 циклов new/prompt/cancel, память стабильна |
| 8 | `AgentCard.url` пустой — карточка невалидна по A2A-спеке | ✅ закрыто (P2-12): url = `config.public_url` + `/agents/<id>/rpc` |
| 9 | `TaskStore::delete` не вызывался ниоткуда — файлы задач копились бесконечно | ✅ закрыто (prod-final): `sweep_expired(ttl)` + фоновая уборка раз в час по mtime файла, `.json.tmp` не трогаются |
| 10 | Направление 2 (A2A reverse-proxy) не имело ни одного теста | ✅ закрыто (prod-final): юнит-тесты `build_target_url` (query, `..`, слеши, лимит тела) + live-прогон (8 проверок: 401/404/query/SSE/400) |

### Репро #1 (continue-таймаут)

```bash
# 1) поднять шлюз с реальным агентом (claurst-main или hermes-main)
# 2) первый message/send без contextId -> OK, запомнить context_id
# 3) второй message/send с message.contextId=<тот же> -> "timed out"
```

## 8. Где что лежит

| Файл | Назначение |
|---|---|
| `gatewayd/src/transport_tcp.rs` | TCP: направления 1 (ACP passthrough) и 3 (ACP→A2A) |
| `gatewayd/src/transport_http.rs` | HTTP: направление 4 (A2A→ACP, `/agents/:id/rpc` + agent.json), парсинг contextId, AgentCard.url |
| `gatewayd/src/transport_a2a_passthrough.rs` | HTTP: направление 2 (A2A→A2A reverse-proxy, `/a2a-proxy/:id/*path`) |
| `gatewayd/src/main.rs` | Конфиг, Registry, `{env:...}`, валидация, фоновая уборка задач |
| `core/src/convert.rs` | `AcpAsA2a` (A2A→ACP) и `A2aAsAcp` (ACP→A2A), сессии, владелец |
| `core/src/owner.rs` | `Owner` (Token hash / Anonymous) — P1-2 |
| `core/src/task_store.rs` | `TaskStore` + `StoredTask{owner, task}` — P1-2 |
| `core/src/stdio_agent.rs` | Процесс ACP-агента, JSON-RPC по id, session/update |
| `core/src/lease.rs` | `TurnLease` — сериализация prompt'ов на сессию |
| `core/src/http_agent.rs` | A2A-агент по HTTP (ops-agent) |
