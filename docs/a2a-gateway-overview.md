# A2A Gateway Description `ACP-A2A_gateway`

> **Language:** English · [Русская версия](a2a-gateway-overview-ru.md)

**Repository:** `GG-QandV/ACP-A2A_gateway`
**Deployment:** host address, systemd service name and public domain live in the private
deployment notes, not in this repository.
**Role:** intermediary A2A client → ACP agents behind it.

## 1. Architecture

```
A2A client (curl / adapterd / any)
   │  HTTP JSON-RPC (see below)
   ▼
gatewayd (transport_http.rs)
   ├─ GET  /agents/:id/.well-known/agent.json   → agent card
   ├─ POST /agents/:id/rpc                        → JSON-RPC (methods below)
   │     └─ dispatch_a2a_method → AcpAsA2a → SupervisedStdioAgent → child (hermes acp)
   └─ (TCP 127.0.0.1:8347 — pure ACP proxy, not published externally)
```

- Agents **spawn lazily** on first access to their `:id`, live until they crash.
- Authentication: `Authorization: Bearer <token>` (tokens from `/srv/gateway/env`,
  checked before the agent spawns).

## 2. Wire format (what the gateway returns and accepts)

### 2.1 Routes

| Path | Method | What it returns |
|---|---|---|
| `/agents/{id}/.well-known/agent.json` | GET | agent card (AgentCard) |
| `/agents/{id}/rpc` | POST | JSON-RPC 2.0 response |

### 2.2 JSON-RPC methods (POST /rpc)

| Method | Params | `result` response |
|---|---|---|
| `message/send` | `{ message: { role, parts, contextId? } }` | **flat Task** (no `task` wrapper) |
| `tasks/get` | `{ id: "<task_id>" }` | flat Task |
| `tasks/cancel` | `{ id: "<task_id>" }` | flat Task |
| (everything else) | — | `error: method_not_found` |

### 2.3 Task format (flat, not `{task: ...}`)

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

- **TaskId / ContextId** — plain strings `task-...` / `ctx-...` (not UUIDs).
- **state** — `#[serde(rename_all = "kebab-case")]` lowercase: `completed`,
  `failed`, `canceled`, etc. (from `protocol/src/a2a.rs:43-44`).

### 2.4 Message / Part format

```json
{ "role": "user", "parts": [ {"kind":"text","text":"..."} ] }
```

- `role` — lowercase `user` / `agent` (`#[serde(rename_all="lowercase")]`).
- `part` — tag `kind`, lowercase: `{"kind":"text","text":...}`,
  `{"kind":"file","file":{uri,bytes,mime_type}}`, `{"kind":"data","data":...}`
  (`protocol/src/a2a.rs:97-118`).

### 2.5 Agent card (agent.json)

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

### 2.6 Errors

| Code | Meaning |
|---|---|
| `-32010` + HTTP 409 | context lost (agent restart) — the client must start over |
| `-32000` | other application error |
| `-32601` | method not found |

### 2.7 Confirmed with live requests (2026-08-17)

- `message/send` → `result` = flat Task with `state: "completed"`, artifact
  `parts: [{"kind":"text","text":"Hello"}, ...]` (hermes replied).
- `tasks/get` → the same flat Task (artifact with the response text).
- Card: `{name, description:null, version, url, capabilities, skills:[]}`.

## 3. Known limitations

- Only 3 methods (`message/send`, `tasks/get`, `tasks/cancel`).
- No `SendMessage`/`GetTask` (camelCase), no proto fields `TASK_STATE_*`,
  no `{task: ...}` wrapper.
- Streaming (`SendStreamingMessage` / SSE) is not implemented ("Phase 1").
- Multi-turn: the second `message/send` into the same session hangs until
  `agent_call_timeout_secs` (known upstream bug, `TECH_DEBT.md`).
- Token hash — not cryptographic (`DefaultHasher`).
