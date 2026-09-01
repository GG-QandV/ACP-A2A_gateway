# Known limitations of driver-a2a-client (spec: docs/SPEC-a2a-dialects-gateway-adapter.md)

> **Language:** English · [Русская версия](KNOWN_LIMITATIONS-ru.md)

Status as of 2026-08-18. This document records what from the spec is fully implemented,
what was deliberately deferred (with a closure trigger), and what was not done at all.
It does not replace the spec itself; it serves as a closure checklist. The format and decisions
mirror `agent-connector/TECH_DEBT.md`.

## Fully done

- **§2.4.1** `wire_format: sdk | spec | auto` in `A2aClientConfig`.
- **§2.4.2** SDK/Spec separation via `trait A2aWire` (`wire/sdk.rs`, `wire/spec.rs`).
- **§2.4.4** `cancel`/`provide_input` via `remote_task_ids: DashMap<TaskId, String>`.
- **§2.5** Error codes `a2a_remote_error` / `a2a_no_task` via `send_error_to_a2a_code()` (commit `9166c52`).
- **§3.2** Detection at the gateway entry by JSON-RPC method name — in `ACP-A2A_gateway`
  (`transport_http.rs`: `SendMessage`/`GetTask`/`CancelTask` → SDK; `message/send`/`tasks/get`/`tasks/cancel` → Spec).
- **§3.3** The probe is idempotent (`GetTask`/`tasks/get` with a dummy `Uuid`, creates no tasks).
- **§3.3** Recognition of "method not found" — by code `-32601` (JSON-RPC 2.0 standard)
  OR by normalized text with several phrasings (D1, commit `9ba5bb9`).
- **§3.4** `wire_format: auto` — lazy resolution via `resolved_wire()` + `OnceCell`,
  D3 one-shot retry on MethodNotFound on a real call (commit `9ba5bb9`).
- **§3.4** Direction 2 (the gateway as a client to third-party A2A agents) —
  `ACP-A2A_gateway/gatewayd/src/dialect_probe.rs` (commits `669c390` + `1e4756d`),
  including D3 cache invalidation on MethodNotFound on a proxied request.
- **§3.5 DoD** SDK priority when ambiguous.
- **§3.5 DoD** A clear error on an unrecognized dialect (`ProtocolError`).

## Deliberate refusals / debt (with closure trigger)

### §2.4.3 Typed SDK parser (`a2a::Task`) — deliberate refusal
- **What**: `wire/sdk.rs::parse_task` parses the response manually via `serde_json::Value`
  (`get`/`as_str`/`as_array`), although the SDK provides typed `a2a::Task`/`TaskState`/`Part`.
- **Why a deliberate refusal (not unfinished work)**: the `a2a` crate (workspace dep, pinned `02ee560`) is
  **v0.3.0, pre-1.0**, with no `#[non_exhaustive]` on any type, only 3 commits in `types.rs`.
  The SDK may silently add an enum variant or a required field (for instance,
  `Task.context_id` has no `#[serde(default)]`) — the typed parser then breaks
  entirely, the manual one does not.
- **Impact**: low (working code, covered by tests); the risk — divergence from the format in future SDK versions.
- **Closure trigger**: a new SDK version ≥1.0 with `#[non_exhaustive]` → switch to `a2a::Task`
  in `sdk.rs` (check `context_id`).

### §3.2 item 4 / §3.5 DoD — "AgentCard takes priority over the probe" — cancelled by the owner
- **What**: `detect_from_agent_card` in `dialect_probe.rs` always returns `None`.
- **Why**: the `protocolVersion` → wire-dialect mapping is semantically wrong (protocol
  version ≠ wire-implementation choice); the AgentCard spec contains no field that reliably
  distinguishes the wire implementation. Owner's decision: the item is cancelled; the key dialect
  determination is the probe (`probe_wire_format`), and it is correct. The `card → probe` order in
  `resolve_auto_wire()` is kept as an extension point.
- **Impact**: none (the card does not participate in resolution; the probe decides everything correctly).
- **Closure trigger**: a new AgentCard spec version with a field distinguishing the wire implementation
  → implement detection in `detect_from_agent_card`; the order in `resolve_auto_wire()` is already ready.

### D3 — full invalidation of the `OnceCell` cache — partial
- **What**: the one-shot retry in `execute()` on MethodNotFound on a real call is done
  (commit `9ba5bb9`), but permanent `OnceCell` invalidation (replacement with `arc-swap` or
  `RwLock<Option<Arc<dyn A2aWire>>>`) is not implemented.
- **Impact**: low; an error from a wrong first resolution is fixed by one retry
  attempt, after which it stays cached until the driver is recreated.
- **Closure trigger**: if real agents show a persistent wrong first
  resolution — replace `OnceCell` with a structure that has `reset()`.

## Remainder: live E2E — manual run only, not in CI

The E2E itself is implemented and verified live in both repos (see below). The only
limitation is that it requires actually running processes (gateway + hermes +
adapterd), so it does not run in the automated CI.
- **`agent-connector`**: `crates/driver-a2a-client/tests/e2e_live.rs` (ignored by default,
  run manually): spec / auto / sdk / smoke — verified live, 4/4 passed against a real
  gateway + hermes and against adapterd (an SDK server). Commits `3f7061b` + `9d057a5`.
- **`ACP-A2A_gateway`**: `gatewayd/tests/e2e_live.rs` (commit `00fe731`, ignored by default):
  spec (`message/send` → Completed), SDK (`SendMessage` → `TASK_STATE_COMPLETED` in `{task}`),
  agent-card — verified live, 3/3 passed against a real gateway + hermes.
  Plus the in-process harness `gatewayd/tests/rest_transport.rs` (commit `39ea530`).
- Requires actually running processes (gateway + hermes + adapterd), not run in CI.
