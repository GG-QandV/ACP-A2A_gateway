# Gateway guide: what it is, how to run it, how to connect agents

Repo: `GG-QandV/ACP-A2A_gateway` (Rust workspace: `protocol`, `core`, `gatewayd`).

The gateway is a shim between **ACP agents** (claurst, hermes, opencode) and
**A2A clients** (external services, other agents). It exposes two ports, serves
four directions, distributes tokens and isolates conversations between clients.

## 1. Directions (which client → which agent)

| # | Incoming side | Agent | Transport | Port/path |
|---|---|---|---|---|
| 1 | ACP client | ACP (stdio) | TCP passthrough | `listen` |
| 2 | A2A client | A2A (HTTP) | HTTP reverse proxy, no converter | `http_listen` → `/a2a-proxy/:id/*path` |
| 3 | ACP client | A2A (HTTP) | TCP, converter ACP→A2A | `listen` |
| 4 | A2A client | ACP (stdio) | HTTP, converter A2A→ACP | `http_listen` → `/agents/:id/rpc` |

- `listen` (default `0.0.0.0:8347`) — TCP: first line is a JSON handshake.
- `http_listen` (default `0.0.0.0:8348`) — HTTP: Bearer token in `Authorization`.

## 2. Build

```bash
rustc --version   # needs 1.80+ (deps: openssl, native-tls)
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 151 tests
cargo clippy --workspace --all-targets -- -D warnings
```

Do **not** commit `Cargo.lock` from the build environment (dependencies are
artificially lowered for an older compiler in third-party artifacts).

## 3. Config (`config.yaml`)

```yaml
listen: "0.0.0.0:8347"          # TCP
http_listen: "0.0.0.0:8348"     # HTTP
public_url: "https://gateway.example.com"  # external address → AgentCard.url

tokens: [ "t-dev-local-001" ]   # valid client tokens

agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    cwd: "/srv/workspaces/main"
    env: { OPENMODEL_API_KEY: "{env:OPENMODEL_API_KEY}" }  # {env:X} read from env
  hermes-main:
    transport: stdio
    command: ["hermes", "acp"]
    cwd: "/tmp"
  ops-agent:
    transport: http
    url: "https://ops.internal/a2a"
    push_token: "{env:OPS_AGENT_PUSH_TOKEN}"

task_store_dir: "/tmp/gateway/tasks"
task_retention_days: 7          # keep finished tasks N days (background sweep, hourly)
turn_lease_timeout_secs: 30     # wait for the lease on one session
agent_call_timeout_secs: 120    # timeout for one JSON-RPC call to a stdio agent
```

Config notes (validated at startup):
- `{env:VAR}` with a missing variable → **startup error** (not an empty string).
- Empty token in `tokens` → **startup error**.
- `public_url` is the address clients see from outside (behind a reverse proxy
  it is the proxy domain), not the bind address. It goes into `AgentCard.url`
  (`.well-known/agent.json`), otherwise the card is invalid per the A2A spec.
- `task_retention_days` (default 7): a background task removes finished tasks
  older than this once an hour, walking directories on disk.

### Optional storage sections

All of these default to disabled and are managed by the same `--setup` wizard.

```yaml
# Durable event buffer: source of truth for streaming / tasks/resubscribe.
event_log:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/event_log.db
  max_size_mb: 100

# Task store backend: sqlite replaces the file storage.
task_store:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/task_store.db
  max_size_mb: 500

# Journal: durable log of health alerts, stream disconnects and approvals.
# View it with `gatewayd --journal` (see CLI below).
journal:
  enabled: true
  storage_backend: sqlite
  storage_path: /tmp/gateway/journal.db
  max_size_mb: 100
  retention_days: 30

# Health monitor: periodic DB size + stream slot usage summary.
health:
  enabled: true
  check_interval_secs: 300
  db_size_warn_pct: 80

# Approvals: human gate for agents. New agents enter "pending" and are NOT
# served until approved with `gatewayd --approve <name>`.
approvals:
  enabled: true
  storage_path: /tmp/gateway/approvals.db
```

## 4. Connecting real agents

ACP mode is enabled by an **acp subcommand** (verified on live binaries):

