# TECH_DEBT

> **Language:** English · [Русская версия](TECH_DEBT-ru.md)

## Open

### 2026-08-19: unit coverage of the 5 `SessionUpdate` variants (criterion 1.2) — 0 tests
- **What**: the `SessionUpdate → A2aEvent` mapping in the converters is not covered by unit tests for each of the 5 enum variants (`AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `UsageUpdate`) — an untested variant = silent information loss when the protocol is extended.
- **Impact**: low-medium — streaming works on the happy path, but a mapping regression is not caught by tests.
- **Fix**: unit tests in `core/src/convert.rs` for each variant (see `docs/streaming-roadmap-checklist.md`, criterion 1.2).

### 2026-08-19: `tasks/resubscribe` — HTTP only, TCP line protocol without RPC
- **What**: `tasks/resubscribe` and `tasks/get-last-seq` are implemented for HTTP (direction 4, `transport_http.rs`); the TCP line protocol (direction 3) has no resubscribe RPC — a client that dropped off a TCP stream reconnects only via a new `session/prompt`.
- **Impact**: low — resubscribe is needed by HTTP clients; the TCP direction works without it.
- **Fix**: on request — add `tasks/resubscribe` to the TCP transport (optional).

## Closed

### 2026-08-19: tasks/resubscribe not implemented (Phase 2.1 → implemented in Phase 3.2)
- **Closed**: durable event buffer (`gatewayd/src/event_log.rs`, monotonic per-task `seq`), `tasks/get-last-seq` + `tasks/resubscribe` in `transport_http.rs` (replay from the event log as an SSE stream) — a client that dropped off mid-stream reconnects to the running task via HTTP. See commit b9c0b8b.

### 2026-08-18: token hash — HMAC-SHA256 (commit a970dcd)
- **Closed**: `RandomState` replaced with HMAC-SHA256 keyed from `{env:GATEWAY_HMAC_KEY}` (default `default-dev-key-do-not-use-in-prod` for development). Cryptographic hash, the `Owner::Token { hash: u64 }` format is unchanged — `StoredTask` needs no migration. Production: the key must be set via env.

### 2026-08-18: T4 — TCP stream, direction 3 (commit a970dcd)
- **Closed**: `HttpA2aAgent::send_task` returns `Reply::Streaming` (SSE client `sse_to_a2a_events`) when the response has `Content-Type: text/event-stream`, otherwise `Reply::Complete`. `blocking: false` for stream requests. Tests: unit `send_task_returns_streaming_on_sse_response` + integration `streaming_tcp.rs` (a TCP client receives `session/update` line by line). Mock servers generate SSE via real `A2aEvent` serialization (like prod `stream_to_sse`), without manual JSON hardcoding.

### 2026-08-18: continue by contextId times out (direction 4) (commit 9cde4e6)
- **Closed**: `ensure_session` already returned the existing session (audits P1-1/P2-10); added the integration test `second_message_send_same_context_returns_same_session`.

### 2026-08-18: streaming in the converters — Phase 2.0 (commits af9c9d9, 1ee5574, 36745ac, 1e2de5d, da3749f, a970dcd)
- **Closed**: `Reply::Streaming` implemented via `prompt_streaming()` (P-20/P-21). Transport: SSE (HTTP, direction 4) + line-delimited TCP (direction 3, SSE client — see T4). `max_concurrent_streams` limit (Semaphore per-agent, try_acquire_stream in HTTP+TCP, fail-closed). Separate first/idle_chunk_timeout in the stream loop. Logging with rotation (tracing-appender). Tests T1-T9 + negative control + P-23/P-24 + HMAC hash. 151 tests, clippy -D warnings clean. `tasks/resubscribe` closed by a separate entry above.

### 2026-08-09: sessions without session/new accumulated in the HashMap (P2-8)
- **Closed**: session only via `session/new`, `prompt` rejects an unknown sessionId before acquire, `cancel` releases the lease, TTL eviction, cap `MAX_SESSIONS_PER_CONNECTION = 256`.

### 2026-08-09: AgentCard.url empty (P2-12)
- **Closed**: url = `config.public_url` + `/agents/<id>/rpc`.

### 2026-08-09: task files accumulated indefinitely
- **Closed**: `sweep_expired(ttl)` + background sweep once an hour by file mtime (`.json.tmp` files are not touched).
