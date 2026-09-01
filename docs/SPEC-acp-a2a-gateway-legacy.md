# TZ: Universal ACP Gateway + A2A ↔ ACP Converter

> **Language:** English · [Русская версия](SPEC-acp-a2a-gateway-legacy-ru.md)

**Status:** draft
**Version:** 0.1
**Date:** 2026-08-07

---

## 1. Goal

Create a universal network layer on top of the ACP (Agent Client
Protocol) that solves two tasks:

1. **ACP Gateway** — any ACP client (Zed, Neovim, VS Code, JetBrains,
   etc.) can connect to any ACP agent over the network, not only to a
   local stdio process. Access is restricted by a token.
2. **A2A ↔ ACP converter** — a bidirectional bridge between the
   Google A2A (Agent-to-Agent, HTTP JSON-RPC) and ACP (JSON-RPC over stdio) ecosystems:
   - A2A client → ACP agent;
   - ACP client → A2A agent.

Both components are **agnostic**: not tied to any specific agent.
Any client with ACP support can connect to any agent with ACP support;
any A2A agent talks to any ACP agent and vice versa.

---

## 2. Terms and Standards

| Term                   | Definition                                                                                                                              |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| **ACP**                | Agent Client Protocol (https://agentclientprotocol.com). JSON-RPC 2.0, newline-delimited, over stdio. The client spawns the agent as a subprocess. |
| **A2A**                | Google Agent-to-Agent protocol. JSON-RPC 2.0 over HTTP(S), discovery via `/.well-known/agent.json` (AgentCard), streaming via SSE.       |
| **Gateway**            | A server that accepts networked ACP connections and proxies them to target agents.                                                       |
| **Converter (bridge)** | A component that translates ACP ↔ A2A requests/events.                                                                                 |
| **Agent**              | Any ACP- or A2A-side that executes prompts.                                                                                             |
| **Client**             | Any side that sends prompts to an agent.                                                                                                |
| **Token**              | A secret (Bearer) restricting access to the gateway/converter.                                                                          |

### 2.1 ACP Methods (relevant)

| Method                       | Direction   | Description                              |
| ---------------------------- | ------------- | ----------------------------------------- |
| `initialize`                 | C → A       | Capability negotiation, protocol version  |
| `authenticate`               | C → A       | Auth (a no-op for claurst — local credentials) |
| `session/new`                | C → A       | Create a session (cwd, MCP roster)       |
| `session/load`               | C → A       | Load an existing session                 |
| `session/prompt`             | C → A       | Execute a turn; streams `session/update` |
| `session/cancel`             | C → A (notif) | Cancel the current turn                 |
| `session/update`             | A → C       | Stream text/tool deltas                  |
| `session/request_permission` | A → C       | Request approval for a tool call         |
| `session/update_step`        | A → C       | Step update (optional)                   |

### 2.2 A2A Methods (relevant)

| Method                 | Direction | Description                                    |
| ---------------------- | ----------- | ------------------------------------------- |
| `agent/getCard`        | C → A     | Agent metadata (deprecated, → discovery) |
| `message/send`         | C → A     | Send a message, get the full response  |
| `message/reply`        | C → A     | Reply within an existing session                 |
| `message/stream`       | C → A     | The same, but SSE streaming                      |
| `task/send`            | C → A     | Create a task; the server works asynchronously   |
| `task/get`             | C → A     | Get the task state/artifacts                     |
| `task/cancel`          | C → A     | Cancel a task                                    |
| `task/resubscribe`     | C → A     | Subscribe to task updates                        |
| `task/status-update`   | A → C     | Push notification about the status               |
| `task/artifact-update` | A → C     | Push notification about artifacts                |
| `message/update`       | A → C     | Push update of a message                         |

A2A types: `AgentCard`, `Message` (parts: text/file/data/audio/video/image),
`Task` (id, sessionId, status: submitted/working/input-required/completed/
canceled/failed/unknown, artifacts), `Part`.

### 2.3 Semantic Mapping (target)

| ACP                               | A2A                                           |
| --------------------------------- | --------------------------------------------- |
| session (`session_id`)            | `sessionId`                                   |
| turn (`session/prompt`)           | `task` / `message`                            |
| text (`TextContent`)              | `TextPart`                                    |
| tool call (`ToolCallContent`)     | `DataPart` (structured JSON)                  |
| tool result (`ToolResultContent`) | `DataPart` (JSON)                             |
| image (`ImageContent`)            | `ImagePart`                                   |
| audio (`AudioContent`)            | `AudioPart`                                   |
| `session/update`                  | SSE `message/update` / `task/artifact-update` |
| `session/request_permission`      | `input-required` (input request)              |
| `session/cancel`                  | `task/cancel`                                 |

---

## 3. Architecture

```
                ┌────────────────────────────────────────────────┐
                │               ACP GATEWAY (core)               │
                │                                                │
  ACP client ──▶│  Transport A: TCP/WS/HTTP-SSE + stdio wrapper  │──▶ ACP agent A (stdio)
  (Zed etc.)    │  Auth: Bearer token                            │──▶ ACP agent B (stdio)
                │  Agent registry: token → agent                 │──▶ ACP agent C (network)
                │                                                │
                └────────────────────────────────────────────────┘

                ┌────────────────────────────────────────────────┐
                │              A2A ↔ ACP CONVERTER               │
                │                                                │
  A2A client ──▶│  A2A endpoint: HTTP JSON-RPC + SSE             │──▶ ACP agent (spawn/network)
                │  Auth: Bearer token                            │
                │                                                │
  ACP client ──▶│  ACP endpoint: stdio/TCP + wrapper             │──▶ A2A agent (HTTP)
                └────────────────────────────────────────────────┘
```

Both components share the **agent registry** — a single config describing the
available agents and their transport. The converter core is built on the
`AcpAgent` and `A2aAgent` interfaces, which gives universality (see §6).

---

## 4. Component 1: ACP Gateway

### 4.1 Purpose

Let an ACP client work with an agent that is not a local stdio process
of the client (a remote host, a shared agent pool, another user),
with token-based authorization.

### 4.2 Inbound Connection Transports

**All** of the listed transports are implemented; the config makes the choice:

1. **TCP** — newline-delimited JSON-RPC 2.0 (pure ACP canon over a
   socket). Port + TLS option.
2. **WebSocket** — the same JSON-RPC, WS frames. Convenient for browser
   clients and proxies.
3. **HTTP + SSE** — JSON-RPC requests via POST `/rpc`, responses and events
   via SSE. Convenient for ACP clients that support an HTTP transport.
4. **stdio wrapper** (`acp-gateway` as a CLI) — the client launches
   `acp-gateway --token <T>` instead of the agent; the wrapper accepts ACP over
   stdio and proxies it to the gateway over one of the network transports.
   This is the key to compatibility: **any** editor able to spawn an ACP
   agent gets network access without changes.

### 4.3 Authentication

- Bearer token in a header or in the first handshake message.
- For TCP/the stdio wrapper: the token is passed as an argument
  (`--token`) or an env var (`ACP_GATEWAY_TOKEN`).
- For WS/HTTP: `Authorization: Bearer <token>`.
- `authenticate` from the ACP canon stays a no-op: the actual check happens
  at the transport level upon connection.
- A token in the registry is bound to **one or more** target agents.

### 4.4 Agent Registry

A config file (JSON/YAML), `agents` section:

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

Routing rules: upon connecting with a token, the client selects the target
agent (explicitly in `initialize`/the first message, or the default from the token).

### 4.5 Proxying

The gateway relays requests/responses **1:1** (with no semantic
transformation):

- `initialize`, `authenticate`, `session/new`, `session/load`,
  `session/prompt`, `session/cancel` — proxied to the target agent.
- `session/update`, `session/request_permission`,
  `session/update_step` — retransmitted back to the client.
- `session_id` mapping: the gateway keeps a table of `external_id ↔
  internal_id`, since different agents may generate collisions.
- Timeouts: connection, idle, turn. Configured per agent.

### 4.6 Agent Lifecycle Management

- **stdio agents**: lazy-spawn on the first session, kill when the last session
  ends (or keep-alive per configuration). One session → one
  process; a session is pinned to a process.
- **network agents**: a pool of persistent connections with reconnection.
- **cancel/oversubscribe**: on `session/cancel` the client immediately gets
  a confirmation back; the cancellation is forwarded to the agent.

### 4.7 Security

- TLS for all network transports (option).
- Rate limiting at the token level.
- Prohibit running an agent with an arbitrary `cwd` from the network unless
  allowed by the config.
- Connection and token logging — without the tokens themselves.

---

## 5. Component 2: A2A ↔ ACP Converter

### 5.1 Direction A2A Client → ACP Agent

The converter brings up an A2A HTTP endpoint (`/.well-known/agent.json` +
`/rpc`) and, for incoming calls, spawns/connects to an ACP agent.

**Request mapping:**

| A2A                      | ACP                              | Details                                                   |
| ------------------------ | -------------------------------- | -------------------------------------------------------- |
| `agent/getCard`          | `initialize`                     | the Card is built from the agent's capabilities          |
| `message/send`           | `session/new` + `session/prompt` | each message is a new turn in the (previously created) session |
| `message/reply`          | `session/prompt`                 | the same ACP session                                     |
| `task/send`              | `session/new` + `session/prompt` | A2A `task.id` ↔ ACP `session_id`                         |
| `task/get`               | session transcript               | aggregate `session/update` events                        |
| `task/cancel`            | `session/cancel`                 | notification, immediate response                          |
| `task/resubscribe` / SSE | `session/update`                 | stream redirection                                       |

**Content mapping:**

| A2A Part          | ACP ContentBlock                                                                  |
| ----------------- | --------------------------------------------------------------------------------- |
| `TextPart`        | `TextContent`                                                                     |
| `DataPart` (JSON) | `ToolCallContent` / `ToolResultContent` (by schema, if the agent emits tool calls) |
| `ImagePart`       | `ImageContent`                                                                    |
| `AudioPart`       | `AudioContent`                                                                    |
| A2A `Artifact`    | aggregated session text                                                           |

**Permissions:** the converter translates the ACP `session/request_permission` into
the A2A `input-required` status (if the A2A client supports input) or
applies the policy from the config (`allow`/`deny`/`ask`).

### 5.2 Direction ACP Client → A2A Agent

The converter accepts an ACP connection (stdio wrapper or network transport,
as in §4.2) and talks to the A2A agent over HTTP.

**Request mapping:**

| ACP                          | A2A                                           | Details                                                                              |
| ---------------------------- | --------------------------------------------- | ----------------------------------------------------------------------------------- |
| `initialize`                 | `agent/getCard`                               | capabilities from the agent card                                                     |
| `session/new`                | —                                             | an internal session is created; the A2A `sessionId` is generated lazily on the first prompt |
| `session/prompt`             | `task/send`                                   | `session_id` ↔ A2A `sessionId`                                                      |
| `session/cancel`             | `task/cancel`                                 | —                                                                                   |
| `session/update`             | SSE `message/update` / `task/artifact-update` | the stream from the agent                                                            |
| `session/request_permission` | —                                             | not directly supported by A2A; the A2A agent decides itself                          |

**Content mapping:** the inverse of §5.1.

### 5.3 Sessions

- A table of `acp_session_id ↔ a2a_session_id ↔ task_id`.
- One A2A `task` = one ACP turn (there may be several `session/update`
  messages within a single turn).
- On `session/new` with a new `cwd` — a new internal session; old
  tasks do not inherit the context (in v1).

### 5.4 Streaming

- A2A → ACP: `message/stream`/`task/resubscribe` (SSE) are read line
  by line; each `task/artifact-update`/`message/update` is
  translated into a `session/update`.
- ACP → A2A: `session/update` is aggregated into artifacts; if the A2A client
  requested a stream — events are pushed over SSE.
- Backpressure: a cap on the event queue per connection.

### 5.5 Errors and Edge Cases

- ACP `session/load` not supported by the A2A agent → an honest
  `method_not_found`.
- A2A `input-required` (no permission on the agent side) → the converter
  answers with an error or waits for input per policy.
- A2A agent timeout → `session/update` with an error + a proper
  `stop_reason`.
- Idempotency of `task/send` by `id`.

---

## 6. Universality (Requirement on the Interfaces)

The converter and gateway core must not know about any specific agent. For this,
two trait-interfaces are introduced:

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

Concrete implementations:

- `StdioAcpAgent` (spawns `claurst acp` / `claude acp`, etc.);
- `GatewayAcpAgent` (client of a remote ACP gateway);
- `HttpA2aAgent` (client of a remote A2A server);
- the converters implement one trait via the other (adapters),
  which automatically yields "any ↔ any".

---

## 7. Non-Functional Requirements

1. **Reliability:** reconnection to the agent with exponential backoff;
   correct state handover after a reconnect; no event loss
   within the bounded queue.
2. **Performance:** real-time streaming (event latency
   < 200 ms on a local network); minimal proxying
   overhead (~1 ms per message is acceptable).
3. **Security:** tokens are never logged; TLS by default for
   network transports; rate limiting; id validation (path traversal).
4. **Observability:** structured logs (tracing), connection metrics
   (number of active sessions, errors, latency), a health endpoint.
5. **Configurability:** everything via config file + env (`{env:VAR}`
   substitution, as in claurst's settings.json).
6. **Testability:** mock agents for both protocols; E2E tests
   "real ACP client → gateway → real claurst".

---

## 8. Implementation Stages

1. **Stage 0 — dotenv in claurst** (precondition): loading `.env` from
   `~/.claurst/.env`/`$CLAURST_ENV_FILE` at startup, so that
   `{env:...}` resolves in all modes.
2. **Stage 1 — ACP Gateway MVP:** TCP transport + token + registry +
   1:1 proxying to one stdio agent (`claurst acp`).
   Criterion: Zed/`acp_e2e.py` works through `acp-gateway` with a token.
3. **Stage 2 — transports:** stdio wrapper, WS, HTTP+SSE; TLS; rate limit.
4. **Stage 3 — A2A ↔ ACP converter:** the A2A→ACP direction over the
   `StdioAcpAgent` implementations; tested via an A2A test client.
5. **Stage 4 — the reverse direction** ACP→A2A over `HttpA2aAgent`.
6. **Stage 5 — universality and stability:** the interfaces of §6,
   reconnection, metrics, health, edge-case polish.

---

## 9. Acceptance Criteria

1. `acp_e2e.py` (or Zed) through the gateway with a token gets `PONG` from
   `claurst acp`.
2. Two different ACP agents in the registry, selection by token.
3. An A2A client (`curl`/Python) sends `task/send` → receives text from
   `claurst acp`; `task/get` returns artifacts; `task/cancel` stops it.
4. An ACP client sends `session/prompt` → the converter creates a `task/send` to
   a mock A2A agent; `session/update` streams.
5. Wrong/missing token → refusal at the transport level.
6. Streaming: first-event latency < 200 ms; no event loss
   with the bounded queue.
7. All crates pass `cargo check --workspace` and clippy without warnings.

---

## 10. Open Questions

1. Default transport for the gateway (TCP vs WS vs HTTP+SSE) — the ACP
   canon does not define a network layer; it must be fixed in the spec.
2. Mapping ACP tool-call → A2A: A2A has no native tool-call
   contract. Using `DataPart` with a JSON schema — the schema must be approved.
3. `session/request_permission` in the A2A direction: the default policy
   (`allow`/`deny`/`ask`), and how to pass "input" back to the A2A agent.
4. The agent-registry schema (YAML vs JSON, where to store secrets).
5. Whether a separate crate `claurst-acp-gateway` is needed, or to extend
   `agent-acp`.

---

## 11. Related Materials

- The claurst ACP server: `crates/acp/` (implementation), `crates/agent-acp/`
  (headless binary).
- ACP spec: https://agentclientprotocol.com
- A2A spec: https://google.github.io/A2A/
- Remote Control bridge (do not confuse): `crates/bridge/` — it is a bridge to
  the claude.ai web UI, not an ACP gateway.