| Agent | Command | Verified |
|---|---|---|
| claurst 0.1.7 | `claurst acp` | yes |
| Hermes Agent | `hermes acp` | yes |
| opencode | `opencode acp` | (per docs) |

Notes:
- **Not** `--bare`, **not** `--print`, not stdin JSON-RPC on a normal start —
  the ACP server starts only via the `acp` subcommand.
- An ACP agent needs a **real pipe** on stdout: redirecting to a file (`> out.log`)
  crashes Hermes with `ValueError: Pipe transport is only for pipes`. The gateway
  launches agents via `Stdio::piped()` — correct.
- claurst: `prompt` in `session/prompt` is a **sequence** of ContentBlocks, not a
  string (a string → `-32602 invalid type ... expected a sequence`). Blocks carry
  a `type` field (not `kind`): `{"type":"text","text":"..."}`.
- claurst env: `CLAURST_DISABLE_MODELS_FETCH=1` (skip model list fetch),
  `CLAURST_SHARE_NO_OPEN=1` (don't open a browser).

## 5. CLI commands (Rust modules, no external sqlite3)

```bash
gatewayd config.yaml                     # run the gateway

gatewayd --journal [--limit N] [--level info|warn|error] [--category NAME] \
         [--since 10m|6h|1d|2w|1mo] [--db PATH]
# view the durable journal as an ASCII table (health alerts, disconnects, approvals)
# time is printed in UTC; --since parses s/m/h/d/w/mo

gatewayd --approvals                     # list agent approval statuses + fingerprints
gatewayd --approve <name>                # approve an agent (served after restart)
gatewayd --reject <name>                 # reject an agent
# an unapproved agent is not served (HTTP 404 unknown agent_id) and is logged
# to the journal under category "approval"

gatewayd --setup                         # interactive config generator
```

## 6. Protocol: ACP client → gateway (directions 1 and 3, TCP)

First line is a handshake:

```json
{"token":"t-dev-local-001","agent_id":"claurst-main"}
```

Then regular ACP JSON-RPC, newline-delimited (`\n`); replies come back the same
way:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"acp-...","prompt":[{"type":"text","text":"hi"}],"contexts":[],"files":[]}}
```

Direction 1 (ACP→ACP) is full passthrough: each agent `session/new` creates a
session in the **same process** (verified: claurst and hermes return different
`sessionId`s for two consecutive `session/new` calls).

## 7. Protocol: A2A client → gateway (directions 2 and 4, HTTP)

Agent card:

```
GET http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json
Authorization: Bearer <token>
```

`AgentCard.url` is built from `config.public_url` + `agent_id`:
`https://gateway.example.com/agents/<agent_id>/rpc`.

RPC (direction 4 — A2A client → ACP agent):

```
POST http://127.0.0.1:8348/agents/<agent_id>/rpc
Authorization: Bearer <token>
Content-Type: application/json
```

`message/send` starts a conversation:

```json
{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"hello"}]}
}}
```

