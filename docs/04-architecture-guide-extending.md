# Architecture guide: adding modules

> **Language:** English · [Русская версия](04-architecture-guide-extending-ru.md)

Default rule: **a new feature = a new file, not an edit of an
existing one**. If a patch requires changing the signature of an existing
public type — that is a signal to stop and check whether it would
break other places (see the extension points table below).

## The "seam" rule — how extension without rewriting works

The key architectural device throughout the project: where functionality is
known to grow (streaming, multi-tenant, rate
limiting), the API signature is designed up front so that a new
variant fits without changing existing code.

An example of an already implemented seam is `enum Reply<T, U>`:

```rust
pub enum Reply<T, U> {
    Complete(T),                                    // Phase 1: the only variant
    Streaming(tokio::sync::mpsc::UnboundedReceiver<U>), // Phase 2: added without
}                                                        // changing trait signatures
```

Any new module adding asynchronous/streaming behavior must
follow the same pattern — not change `T` to `Result<T, Streaming>` or
anything similar ad-hoc, but check whether the existing `Reply` can be used.

## Extension points table (from the final thread architecture)

| What is added | New file | Existing files — what changes | What is NOT touched |
|---|---|---|---|
| Streaming (session/update, SSE) | — (existing files are used) | `core/convert.rs`: replace `unreachable!()` with the real SessionUpdate↔A2aEvent mapping; `gatewayd/transport_*.rs`: handle the `Reply::Streaming` branch | `AcpAgent`/`A2aAgent` signatures, `Reply<T,U>`, `registry.rs` |
| WS transport | `gatewayd/src/transport_ws.rs` | `main.rs`: add a `tokio::spawn` for the new server | `core/`, `protocol/`, the other `transport_*.rs` |
| TLS | — | `transport_tcp.rs`/`transport_http.rs`: wrap `TcpListener`/`axum::serve` in a `rustls` acceptor | Everything above the transport layer |
| Rate limiting | `gatewayd/src/rate_limit.rs` | `transport_tcp.rs`/`transport_http.rs`: call `rate_limit.check(...)` before `registry.check_token(...)` | Logic after the token check and dispatch |
| Multiple agents per token | — | Only `config.yaml` (the `agents:` section is extended) | Code doesn't change at all — `Registry` already supports N agents |
| OAuth2 instead of a static token | `gatewayd/src/auth/oauth2.rs` | `registry.rs`: `check_token` is replaced with a `TokenValidator` trait with two impls (`StaticToken`, `OAuth2Validator`) | `core/` knows nothing about the authentication method at all |
| Multi-tenant routing | `gatewayd/src/tenant_router.rs` | `registry.rs`: `Registry` gains a `tenant_id` field in `AgentEntry` | `core/convert.rs` knows nothing about tenants — the protocol mapping is tenant-agnostic |
| Persistent task storage (Postgres instead of files) | `core/src/task_store_pg.rs` | `core/lib.rs`: `pub mod task_store_pg`; the calling code (`transport_http.rs`) picks the implementation via enum/trait | `TaskStore` (file-based) stays as is — not removed, it becomes one of the implementations |

### Already implemented modules (examples of past extensions)

| Module | File | What it added | How it's wired in |
|---|---|---|---|
| Durable event buffer | `gatewayd/src/event_log.rs` | Persistent event log of the stream with a monotonic per-task `seq`, `events_after(after_seq)` | Background writer task over mpsc; the `Arc` goes into `HttpState` → `stream_to_sse`/`dispatch_a2a_method` |
| Journal + health | `gatewayd/src/journal.rs`, `gatewayd/src/health.rs` | Durable journal (alerts, disconnects, approvals) + periodic DB size summary | One background writer, self-cleanup per retention; viewing via the CLI module |
| Approvals | `gatewayd/src/approvals.rs` | SQLite store of statuses (pending/approved/rejected), agent fingerprint | Filter in `build_registry(raw, allowed)`; non-approved agents are excluded and written to the journal (category `approval`) |
| Rust CLI | `gatewayd/src/cli.rs` | `--journal`, `--approvals`, `--approve/--reject`, `--setup` — no external sqlite3 | Separate branches in `main.rs` before the servers start; doesn't touch transport |

