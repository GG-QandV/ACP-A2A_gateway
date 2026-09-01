# Final structure: a minimal, reliable, extensible gateway (Rust)

> **Language:** English · [Русская версия](FINAL-ARCHITECTURE-minimal-reliable-ru.md)

Synthesis of all decisions across the thread: a universal converter instead of two bridges,
token as pure allow/deny, a seam approach via `Reply<T,U>` for future
streaming, `TurnLease`, and the modularity patterns from `hermes-agent/gateway/`.

---

## 1. Crate tree (3 crates — the minimum without losing extensibility)

```
gateway/
├── Cargo.toml                  # workspace
├── config.example.yaml
│
├── protocol/                   # TYPES. Knows nothing about Reply/lease/dispatch.
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── acp.rs               # SessionId, Prompt, PromptResponse, SessionUpdate
│       └── a2a.rs               # TaskId, Task, AgentCard, A2aEvent
│
├── core/                        # CORE. The only place with real complexity.
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── reply.rs              # enum Reply<T, U> — seam for streaming (Phase 2)
│       ├── agent.rs               # trait AcpAgent, trait A2aAgent
│       ├── convert.rs             # AcpAsA2a, A2aAsAcp — the universal converter
│       ├── lease.rs               # TurnLease — serialization of prompts per session
│       └── stdio_agent.rs         # StdioAcpAgent — spawn + framing (Phase 1.1)
│
└── gatewayd/                    # BINARY. Wiring only, no business logic.
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── registry.rs            # flat token-set + agent map (id -> transport)
        ├── dispatch.rs             # token check -> registry lookup -> lease -> convert
        └── transport_tcp.rs        # newline-delimited JSON-RPC for accepting connections
```

**Why 3, not 5 or 1**: `protocol` is separated because the types are reused
and tested independently of the logic. `core` is separated from `gatewayd` because
the core (traits, converter, lease) is the only thing worth covering with unit tests
in isolation from the network; `gatewayd` is just I/O scaffolding, which changes
when transports are added, but must not drag along a rebuild of the core tests.

---

## 2. Request flow (what happens on every incoming byte)

```
TCP connection
   │
   ▼
gatewayd::transport_tcp   — reads newline-delimited JSON-RPC
   │
   ▼
gatewayd::dispatch::handle_connection
   │
   ├─▶ registry.check_token(token)?          // allow/deny, no knowledge of agents
   │        │ deny → close the connection, before reading the payload
   │        ▼ allow
   ├─▶ registry.lookup(agent_id)              // id -> AgentEntry{transport}
   │
   ├─▶ lease.acquire(session_id).await?       // serialization of turns per session
   │        │ timeout → TurnLeaseTimeoutError, refuse the client
   │        ▼ acquired
   ├─▶ core::convert::{AcpAsA2a|A2aAsAcp|identity}.prompt(...)
   │        │
   │        └─▶ match Reply<T,U> {
   │               Complete(resp)   => hand to the client  (Phase 1: the only path)
   │               Streaming(rx)    => unreachable!()   (Phase 2: pump into the client)
   │            }
   │
   └─▶ lease.release(session_id)
```

The only real logic is the converter and the lease. Everything else is byte
passthrough.

---

## 3. Key types (final version)

```rust
// core/src/reply.rs — seam for future streaming, no changes when it arrives
pub enum Reply<T, U> {
    Complete(T),
    Streaming(tokio::sync::mpsc::UnboundedReceiver<U>),
}

// core/src/lease.rs — reliability: prevents two prompts in one session from colliding
pub struct TurnLease {
    locks: tokio::sync::Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
}

impl TurnLease {
    pub async fn acquire(&self, session: &SessionId, timeout: Duration)
        -> Result<TurnGuard, TurnLeaseTimeoutError> { /* fail-closed */ }
}

// core/src/agent.rs — both protocols behind one contract
#[async_trait]
pub trait AcpAgent: Send + Sync {
    async fn prompt(&self, s: SessionId, p: Prompt) -> Result<Reply<PromptResponse, SessionUpdate>>;
    async fn cancel(&self, s: SessionId) -> Result<()>;
}

#[async_trait]
pub trait A2aAgent: Send + Sync {
    async fn send_task(&self, t: Task) -> Result<Reply<Task, A2aEvent>>;
    async fn cancel_task(&self, id: TaskId) -> Result<Task>;
}

// gatewayd/src/registry.rs — the token does NOT know about agents, the agent does NOT know about the token
pub struct Registry {
    tokens: HashSet<String>,               // allow/deny at entry, and only that
    agents: HashMap<String, AgentEntry>,   // id -> {transport} (protocol derived from transport)
}
```

---

## 4. What makes the solution reliable already in the MVP (not deferred to "later")

| Risk                                                                 | Mechanism in the MVP                                                                                                                     |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Two prompts simultaneously into one ACP session corrupt the agent's stdio stream | `TurnLease` — fail-closed, `TurnLeaseTimeoutError` instead of a silent hang                                                               |
| An invalid token reaches JSON-RPC parsing                        | The token check is the first operation after accept(), before reading the payload                                                                      |
| The agent process died, yet the gateway keeps sending it requests          | `StdioAcpAgent` holds the `Child` and checks `try_wait()` before each `prompt` — if the process is dead, `Result::Err` instead of a silent timeout |
| One hung client blocks the whole service                           | Each connection gets its own `tokio::spawn`; `TurnLease` blocks only at session level, not globally                                     |

---

## 5. Extension points (without changing existing files)

| What gets added                      | Where                                                                                                                   | What is NOT touched                                             |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Streaming                             | `core/convert.rs`: replace `unreachable!()` with real mapping; `gatewayd/dispatch.rs`: handle the `Streaming` branch | `AcpAgent`/`A2aAgent` signatures, `Reply<T,U>`, `registry.rs` |
| WS/HTTP+SSE transport                | New file `gatewayd/src/transport_ws.rs` calling the same `dispatch::handle_connection`                             | `core/`, `protocol/`, `transport_tcp.rs`                     |
| TLS                                  | A wrapper around the `TcpListener` in `transport_tcp.rs` (rustls)                                                             | Everything above the transport layer                                  |
| Rate limiting                        | New `gatewayd/src/rate_limit.rs`, hooked in before `registry.check_token`                                          | `dispatch.rs` logic after the token                            |
| Multiple agents on one token   | Just an extension of `agents:` in YAML — `Registry` already supports it                                                       | The code does not change at all                                       |
| `HttpA2aAgent` (a real A2A client) | New file `core/src/http_agent.rs` implementing `A2aAgent`                                                              | `convert.rs` already works with any trait implementation   |

---

## 6. Estimate (consolidated with all simplifications across the thread)

| Part                                                             | Days         |
| ----------------------------------------------------------------- | ----------- |
| `protocol` (types, no logic)                                     | 1           |
| `core`: `Reply`, traits, `TurnLease`                             | 1.5         |
| `core`: `convert.rs` (AcpAsA2a + A2aAsAcp, synchronous path)       | 2.5         |
| `core`: `StdioAcpAgent` (spawn, framing, dead-process check)      | 1           |
| `gatewayd`: registry + dispatch + TCP transport                   | 1.5         |
| Tests (lease concurrency, token deny, both converter directions) | 1.5         |
| **MVP total**                                                     | **~9 days** |

On top (modules from §5, as needed): streaming +3-4 days, WS/HTTP+SSE
+1-2 days each, TLS +1 day, rate limiting +0.5 days.
