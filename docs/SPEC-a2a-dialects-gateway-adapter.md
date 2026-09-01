# TZ: A2A Dialects (SDK/Spec) in the Gateway and the Adapter + a Shared Dialect Probe

> **Language:** English · [Русская версия](SPEC-a2a-dialects-gateway-adapter-ru.md)

- **Status:** draft (pending owner approval). The code is not changed.
- **Date:** 2026-08-17
- **Products:** `ACP-A2A_gateway` (the gateway), `agent-connector` (the adapter-connector).
- **Merges:** `ACP-A2A_gateway/docs/SPEC-add-adapterd-wire-format.md` (Section 1),
  `agent-connector/docs/design/TZ-driver-a2a-wire-format.md` (Section 2),
  and the dialect probe from `A2A-protocol-strategy-2026.md` §9.2 (Section 3).
- **Goal:** the baseline A2A dialect is **SDK (v1.0/ProtoJSON)**, the fallback is **Spec (pre-1.0)**;
  both products work; a client speaking either one is correctly identified
  by the shared probe request.

---

# Section 1. The `ACP-A2A_gateway` Gateway: Accepting the SDK Wire Format (adapterd)

## 1.1 Context

`driver-a2a-client` (in `agent-connector`) is written for the wire format of the **JSON-RPC layer
of the `a2a-rs` SDK** (method `SendMessage`, proto-serialized fields, `{task: ...}` wrapper).
The gateway currently replies only in its own semantic format (`message/send`,
flat Task, lowercase). For `adapterd` ↔ gateway to work "out of the box", the gateway
needs to accept/emit the SDK format **in parallel** with its own (at the client's choice).

## 1.2 What to Add

### 1.2.1 Input: Accept the `SendMessage` Method (camelCase) on the Same `/rpc`

Currently `dispatch_a2a_method` matches `"message/send"`, `"tasks/get"`,
`"tasks/cancel"`. Add aliases for the SDK method names:

| SDK method (a2a-rs) | Gateway analogue |
| ------------------- | ---------------- |
| `SendMessage`       | `message/send`   |
| `GetTask`           | `tasks/get`      |
| `CancelTask`        | `tasks/cancel`   |

Plus — possibly — `ListTasks`, `SubscribeToTask` (if needed for compatibility;
in the MVP — only the first three, mirroring the current set).

**Source of the names:** `a2a/src/jsonrpc.rs:138-148` (SDK, `methods`).

### 1.2.2 Input: Deserialize the `SendMessage` Parameters in Proto Format

The SDK client sends `message` in proto form:

```json
{ "message": { "role": "ROLE_USER", "parts": [ {"text": "..."} ] } }
```

The gateway currently expects `role: "user"`, part `{"kind":"text",...}`. Normalization is
needed **on input**: recognize both variants and reduce them to the internal `protocol::a2a::Message`:

- `role`: `ROLE_USER`/`user` → `User`; `ROLE_AGENT`/`agent` → `Agent`.
- part:
  - SDK `{"text": "..."}` → internal `Part::Text`
  - SDK `{"raw": <base64>}` / `{"url": "..."}` → `Part::File` (or `Data`)
  - gateway's `{"kind":"text","text":"..."}` → as it is now
- The SDK may not send `kind` — protojson format.

> Implementation: `fn normalize_message(Value) -> protocol::a2a::Message`,
> try the SDK layout first, fall back to the current one on failure.

### 1.2.3 Output: Return the Task as `{task: ...}` + `TASK_STATE_*` + proto parts

When the client called an SDK method (`SendMessage`) — the response must be in SDK format
so that `driver-a2a-client` (which expects `result.task`) can parse it:

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

The transformation (from the internal `Task`):

- `id` → `id` (the string stays `task-...`; the SDK TaskId is a string).
- `context_id` → `contextId` (camelCase).
- `status.state` → `TASK_STATE_<UPPER>` (see the serde in `a2a/src/types.rs`).
- `message.message_id` → `messageId`.
- `message.role` → `ROLE_AGENT` / `ROLE_USER`.
- part `{kind:"text",text}` → `{text}`; `{kind:"file",file}` → `{url|raw}`;
  `{kind:"data",data}` → `{data}`.
- The `{ "task": ... }` wrapper — **mandatory** (the SDK client expects it).
- `artifacts` — the `artifact_id` field (the SDK expects `artifact_id`, same as in the gateway).

> **Important:** the SDK format on output — only for SDK requests. For
> `message/send` requests (the gateway's own format) the response stays flat, so as
> not to break existing gateway clients.

### 1.2.4 How to Tell the Client's Format Apart

By the **method name of the request**:

- `SendMessage` / `GetTask` / `CancelTask` → SDK format (input normalized,
  output in `{task:...}` + `TASK_STATE_*`).
- `message/send` / `tasks/get` / `tasks/cancel` → the current semantic format
  (unchanged).

This is deterministic: a client cannot "switch" the format mid-session.

## 1.3 Internal Normalization Flow

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

## 1.4 Files to Change (in the gateway repo)

| File                             | Edit                                                                                             |
| -------------------------------- | ------------------------------------------------------------------------------------------------- |
| `gatewayd/src/transport_http.rs` | add `SendMessage`/`GetTask`/`CancelTask` to `dispatch_a2a_method`; select the renderer by method |
| `gatewayd/src/transport_http.rs` | `build_task_from_send_params` — normalization of the SDK/semantic message                        |
| `protocol/src/a2a.rs`            | (opt.) helpers `role_to_sdk`, `part_to_sdk`, `state_to_sdk` — or in `transport_http.rs`          |
| `gatewayd/src/transport_http.rs` | `render_task_sdk` (`{task}` wrapper + `TASK_STATE_*` + proto parts)                              |

Neither `protocol-acp` nor `core` changes — the SDK format concerns only
the A2A boundary (HTTP input/output).

---

# Section 2. The `agent-connector` Adapter: `driver-a2a-client` with Two Wire Formats

## 2.1 Context

`driver-a2a-client` is written for **one** wire format — the JSON-RPC layer of the official
SDK `a2a-rs` (method `SendMessage`, proto fields). A live check showed that
the `ACP-A2A_gateway` gateway implements a **different** wire format (method `message/send`,
flat Task, lowercase states), which the driver does not understand.

### Key fact: there are two wire "standards"

The official SDK `a2a-rs` itself provides **two** wire representations:

| SDK layer | Wire                             | Source                                                                                                                                 |
| --------- | -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| REST      | path `/message:send`             | `a2a-client/src/rest.rs:17` (`REST_SEND_MESSAGE_PATH`)                                                                                 |
| JSON-RPC  | method `SendMessage` + proto fields | `a2a/src/jsonrpc.rs:138` (`methods::SEND_MESSAGE`) + `a2a-server/src/jsonrpc.rs:73` (`protojson_conv::from_value::<SendMessageRequest>`) |

The gateway implements a third kind — **semantic** JSON-RPC per the A2A spec (method
`message/send`): `gatewayd/src/transport_http.rs:254`, its own types in
`protocol/src/a2a.rs`.

The result — **two** wire kinds are relevant for the driver:

1. **A2aSdkJsonRpc** — method `SendMessage`, proto fields (`TASK_STATE_*`, `{text}`,
   `ROLE_USER`). This is the format of our `protocol-a2a-server` (built on the same SDK).
2. **A2aSpecJsonRpc** — method `message/send`, semantic fields
   (`completed`, `{kind: text}`, `user`, flat Task). This is the gateway's format.

The driver must be able to work with both — selectable at the endpoint level.

## 2.2 Goal

Make `driver-a2a-client` **wire-format-neutral**: able to send a request
and parse a response in either of the two formats, selected by the agent's configuration.
This lets a single driver connect both to our `adapterd` (SDK format)
and to the gateway (spec format) — without code duplication.

## 2.3 Exact Comparison of the Two Formats (per code)

### 2.3.1 Request Method

|                 | SDK format               | Spec format                                          |
| --------------- | ------------------------ | ---------------------------------------------------- |
| JSON-RPC method | `"SendMessage"`          | `"message/send"`                                     |
| Source          | `a2a/src/jsonrpc.rs:138` | `ACP-A2A_gateway/gatewayd/src/transport_http.rs:254` |

> **Why you cannot "guess" on the sending side:** the server accepts exactly one method
> name. `SendMessage` → our adapterd; `message/send` → the gateway. A wrong choice =
> `method not found` (confirmed by a live test: adapterd returned `-32601
> METHOD_NOT_FOUND` for `message/send`).

### 2.3.2 Request Parameters `message`

Both sides accept a `{ message: { role, parts } }` object at the top level.
The difference lies inside `part` and `role`:

| Field           | SDK format                                                                        | Spec format                                           |
| --------------- | --------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `role`          | `"ROLE_USER"`                                                                     | `"user"`                                              |
| text part       | `{ "text": "..." }`                                                               | `{ "kind": "text", "text": "..." }`                   |
| resource part   | `{ "url": ..., "media_type": ... }`?                                              | `{ "kind": "resource", "uri": ..., "mimeType": ... }` |
| Source          | protojson (SDK deserialization: `unknown field \`kind\`` on `{kind}` — live test) | `protocol/src/a2a.rs` (`Message`, `Part` with `kind`) |

> Live confirmation of the SDK-side incompatibility: adapterd, on part
> `{ "kind": "text" }`, returned `-32700 PARSE_ERROR: unknown field \`kind\``.
> Conversely, the gateway expects exactly `{ kind, text }`.

### 2.3.3 Response `SendMessageResponse` / Task

| Aspect       | SDK format                                             | Spec format                               |
| ------------ | ------------------------------------------------------ | ----------------------------------------- |
| Wrapper      | `{ "task": { ... } }`                                  | flat `{ id, context_id, status, ... }`    |
| State        | `"TASK_STATE_COMPLETED"`                               | `"completed"`                             |
| message.role | `"ROLE_AGENT"`                                         | `"agent"`                                 |
| part         | `{ "text": ... }`                                      | `{ "kind": "text", "text": ... }`         |
| Source       | `a2a/src/types.rs` (`TaskState` serde: `TASK_STATE_*`) | `transport_http.rs` (flat Task)           |

> Live confirmation: the driver (looking for `result.task`) got from the gateway a flat
> Task in `result` → error `A2A response missing task: no task in result`.
> And conversely, `a2a::TaskState` will not parse `"completed"` (it expects
> `"TASK_STATE_COMPLETED"`).

## 2.4 Design

### 2.4.1 Configuration: New Field `wire_format`

In `A2aClientConfig` (crates/driver-a2a-client) add:

```rust
/// Wire format of the A2A agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum A2aWireFormat {
    /// Official a2a-rs SDK JSON-RPC layer: SendMessage method, proto fields.
    #[default]
    Sdk,
    /// Semantic A2A JSON-RPC (ACP-A2A_gateway gateway): message/send method.
    Spec,
}
```

In the `A2aClient` variant of `adapterd-config` — a field:

```yaml
- id: hermes
  driver: a2a-client
  endpoint: https://agentmesh-labs.mnemostroma.com/agents/hermes/rpc
  wire_format: spec          # sdk (default) | spec
```

Deserialization: `#[serde(default)]` + a string enum (`sdk`/`spec`). Absence of
the field → `sdk` (backward compatibility, current behavior).

### 2.4.2 Internal Structure: Two Wire Modules

No shared `if wire_format` branches scattered throughout the code:

```
crates/driver-a2a-client/src/
├── lib.rs            # AgentDriver impl, dispatch by wire_format
├── wire/mod.rs       # trait A2aWire { method(); build_message(); parse_task() }
├── wire/sdk.rs       # A2aSdkWire  — SendMessage + protojson + a2a::Task
└── wire/spec.rs      # A2aSpecWire — message/send + semantic fields
```

```rust
trait A2aWire: Send + Sync {
    fn jsonrpc_method(&self) -> &'static str;
    fn build_message(&self, input: &[Part], task_id: Option<&TaskId>) -> Value;
    /// Returns the normalized Task (the single internal type), or an error.
    fn parse_response(&self, result: &Value) -> Result<NormalizedTask, A2aClientError>;
}
```

`A2aClientDriver` holds `wire: Box<dyn A2aWire>` (or an enum), selected in
`A2aClientDriver::new(config)`.

### 2.4.3 The Single Internal Type `NormalizedTask`

```rust
struct NormalizedTask {
    id: String,
    state: NormalizedState,      // Working | InputRequired | Completed | Failed | Cancelled
    message: String,
    output_parts: Vec<Part>,
}
```

- `A2aSdkWire::parse_response` — deserializes `a2a::Task` (typed),
  maps `a2a::TaskState` → `NormalizedState`.
- `A2aSpecWire::parse_response` — parses the flat Value manually
  (`completed`/`failed`/`canceled`/`inputRequired` lowercase),
  parts `{kind,text}` → `Part`.

`invoke()` works only with `NormalizedTask` → `DriverEvent`.

> **Rationale for the single type:** `AgentDriver::invoke` must emit
> `DriverEvent` — one set of events. The wire format is a transport detail
> and must not leak into the driver's logic.

### 2.4.4 cancel / provide_input

- `cancel` — a local `CancellationToken` (format-independent) + best-effort
  HTTP cancel. For the SDK format the cancel signal is a separate call (`tasks/cancel`).
- `provide_input` — a repeated `message/send` with the same `task_id`/`taskId` through
  the selected wire. Field names depend on the format (`task_id` vs `taskId`),
  so `build_message` already takes `task_id` and decides itself where to put it.

## 2.5 Error Mapping

| Situation                                                                     | Result                                                                           |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| the server returned `error` (JSON-RPC)                                        | `DriverEvent::Failed` with code `a2a_remote_error`                                 |
| `result` missing / no task                                                    | `DriverEvent::Failed` `a2a_no_task`                                                |
| unsupported wire (future formats)                                             | error in `new()`, the agent does not start                                         |
| format mismatch (`SendMessage` sent, server expected `message/send`)          | `-32601 METHOD_NOT_FOUND` → `DriverEvent::Failed`; the log carries a hint about `wire_format` |

## 2.6 Tests (Section 2)

1. **Unit: sdk-wire** — `build_message` yields `SendMessage` + `ROLE_USER` +
   part `{text}`; `parse_response` from `{task:{...}, TASK_STATE_COMPLETED}` → `NormalizedTask`.
2. **Unit: spec-wire** — `build_message` yields `message/send` + `user` +
   part `{kind,text}`; `parse_response` from a flat `{completed}` → `NormalizedTask`.
3. **Contract (mock axum server)** — two mock servers (SDK format and spec format),
   a full `invoke` → `Completed` through each wire.
4. **Live E2E** (after the build, manually/by script):
   - our `adapterd` (SDK format) ← `driver-a2a-client` with `wire_format: sdk`
   - the hermes gateway (spec format) ← `driver-a2a-client` with `wire_format: spec`

DoD: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`.

---

# Section 3. Shared Dialect Probe: a Request to Determine the Protocol and Dialect

A shared key subtask for both sections: **a short initial request
that immediately shows which protocol/dialect the client can communicate over**
(SDK / Spec / ACP / unknown).

## 3.1 Principle

The probe must be **idempotent** — it must not create tasks and must have no side
effects. We use `GetTask`/`tasks/get` with a deliberately nonexistent `task_id`
(a random UUID), and **not** `SendMessage`/`message/send` (those create a real task).

## 3.2 Detection on Input (Server Side, Both Products)

```
1. Accept the first request to the agent.
2. Determine the dialect by method name:
     SendMessage | GetTask | CancelTask | ListTasks → SDK (v1.0)
     message/send | tasks/get | tasks/cancel       → Spec (pre-1.0)
     otherwise                                     → ACP/other → see step 5
3. If the method is recognized — answer in the same dialect (parser/renderer by method).
4. Additionally, for clients that have not made a single call yet:
   GET /.well-known/agent.json → protocolVersion ("1.0" → SDK, "0.x" → Spec).
   This is the preferred detection channel (no probe needed).
5. If the method is recognized by no dialect → return method_not_found
   with a hint about the known dialects (SDK/Spec) and a link to the strategy.
```

## 3.3 Probe (Client Side, If the Agent Card Is Unavailable)

```
POST /agents/:id/rpc
{ "jsonrpc": "2.0", "id": 1, "method": "GetTask",
  "params": { "name": "tasks/<uuid>" } }            # SDK style
```

Interpretation of the response:

| Response                                                             | Verdict                                       |
| ------------------------------------------------------------------ | -------------------------------------------- |
| `result` (or a "task not found" error without `method_not_found`)  | the server understands **SDK** → work over SDK   |
| `-32601` / `-32000` + `method_not_found:`                          | not SDK → try Spec:                          |
| `POST ... { "method": "tasks/get", "params": { "id": "<uuid>" } }` |                                              |
| a "task not found" error                                           | the server understands **Spec** → work over Spec |
| `method_not_found` also for `tasks/get`                            | not A2A → try ACP (another interface)        |
| ACP not recognized either                                          | explicit error: "client dialect not determined" |

Caching: the detection result is stored **per endpoint** (one probe on the first
contact); repeated requests do not invoke the probe again.

## 3.4 Application in the Products

- **Gateway (Section 1):** dialect detection is already determined by the method name
  (§1.2.4). The probe is not needed on input — it is needed for the gateway's *own*
  outbound calls, if the gateway itself reaches third-party agents (client side, §3.3).
- **Adapter (Section 2):** `wire_format: auto` (a new enum value) — on the first
  contact with the endpoint a probe runs (§3.3), the result is cached, and the choice of
  `A2aSdkWire`/`A2aSpecWire` is made accordingly. On ambiguity the priority is
  **SDK**.

## 3.5 Probe DoD

- [ ] the probe creates no tasks (only `GetTask`/`tasks/get` with a nonexistent id);
- [ ] detection via the Agent Card (`protocolVersion`) takes precedence over the probe;
- [ ] dialect cache per endpoint;
- [ ] SDK priority on ambiguity;
- [ ] a clear error listing the supported dialects if none was determined.

---

# Common Acceptance Criteria

1. `cargo test --workspace` (both repos) — green.
2. Live E2E: `adapterd` (driver-a2a-client) → gateway → hermes: `invoke` →
   `Completed` (hermes text) — via both SDK and Spec.
3. Regression: existing gateway clients (semantic format) are untouched.
4. The probe leaves no tasks on the server and has no side effects.
