# Roadmap: enabling streaming in ACP-A2A_gateway

> **Language:** English · [Русская версия](streaming-roadmap-checklist-ru.md)

Full workflow from the current state (P-18 in `docs/decisions.md`: `Reply::Streaming` is declared, but
returns an error) to a production-ready streaming gateway with configurable scale limits.
Each checkpoint incl. default log traps (WARN/ERROR), transition criteria between stages,
and a tests section. Logging right now — only `tracing_subscriber::fmt()` to stdout without rotation
(`gatewayd/src/main.rs`) — that is covered separately in Part 4.

---

## Part 0 — Prerequisites (Gate 0)

None of the below starts until the following are done:

- [ ] The base branch is tagged (`git tag pre-streaming-baseline`) — rollback must be trivial.
- [ ] `cargo test --workspace` green at the current HEAD (69+ tests, `docs/06-gateway-guide.md` §2).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` with no warnings.
- [ ] P-18 in `docs/decisions.md` read and confirmed — the team agrees that `unreachable!()` in
      a network service is unacceptable, only `anyhow::Result::Err`.
- [ ] A separate TECH_DEBT item "streaming in development" is filed with a link to this roadmap, so
      that the current entry TECH_DEBT.md:17-21 does not get lost in the process.

**🔒 Default log trap (WARN):** if `cargo test` is red at the start of the work — it should
surface in CI as a WARN "baseline unstable before streaming work started", not ERROR (it does not
block yet, but it is recorded).

---

## Part 1 — Streaming mechanism (Stage 1)

### 1.1 `core/src/stdio_agent.rs` — a channel instead of a buffer

- [ ] Replace `UpdatesMap = HashMap<String, Vec<ContentBlock>>` with
      `HashMap<SessionId, mpsc::UnboundedSender<SessionUpdate>>`.
- [ ] `collect_session_update()`: each parsed `session/update` goes straight into `tx.send(...)`,
      without accumulating.
- [ ] `prompt()`: after writing `session/prompt` to stdin, if a receiver is registered for the
      session — return `Reply::Streaming(rx)`; otherwise (backward compatibility for the transition
      period) — the old `Reply::Complete` behavior.
- [ ] Terminal item of the stream — mapping the final `PromptResponse` from `call()` into the last
      channel message, closing it (`drop(tx)` after sending the last item).

**🔒 Log trap (ERROR, on by default):**
```rust
tracing::error!(session_id = %session_key, "stream channel closed before terminal event — возможная утечка ресурсов агента");
```
Fires when the dispatcher's `rx` receives `None` before a `completed/failed/cancelled` status was
received.
This indicates a mapping bug, not normal behavior — it must be ERROR, not WARN.

**🔒 Log trap (WARN, on by default):**
```rust
tracing::warn!(session_id = %session_key, chunk_count, "stream produced 0 chunks before terminal event");
```
Fires if the stream finished without a single intermediate chunk — not a bug per se (an agent may
reply in one message), but a useful diagnostic signal, on by default.

- [ ] **Readiness criterion 1.1**: unit test `stream_emits_chunks_incrementally` — a mock agent sends 3
      `session/update`s with a 50 ms delay between each; the test catches them **one by one** via
      `rx.recv()` with a check that time actually elapsed between receives (not everything at once
      at the end).
      ✅ Done — the stream loop in `stdio_agent.rs` + unit tests in `core/src/stdio_agent.rs`.

### 1.2 `core/src/convert.rs` — real mapping

- [x] `AcpAsA2a`: `Reply::Streaming(rx)` → a loop translating each `SessionUpdate` into
      `A2aEvent::TaskStatusUpdate`/`TaskArtifactUpdate` (modeled on `map_event()` from
      `protocol_a2a_mapper.rs` in the adjacent `agent-connector` — a ready translation template
      `CoreEventKind → A2aStreamEvent`, adapt to local types). ✅ Done (Phase 3.2).
- [x] `A2aAsAcp`: the reverse mapping `A2aEvent → SessionUpdate` (`agent_message_chunk` for text,
      a final notification at the terminal state). ✅ Done.
- [x] Remove all `unreachable!()`/`anyhow::bail!("Фаза 1: ...")` — replace with real logic. ✅

**🔒 Log trap (ERROR, on by default):**
```rust
tracing::error!(agent_id = %agent_id, event = ?unmapped_event, "получено A2aEvent/SessionUpdate без маппинга — протокол расширился без обновления конвертера");
```
Fires on the `_ => ...` arm of the `match` on event type — a signal that the protocol schema
(ACP/A2A) changed while the converter did not keep up.

- [ ] **Readiness criterion 1.2**: a unit test per `SessionUpdate` variant (5 enum variants:
      `AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `UsageUpdate`) — the mapping into
      `A2aEvent` does not panic and does not silently drop information (untested variant = red test
      by default, not a skip).

