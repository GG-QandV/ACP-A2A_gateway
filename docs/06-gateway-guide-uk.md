# Гайд по гейтвею: що це, як запустити, як підключати агентів

> **English version**: [docs/06-gateway-guide.md](docs/06-gateway-guide.md) ·
> **Русская версия**: [docs/06-gateway-guide.md](06-gateway-guide.md)

Репозиторій: `GG-QandV/ACP-A2A_gateway` (Rust workspace: `protocol`, `core`, `gatewayd`).

Гейтвей — прошарок між **ACP-агентами** (claurst, hermes, opencode) та
**A2A-клієнтами** (зовнішні сервіси, інші агенти). Піднімає два порти,
обслуговує чотири напрями, роздає токени та ізолює розмови між клієнтами.

## 1. Напрями (який клієнт → який агент)

| # | Вхідна сторона | Агент | Транспорт | Порт/шлях |
|---|---|---|---|---|
| 1 | ACP-клієнт | ACP (stdio) | TCP passthrough | `listen` |
| 2 | A2A-клієнт | A2A (HTTP) | HTTP reverse proxy, без конвертера | `http_listen` → `/a2a-proxy/:id/*path` |
| 3 | ACP-клієнт | A2A (HTTP) | TCP, конвертер ACP→A2A | `listen` |
| 4 | A2A-клієнт | ACP (stdio) | HTTP, конвертер A2A→ACP | `http_listen` → `/agents/:id/rpc` |

- `listen` (за замовч. `0.0.0.0:8347`) — TCP: перший рядок — JSON handshake.
- `http_listen` (за замовч. `0.0.0.0:8348`) — HTTP: Bearer-токен у `Authorization`.

## 2. Збірка

```bash
rustc --version   # потрібен 1.80+ (залежності: openssl, native-tls)
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 151 тест
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` із середовища збірки **не комітити** (залежності штучно занижені
під старий компілятор у чужих артефактах).

## 3. Конфіг (`config.yaml`)

```yaml
listen: "0.0.0.0:8347"          # TCP
http_listen: "0.0.0.0:8348"     # HTTP
public_url: "https://gateway.example.com"  # зовнішня адреса → AgentCard.url

tokens: [ "t-dev-local-001" ]   # дійсні токени клієнтів

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/srv/workspaces/main"
    env: { OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}" }  # {env:X} з оточення
  hermes-main:
    transport: stdio
    command: ["hermes", "acp"]
    cwd: "/tmp"
  ops-agent:
    transport: http
    url: "https://ops.internal/a2a"
    push_token: "{env:OPS_AGENT_PUSH_TOKEN}"

task_store_dir: "/tmp/gateway/tasks"
task_retention_days: 7          # зберігати завершені задачі N днів (фоновий sweep щогодини)
turn_lease_timeout_secs: 30     # очікування лізи на одну сесію
agent_call_timeout_secs: 120    # таймаут одного JSON-RPC до stdio-агента
```

Нотатки щодо конфіга (валідуються на старті):
- `{env:VAR}` з відсутньою змінною → **помилка старту** (не порожній рядок).
- Порожній токен у `tokens` → **помилка старту**.
- `public_url` — це адреса, за якою клієнти бачать шлюз ззовні (за reverse
  proxy — домен проксі), а не адреса прив'язки. Вона йде в `AgentCard.url`
  (`.well-known/agent.json`), інакше картка невалідна за A2A-спекою.
- `task_retention_days` (за замовч. 7): фонова задача щогодини прибирає
  завершені задачі старші за цей термін, обходячи каталоги на диску.

### Опційні секції зберігання

Усі за замовчуванням вимкнені; генеруються тим самим майстром `--setup`.

```yaml
# Durable буфер подій: джерело істини для стримінгу / tasks/resubscribe.
event_log:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/event_log.db
  max_size_mb: 100

# Task store backend: sqlite замінює файлове зберігання.
task_store:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/task_store.db
  max_size_mb: 500

# Журнал: durable лог health-алертів, обривів стрімів та апрувів.
# Перегляд — `gatewayd --journal` (див. CLI нижче).
journal:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/journal.db
  max_size_mb: 100
  retention_days: 30

# Health-моніторинг: періодична зведення розмірів БД + зайнятість слотів стріму.
health:
  enabled: true
  check_interval_secs: 300
  db_size_warn_pct: 80

# Апруви: людський контроль агентів. Нові агенти стають "pending" і НЕ
# обслуговуються, поки не схвалені командою `gatewayd --approve <name>`.
approvals:
  enabled: true
  storage_path: /tmp/gateway/approvals.db
```

