# Spec: add the adapterd wire format (a2a-rs SDK) to the `ACP-A2A_gateway` gateway

> **Language:** English · [Русская версия](SPEC-add-adapterd-wire-format-ru.md)

> **SUPERSEDED:** merged into the unified spec
> `docs/SPEC-a2a-dialects-gateway-adapter.md` → Section 1. This file is kept as
> the original source; edits go into the merged document.

- **Status:** superseded (see above). Code was not changed.
- **Date:** 2026-08-17
- **Goal:** the gateway must understand the format used by `agent-connector`
  `adapterd` (the JSON-RPC layer of the official `a2a-rs` SDK), so that
  `adapterd` can call the gateway's agents through the existing
  `driver-a2a-client` without changing the driver.

---

## 1. Context

`driver-a2a-client` (in `agent-connector`) is written for the wire format of the
**JSON-RPC layer of the `a2a-rs` SDK** (the `SendMessage` method,
proto-style field serialization, the `{task: ...}` wrapper). The
`ACP-A2A_gateway` gateway currently responds only in its own semantic format
(`message/send`, flat Task, lowercase). For `adapterd` ↔ gateway to work "out
of the box", the gateway needs to accept/emit the SDK format **in parallel**
with its own (chosen by the client).

> Related document: `agent-connector/docs/design/TZ-driver-a2a-wire-format.md`
> (there too — a code-based comparison of the two formats on both sides).

---

## 2. What exactly needs to be added

### 2.1 Input: accept the `SendMessage` (camelCase) method on the same `/rpc`

Currently `dispatch_a2a_method` matches `"message/send"`, `"tasks/get"`,
`"tasks/cancel"`. Add aliases for the SDK method names:

| SDK method (a2a-rs) | Gateway equivalent |
|---|---|
| `SendMessage` | `message/send` |
| `GetTask` | `tasks/get` |
| `CancelTask` | `tasks/cancel` |

Plus — possibly — `ListTasks`, `SubscribeToTask` (if needed for compatibility;
for the MVP spec — only the first three, mirroring the current set).

**Source of the names:** `a2a/src/jsonrpc.rs:138-148` (SDK, `methods`).

### 2.2 Input: deserialize `SendMessage` parameters in proto format

The SDK client sends `message` in proto form:

```json
{ "message": { "role": "ROLE_USER", "parts": [ {"text": "..."} ] } }
```

The gateway currently expects `role: "user"`, part `{"kind":"text",...}`.
Normalization **on input** is needed: recognize both variants and reduce them
to the internal `protocol::a2a::Message`:

- `role`: `ROLE_USER`/`user` → `User`; `ROLE_AGENT`/`agent` → `Agent`.
- part:
  - SDK `{"text": "..."}` → internal `Part::Text`
  - SDK `{"raw": <base64>}` / `{"url": "..."}` → `Part::File` (or `Data`)
  - gateway `{"kind":"text","text":"..."}` → as now
- The SDK may not send `kind` — protojson format.

> Implementation: `fn normalize_message(Value) -> protocol::a2a::Message`,
> try the SDK layout first, on failure — the current one.

### 2.3 Output: return the Task in `{task: ...}` + `TASK_STATE_*` + proto parts

When the client called an SDK method (`SendMessage`) — the response must be in
SDK format so that `driver-a2a-client` (which expects `result.task`) can parse it:

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

Transformation (from the internal `Task`):
- `id` → `id` (the string stays `task-...`, SDK TaskId is a string).
- `context_id` → `contextId` (camelCase).
- `status.state` → `TASK_STATE_<UPPER>` (see serde in `a2a/src/types.rs`).
- `message.message_id` → `messageId`.
- `message.role` → `ROLE_AGENT` / `ROLE_USER`.
- part `{kind:"text",text}` → `{text}`; `{kind:"file",file}` → `{url|raw}`;
  `{kind:"data",data}` → `{data}`.
- The `{ "task": ... }` wrapper — **mandatory** (the SDK client expects it).
- `artifacts` — field `artifact_id` (the SDK expects `artifact_id`, as in the gateway).

> **Important:** SDK format on output — only for SDK requests. For
> `message/send` requests (the gateway's own format) the response stays flat,
> so as not to break existing gateway clients.

### 2.4 How to tell the client's format apart

By the **request method name**:
- `SendMessage` / `GetTask` / `CancelTask` → SDK format (normalize input,
  output in `{task:...}` + `TASK_STATE_*`).
- `message/send` / `tasks/get` / `tasks/cancel` → current semantic format
  (unchanged).

This is deterministic: the client cannot "switch" the format mid-session.

---

## 3. Internal normalization scheme

```
POST /agents/:id/rpc
  │
  ├─ method == "SendMessage" ──► normalize SDK-params → protocol::a2a::Message
  │                               → adapter.send_task_as(...)
  │                               → render Task → SDK format ({task, TASK_STATE_*})
  │
  ├─ method == "message/send" ─► (current path, unchanged)
  │
  ├─ GetTask / CancelTask  ───► aliases → tasks/get, tasks/cancel (SDK response)
  │
  └─ otherwise ───────────────► method_not_found
```

Two Task renderers:
- `render_task_semantic(Task) -> Value` (current, flat).
- `render_task_sdk(Task) -> Value` (`{task:{...}}` + `TASK_STATE_*` + proto parts).

---

## 4. Files to change (in the gateway repo)

| File | Edit |
|---|---|
| `gatewayd/src/transport_http.rs` | add `SendMessage`/`GetTask`/`CancelTask` to `dispatch_a2a_method`; select the renderer by method |
| `gatewayd/src/transport_http.rs` | `build_task_from_send_params` — normalization of SDK/semantic message |
| `protocol/src/a2a.rs` | (opt.) helpers `role_to_sdk`, `part_to_sdk`, `state_to_sdk` — or in `transport_http.rs` |
| `gatewayd/src/transport_http.rs` | `render_task_sdk` (the `{task}` wrapper + `TASK_STATE_*` + proto parts) |

Neither `protocol-acp` nor `core` changes — the SDK format concerns only the
A2A boundary (HTTP input/output).

---

## 5. Tests

1. **Unit:** `normalize_message` — SDK params (`ROLE_USER`, `{text}`) and
   semantic ones (`user`, `{kind,text}`) → one `protocol::a2a::Message`.
2. **Unit:** `render_task_sdk` — internal `Task` with `Completed` →
   `{task:{status:{state:"TASK_STATE_COMPLETED"}, message:{role:"ROLE_AGENT", parts:[{text}]}}}`.
3. **Contract:** POST `/rpc` with `SendMessage` (SDK body) → `result.task` with
   `TASK_STATE_COMPLETED`; and `message/send` → flat Task (regression of the current behavior).
4. **Live E2E:** `adapterd` (driver-a2a-client, `wire_format: sdk`) →
   gateway → hermes: `invoke` → `Completed` (hermes text).

Gateway DoD: `cargo test` in `ACP-A2A_gateway`, no regression for current
clients (the semantic format untouched).

---

## 6. Scope

- Medium, ~0.5–1 day. Only the gateway's A2A boundary; the core (`core`) is not touched.
- Does not require changing `driver-a2a-client` (it stays SDK-format).
- The parallel document `agent-connector/docs/design/TZ-driver-a2a-wire-format.md`
  describes the reverse option (adapting the driver) — in case it is decided
  to change the driver rather than the gateway. The decision is the owner's.