### 1.3 `gatewayd/src/transport_http.rs` — SSE response (direction 4)

Three places with the same stub (`message/send`, `SendMessage`, REST `message:send` in
`dispatch_a2a_method()` and `rest_send_message_core()`) — extract into one shared rendering function:

- [x] Add `tokio-stream = "0.1"` to `gatewayd/Cargo.toml`. ✅
- [x] Shared function `fn stream_to_sse(rx: UnboundedReceiver<A2aEvent>) -> Sse<impl Stream<...>>`. ✅
- [x] Replace all three stub calls with a call to this function. ✅
- [x] `tasks/resubscribe` — a new JSON-RPC method in `dispatch_a2a_method()`, reading the state of an
      existing task from `TaskStore` and reconnecting to its stream (groundwork for stage 2 resume).
      ✅ Done (Phase 3.2, commit b9c0b8b): durable event buffer (`gatewayd/src/event_log.rs`,
      monotonic per-task `seq`) + `tasks/get-last-seq` + `tasks/resubscribe` — replay from event log
      as an SSE stream. HTTP (direction 4). The TCP line protocol has no resubscribe RPC (see TECH_DEBT).

**🔒 Log trap (WARN, on by default):**
```rust
tracing::warn!(agent_id, task_id = %id, "SSE клиент отключился до terminal event — задача продолжает выполняться в фоне");
```
Fires on a write `Err` to the HTTP connection mid-stream — the client left, but the ACP process
is still working on the task. An explicit signal of a possible compute leak.

- [ ] **Readiness criterion 1.3**: a live run (as in `docs/06-gateway-guide.md` §7 item 10 for
      direction 2) — a real HTTP client receives several SSE events `data: {...}\n\n` before
      the final `final: true`.

### 1.4 `gatewayd/src/transport_tcp.rs` — line-by-line stream (direction 3)

- [ ] Handle `Reply::Streaming` — each item is serialized as an ACP `session/update` notification
      and written newline-delimited to the same TCP socket.

**🔒 Log trap (ERROR, on by default):**
```rust
tracing::error!(session_id = %session.0, error = %e, "не удалось записать session/update в TCP-сокет клиента — соединение будет закрыто");
```

- [x] **Readiness criterion 1.4**: a unit test on a TCP harness — the client receives a stream of
      lines, each a valid JSON-RPC notification with method `session/update`.
      ✅ Done — integration test `streaming_tcp.rs` (closed in TECH_DEBT, T4).

### ✅ Gate 1 — moving to stage 2

All readiness criteria 1.1–1.4 are met, `cargo test --workspace` is green, new tests were added
(they did not replace the old ones — the old tests for the `Reply::Complete` path must keep passing,
backward compatibility is mandatory). Negative control per the project convention (`docs/decisions.md`,
section "How this was written"): roll back each fix one at a time, verify the corresponding test goes red.

---

## Part 2 — Scale limits (Stage 2)

### 2.1 `gatewayd/src/registry.rs` — Semaphore per-agent

- [x] `AgentEntry` gets `stream_permits: Arc<tokio::sync::Semaphore>`. ✅
- [x] `Registry::try_acquire_stream(agent_id) -> Result<SemaphorePermit, StreamCapacityExhausted>`. ✅

**🔒 Log trap (WARN, on by default):**
```rust
tracing::warn!(agent_id, active_streams, limit, "agent stream capacity exhausted — запрос отклонён fail-closed");
```

- [ ] **Readiness criterion 2.1**: unit test — with a limit of `max_concurrent_streams=2` the third
      concurrent stream request gets an explicit error, not a hang.

### 2.2 `gatewayd/src/main.rs` — configuration

