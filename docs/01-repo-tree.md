# Repository tree

> **Language:** English · [Русская версия](01-repo-tree-ru.md)

Final structure at the end of the thread — 3 crates, 4 of 4 directions
(1: ACP↔ACP, 2: A2A↔A2A, 3: ACP client→A2A agent, 4: A2A client→ACP agent).

```
gateway/
├── Cargo.toml                        # workspace, resolver = "2"
├── config.example.yaml               # config template, copied to config.yaml
│
├── protocol/                         # TYPES. Zero business logic.
│   ├── Cargo.toml                     # deps: serde, serde_json
│   └── src/
│       ├── lib.rs                      # pub use acp::*; pub use a2a::*;
│       ├── acp.rs                      # SessionId, ContentBlock, PromptRequest,
│       │                               # StopReason, SessionUpdate (Phase 2)
│       └── a2a.rs                      # TaskId, Task, TaskState (8 states),
│                                       # Message, Part, AgentCard, A2aEvent (Phase 2)
│
├── core/                             # CORE. All the real complexity lives here.
│   ├── Cargo.toml                     # deps: protocol, tokio, async-trait, anyhow,
│   │                                   # thiserror, chrono, reqwest, serde_json
│   │                                   # dev-deps: tempfile
│   └── src/
│       ├── lib.rs                      # pub mod agent/convert/http_agent/lease/
│       │                               # reply/stdio_agent/task_store
│       ├── reply.rs                    # enum Reply<T,U> — seam for streaming
│       ├── agent.rs                    # trait AcpAgent, trait A2aAgent
│       ├── convert.rs                  # AcpAsA2a, A2aAsAcp — universal
│       │                               # converter (ContentBlock↔Part,
│       │                               # TaskState↔StopReason)
│       ├── lease.rs                    # TurnLease — serializes prompts
│       │                               # per session, fail-closed timeout
│       ├── task_store.rs               # TaskStore — file-based Task store
│       │                               # (JSON, atomic write, for get_task)
│       ├── stdio_agent.rs              # StdioAcpAgent — process spawn,
│       │                               # request/response by JSON-RPC id
│       └── http_agent.rs               # HttpA2aAgent — HTTP JSON-RPC client
│                                       # to an external A2A agent
│
└── gatewayd/                         # BINARY. Wiring only (I/O).
    ├── Cargo.toml                     # deps: protocol, core, tokio, axum,
    │                                   # reqwest, serde_yaml, tracing-subscriber
    └── src/
        ├── main.rs                      # reads config.yaml, builds Registry,
        │                               # runs TCP+HTTP in parallel,
        │                               # background task cleanup (sweep_expired)
        ├── registry.rs                  # Registry: token set + agent map
        │                               # (Transport::Stdio | Transport::Http)
        ├── transport_tcp.rs             # Directions 1 and 3: ACP client as
        │                               # the inbound side (TCP, ndjson)
        ├── transport_http.rs            # Direction 4: A2A client → ACP agent
        │                               # (axum: /agents/:id/rpc + agent.json)
        └── transport_a2a_passthrough.rs # Direction 2: A2A client → A2A agent
                                        # (reverse-proxy, /a2a-proxy/:id/*path)
```

## Why exactly this split (no more and no fewer files)

`protocol` is separated from `core` because the types are tested and reused
independently of the conversion logic — if tomorrow a second gateway
appears on other transports, the `protocol` crate is used unchanged.

`core` is the only crate where it makes sense to write unit tests in isolation
from the network and processes (except `stdio_agent.rs`, which itself requires
a real subprocess). All the substantive logic is concentrated here:
protocol mapping, reliability (`lease.rs`), persistence (`task_store.rs`).

`gatewayd` is pure wiring: config parsing, network scaffolding, dispatch by
`agent_id`. No file here should grow into a "God module" —
if `transport_http.rs` starts to contain business logic (unrelated to
HTTP framing), that is a signal that it should move into `core`.
