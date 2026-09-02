---
name: gateway-client
description: >-
  How an agent (Claude Code, Gemini CLI, Codex, Cline, opencode, hermes, any
  ACP/A2A client) talks to ACP-A2A_gateway: addresses, tokens, message/send and
  session/prompt formats, continuing a conversation via contextId, isolation
  between clients, known limitations. Trigger: "send a task through the
  gateway", "talk to agent X through the gateway", "A2A message/send", "why
  does continue time out".
---

# gateway-client — how an agent uses ACP-A2A_gateway

> **Language:** English · [Русская версия](SKILL-ru.md) · [Українська версія](SKILL-uk.md)

The gateway (`GG-QandV/ACP-A2A_gateway`) is a router between clients and
ACP agents. Two ports, four directions. You can be both a client (reaching
agents through the gateway) and an agent (you get connected as a stdio agent).

## 0. Which agents speak ACP

Register any of these as an `agents[]` entry; the gateway spawns the process and
talks ACP over its stdio.

| Agent | How it exposes ACP |
|---|---|
| opencode | native: `opencode acp` |
| hermes | native: `hermes acp` |
| Claude Code (Anthropic) | adapter: `claude-agent-acp` — <https://github.com/agentclientprotocol/claude-agent-acp> |
| Codex (OpenAI) | adapter: `codex-acp` — <https://github.com/agentclientprotocol/codex-acp> |
| Gemini CLI | native mode: `gemini --acp` |
| Cline | ACP CLI — <https://cline.bot/cli> |
| Cursor | ACP mode — <https://cursor.com/docs/cli/acp> |

The full curated list is the [ACP agent registry](https://agentclientprotocol.com/get-started/registry).
An agent that speaks neither ACP nor A2A (e.g. OpenClaw) cannot be registered as a stdio
agent: put it behind an adapter, or expose it as an A2A server and reach it through
direction 2. Agent-specific environment variables go into `agents[].env` of `config.yaml`;
the `agent_id` values available on a given host are whatever that host's `config.yaml` defines.

## 1. When you are a client (reaching agents through the gateway)

### TCP (ACP client, directions 1/2)

```bash
# listen port (default 8347), first line — handshake, then line-delimited ACP JSON-RPC
{ printf '%s\n' '{"token":"TOKEN","agent_id":"<agent_id>"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[],"additionalDirectories":[]}}'
} | nc 127.0.0.1 8347
```

Each `session/new` gives a **new** session on the same agent process — you can
run many conversations over one TCP stream. Responses are JSON lines, correlated by `id`.

### HTTP (A2A client, directions 3/4)

```bash
# http_listen port (default 8348)
curl -s http://127.0.0.1:8348/agents/<agent_id>/.well-known/agent.json \
  -H "Authorization: Bearer <token>"

curl -s -X POST http://127.0.0.1:8348/agents/<agent_id>/rpc \
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send","params":{
        "message":{"role":"user","parts":[{"kind":"text","text":"привет"}]}
      }}'
```

## 2. Formats — critical, verified against live agents

### A2A `message/send`

- `message.parts[].kind` — `text` / `file` / `data` (tagged enum, kebab/lowercase).
- Response: `result.context_id` (**snake_case**), `result.id` (task), `result.status`,
  `result.artifacts`.
- **Continuing a conversation**: `contextId` goes into `message.contextId`
  (or `params.contextId`), **NOT** into `configuration` — configuration does not read it.
- Without `contextId` → the gateway starts a new conversation.
- Another client's existing `contextId` → error `contextId принадлежит другому клиенту`
  (isolation between tokens, IDOR closed).
- A nonexistent `contextId` → not rejected, a new one is created (by design).
- A session lives 24h idle; cap of 256 conversations per agent.

### ACP `session/prompt` (if you talk to an agent directly over TCP)

- `prompt` — a **sequence** of ContentBlocks, NOT a string (string → `-32602`).
- Blocks are written with a `type` field (not `kind`): `{"type":"text","text":"..."}`.
- `initialize` → `session/new` (remember `sessionId`) → `session/prompt` with that `sessionId`.

## 3. When you are connected as an agent (stdio)

- Your command must be the agent's **ACP entry point**:
  `opencode acp`, `hermes acp`, `gemini --acp`, or an adapter binary such as
  `claude-agent-acp` / `codex-acp`. This is **not** `--bare`, **not** `--print`.
- Your stdout must be a **real pipe** (not a file): `> log` crashes Hermes
  with `Pipe transport is only for pipes`. The gateway spawns via `Stdio::piped()` — OK.
- Headless agents typically need flags so they neither open a browser nor re-authenticate
  on every spawn — pass them through `agents[].env` in `config.yaml`.
- Expect the gateway/client to call `session/new` **repeatedly** on one process —
  that's normal; reply with a new `sessionId` each time.

## 4. Known limitations (don't panic, it's not your bug)

| Symptom | Cause | Workaround |
|---|---|---|
| `continue` by contextId times out (2nd `message/send` into the same session) | Gateway converter defect (direction 4), reproduces on stdio agents on this host | Keep each request in a new session (no contextId) or fix the gateway; continuation works via direct `session/prompt` |
| `Reply::Streaming` — error "Phase 1: streaming is not implemented" | Streaming not implemented in the converter | Blocking calls only |
| Agent response timeout | `agent_call_timeout_secs` (default 120) | Increase in `config.yaml` |
| Error `-32010` / HTTP 409 on an old `contextId` | Agent died and was respawned (P2-10): the session belongs to a previous process generation (`ContextLost`) | Repeat the same call — a fresh session gets created. The notice is one-time |
| Agent has hung but appears "alive" | `is_alive` catches death, not a hang | Hits `agent_call_timeout_secs`, increase the timeout |

## 5. Quick check that the gateway is alive

```bash
ss -tlnp | grep -E "8347|8348"        # both ports are listening
curl -s http://127.0.0.1:8348/agents/<id>/.well-known/agent.json -H "Authorization: Bearer <token>"
```