Response:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "context_id":"ctx-...",
  "id":"task-...",
  "status":{"state":"completed","message":{...}},
  "artifacts":[{"name":"response","parts":[{"kind":"text","text":"..."}]}]
}}
```

### Direction 2: A2A client → A2A agent (reverse proxy)

```
POST http://127.0.0.1:8348/a2a-proxy/<agent_id>/<path>?<query>
Authorization: Bearer <token>
Content-Type: application/json
Accept: application/json        # or text/event-stream for SSE
```

- The agent must be `transport: http` — otherwise `400 agent_id is not an
  A2A/http agent`.
- The request is proxied as-is, without semantic transformation, together with
  the query string and SSE streams (`text/event-stream`).
- The path is normalized: `..`, `.`, double slashes are stripped — the request
  cannot escape the agent's address.
- Body limit 32 MiB, timeouts 300s/10s.
- The agent's `push_token` goes into `Authorization: Bearer` upstream.

### Continuing a conversation (contextId)

`contextId` is passed in the `message.contextId` field (or `params.contextId`),
**not** in `configuration`:

```json
{"jsonrpc":"2.0","id":2,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"continue"}],"contextId":"ctx-..."}
}}
```

- A conversation is bound to a token: another client's `contextId` is rejected
  (`contextId belongs to another client`) — IDOR is closed.
- Without `contextId` the gateway starts a new conversation and returns a fresh
  `context_id`.
- A nonexistent `contextId` is **not** rejected but started anew — by design.
- A session lives 24h of idle (`DEFAULT_SESSION_TTL`), cap
  `MAX_SESSIONS_PER_AGENT = 256` per agent.
- Task attribution survives session eviction and gateway restart (`StoredTask
  { owner, task }` in the TaskStore).
- Agent death: the supervisor respawns the process (5s backoff); talking to a
  conversation whose session belonged to a previous process generation gives
  `ContextLost` (`-32010` / HTTP 409); retrying with the same `contextId` starts
  a fresh session and works. A foreign `contextId` after a restart is refused by
  owner, not `ContextLost`.

## 8. Streaming (directions 3 and 4)

`message/send` with streaming capable agents returns SSE events for direction 4
and newline-delimited `session/update` notifications for direction 3:

- `first_chunk_timeout_secs` / `idle_chunk_timeout_secs` guard the stream loop;
  a per-agent `max_concurrent_streams` semaphore enforces a cap (fail-closed).
- Durable events are appended to `event_log` with a monotonic per-task `seq`.
- `tasks/get-last-seq` returns the last sequence for a task; `tasks/resubscribe`
  replays events after a given `seq` from the event log as an SSE stream — a
  client that disconnected mid-stream can rejoin the task.

## 9. Known limitations and open problems

| # | Problem | Status |
|---|---|---|
| 1 | `tasks/resubscribe` is implemented for HTTP; the TCP line protocol has no resubscribe RPC | 🟢 open question |
| 2 | `tasks/get`, `tasks/cancel` are unit-tested but not exercised over live HTTP on a staging box | ⏳ |
| 3 | `tasks/resubscribe` TECH_DEBT entry (2026-08-18) is stale — the feature is implemented (Phase 3.2) | 🟢 doc debt |
| 4 | HMAC token hash — key comes from `{env:GATEWAY_HMAC_KEY}`, default `default-dev-key-do-not-use-in-prod` | ✅ closed |

## 10. Where things live

| File | Purpose |
|---|---|
| `gatewayd/src/main.rs` | Config, Registry, `{env:...}`, validation, startup, CLI dispatch |
| `gatewayd/src/cli.rs` | Rust CLI: `--journal`, `--approvals`, `--approve`/`--reject`, tables, UTC formatter |
| `gatewayd/src/approvals.rs` | SQLite approval store (pending/approved/rejected, fingerprints) |
| `gatewayd/src/journal.rs` | Durable journal writer + `query_recent` (level/category/since filters) |
| `gatewayd/src/event_log.rs` | Durable event buffer with per-task `seq`, replay for resubscribe |
| `gatewayd/src/health.rs` | Periodic DB size + stream usage summary |
| `gatewayd/src/config.rs` | `RawConfig`, optional sections (`event_log`, `task_store`, `journal`, `health`, `approvals`) |
| `gatewayd/src/setup.rs` | Interactive `--setup` config wizard |
| `gatewayd/src/transport_tcp.rs` | TCP: directions 1 (ACP passthrough) and 3 (ACP→A2A) |
| `gatewayd/src/transport_http.rs` | HTTP: direction 4 (`/agents/:id/rpc` + agent.json), contextId, AgentCard.url, resubscribe |
| `gatewayd/src/transport_a2a_passthrough.rs` | HTTP: direction 2 (A2A→A2A reverse proxy, `/a2a-proxy/:id/*path`) |
| `core/src/convert.rs` | `AcpAsA2a` (A2A→ACP) and `A2aAsAcp` (ACP→A2A), sessions, owner |
| `core/src/owner.rs` | `Owner` (Token hash / Anonymous) |
| `core/src/task_store.rs` | `TaskStore` + `StoredTask{owner, task}` |
| `core/src/stdio_agent.rs` | ACP agent process, JSON-RPC by id, session/update |
| `core/src/lease.rs` | `TurnLease` — serializes prompts per session |
| `core/src/http_agent.rs` | A2A agent over HTTP (ops-agent) |
