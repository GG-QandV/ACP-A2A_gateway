# Streaming rollout plan for ACP-A2A_gateway: from "officially turn it on" to scaling

> **Language:** English · [Русская версия](stream-rollout-plan-ru.md)

Two separate stages, as requested: (1) close the architectural debt P-18/TECH_DEBT — actually implement
`Reply::Streaming` in code; (2) add configurable scale limits on top of the already-working mechanism.
None of this changes the signatures of `Reply<T,U>`, `AcpAgent`, `A2aAgent` — both stages fit within the
already-accepted seam (`docs/04-architecture-guide-extending.md`, decisions.md P-18).

---

## Stage 1 — officially enable streaming (close Phase 2)

Goal: `Reply::Streaming` stops being an unreachable variant and actually starts relaying
`session/update` ↔ `A2aEvent` through both converting directions (3 and 4). Direction 2
(A2A↔A2A reverse-proxy) is untouched — it already streams via SSE passthrough (P-18).

### 1.1 `core/src/stdio_agent.rs` — stop flattening chunks

Currently `UpdatesMap` accumulates a `Vec<ContentBlock>` and hands it over wholesale in `Reply::Complete`
after the turn finishes. It changes to a channel:

- `UpdatesMap` is replaced by `HashMap<SessionId, mpsc::UnboundedSender<SessionUpdate>>` — created upon
  entry into `prompt()`, not filled in after the fact.
- `collect_session_update()` forwards each parsed `session/update` straight into `tx.send(...)`,
  without accumulating into a `Vec`.
- `prompt()` returns `Reply::Streaming(rx)` **immediately after writing `session/prompt` to stdin**,
  without waiting for `call()`'s response. The final `PromptResponse` from `call()` is home-mapped into
  the last element of the stream (terminal event), closing the channel.
- Important (from P-18): when there is no stream — `anyhow::Result::Err`, no `unreachable!()`/`panic!()`,
  so that a single error does not take down the whole `tokio` worker of the network service.

### 1.2 `core/src/convert.rs` — real mapping instead of `unreachable!()`

Both directions get a body instead of a stub:

- `AcpAsA2a` (direction 4, A2A client → ACP agent): `match reply { Reply::Streaming(rx) => { ... } }`
  reads `SessionUpdate` from `rx` and maps it to `A2aEvent::TaskStatusUpdate`/`TaskArtifactUpdate` —
  the signature is nearly identical to `map_event()` from `protocol_a2a_mapper.rs` in the neighbouring
  project (there is already a ready mapping example `CoreEventKind → A2aStreamEvent`, reusable as a
  1-in-1 template with the types adapted).
- `A2aAsAcp` (direction 3, ACP client → A2A agent): the reverse mapping `A2aEvent → SessionUpdate`
  (`agent_message_chunk` for text chunks, the final status — a terminal notification).
- Both adapters pass `Reply::Streaming(rx)` further to the dispatcher without blocking — the converter
  itself must not wait for the channel to close, only forward it.

### 1.3 `gatewayd/transport_http.rs` — the `Reply::Streaming` branch on HTTP (direction 4)

- `axum` is already in the dependencies (`axum = "0.7"`, `gatewayd/Cargo.toml`) — use the built-in
  `axum::response::sse::Sse` + `axum::response::sse::Event`, no new crates.
  `Sse::new(stream)` wraps a `tokio_stream::wrappers::UnboundedReceiverStream` (the `tokio-stream`
  dependency must be added — the only genuinely new dependency across all of stage 1).
- The response to `tasks/resubscribe` — not JSON but `text/event-stream`, analogous to how SSE is
  already served in direction 2 (`transport_a2a_passthrough.rs`), except the data source is not
  upstream bytes but an `A2aEvent` serialized into an SSE frame by this same handler.

### 1.4 `gatewayd/transport_tcp.rs` — the `Reply::Streaming` branch on TCP (direction 3)

- Simpler than HTTP: the TCP connection is already line-based JSON-RPC. Each item from `rx` is
  serialized as an ACP `session/update` notification and written to the same client TCP socket
  newline-delimited — without additional abstractions over the existing read/write loop.

### 1.5 Tests (a mandatory part of the stage, not a separate stage)

- Unit test on `stdio_agent.rs`: a mock agent sends several `session/update`s before the final response —
  the test checks that `rx` receives them **one at a time**, not in one batch at the end (this is a direct
  regression against the current behavior described in P-18).