- [x] `RawAgentEntry` (both the Stdio and Http variants) gets an optional section:
  ```yaml
  streaming:
    max_concurrent_streams: 4
    first_chunk_timeout_secs: 15
    idle_chunk_timeout_secs: 120
    buffer_capacity: 256
  ```
  ✅ Done.
- [x] `RawConfig` gets `runtime.global_max_concurrent_streams` (default — the sum of all per-agent
      limits, computed automatically if not specified explicitly). ✅
- [x] Startup validation: `max_concurrent_streams == 0` → startup error (per the project convention —
      an empty token/missing env variable already fail startup in the same way). ✅

**🔒 Log trap (ERROR at startup, on by default):**
```rust
anyhow::bail!("agent {id}: streaming.max_concurrent_streams не может быть 0 — используйте отдельный флаг disable_streaming: true, если стрим не нужен");
```

- [ ] **Readiness criterion 2.2**: config-parsing test — YAML without a `streaming:` section uses
      defaults (`max_concurrent_streams: 1` for stdio, a safe minimum), does not panic.

### 2.3 `core/src/supervisor.rs` + `core/src/stdio_agent.rs` — separate timeouts

- [x] `SpawnConfig` gets `first_chunk_timeout: Duration`, `idle_chunk_timeout: Duration`. ✅
- [x] In the stream loop: the first `rx.recv()` — `first_chunk_timeout`; subsequent ones — `idle_chunk_timeout`. ✅

**🔒 Log trap (WARN, on by default):**
```rust
tracing::warn!(session_id = %key, elapsed = ?elapsed, "idle_chunk_timeout сработал — агент не присылал чанков дольше лимита, поток закрыт");
```
Separate from the current `agent did not respond to {method} within {:?}` (that one is an ERROR for a
complete non-start), because an idle timeout inside an already started stream is a situation that is
different by meaning (the agent started replying but hung midway).

- [ ] **Readiness criterion 2.3**: unit test — a mock agent sends 1 chunk, then stays silent for 200 ms
      with `idle_chunk_timeout_secs=0.1` — the stream closes on timeout instead of hanging until
      `call_timeout`.

### ✅ Gate 2 — moving to production

All criteria 2.1–2.3 are met. The load run (see Part 3) passed with no ERROR-level log traps.

---

## Part 3 — Tests section (summary)

| # | Test type | File | What it checks | Stage |
|---|---|---|---|---|
| T1 | Unit | `core/src/stdio_agent.rs` (`#[cfg(test)]`) | Chunks arrive one at a time, not as a batch | Stage 1 |
| T2 | Unit | `core/src/convert.rs` (`#[cfg(test)]`) | Every `SessionUpdate`/`A2aEvent` variant maps, no variant panics | Stage 1 |
| T3 | Integration | new `gatewayd/tests/streaming_http.rs` | A real HTTP SSE client receives several events before `final: true` | Stage 1 |
| T4 | Integration | new `gatewayd/tests/streaming_tcp.rs` | The TCP client receives line-by-line `session/update` notifications | Stage 1 |
| T5 | Regression | existing tests of the `Reply::Complete` path | The non-streaming path does not break after the `stdio_agent.rs` refactor | Stage 1 |
| T6 | Negative control | roll back each fix one at a time | The corresponding test from T1–T4 goes red on rollback | Stage 1 |
| T7 | Unit | `gatewayd/src/registry.rs` (`#[cfg(test)]`) | `Semaphore` rejects a request beyond the limit | Stage 2 |
| T8 | Unit | `gatewayd/src/main.rs` (`#[cfg(test)]`) | Config without a `streaming:` section — defaults, not a panic | Stage 2 |
| T9 | Unit | `core/src/stdio_agent.rs` (`#[cfg(test)]`) | `idle_chunk_timeout` closes a hung stream, `first_chunk_timeout` one that never started | Stage 2 |
| T10 | Load | `gatewayd/tests/streaming_load.rs` (new) | 5 mock agents, 20 parallel streams per agent — no ERROR logs, memory stable over 10 minutes. *Test written (fast version: 100 streams, all 200 + final:true); the full 10-minute run is manual* | Gate 2 |
| T11 | Live | manual run modeled on §7 item 10 of `docs/06-gateway-guide.md` | A real ACP agent (claurst/hermes) streams through direction 4 | Gate 2 |
| T12 | Clippy/build | `cargo clippy --workspace --all-targets -- -D warnings` | No stage leaves warnings | All stages |