## How to add a new transport (step by step, using WS as an example)

1. Create `gatewayd/src/transport_ws.rs`.
2. The transport **must not** re-parse JSON-RPC methods itself — if the
   client protocol matches an existing branch (for example, ACP
   over WS instead of TCP), reuse a `dispatch_acp_method`-like
   function, moving it into `core/` or a separate `gatewayd/src/dispatch_common.rs`
   if several transports need it at once.
3. In `main.rs` add a third `tokio::spawn` next to `tcp_server`/`http_server`,
   join them through the same `tokio::select!` (fail-fast: any transport
   going down stops the whole gatewayd).
4. Config: add `ws_listen` to `RawConfig`, analogous to `http_listen`.

## How to add a new transformation protocol (not ACP/A2A)

If a third protocol appears (hypothetically — MCP-as-agent-protocol),
the architecture already anticipates this path:

1. `protocol/src/<new_protocol>.rs` — types of the new protocol.
2. `core/src/agent.rs` — a new trait `NewProtocolAgent`, mirroring
   `AcpAgent`/`A2aAgent`.
3. `core/src/convert.rs` — two new adapters: `AcpAsNewProtocol`,
   `NewProtocolAsAcp` (and similarly for A2A if cross-translation between
   all three is needed — up to 6 adapters then, but each one is an
   independent file/struct, not a single God-converter).
4. The existing `AcpAsA2a`/`A2aAsAcp` don't change — they know nothing
   about the new protocol's existence.

## How to add a durable store (following the journal/approvals/event_log pattern)

1. Create `gatewayd/src/<name>.rs`: `struct <Name>Store { ... }` with `open(path)`,
   write and read operations. The SQLite (WAL) table — via rusqlite, as in the
   existing modules. `pub struct ...` + `#[derive(Clone, Debug)]`.
2. In `gatewayd/src/lib.rs` — `pub mod <name>;`.
3. Config section: a field in `RawConfig` (config.rs) with `#[serde(default)]`; a section
   in `--setup` (setup.rs) and in `render()`.
4. Wiring: if it's background writing — `tokio::spawn` a writer task with mpsc in
   `main.rs`; if it's a start-up filter — a separate branch in `main()`. Don't mix it
   with transport: the store is a separate module, the transport gets an `Arc<Store>`.
5. Tests in the same file: tempdir store, a write/read cycle, retention/cleanup.

## How to add a CLI command (following the cli.rs pattern)

1. In `main.rs` — a new argument-parsing branch: `--<cmd>` → `cli::run_<cmd>(...)`.
   CLI branches run **before** the TCP/HTTP servers start.
2. In `cli.rs` — `pub fn run_<cmd>(args: &[String]) -> anyhow::Result<()>`.
   Parse arguments manually (`--db PATH`, positional), without clap — the project
   doesn't depend on clap.
3. Output — an ASCII table with `unix_to_utc` (time in UTC) and truncated long fields;
   English text only (the gateway interface is in English).
4. Unit tests for parsing/formatters in the same file; e2e — a manual binary run
   against a real SQLite from `/tmp/gateway/`.

## Anti-patterns — what NOT to do when extending

- **Don't add parameters to existing constructors without a strong
  need.** A real example from this project: adding
  `lease_timeout` required touching 4 files (`convert.rs`, both
  `transport_*.rs`, `main.rs`) — that is acceptable because the parameter
  genuinely must be configurable, but every such change costs caller code.
  Before adding a parameter, check whether a builder or config-struct
  could be created instead.
- **Don't mix transport code with business logic.** If
  `transport_tcp.rs` gains code that transforms protocol data
  (rather than just parsing the JSON-RPC envelope), — that is a signal that
  the logic should move into `core/convert.rs`.
- **Don't create a shared "Utils" or "Helpers" module.** Every file in
  `core/` is already named after its responsibility (`lease.rs`,
  `task_store.rs`) — if new functionality doesn't fit any existing name,
  that is a reason for a new file with a precise name,
  not for adding to a catch-all module.