## 4. Підключення реальних агентів

ACP-режим вмикається **підкомандою acp** (перевірено на живих бінарниках):

| Агент | Команда | Перевірено |
|---|---|---|
| claurst 0.1.7 | `claurst acp` | так |
| Hermes Agent | `hermes acp` | так |
| opencode | `opencode acp` | (за докою) |

Нотатки:
- **Не** `--bare`, **не** `--print`, не stdin-JSON-RPC при звичайному запуску —
  ACP-сервер стартує лише через підкоманду `acp`.
- ACP-агенту потрібен **справжній pipe** на stdout: перенаправлення у файл
  (`> out.log`) валить Hermes з `ValueError: Pipe transport is only for pipes`.
  Гейтвей запускає агентів через `Stdio::piped()` — коректно.
- claurst: `prompt` у `session/prompt` — це **послідовність** ContentBlock-ів,
  не рядок (рядок → `-32602 invalid type ... expected a sequence`). Блоки
  пишуться з полем `type` (не `kind`): `{"type":"text","text":"..."}`.
- claurst env: `CLAURST_DISABLE_MODELS_FETCH=1` (не ходити за списком моделей),
  `CLAURST_SHARE_NO_OPEN=1` (не відкривати браузер).

## 5. CLI-команди (Rust-модулі, без зовнішнього sqlite3)

```bash
gatewayd config.yaml                     # запуск гейтвею

gatewayd --journal [--limit N] [--level info|warn|error] [--category NAME] \
         [--since 10m|6h|1d|2w|1mo] [--db PATH]
# перегляд durable журналу ASCII-таблицею (алерти, обриви, апруви), час в UTC

gatewayd --approvals                     # статуси агентів + фінґерпринти
gatewayd --approve <name>                # схвалити агента (обслуговується після рестарту)
gatewayd --reject <name>                 # відхилити агента
# несхвалений агент не обслуговується (HTTP 404 unknown agent_id) і пишеться
# в журнал (category "approval")

gatewayd --setup                         # інтерактивний майстер генерації конфіга
```

## 6. Протокол: ACP-клієнт → гейтвей (напрями 1 і 3, TCP)

Перший рядок — handshake:

```json
{"token":"t-dev-local-001","agent_id":"claurst-main"}
```

Далі звичайний ACP JSON-RPC, newline-delimited (`\n`); відповіді приходять
тими самими рядками:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"acp-...","prompt":[{"type":"text","text":"hi"}],"contexts":[],"files":[]}}
```

Напрям 1 (ACP→ACP) — повний passthrough: кожен `session/new` агента створює
сесію в **тому самому процесі** (перевірено: claurst і hermes повертають різні
`sessionId` на два підряд `session/new`).

## 7. Протокол: A2A-клієнт → гейтвей (напрями 2 і 4, HTTP)

Картка агента:

```
GET http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json
Authorization: Bearer <token>
```

`AgentCard.url` формується з `config.public_url` + `agent_id`:
`https://gateway.example.com/agents/<agent_id>/rpc`.

RPC (напрям 4 — A2A-клієнт → ACP-агент):

```
POST http://127.0.0.1:8348/agents/<agent_id>/rpc
Authorization: Bearer <token>
Content-Type: application/json
```

`message/send` починає розмову:

```json
{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"привіт"}]}
}}
```