- Integration test modeled on the existing `docs/06-gateway-guide.md` §7 item 10 (live run for the
  reverse-proxy) — the same thing, but for direction 4: `tasks/resubscribe` actually receives several
  SSE events before `completed`.
- Negative control (per the project convention from `docs/decisions.md`, section "How this was written"):
  revert the fix and verify that the test really goes red, not that it is tautological.

**Stage 1 estimate**: matches the already-recorded "+3-4 days" estimate from
`docs/FINAL-ARCHITECTURE-minimal-reliable.md` §6 — the plan does not expand the scope, it only details it
per concrete files.

---

## Stage 2 — scaling and control via config

Goal: the very same limits discussed earlier (per-agent concurrency, timeout separation,
active-stream counter), but not hardcoded — parameters in `config.yaml`, changeable without a rebuild.

### 2.1 New config fields

Extension of the existing `agents:` section in `config.yaml` (not a new file — per the "extension
points" rule from `docs/04-architecture-guide-extending.md`, where "multiple agents per token"
is already described as "YAML edit only, code does not change"):

```yaml
agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    streaming:
      max_concurrent_streams: 4        # ceiling of concurrent Reply::Streaming for this agent
      first_chunk_timeout_secs: 15     # timeout BEFORE the first chunk (equivalent of the current agent_call_timeout)
      idle_chunk_timeout_secs: 120     # timeout BETWEEN chunks (does not block a long but live stream)
      buffer_capacity: 256             # mpsc channel capacity per stream (backpressure)

runtime:
  global_max_concurrent_streams: 64    # global ceiling for the gatewayd process, protection against resource exhaustion
```

Validation follows the same convention already in place for `{env:VAR}` and empty tokens (`docs/06-gateway-guide.md`
§3): a missing required field or a limit of `0` — a startup error, not a silent default.

### 2.2 `gatewayd/src/registry.rs` — `Semaphore` per-agent

- `AgentEntry` gains the field `stream_permits: Arc<tokio::sync::Semaphore>`, initialized from
  `streaming.max_concurrent_streams`.
- Before entering the `Reply::Streaming` branch the dispatcher takes `try_acquire` — on denial the client
  gets an explicit error (`503`/`-32000` "agent stream capacity exhausted"), not a silent hang. This is
  the same fail-closed principle already applied in `TurnLease` (`docs/decisions.md` — the project's
  general style: an explicit refusal is better than silent passage).
- The shared `global_max_concurrent_streams` — a second `Semaphore` in `main.rs`, common to all agents,
  checked first (before the per-agent one), so that degradation is predictable under systemic overload.

### 2.3 `core/src/stdio_agent.rs` — separate timeouts

- `call_timeout: Duration` (an already existing field) stays as is for the non-streaming path.
- `first_chunk_timeout: Duration` and `idle_chunk_timeout: Duration` are added, read from the same
  `streaming:` config section via `SpawnConfig` (analogous to how `call_timeout` is already plumbed
  there — `core/src/supervisor.rs::SpawnConfig`).
- Mechanics: `tokio::time::timeout(first_chunk_timeout, rx.recv())` for the first element,
  `tokio::time::timeout(idle_chunk_timeout, rx.recv())` for subsequent ones — the same pattern already
  used in `call()` for the non-streaming path, just with two different durations.

### 2.4 Observability (minimally necessary, not a separate stage)

- `tracing` events on: stream open/close, denial by `Semaphore`, `idle_chunk_timeout` firing. The
  project already logs respawns and degradations via `tracing::warn!` (`core/src/supervisor.rs`) — the
  new events follow the same style, without a separate metrics system.
- The active-stream counter per agent — not in `TaskStore` (that is about completed tasks and their
  retention), but as an atomic counter next to the `Semaphore` in `AgentEntry`, optionally readable for
  a future health/status endpoint.

**Stage 2 estimate**: 1.5–2 days — config extension and `Semaphore` plumbing, with no new business
mapping logic (that is already done in stage 1).

---

## Why exactly this order

Doing scaling before the streaming mechanism is pointless — there is nothing to limit while
`Reply::Streaming` returns an error. Doing both stages in one PR contradicts the project rule
"new capability = new file/diff, do not mix with another change" (`docs/04-architecture-guide-extending.md`,
section "Anti-patterns") and would complicate rollback: if a mapping bug surfaces in stage 1, it is easier
to fix without simultaneously debugging concurrency limits.