**Rule for all tests T1–T11** (project convention, `docs/decisions.md`, section "How this was written"):
every new test must be verified by rolling back the corresponding fix — if the test does not go red on
rollback, it checks something other than what was intended, and must be rewritten.

---

## Part 4 — Logging configuration and rotation

### 4.1 Current state (before this work)

`gatewayd/src/main.rs::tracing_subscriber_init()` — only `tracing_subscriber::fmt()` to stdout,
level control via `RUST_LOG`/`EnvFilter`, **no file output and no rotation**. For a containerized
environment (Docker/systemd journal) this is usually acceptable — rotation is taken over by the
docker logger or journald. But enabling streaming grows log volume (each chunk is a potential
logging point at DEBUG level), and if logs are written to a file (e.g. a local run without a
container) — an explicit limit is needed.

### 4.2 Log volume calculation

Load model: 5 agents, 20 streams/hour per agent, on average 15 chunks per stream, average log
line size (`tracing` fmt with `session_id`, `agent_id`, `seq`, `timestamp` fields) — ~220 bytes.

| Log level | What is logged | Volume/day (nominal) | Volume/day (peak ×5) |
|---|---|---|---|
| `ERROR` + `WARN` only | Traps from Parts 1–2, without chunks | ~1.5 MB | ~7.5 MB |
| `INFO` (default) | + stream open/close, agent start/stop | ~1.5–3 MB | ~15 MB |
| `DEBUG` (including chunks) | + every `session/update`/`A2aEvent` chunk | ~7.5 MB | ~37 MB |
| `TRACE` | + full JSON-RPC message bodies | ×3–5 of DEBUG (~25–40 MB) | ~150–200 MB |

Takeaway: at the default `INFO` level rotation is practically unnecessary even without a limit (a few
MB per day). A rotation threshold is needed in case the operator temporarily enables `DEBUG`/`TRACE`
for diagnostics and forgets to switch back — that very scenario is what most often fills up the disk
on real servers.

### 4.3 Rotation config (new `config.yaml` section)

```yaml
logging:
  level: "info"                    # info | debug | trace | warn | error — default info
  output: "stdout"                 # stdout | file | both — default stdout (no behavior change)
  file:
    path: "/var/log/acp-a2a-gateway/gateway.log"
    max_file_size_mb: 100          # per-file threshold before rotation
    max_files: 10                  # how many rotated files to keep (oldest are overwritten)
    max_total_size_mb: 1000        # HARD ceiling on total volume — takes priority over max_files
    compress_rotated: true         # gzip old files right after rotation
```

**Rationale for the numbers:** `max_file_size_mb: 100` — at peak `DEBUG` level (~37 MB/day) one file
covers ~2.5 days, which is enough for a diagnostics window without frequent rotation. `max_files: 10` ×
`max_file_size_mb: 100` = 1000 MB, which is exactly what `max_total_size_mb: 1000` sets — the same
figures from two sides, so that `max_total_size_mb` is a real control ceiling, not a decorative field.

### 4.4 Rotation mechanism — implementation

- [x] Add `tracing-appender = "0.2"` to `gatewayd/Cargo.toml`.
- [x] `tracing_appender::rolling::RollingFileAppender` with `rolling::Builder` — `max_log_files(10)`
      provides automatic overwrite of older versions when `max_files` is exceeded.
- [x] If `logging.output: both` — `tracing_subscriber::registry()` with two layers (`fmt::layer()`
      for stdout + `fmt::layer()` for the file), not replacing each other.
- [x] `max_total_size_mb` check — a separate background task (by analogy with the already existing
      `sweeper` in `main.rs` for `TaskStore`, the same `tokio::spawn` + `interval` pattern) that
      computes the total log-directory size once an hour and **forcefully** deletes the oldest
      rotated files if the `tracing-appender`'s own `max_files` somehow lagged (protection against
      a divergence between "N files" and "N megabytes" during a sharp jump in a single file's size).
- [x] `compress_rotated: true` (default) — gzip compression of rotated files during directory
      cleanup (`prune_log_dir` in `main.rs`, `flate2`).
- [x] Real cleanup instead of a log message: `prune_log_dir` gzip-compresses old files and deletes
      the oldest ones until the total size returns below `max_total_size_mb` (the active file is
      left untouched).

