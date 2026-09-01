# Decision Register

> **Language:** English · [Русская версия](decisions-ru.md)

Why the code is structured this way and not another. Each entry: what was
decided, because of what, and what will become of it on the move to the cloud.

The "cloud" column is not a work plan but a warning: where a Phase 1
decision will stop working with >1 gatewayd instance. Plan details are in
`05-cloud-architecture-draft.md`, but note that the draft was written before
the P1-1/P1-2/P2-8 edits and in places describes code that no longer exists (see P-16).

---

## Protocol and types

### P-1. The crate is named `gateway-core`, not `core`

The name `core` conflicts with the built-in crate from the extern prelude:
`use core::{...}` in gatewayd gave `error[E0659]: core is ambiguous`.
The project did not build at all.

**Cloud:** no consequences.

### P-2. `protocolVersion` is `u32` — liberal intake, strict output

Per ACP the version is numeric. claurst replies with the number `1`; string-based
implementations exist. Tolerance lives in the deserializer
(`de_protocol_version`: `1`, `"1"`, `"2.1"` → major part), while the
internal type matches the protocol and a number always goes out on the wire.

An intermediate state — accepting both but storing a `String` — was
rejected: a type that lies about its contents outlives its authors.
The gateway in that variant kept sending the agent the string `"1"`; claurst
swallowed it, a strict parser would have rejected it.

**Cloud:** no consequences.

### P-3. The version `default` is a named function, not `Default::default()`

The `u32` default is zero, but a missing field means version `1`.
Caught by a test, not by review.

**Cloud:** no consequences.

### P-4. Client capabilities in `initialize` are declared empty

The gateway does not implement fs, terminal, or permission requests — so it must
not declare them. A correct agent, seeing empty capabilities, will not
send counter-requests.

Consequence: the "see what the agent is asking" scenario is unreproducible
not by accident, but by construction. Until capabilities are declared, the agent
does not use them.

**Cloud:** when implementing the ACP client side (proxying agent
requests to the end A2A client), this decision will have to be revisited —
and P-5 becomes relevant at the same time.

### P-5. The Reader tells agent requests from responses by the presence of `method`

ACP is two-way JSON-RPC. The agent sends requests to the client
(`session/request_permission`, `fs/read_text_file`), and their `id` numbering
starts at one, i.e. it collides with ours. Previously any line with a numeric
`id` was treated as a response: an agent request consumed an entry from
`pending`, there was nobody left to resolve the real response, and the call hung until
the timeout.

We answer an agent request with `-32601`. Staying silent is not an option — otherwise
instead of our own timeout we get an agent hang.

**Caveat:** there is no live confirmation. claurst never sent counter-requests
(see P-4), so this most likely was not the cause of the observed hang —
the real cause turned out to be P-6. The fix is kept as protocol-correct,
but not as verified.

**Cloud:** no consequences.

---

## Agent process lifecycle

### P-6. `initialize` is part of process bring-up, not a separate call

Before: `initialize` was called only from `card()`, i.e. on a request for
`agent.json`. A client going straight to `message/send` brought the agent to
`session/new` without a handshake. After a respawn, the fresh process never received
`initialize`.

Now the handshake is done by `SupervisedStdioAgent::spawn_and_handshake` —
every live process is initialized exactly once. The response is cached,
a repeated `initialize` is not sent: ACP does not contemplate one.

**Live-confirmed:** after kill/respawn the request completes in 2.7s
instead of the 60-second timeout.

**Cloud:** no consequences if the agent process stays local to the
instance (see P-8).

### P-7. Respawn marks lost conversations rather than hiding them

A dead agent used to be cached forever: every request to it failed until
a manual restart of the gateway.

Of the three options (refuse until intervention / quiet respawn / respawn with
a mark) the third was chosen. Reason: with a quiet respawn the client cannot tell
"the agent remembers the conversation" from "the agent restarted and does not remember". For
a gateway that sells conversation continuity, this is unacceptable.

The mechanic is a process generation number. `SessionEntry` remembers which
generation the ACP session was created in; addressing a conversation of a past generation
yields `ContextLost` → `409` / `-32010`. The mark is one-shot: the next
request with the same `contextId` creates a fresh session.

`ensure_ready()` exists in the trait precisely because of ordering: under lazy
respawn the generation was read before the supervisor noticed the death, and the
client got "Invalid params" from the agent instead of an honest `ContextLost`.

**Cloud:** `contextId` must land on the same instance — otherwise the
generation is foreign and the client gets a false `ContextLost`. See P-16.

