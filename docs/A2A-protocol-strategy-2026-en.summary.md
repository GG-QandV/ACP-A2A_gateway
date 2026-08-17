# A2A Protocol Strategy 2026 — Summary (EN)

**Products:** `ACP-A2A_gateway` (gateway) · `agent-connector` (adapter).

**Decision in one sentence:** base A2A dialect = **SDK (v1.0, ProtoJSON)**; **Spec (pre-1.0)** as fallback for legacy clients; **ACP** as deep fallback for inherited installations. ANP (W3C DID) — separate niche, out of scope.

## Why

- The agent-protocol ecosystem consolidated in 2026: **A2A won** agent↔agent (Linux Foundation, v1.0, TSC of 8 vendors: AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, ServiceNow, 150+ orgs). **MCP** is the complementary agent↔tools layer. **ACP** was wound down and merged into A2A (repo archived 2025-08-29).
- A2A **v1.0 introduced a breaking change**: canonical wire is now **ProtoJSON** — PascalCase methods (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE` enums, unified `Part`. This is the **SDK dialect** = our base.
- The old wire (`message/send`, `tasks/get`, lowercase) is **pre-1.0 legacy** = the **Spec dialect** = our fallback, so clients still on the old dialect keep working.
- Official SDKs ship an opt-in `legacyCompat` layer — we follow the standard's own mechanism, not inventing our own.

## Priority

| # | Dialect | Role |
|---|---|---|
| 1 | **A2A SDK (v1.0)** | base |
| 2 | **A2A Spec (pre-1.0)** | fallback |
| 3 | **ACP** | deep fallback |
| — | **ANP (W3C DID)** | out of scope |

Older dialects are supported **for a defined period** — migration/refactoring always has a cost for clients.

## Key subtask

**Dialect probe** — one idempotent primary request (`GetTask`/`tasks/get` with a nonexistent `task_id`) that immediately reveals which dialect the client speaks. Agent Card `protocolVersion` takes priority; result is cached per endpoint; SDK wins on ambiguity. Details: §9.2.

## Links

- **Full strategy (EN):** [A2A-protocol-strategy-2026-en.md](A2A-protocol-strategy-2026-en.md)
- Ukrainian version: [A2A-protocol-strategy-2026-uk.md](A2A-protocol-strategy-2026-uk.md)
- Russian version: [A2A-protocol-strategy-2026.md](A2A-protocol-strategy-2026.md)
- Unified ТЗ (gateway / adapter / probe): [TZ-a2a-dialects-gateway-adapter.md](TZ-a2a-dialects-gateway-adapter.md) (in the gateway repo)