Відповідь:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "context_id":"ctx-...",
  "id":"task-...",
  "status":{"state":"completed","message":{...}},
  "artifacts":[{"name":"response","parts":[{"kind":"text","text":"..."}]}]
}}
```

### Напрям 2: A2A-клієнт → A2A-агент (reverse proxy)

```
POST http://127.0.0.1:8348/a2a-proxy/<agent_id>/<path>?<query>
Authorization: Bearer <token>
Content-Type: application/json
Accept: application/json        # або text/event-stream для SSE
```

- Агент має бути `transport: http` — інакше `400 agent_id is not an
  A2A/http agent`.
- Запит проксіюється як є, без семантичного перетворення, разом із query-
  рядком і SSE-стрімом (`text/event-stream`).
- Шлях нормалізується: `..`, `.`, подвійні слеші вирізаються — запит не може
  вийти за межі адреси агента.
- Ліміт тіла 32 MiB, таймаути 300с/10с.
- `push_token` агента йде в `Authorization: Bearer` upstream.

### Продовження розмови (contextId)

`contextId` передається в полі `message.contextId` (або `params.contextId`),
**не** в `configuration`:

```json
{"jsonrpc":"2.0","id":2,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"продовжуй"}],"contextId":"ctx-..."}
}}
```

- Розмова прив'язана до токена: чужий існуючий `contextId` відхиляється
  (`contextId belongs to another client`) — IDOR закрито.
- Без `contextId` гейтвей заводить нову розмову й повертає свіжий `context_id`.
- Неіснуючий `contextId` **не відхиляється**, а заводиться заново — так задумано.
- Сесія живе 24 год простою (`DEFAULT_SESSION_TTL`), стеля
  `MAX_SESSIONS_PER_AGENT = 256` на агента.
- Атрибуція задачі переживає виселення сесії та рестарт шлюзу
  (`StoredTask { owner, task }` у TaskStore).
- Смерть агента: супервізор перезапускає процес (backoff 5с); звернення до
  розмови, чия сесія належала попередньому поколінню процесу, дає `ContextLost`
  (`-32010` / HTTP 409); повторний виклик з тим самим `contextId` заводить свіжу
  сесію і працює. Чужий `contextId` після рестарту — відмова за власником, не
  `ContextLost`.

## 8. Стрімінг (напрями 3 і 4)

`message/send` з агентами, що підтримують стрімінг, повертає SSE-події для
напряму 4 і newline-delimited `session/update`-нотифікації для напряму 3:

- `first_chunk_timeout_secs` / `idle_chunk_timeout_secs` захищають цикл стріму;
  пер-агентний семафор `max_concurrent_streams` тримає стелю (fail-closed).
- Durable-події дописуються в `event_log` з монотонним per-task `seq`.
- `tasks/get-last-seq` повертає останній `seq` для задачі; `tasks/resubscribe`
  відтворює події після заданого `seq` з event log як SSE-стрім — клієнт, що
  від'єднався посеред стріму, може перепідключитися до задачі.

## 9. Відомі обмеження та відкриті проблеми

| # | Проблема | Статус |
|---|---|---|
| 1 | `tasks/resubscribe` реалізований для HTTP; TCP line-протокол не має resubscribe RPC | 🟢 відкрите питання |
| 2 | `tasks/get`, `tasks/cancel` покриті юніт-тестами, але не проганялися через живий HTTP на стенді | ⏳ |
| 3 | Запис TECH_DEBT про `tasks/resubscribe` (2026-08-18) застарілий — фічу реалізовано (Phase 3.2) | 🟢 doc debt |
| 4 | HMAC хеш токена — ключ з `{env:GATEWAY_HMAC_KEY}`, дефолт `default-dev-key-do-not-use-in-prod` | ✅ закрито |

## 10. Де що лежить

| Файл | Призначення |
|---|---|
| `gatewayd/src/main.rs` | Конфіг, Registry, `{env:...}`, валідація, старт, CLI dispatch |
| `gatewayd/src/cli.rs` | Rust CLI: `--journal`, `--approvals`, `--approve`/`--reject`, таблиці, UTC-форматер |
| `gatewayd/src/approvals.rs` | SQLite-сховище апрувів (pending/approved/rejected, фінґерпринти) |
| `gatewayd/src/journal.rs` | Durable-журнал writer + `query_recent` (фільтри level/category/since) |
| `gatewayd/src/event_log.rs` | Durable-буфер подій з per-task `seq`, replay для resubscribe |
| `gatewayd/src/health.rs` | Періодична зведення розмірів БД + зайнятості стрімів |
| `gatewayd/src/config.rs` | `RawConfig`, опційні секції (`event_log`, `task_store`, `journal`, `health`, `approvals`) |
| `gatewayd/src/setup.rs` | Інтерактивний `--setup` майстер конфіга |
| `gatewayd/src/transport_tcp.rs` | TCP: напрями 1 (ACP passthrough) і 3 (ACP→A2A) |
| `gatewayd/src/transport_http.rs` | HTTP: напрям 4 (`/agents/:id/rpc` + agent.json), contextId, AgentCard.url, resubscribe |
| `gatewayd/src/transport_a2a_passthrough.rs` | HTTP: напрям 2 (A2A→A2A reverse proxy, `/a2a-proxy/:id/*path`) |
| `core/src/convert.rs` | `AcpAsA2a` (A2A→ACP) і `A2aAsAcp` (ACP→A2A), сесії, власник |
| `core/src/owner.rs` | `Owner` (Token hash / Anonymous) |
| `core/src/task_store.rs` | `TaskStore` + `StoredTask{owner, task}` |
| `core/src/stdio_agent.rs` | Процес ACP-агента, JSON-RPC по id, session/update |
| `core/src/lease.rs` | `TurnLease` — серіалізація prompt-ів на сесію |
| `core/src/http_agent.rs` | A2A-агент по HTTP (ops-agent) |