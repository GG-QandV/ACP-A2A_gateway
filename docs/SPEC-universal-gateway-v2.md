# SPEC v2: universal ACP/A2A gateway (simplified architecture)

> **Language:** English · [Русская версия](SPEC-universal-gateway-v2-ru.md)

Revision in response to the review comment: the artificial boundary between
"Gateway" and "Bridge" as two separate binaries has been removed. One process,
one entry point, logic branches on whether the client and agent protocols match.

---

## 1. The idea in one sentence

The token decides whether to admit a client **to the gateway at all** — nothing
more. Further on: client protocol == agent protocol → bare proxying without
conversion. Different protocols → universal converter (the same code for both
A2A→ACP and ACP→A2A directions, thanks to adapters of one trait through the other).

```
                 ┌───────────────────────────────────────────┐
                 │              acp-a2a-gateway               │
   ACP client ──▶│  1. token check (allow/deny)               │
   A2A client ──▶│  2. lookup target agent (id → protocol)    │
                 │  3. client.proto == agent.proto ?          │
                 │       yes → passthrough (raw proxy)        │──▶ ACP agent
                 │       no  → universal converter            │──▶ A2A agent
                 └───────────────────────────────────────────┘
```

No separate `acp-gateway` binary and no separate `bridge` binary — one service,
one config, one port (or a set of ports per transport, see §5).

---

## 2. Token

- The token is an **allow/deny for entering the gateway**, full stop. It is not
  tied to specific agents and does not spawn an ACL matrix "token → agent list".
- The list of valid tokens is a flat set in the config (or a single shared
  secret for the first iteration).
- Target agent selection is a separate request parameter (agent id in
  `session/new`/`initialize` for ACP, path `/agents/{id}/rpc` for A2A), not
  tied to the token in any way.
- Invalid/missing token → connection dropped at the transport level, before
  parsing the ACP/A2A payload.

```rust
fn check_token(token: &str, valid: &HashSet<String>) -> bool {
    valid.contains(token)
}
```

This is all that is required from the auth layer for the MVP.

---

## 3. Passthrough (same protocols, no conversion)

When the client and the target agent speak the same protocol (ACP↔ACP or
A2A↔A2A), the gateway **does not parse the semantics** of JSON-RPC methods —
it only shuttles frames:

- ACP↔ACP: newline-delimited JSON-RPC is already identical both at the client
  (network transport) and at the agent (stdio) — just a readline loop in both
  directions, without `InitializeRequest`/`Prompt`/etc. structs.
- A2A↔A2A: the HTTP request is forwarded as a reverse-proxy (including the
  SSE stream as-is).

**Important caveat**: "without conversion" refers to the absence of
*semantic* method mapping. The transport difference still exists (the client's
TCP frame ≠ the agent's stdin/stdout bytes) and must be relayed — but that is
a trivial byte/line copy, not type parsing.

```rust
trait Passthrough {
    async fn pump(self, client: impl AsyncRead + AsyncWrite,
                         agent:  impl AsyncRead + AsyncWrite) -> Result<()>;
}
```

One generic pump for both ACP↔ACP and A2A↔A2A (the only difference is in the
transport wrappings: TCP socket vs stdio channel vs HTTP stream).

---

## 4. Universal converter (different protocols)

When the protocols do not match — the very same converter works in both
directions thanks to two traits and adapters of one through the other (as in §6
of the original spec, we keep it as the core — this is the only place where
complexity is genuinely needed):

```rust
#[async_trait]
trait AcpAgent {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> Result<SessionId>;
    async fn prompt(&self, session: SessionId, prompt: Prompt) -> Result<PromptResponse>;
    async fn cancel(&self, session: SessionId) -> Result<()>;
    fn subscribe(&self, session: SessionId) -> UnboundedReceiver<SessionUpdate>;
}

#[async_trait]
trait A2aAgent {
    async fn card(&self) -> Result<AgentCard>;
    async fn send_task(&self, task: Task) -> Result<Task>;
    async fn get_task(&self, id: TaskId) -> Result<Task>;
    async fn cancel_task(&self, id: TaskId) -> Result<Task>;
    async fn stream(&self, id: TaskId) -> UnboundedReceiver<A2aEvent>;
}

// one universal adapter per direction — this is exactly the "converter"
struct AcpAsA2a<T: AcpAgent>(T);   // an A2A client sees an ACP agent as an A2aAgent
impl<T: AcpAgent> A2aAgent for AcpAsA2a<T> { /* mapping from §5.1 of the original spec */ }

struct A2aAsAcp<T: A2aAgent>(T);   // an ACP client sees an A2A agent as an AcpAgent
impl<T: A2aAgent> AcpAgent for A2aAsAcp<T> { /* mapping from §5.2 of the original spec */ }
```

The gateway does not care who is outside (an ACP or an A2A client) or who is
inside (an ACP or an A2A agent) — if the protocols did not match, it takes the
needed adapter and hands the client the interface of its native protocol. This
is the "universal a2a/acp/a2a converter" as one piece of code, not two
different bridge binaries.

---

## 5. Crate layout

```
gateway/
├── proto-acp/       # ACP types + framing (JSON-RPC over stdio/TCP)
├── proto-a2a/       # A2A types + framing (JSON-RPC over HTTP/SSE)
├── core/            # trait AcpAgent, trait A2aAgent, AcpAsA2a, A2aAsAcp
├── passthrough/     # generic byte-copy pump for identical protocols
└── gatewayd/         # bin: token check → agent lookup → passthrough | converter
```

The agent registry is a flat config `id → {protocol, transport, endpoint}`,
without token-specific access lists:

```yaml
listen: "0.0.0.0:8347"
tokens: ["t-dev-1", "t-dev-2"]
agents:
  claurst-main: { protocol: acp, transport: stdio, command: ["claurst", "acp"] }
  ops-agent:    { protocol: a2a, transport: http,  url: "https://ops.internal/a2a" }
```

---

## 6. Acceptance criteria

1. ACP client → `claurst-main` (ACP agent): passthrough, PONG passes through with unchanged fields.
2. A2A client → `ops-agent` (A2A agent): reverse-proxy, including the SSE stream.
3. A2A client → `claurst-main` (ACP agent): via `AcpAsA2a`, `task/send` arrives as `session/prompt`, the response is mapped back.
4. ACP client → `ops-agent` (A2A agent): via `A2aAsAcp`, `session/prompt` arrives as `task/send`.
5. Wrong token → drop in any of the four scenarios above, before reading the payload.
6. `cargo check --workspace` + clippy with no warnings.

---

## 7. Estimate

| Part                                                    | Days     |
| ---------------------------------------------------------- | ------- |
| `proto-acp` + `proto-a2a` (types, framing)                   | 1.5     |
| Token-check + agent registry + dispatch (passthrough vs converter) | 1     |
| Passthrough pump (ACP↔ACP, A2A↔A2A)                         | 1.5     |
| Universal converter: `AcpAsA2a` + `A2aAsAcp` (mappings §5.1/5.2 of the original spec) | 4–5 |
| Tests: the 4 scenarios from §6 acceptance + mock agents                | 2       |
| **Total MVP (one binary, both directions, TCP+HTTP)**    | **10–11** |

On top of this (not part of the MVP, but present in the original spec): WS
transport, TLS, rate limiting, reconnect+backoff, metrics/health, `session/load`,
permission-policy `allow/deny/ask` — roughly **+6–8 days**.

**Total scope: ≈16–19 person-days** — less than the previous estimate because
one universal converter covers both directions at once, instead of two
separate bridge binaries.