### P-8. Respawn with 5s backoff, ceiling of 256 conversations per agent

Without backoff, an agent crashing on startup produces a restart loop. The conversation
ceiling is so that the client runs into a limit, not into the gateway's memory.

Both numbers are hardcoded. Deliberately: the config already has nine parameters, and
these two never once required tuning.

**Cloud:** the ceiling becomes per-instance, i.e. the effective limit
is multiplied by the number of replicas. Per-tenant limits need an external counter
(draft §2.3).

---

## Isolation and ownership

### P-9. Session per `contextId`, not per token

Before: one ACP session for the whole adapter, and the adapter is cached by
`agent_id` — any two A2A clients of one agent ended up in a shared
conversation and saw each other's context.

The variant "cache key `(token, agent_id)`" was considered: simpler, but the
isolation comes out per token rather than per conversation, and every new client
requires a separate token in the config plus a gateway restart. The
`contextId` variant requires nothing from the client — it is a standard A2A field
it already sends.

Key observation: one stdio process holds many ACP sessions, so there is no need
to multiply processes. **Live-confirmed on claurst 0.1.7.**

**Cloud:** session registry in process memory. Draft §2.4 proposes
sticky routing — that is enough, but precisely because of this decision, not
because of what is written there (see P-16).

### P-10. The owner is the token hash, not the token itself

To answer the question "the same client?" equality is enough, and there is no reason
to keep the secret in memory and on disk longer than necessary.
`DefaultHasher`, not cryptographic: brute-forcing is not in the threat model here,
the hash never leaves the process and the store.

**Cloud — this is where the landmine is.** On the move to OAuth2 tokens become
short-lived and rotate. An owner recorded as a token hash,
after rotation, will stop matching, and the client loses access to
its own tasks. The owner should become `tenant_id` (or `sub`) from
`Identity`, not the token. The edit is pinpoint — `Owner::from_token`
is replaced by `Owner::from_identity` — but mandatory, and it requires
migrating already-recorded tasks.

### P-11. Task owner lives in the storage envelope, not in `Task.metadata`

`metadata` is returned to the client in the response; internal attribution has no place
there. The file stores `StoredTask { owner, task }`.

The check goes against the store, not the live session registry — because otherwise
it would not survive session TTL eviction and a gateway restart: the same hole,
just with a delay.

Old-format files (bare `Task`) are read as `owner: None` and are not
rejected — otherwise a gateway upgrade would make accumulated tasks
unavailable to their own owners.

**Cloud:** the envelope format will survive the move to a DB; see P-10 on the
contents of the field.

### P-12. `session/prompt` rejects an unknown `sessionId`

Before: `prompt` took `sessionId` straight from the request and keyed `TurnLease`
with it, and `forget` was never called anywhere. Any submitted identifier permanently
added an entry — a client with a valid token could fill up the gateway's memory
by generating `sessionId` values on the fly.

Now a session exists only if created through `session/new`. That is what
ACP requires; the check runs **before** `acquire`, otherwise a rejected
identifier would have time to leave an entry behind.

The audit phrased this item as "a `session/close` is needed, which A2A does not have". The phrasing
turned out to be wrong: the problem is on the ACP side, where
`session/cancel` exists and comes from the client — it just did not release the lease.
Worth remembering as an example: fixing by the defect description without looking at the code
did not work here.

**Cloud:** `TurnLease` is a mutex in process memory. With >1 instance,
the "one turn per session" guarantee disappears if requests of one session
land on different replicas. Sticky routing restores it; without it
a distributed lease is needed.

---

## Storage and errors

### P-13. Task cleanup by file mtime, not by a time inside the task

`mtime` is updated by the atomic write and does not depend on what the agent
put in the time field (it may put anything, or nothing).

`.json.tmp` files are not touched — that is someone else's half-written record. Cleanup
walks directories on disk, not live adapters: tasks of a
stopped agent also need removing, and its adapter is no longer in memory. The default age
is a week: the client is entitled to pick up the result via
`tasks/get` long afterwards.

**Cloud — the second landmine.** The cleaner runs in every gatewayd
process. With shared storage (network volume, object storage), N replicas
will walk one directory in parallel. Not destructive in itself
(removal is idempotent), but this is N-fold redundant traversal and races on
`remove_file`. Needed: either leader election, or moving cleanup out to a separate
cron job.

### P-14. Agent refusal codes separated by meaning

`404` — about addressing, a retry will not help. `503` — about availability, worth
retrying. `400` — wrong transport.

