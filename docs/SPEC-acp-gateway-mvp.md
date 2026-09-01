# SPEC: acp-gateway (minimal version of the spec v0.1)

> **Language:** English · [Русская версия](SPEC-acp-gateway-mvp-ru.md)

Based on `SPEC-acp-a2a-gateway-legacy.md`. The goal is to cut the spec down to
an MVP implementable in one iteration in Rust, without losing the acceptance criteria
from §9 of the original document.

---

## 0. What is deliberately cut from the spec for the MVP

| Spec item                                    | Decision for the MVP                                  |
| -------------------------------------------- | ------------------------------------------------- |
| 3 transports (TCP/WS/HTTP+SSE)                | **TCP** only. The rest — v1.1.                 |
| Multi-agent registry, token-based routing         | 1 static token → 1 agent. The registry — later.    |
| Reverse direction ACP→A2A (bridge #2)      | Not in the MVP, a separate milestone.             |
| TLS, rate limiting                            | Stubs/TODO, do not block acceptance.               |
| Reconnect with backoff, health-endpoint, metrics | Deferred to NFR v1.1.                               |
| `session/load`, permission policy (allow/deny/ask) | Stub: always `allow`.                     |
| session_id remapping (several sessions/process) | Not needed: 1 session = 1 process in the MVP.          |

This yields two independent, separately deliverable pieces: **(A) ACP Gateway
MVP** and **(B) A2A→ACP bridge MVP** — instead of one big release.

---

## 1. Crate layout

```
acp-gateway/
├── Cargo.toml                # workspace
├── crates/
│   ├── acp-proto/            # ACP JSON-RPC 2.0 types, (de)serialize
│   ├── acp-core/             # trait AcpAgent + domain types
│   ├── acp-stdio-agent/      # StdioAcpAgent: spawn + framing over stdio
│   ├── acp-gateway/          # bin: TCP server, token-auth, proxy loop
│   └── acp-a2a-bridge/       # bin: HTTP(A2A) endpoint -> AcpAgent
└── config.example.yaml
```

## 2. Key trait (the only one needed for the MVP)

```rust
#[async_trait::async_trait]
pub trait AcpAgent: Send + Sync {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId>;
    async fn prompt(&self, session: SessionId, prompt: Prompt) -> Result<PromptResponse>;
    async fn cancel(&self, session: SessionId) -> Result<()>;
    fn subscribe(&self, session: SessionId) -> tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>;
}
```

The `A2aAgent` trait from §6 of the original spec is deferred until the second
direction of the bridge — it is not needed for the MVP.

## 3. Config (minimal)

```yaml
gateway:
  bind: "0.0.0.0:8347"
agent:
  command: ["claurst", "acp"]
  cwd: "/srv/workspace"
token: "t-static-dev"
```

Without the `agents:` section (multi-agent registry) and without a `tokens:`
list — one token, one agent.

## 4. Transport

TCP only, newline-delimited JSON-RPC 2.0 (the ACP canon as-is, without
adaptation for WS/SSE). Handshake: the first line from the client is
`{"token": "<T>"}`; on mismatch — close the connection with an error code;
until that moment no ACP messages are accepted or forwarded to the agent.

## 5. Proxying

1:1 forwarding between the TCP socket and the stdin/stdout of the child process
(`StdioAcpAgent`), without `session_id` remapping (not needed at 1
session/process). Lazy spawn on the first `session/new`, kill on connection
close.

## 6. MVP acceptance criteria (reduced to a verifiable minimum from §9 of the spec)

1. `acp_e2e.py` via `acp-gateway --config config.yaml` gets PONG from `claurst acp`.
2. Invalid/missing token → refusal at the transport level.
3. `cargo check --workspace` and `clippy` without warnings.

(Items 2, 4, 6 from §9 — multi-agent, reverse bridge, streaming-latency — are out of the MVP, they go to v1.1/v2.)

## 7. Effort estimate

| Stage                                              | Days     |
| -------------------------------------------------- | ------- |
| Workspace skeleton + `acp-proto` (JSON-RPC types)    | 1       |
| `AcpAgent` trait + `StdioAcpAgent` (spawn + framing) | 1.5     |
| TCP transport + token-auth + proxy loop             | 1.5     |
| Tests: mock-agent unit + e2e with real `claurst acp` | 1     |
| **Total: ACP Gateway MVP**                          | **5**   |
| A2A HTTP endpoint (`/rpc`, `/.well-known/agent.json`), A2A→ACP only | 3–4 |
| **Total: Gateway MVP + one-directional bridge**    | **8–9** |

The full scope of the original spec (both bridge directions, 3 transports, TLS,
rate limiting, reconnect+backoff, metrics/health, multi-agent registry,
edge-case tests from §5.5) — approximately another **+10–15 days** on top.

**Total for the entire spec: ≈ 18–24 person-days (3.5–5 weeks of a single middle+/senior-level Rust developer).**