**🔒 Log trap (WARN, on by default, written even when file logging is fully
disabled — stdout only):**
```rust
tracing::warn!(current_size_mb, limit_mb, "лог-каталог приближается к max_total_size_mb (>80%) — рассмотрите понижение уровня логирования или увеличение лимита");
```

**🔒 Log trap (ERROR, on by default):**
```rust
tracing::error!(current_size_mb, limit_mb, "лог-каталог превысил max_total_size_mb — принудительное удаление старейших файлов");
```

### 4.5 Completely disabling logging (emergency valve)

A separate flag, not a side effect of `level`:

```yaml
logging:
  level: "off"        # disables even ERROR completely — use only in an emergency
```

- [x] `EnvFilter::new("off")` instead of building the filter from `level`.
- [x] With `level: "off"` — the startup message is **mandatorily** printed once before the filter is
      disabled (otherwise the operator cannot tell apart "the gateway writes no logs per config" from
      "the gateway did not start"):
  ```rust
  eprintln!("[gatewayd] ВНИМАНИЕ: логирование полностью отключено (logging.level: off) — диагностика по логам будет недоступна");
  ```
  Written directly to stderr, bypassing `tracing`, because by this point the filter may already be `off`.

### 4.6 Expanding logging (diagnostic mode)

The reverse emergency valve — temporary expansion without editing `config.yaml` and without a restart:

- [x] Runtime level-change HTTP endpoint: `POST /debug/level` (body `{"level":"debug"}`) via
      `tracing_subscriber::reload::Handle` — the standard `tracing-subscriber` pattern for hot-swapping
      the filter without a rebuild or process restart. GET returns the current level.
      Authorization check — the same `Authorization: Bearer <token>` as for RPC.
- [x] Time limit: the expanded level (`debug`/`trace`) automatically returns to the default `info`
      after a configurable `logging.debug_ttl_minutes` (default 60), so a forgotten diagnostic mode
      does not run forever and eat the disk past `max_total_size_mb`.

**🔒 Log trap (WARN, on by default):**
```rust
tracing::warn!(new_level = %level, ttl_minutes, "уровень логирования временно расширен — автоматический откат через ttl_minutes");
```

### ✅ Gate 4 — logging readiness criterion

- [ ] With `DEBUG` enabled over 24 hours of peak traffic (T10 from Part 3) the total
      log volume stays under `max_total_size_mb`, rotation fires without losing the latest records.
      *(manual run — not performed; the automated test `streaming_load.rs` (T10) is written)*
- [ ] Rolling `level: off → info` and back is verified manually — the process requires no restart.
      *(check via GET/POST /debug/level — manual run, not performed)*
- [ ] The "approaching the limit" log trap (WARN at 80%) actually fires in the load test before
      the forced deletion (ERROR) fires — the escalation order is confirmed.
      *(manual run — not performed)*

---

## Summary table: files, log traps, and tests by stage

| Stage | Files (edit) | New files | Log traps (count WARN/ERROR) | Tests |
|---|---|---|---|---|
| 1. Streaming mechanism | `stdio_agent.rs`, `convert.rs`, `transport_http.rs`, `transport_tcp.rs`, `Cargo.toml` (gatewayd) | 1 mandatory (integration test) | 2 ERROR, 2 WARN | T1–T6 |
| 2. Scale limits | `registry.rs`, `main.rs`, `supervisor.rs`, `stdio_agent.rs` (again) | 0 mandatory | 1 ERROR (startup), 2 WARN | T7–T9 |
| 3. Load/live | — | 2 new test files (`streaming_load.rs`, manual checklist) | — (checking existing ones) | T10–T12 |
| 4. Logging/rotation | `main.rs` (extending `tracing_subscriber_init`), `Cargo.toml` (gatewayd) | 0 mandatory | 2 WARN, 1 ERROR, 1 startup eprintln | Gate 4 checklist |

**Total across the whole roadmap:** 8 unique existing files to edit, 1 mandatory + 2
optional new code files, 1 new mechanism dependency (`tokio-stream`) + 1 for logging
(`tracing-appender`), 12 named tests plus a load run, 6 ERROR traps and 6 WARN traps,
on by default with no extra operator configuration.