Before, all three cases returned `404` / `-32601`, and a spawn failure
looked like a typo in `agent_id`. It is exactly what led diagnostics astray during
the live-test analysis with `protocolVersion`: the message lied about the nature of
the problem. In production such an error costs not one debugging cycle but every
incident.

**Cloud:** the codes will survive the move; `401`/`403` from the Auth layer will be added.

### P-15. `AgentCard.url` is the specific agent's address, not the gateway's

The card describes the endpoint the client will send `message/send` to:
`{public_url}/agents/{agent_id}/rpc`. Before, the field was
empty, i.e. the card was invalid per the A2A spec — and `agent.json` is the
first thing an external client reads.

`public_url` is the external address, not the bind address: behind a reverse proxy this is
the proxy's domain. The default `http://localhost:8348` is safe, but almost
certainly wrong for any real deployment.

**Cloud:** the value will become tenant-dependent if per-tenant domains
appear.

---

## System boundaries

### P-16. The cloud architecture draft is partially outdated

`05-cloud-architecture-draft.md` §2.4 describes
`session: Mutex<Option<SessionId>>` as the current state. That field
has not existed since P-9: the state is now `HashMap<ContextId, SessionEntry>`
plus the session registry in `A2aAsAcp` (P-12) plus the adapter cache plus
`TurnLease`.

The §2.4 conclusion nonetheless remains correct and even strengthens: instance-local
state has become **more**, not less, and sticky routing has turned from "desirable"
into "mandatory" for four independent structures:

| State | Where | What breaks without sticky |
|---|---|---|
| `sessions: HashMap<ContextId, SessionEntry>` | `AcpAsA2a` | Conversation not found on another replica |
| process `generation` | `SupervisedStdioAgent` | False `ContextLost` from a foreign generation (P-7) |
| ACP-side `sessions` | `A2aAsAcp` | `session/prompt` rejects a valid session (P-12) |
| `TurnLease` | both converters | The "one turn per session" guarantee disappears |

Before the cloud stage, the draft should be re-read against the current code, not
against the description inside it.

### P-17. TLS and rate limiting are delegated to the reverse proxy

Certificates, rotation, HTTP/2, limits — not the gateway's concern. The gateway listens
over HTTP and does not terminate TLS itself; exposing it directly to an untrusted
network is not an option, tokens would travel in cleartext. Recorded in
`config.example.yaml` so the decision does not look like an oversight.

**Cloud:** for rate limiting the decision changes — per-tenant limits
(draft §2.3) are not implemented by the proxy, because it does not know about tenants.
TLS stays with the proxy/ingress.

### P-18. Streaming is not implemented — Phase 2

`Reply<T, U>` exists as a seam: `Complete` now, `Streaming` later,
without changing trait signatures. `Reply::Streaming` returns an error
rather than panicking — in a network service `unreachable!()` takes down a worker task.

Practical consequence: `session/update` messages from the agent are collected into a buffer and
delivered as one chunk at the end of the turn. The client does not see the answer as it is
generated.

**Cloud:** SSE through passthrough (direction 2) works and is
live-tested — there the gateway simply pours bytes through. Streaming is absent
precisely on the converting directions 3 and 4.

### P-19. Non-ASCII `taskId` values collapse to one filename — left as is

`sanitize_task_id` strips all non-ASCII characters, so two different
Cyrillic identifiers produce one filename and overwrite each other. Found by accident: a test
task named "старая" ("old") turned into
`.json`.

Not reproducible from outside — the gateway generates the identifiers, and they are ASCII.
Fixing it now would mean changing the naming scheme and migrating existing files for
a problem that does not exist. The behavior is pinned by the test
`non_ascii_ids_collapse_to_same_name`.

**Cloud:** becomes relevant if `taskId` starts being accepted from
the client, or if storage moves to a DB with a different key scheme.

---

## How this was written

Two practical takeaways from the process that deserve a line of their own.

**Unit tests against a fake agent do not catch everything.** A fake is by
definition obedient: it did not die lazily, did not ask counter-questions, and did not demand a
handshake — which is why three defects (P-6, the ordering in P-7, P-5)
survived to the live stand. Every time the fake was taught to misbehave, the tests
started catching.

**Negative control is mandatory.** A test added together with the fix is
checked by reverting the fix: if it does not go red, it is checking the wrong thing.
That is how `body_limit_is_bounded` in its first revision was weeded out — an `assert!`
over constants that clippy rightly called a tautology.
