# A2A Protocol Strategy 2026 — SDK-first (v1.0) with Spec (pre-1.0) and ACP fallbacks

**Rationale for the architectural decision in `ACP-A2A_gateway` and `agent-connector`**

- **Status:** approved for implementation (prepared for presentation in the remote GitHub repos).
- **Date:** 2026-08-17
- **Products:** `ACP-A2A_gateway` (gateway) and `agent-connector` (adapter/connector).
- **Decision in one sentence:** the base A2A dialect is **SDK (v1.0, ProtoJSON)**; **Spec (pre-1.0)** is supported as a fallback for backward compatibility with legacy clients; **ACP** is kept as a deep fallback for inherited installations. ANP (W3C DID) is out of scope (a separate niche).

---

## 1. TL;DR

1. The agent-protocol ecosystem **consolidated in 2026**: A2A won the horizontal layer (agent↔agent), MCP the vertical layer (agent↔tools), and ACP was wound down and merged into A2A.
2. A2A reached **v1.0** (March 2026, Linux Foundation, TSC of 8 vendors, 150+ organizations) — a stable production standard.
3. v1.0 introduced a **breaking change**: the canonical wire is now **ProtoJSON** — PascalCase methods (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE` enums, a unified `Part` type. This is the "SDK dialect".
4. The old JSON-RPC binding (`message/send`, `tasks/get`, lowercase) is the **pre-1.0 wire**, officially designated as legacy. This is the "Spec dialect".
5. **Our products** speak/accept: **base = SDK (v1.0)**; **fallback = Spec (pre-1.0)** — so clients that only speak the old dialect keep working; **deep fallback = ACP** — for inherited installations.
6. **ANP (W3C DID)** is a separate niche for the open web (decentralized identity) and does not replace A2A in our scope.
7. Support policy: **older dialects are supported for a defined period**, acknowledging that migrating/refactoring products always has a cost (see §6).

---

## 2. Context: consolidation of the agent-protocol ecosystem (2026)

### 2.1 A2A — winner of the horizontal layer

A2A was created by Google in April 2025 and donated to the Linux Foundation:

> "Agent2Agent (A2A) is an open protocol enabling communication and interoperability between opaque agentic applications."
> — [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)

> "A2A is an open protocol created by Google for secure agent-to-agent communication and collaboration... with growing support from more than 100 leading technology companies."
> — [Linux Foundation: Launch of the Agent2Agent Protocol Project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)

Governance moved to a neutral TSC at v1.0:

> "The A2A Technical Steering Committee includes representatives from AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, and ServiceNow."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "A2A has reached v1.0 with cryptographically signed Agent Cards, gRPC bindings, and production deployments inside Azure AI Foundry, Amazon Bedrock AgentCore, and Google Agent Engine."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 2.2 ACP — wound down and merged into A2A

IBM Research launched ACP in March 2025 as a competitor to A2A. In August 2025 the protocol officially merged with A2A, the repository was archived, and active development ceased:

> "Today, we're excited to share that ACP is officially merging with the A2A under the Linux Foundation umbrella... As part of this transition, the ACP team will be winding down active development and will begin contributing its technology and expertise directly to A2A."
> — [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)

> "The repo was archived on August 27, 2025 and set to read-only. In other words, ACP as a standalone protocol was over."
> — [DEV Community: Mapping MCP, A2A, and ACP (2026-06-28)](https://dev.to/kanywst/mapping-mcp-a2a-and-acp-telling-ai-agent-protocols-apart-in-2026-1hha)

> "The ecosystem consolidated on A2A, with ACP's REST-first instincts absorbed into the surviving standard rather than thrown away."
> — [Oshri Cohen: ACP: The Agent Protocol That Merged Into A2A (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

### 2.3 MCP and A2A — two complementary layers (not competitors)

The standard explicitly separates the roles:

> "MCP is for application-to-tool interaction; A2A is for agent-to-agent."
> — [Tyk.io: Agent protocols — MCP, A2A, ACP (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

Both protocols are now under a single Linux Foundation umbrella via the Agentic AI Foundation (AAIF):

> "The Linux Foundation... today announced the formation of the Agentic AI Foundation (AAIF), and founding contributions of three leading projects... Anthropic's Model Context Protocol (MCP), Block's goose, and OpenAI's AGENTS.md."
> — [Linux Foundation: Formation of AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)

### 2.4 Conclusion for our strategy

- A **third dialect will not emerge** in the A2A horizon — protocol competition is over, there is one standard.
- The SDK/Spec "dialects" in our code are **not two protocols but two eras of the same A2A**: v1.0 (SDK/ProtoJSON) and pre-1.0 (Spec/legacy JSON-RPC binding).
- The correct strategy is therefore to **base on v1.0 (SDK)** and keep the old wire as a compatibility layer.

---

## 3. Why base = A2A SDK (v1.0 / ProtoJSON)

### 3.1 v1.0 — a stable production standard

> "A2A Protocol Ships v1.0: Production-Ready Standard for Agent-to-Agent Communication... The v1.0 release emphasizes maturity rather than reinvention."
> — [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)

> "The v1.0 release in early 2026 added four capabilities that moved A2A from prototype to production-grade."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

### 3.2 ProtoJSON: a single canonical wire

At v1.0 the protocol became proto-first, with ProtoJSON serialization (ADR-001). Methods are PascalCase, matching the gRPC RPCs:

> "Method names are **PascalCase, matching the gRPC RPCs** — `SendMessage`, `GetTask`, `CancelTask`, etc.... `message/send` / `tasks/get` were the **pre-1.0** JSON-RPC binding."
> — [a2aproject/a2a-rs, issue #35: Wire incompatibility (maintainer response)](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "Enums are **SCREAMING_SNAKE_CASE** — `"role": "ROLE_USER"`, `"state": "TASK_STATE_COMPLETED"`. ADR-001 calls the casing change out explicitly as a breaking change from the old lowercase form."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

> "The big one: `text`, `file`, and `data` are unified into a single `Part` type — no more separate `TextPart`/`FilePart`/`DataPart`, and no `kind` discriminator to carry around."
> — [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

Canonical method set (SDK dialect):

| Operation | JSON-RPC method (v1.0) | REST (v1.0) |
|---|---|---|
| Send a message | `SendMessage` | `POST /v1/message:send` |
| Streaming | `SendStreamingMessage` | `POST /v1/message:stream` |
| Get a task | `GetTask` | `GET /v1/tasks/{id}` |
| List tasks | `ListTasks` | `GET /v1/tasks` |
| Cancel a task | `CancelTask` | `POST /v1/tasks/{id}:cancel` |
| Subscribe | `SubscribeToTask` | `GET /v1/tasks/{id}:subscribe` |

> Source: [a2a-rs-core on docs.rs](https://docs.rs/crate/a2a-rs-core/latest) (JSON-RPC at `POST /v1/rpc`, REST at `/v1/message:send`, `/v1/tasks/*`) and [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs).

### 3.3 Built-in versioning and compatibility

v1.0 solved the "which dialect does the server speak" problem **at the standard level**:

- **Agent Card** carries `protocolVersion` (`"0.3"`, `"1.0"`, etc.) — the client selects the format from it (no probe).
- **`A2A-Version` header** — the client declares its version, the server answers `VersionNotSupportedError` on mismatch.
- Official SDKs include an **opt-in pre-1.0 compatibility layer** — exactly the "translator" we discussed:

> "A v1.0 server can transparently accept v0.3 clients (and a v1.0 client can transparently talk to v0.3 servers) by opting into the compat layer with `legacyCompat: { enabled: true }`."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js)

> "Client Agents that require latest features of the protocol should be configured to request specific versions and avoid automatic fallback to older versions, to prevent silently losing functionality."
> — [A2A Protocol Specification v1.0, §3.6 Versioning](https://a2a-protocol.org/latest/specification/)

### 3.4 Official SDKs and transports

The reference implementation (`a2a-rs`, workspace under A2A v1) supports: JSON-RPC 2.0 over HTTP, REST/HTTP+JSON, gRPC (tonic), SLIMRPC, SSE. The client itself negotiates the transport from the Agent Card:

> "The CLI resolves the public agent card from a base URL, negotiates JSON-RPC or HTTP+JSON..."
> — [a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs)

**Conclusion:** SDK (v1.0) is the only dialect with a future: official SDKs, signed cards, OAuth 2.0 (PKCE, Device flow), gRPC, built-in version negotiation. This is our base.

---

## 4. Why fallback = A2A Spec (pre-1.0)

Despite v1.0, the **old wire is still alive** — it is spoken by
- clients/agents locked to the pre-1.0 binding (`message/send`, `tasks/get`, lowercase);
- Python `a2a-sdk` and other SDKs that have not yet migrated:

> "the `message/send` + lowercase-`user` expectation is the **pre-1.0** A2A wire (the older Google spec, or an SDK still on it such as the Python `a2a-sdk`)."
> — [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

The standard itself keeps backward compatibility via **legacy aliases**:

> "The v1.0 SDKs... an opt-in compatibility layer for v0.3 peers" / "`message/send`-style slash aliases + lowercase enum deserialization... gated behind a feature/flag, as long as the default stays v1.0.0."
> — [a2aproject/a2a-js](https://github.com/google-a2a/a2a-js) and [a2aproject/a2a-rs, issue #35](https://github.com/EmilLindfors/a2a-rs/issues/35)

Therefore **Spec (pre-1.0) stays in our products as a fallback**: the gateway and the adapter understand both `SendMessage` and `message/send` — so clients that only speak the old dialect keep working without rework. This is a business-compatibility requirement, not a technical necessity.

---

## 5. Why deep fallback = ACP

ACP as a standalone protocol is dead (see §2.2), but:

- there are **inherited installations** of BeeAI/ACP agents;
- migration takes time, and until clients move — they must keep working;

> "If you built on ACP, you weren't wrong... when the merge came, migrating to A2A was a port, not a rewrite, because the concepts mapped almost one to one."
> — [Oshri Cohen (2026-07-18)](https://www.oshricohen.me/blog/acp-the-protocol-that-merged-into-a2a/)

The official stance: **new projects should target A2A**, ACP is only historical compatibility:

> "New projects should use A2A; ACP is now historical context."
> — [MegaOneAI: MCP vs A2A vs ACP (2026-06-01)](https://megaoneai.com/analysis/mcp-vs-a2a-vs-acp-ai-agent-protocols/)

> "Migrate to A2A" — official IBM guidance: [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)

**Conclusion:** ACP in our products is a **deep fallback** (the last step), only for inherited clients. New connections use A2A (SDK or Spec).

---

## 6. Version support policy

We **support the older dialects for a defined period** (Spec pre-1.0 and ACP) in parallel with the base SDK (v1.0), because:

1. Migrating/refactoring software and products always has a cost (time, risk, testing).
2. Some clients physically cannot migrate at once — they run on frozen environments, internal installations, legacy code.
3. The standard itself guarantees compatibility — we only follow its mechanism (compat layers, legacy aliases).

Support order (strict priority for incoming requests):

| Priority | Dialect | Status | Role |
|---|---|---|---|
| 1 | **A2A SDK (v1.0, ProtoJSON)** | current, evolving | **base** |
| 2 | **A2A Spec (pre-1.0)** | legacy, no new features | fallback |
| 3 | **ACP** | frozen, merged into A2A | deep fallback |
| — | **ANP (W3C DID)** | separate niche | out of scope (§7) |

Deprecation policy: a dialect is removed no earlier than one major cycle after confirming (from gateway logs) that no active connections remain on it.

---

## 7. ANP (W3C DID) — a separate niche, out of scope

ANP (Agent Network Protocol) solves a different layer of the problem: **decentralized identity and open-web infrastructure** for agents (DID, `did:wba`, WNS handles, discovery, E2E messaging) — not a competing JSON-RPC dialect for task delegation.

> "ANP aims to become the HTTP of the Agentic Web era: a protocol suite for identity, naming, discovery, negotiation, secure messaging, and application-level collaboration."
> — [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)

> "ANP... based on the W3C DID standard... ensuring that any two agents can securely verify each other's identity and establish private, reliable encrypted communication channels without central authority intervention."
> — [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)

The ecosystem sees A2A and ANP as **complementary**, not replacements:

> "Together with Anthropic's Model Context Protocol (MCP), these specifications now form a coherent layered stack: MCP handles vertical tool access, A2A handles horizontal agent-to-agent delegation, and ANP extends that federation to the open web."
> — [Zylos Research (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)

**Decision:** ANP is not added as a dialect. If decentralized identity is ever needed, it is a separate project on top of A2A (e.g., `did:wba` as an authentication method in the gateway), not a change to the wire layer. Standards: [W3C DID Core](https://www.w3.org/TR/did-core/), [W3C DID v1.1](https://www.w3.org/TR/did-1.1/).

---

## 8. References (official documents)

### A2A / standard
- [A2A Protocol Specification v1.0](https://a2a-protocol.org/latest/specification/)
- [A2A Protocol v1.0 announcement](https://a2a-protocol.org/latest/announcing-1.0/)
- [GitHub: a2aproject/A2A](https://github.com/a2aproject/A2A)
- [ADR-001: ProtoJSON serialization](https://github.com/a2aproject/A2A/blob/main/adrs/adr-001-protojson-serialization.md)
- [Commit ae6a562: v1.0 operation-name standardization](https://github.com/a2aproject/A2A/commit/ae6a562d5d972f2c4b184f748bb32e1fa9aa7bf2)

### SDK
- [a2aproject/a2a-rs (Rust)](https://github.com/a2aproject/a2a-rs)
- [a2aproject/a2a-js (TypeScript) — incl. legacyCompat](https://github.com/google-a2a/a2a-js)
- [a2a-rs-core on docs.rs (methods/transports)](https://docs.rs/crate/a2a-rs-core/latest)
- [a2a-rs, issue #18: JSON-RPC method names](https://github.com/a2aproject/a2a-rs/issues/18)
- [a2a-rs, issue #35: wire incompatibility pre-1.0 vs v1.0](https://github.com/EmilLindfors/a2a-rs/issues/35)

### Ecosystem / consolidation
- [Linux Foundation: Launch of A2A project (2025-06-23)](https://www.linuxfoundation.org/press/linux-foundation-launches-the-agent2agent-protocol-project-to-enable-secure-intelligent-communication-between-ai-agents)
- [LF AI & Data: ACP Joins Forces with A2A (2025-08-29)](https://lfaidata.foundation/communityblog/2025/08/29/acp-joins-forces-with-a2a-under-the-linux-foundations-lf-ai-data/)
- [BeeAI: ACP to A2A Migration Guide](https://github.com/i-am-bee/beeai-platform/blob/main/docs/community-and-support/acp-a2a-migration-guide.mdx)
- [Linux Foundation: Formation of Agentic AI Foundation / AAIF (2025-12-09)](https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation)
- [Anthropic: Donating MCP and establishing AAIF](https://www.anthropic.com/news/donating-the-model-context-protocol-and-establishing-of-the-agentic-ai-foundation)
- [OpenAI: co-founding AAIF](https://openai.com/index/agentic-ai-foundation/)
- [Zylos Research: A2A, ACP, ANP in Production (2026-04-18)](https://zylos.ai/research/2026-04-18-agent-to-agent-interoperability-protocols/)
- [Tyk.io: Agent protocols guide (MCP, A2A, ACP) (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)
- [Google Cloud Blog: What's New in A2A v1.0 (2026-07-02)](https://medium.com/google-cloud/whats-new-in-a2a-protocol-v1-release-b36dc6b4febd)

### ANP / DID (separate niche)
- [GitHub: agent-network-protocol/AgentNetworkProtocol](https://github.com/agent-network-protocol/AgentNetworkProtocol)
- [ANP Technical White Paper](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/01-agentnetworkprotocol-technical-white-paper.md)
- [did:wba Method Specification (ANP-03)](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/main/03-did-wba-method-design-specification.md)
- [W3C DID Core v1.0](https://www.w3.org/TR/did-core/)
- [W3C DID v1.1 (Candidate Recommendation)](https://www.w3.org/TR/did-1.1/)

---

## 9. ТЗ: correcting/refactoring the products to the strategy

Goal: **align the wire layer of the gateway and the adapter to the SDK → Spec → ACP priorities**, with automatic dialect detection for the client.

> The detailed unified ТЗ lives in `docs/TZ-a2a-dialects-gateway-adapter.md` (Section 1 — gateway, Section 2 — adapter, Section 3 — common dialect probe).

### 9.1 Target connection scheme

| Step | Role | Dialect | Components |
|---|---|---|---|
| **1 (base)** | primary inbound/outbound | **A2A SDK (v1.0)** | gateway: `SendMessage`/`GetTask`/`CancelTask`; adapter: `driver-a2a-client` (`wire_format: sdk`) |
| **2 (fallback)** | compatibility with legacy clients | **A2A Spec (pre-1.0)** | gateway: `message/send`/`tasks/get`/`tasks/cancel`; adapter: `wire_format: spec` |
| **3 (deep fallback)** | inherited installations | **ACP** | adapter: `driver-acp-client` + `protocol-acp-mapper` (only for known legacy agents) |

Rules:
1. **New connections default to SDK (v1.0)**. Spec only if the client's dialect is detected as pre-1.0. ACP only with explicit legacy-agent configuration.
2. The gateway keeps accepting **both A2A dialects inbound** (`/rpc`): by method name (`SendMessage` vs `message/send`) the parser/renderer is chosen (already partially implemented in `transport_http.rs:381-417`).
3. The adapter outbound (toward agents) — base SDK; inbound (A2A server, `protocol-a2a-server`) — also accept Spec (the current `a2a-server` base understands only SDK methods).

### 9.2 Key subtask: the dialect probe (a short primary request)

**Goal:** from a single short request in A2A-SDK style, immediately determine **which dialect the client can/may communicate on** (SDK / Spec / ACP / unknown).

**Principle:** the probe must be **idempotent** — it must not create tasks or have side effects. Use `GetTask`/`tasks/get` with a guaranteed-nonexistent `task_id` (random UUID), not `SendMessage`/`message/send` (those create a real task).

**Algorithm (server-side inbound detection, same in both products):**

```
1. Accept the first request to the agent.
2. Determine the dialect by method name:
     SendMessage | GetTask | CancelTask | ListTasks → SDK (v1.0)
     message/send | tasks/get | tasks/cancel       → Spec (pre-1.0)
     otherwise                                      → ACP/other → see step 5
3. If recognized — answer in the same dialect (parser/renderer by method).
4. Additionally, for clients that have made no call yet:
   GET /.well-known/agent.json → protocolVersion ("1.0" → SDK, "0.x" → Spec).
   This is the preferred channel (no probe needed).
5. If no dialect recognized → return method_not_found with a hint about the
   known dialects (SDK/Spec) and a link to the strategy.
```

**Probe (client side, if the Agent Card is unavailable):**

```
POST /agents/:id/rpc
{ "jsonrpc": "2.0", "id": 1, "method": "GetTask",
  "params": { "name": "tasks/<uuid>" } }            # SDK style
```

Response interpretation:

| Response | Verdict |
|---|---|
| `result` (or a "task not found" error without `method_not_found`) | the server understands **SDK** → work on SDK |
| `-32601` / `-32000` + `method_not_found:` | not SDK → try Spec: |
|   `POST ... { "method": "tasks/get", "params": { "id": "<uuid>" } }` | |
|   "task not found" error | the server understands **Spec** → work on Spec |
|   `method_not_found` for `tasks/get` too | not A2A → try ACP (other interface) |
|   ACP did not recognize it either | explicit error: "client dialect not determined" |

Caching: the detection result is stored **per endpoint** (one probe on first contact); repeat requests do not trigger the probe.

**DoD for the subtask:**
- [ ] the probe does not create tasks (only `GetTask`/`tasks/get` with a nonexistent id);
- [ ] detection via Agent Card (`protocolVersion`) takes priority over the probe;
- [ ] dialect cached per endpoint;
- [ ] SDK priority on ambiguity;
- [ ] a clear error listing the supported dialects when none is detected.

### 9.3 Scope and boundaries

- **Gateway (`ACP-A2A_gateway`):** already accepts SDK+Spec inbound (`transport_http.rs`). Add: detection via Agent Card/`protocolVersion`; SDK-format output for SDK requests.
- **Adapter (`agent-connector`):** `driver-a2a-client` — add `wire_format: auto` (probe + cache) with SDK priority; `protocol-a2a-server` — accept Spec inbound.
- **ACP** — do not extend, only keep the existing `driver-acp-client` for explicitly configured legacy agents.
- **Out of scope:** ANP, DID authentication, new transport (gRPC) — separate tasks.

### P.S. MCP as an alternative

MCP is **not an alternative to A2A**, but a complementary layer: A2A — agent↔agent, MCP — agent↔tools (§2.3). In `agent-connector`, MCP already exists as `driver-mcp` (tools → skills). Using MCP **instead of** A2A is impossible without losing horizontal delegation. Correct pattern: the orchestrator communicates over A2A and calls tools over MCP.

> "A high-level orchestrator agent uses A2A to manage workflows and delegate to specialist agents; each specialist agent then uses MCP internally to call its own tools."
> — [Tyk.io (2026-06-04)](https://tyk.io/learning-center/agent-protocols-a-complete-guide-to-mcp-a2a-and-acp/)

---

## 10. Decision summary

1. **Base — A2A SDK (v1.0/ProtoJSON)**: the future of the standard, official SDKs, production features.
2. **Fallback — A2A Spec (pre-1.0)**: compatibility with legacy clients (Python a2a-sdk and others).
3. **Deep fallback — ACP**: only inherited installations, no development.
4. **ANP/W3C DID — out of scope**: a separate open-web niche.
5. **MCP — not an alternative but the vertical layer** beside/on top.
6. **Older dialects are supported for a defined period**, respecting the migration cost for clients.