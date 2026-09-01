# Draft: gateway in the cloud

> **Language:** English · [Русская версия](05-cloud-architecture-draft-ru.md)

The current architecture (Phase 1) is single-process, static token,
in-process `Registry`, file-based `TaskStore` on local disk. This
document records what **gets added** to the architecture for cloud
multi-tenant deployment — not a rewrite, but a layer on top of the
existing core.

---

## 1. What stays unchanged

`core/` (agent.rs, convert.rs, lease.rs, reply.rs) — the protocol logic
knows nothing about tenants, cloud, OAuth2 authorization. This is a
deliberate architectural decision from previous stages of the thread: `core`
works with one agent at a time, regardless of how the calling code found
and authorized it. This property directly simplifies the cloud transition —
there is no need to thread `tenant_id` through the entire converter.

---

## 2. New modules for the cloud

### 2.1 Auth: OAuth2 instead of a static token

A static `tokens: [...]` in config.yaml does not scale to many
clients/organizations — no revoke without a redeploy, no permission
separation between tenants, no audit of "who exactly made the request" [web:106].

**Decision — Client Credentials Grant** (server-to-server, without a
user browser) as the primary flow for agents/services, and
Authorization Code + PKCE for human-managed clients (Zed,
VS Code) [web:106]:

```
gatewayd/src/auth/
├── mod.rs               # TokenValidator trait (replaces Registry::check_token)
├── static_token.rs      # StaticTokenValidator — current behavior, for dev/on-prem
├── oauth2_jwt.rs         # OAuth2JwtValidator — validates JWTs from an external IdP
└── token_cache.rs        # short-lived cache of validated tokens (avoid
                          # an IdP round trip on every request)
```

```rust
#[async_trait]
pub trait TokenValidator: Send + Sync {
    /// Returns identity (tenant_id + scopes), not just a bool —
    /// this is exactly the extension point the architecture guide mentioned.
    async fn validate(&self, token: &str) -> anyhow::Result<Identity>;
}

pub struct Identity {
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub subject: String, // sub claim from the JWT, for audit
}
```

Flow for the JWT variant, following the AgentCore Gateway pattern [web:93]: the client
obtains a JWT from the Authorization Server (Okta/Auth0/Cognito) via
client_credentials, sends it in `Authorization: Bearer <jwt>`, gatewayd
validates the signature and claims **itself** (via the IdP's JWKS endpoint, with a cache
of public keys) — without an IdP round trip on every request, only on the
first validation or when the key cache expires.

**On-behalf-of (OBO) for multi-agent scenarios**: if the gateway itself
must call a third-party service on behalf of the user (not merely proxy),
a token exchange is needed — the incoming JWT is exchanged for a new,
audience-bound token for a specific downstream agent, without passing the
original user token further down the chain [web:93]. This is a separate
module `auth/token_exchange.rs`, needed only if downstream agents themselves
check the audience claim.

### 2.2 Multi-tenant routing

The current `Registry` is a flat `HashMap<String, AgentEntry>`, shared by
all clients. In the cloud, agents belong to tenants, and one tenant must
not see/use another tenant's agents:

```rust
// registry.rs — an extension, not a replacement
pub struct AgentEntry {
    pub transport: Transport,
    pub tenant_id: String,        // NEW
}

impl Registry {
    // BEFORE: pub fn lookup(&self, agent_id: &str) -> Option<&AgentEntry>
    // AFTER: tenant_id is mandatory in lookup — an agent from another tenant
    //        will not be found, even if agent_id matches as a string.
    pub fn lookup(&self, tenant_id: &str, agent_id: &str) -> Option<&AgentEntry> {
        self.agents.get(agent_id).filter(|e| e.tenant_id == tenant_id)
    }
}
```

`tenant_id` comes from the `Identity` returned by `TokenValidator` — not
from the client's request body (you cannot trust a client to claim "I am tenant X").

**Registry storage**: at this scale a flat YAML file no longer
works — a database is needed (Postgres, given the stack already used in the
user's other projects) with an `agents(tenant_id, agent_id,
transport_config)` table, and `Registry` becomes a thin wrapper over a
DB query with a cache (TTL 30-60s, so as not to hit the DB on every request).

### 2.3 Rate limiting — per-tenant, not per-IP

The key rule from multi-tenant SaaS practice: **the bucket must be
keyed by tenant_id, not by IP** — corporate clients often share
egress IPs, and a per-IP limit punishes the wrong client [web:102][web:103].
Recommended hierarchy — three layers of limits [web:103]:

```
Layer 1: global infrastructure limit  (circuit breaker, last resort)
Layer 2: per-tenant limit             (the main isolation boundary)
Layer 3: per-agent (or per-API-key) sublimit (inside a tenant)
```

```rust
gatewayd/src/rate_limit.rs

pub struct RateLimiter {
    // token bucket per tenant_id, with a fast path in process memory and
    // periodic reconciliation with a central store (Redis) — hybrid pattern,
    // a trade-off between latency (in-memory) and accuracy in multi-instance
    // deployments (Redis) [web:107].
    local_buckets: DashMap<String, TokenBucket>,
    redis: Option<redis::Client>, // None = single-instance mode, no Redis
}

impl RateLimiter {
    pub async fn check(&self, tenant_id: &str) -> Result<(), RateLimitExceeded>;
}
```

The middleware order in the request is fixed — this matters for billing
correctness [web:102]: **Auth → Billing quota → Rate limit → Dispatch**.
Token validation must happen before the limit check (not the other way around) —
otherwise anonymous/invalid requests would consume the rate-limit budget.

### 2.4 Sticky routing for stateful sessions

`AcpAsA2a`/`A2aAsAcp` keep state in process memory (`session:
Mutex<Option<SessionId>>`, `adapters: HashMap` in `transport_http.rs`).
With horizontal scaling (>1 gatewayd instance behind a balancer),
this creates a problem: if a session was created on instance A and the next
request is sent by the balancer to instance B — the session will not be found.

Three solution options, from simple to complex:

| Option | Mechanism | When sufficient |
|---|---|---|
| Session affinity at the balancer | L4/L7 sticky sessions by cookie/client-IP [web:95][web:104] | Small number of instances, WebSocket/long TCP connections — the simplest path for the cloud-MVP phase |
| Consistent hashing by session_id | The balancer/ingress hashes `session_id` from the request to an instance | If you need to scale beyond the balancer's sticky-sessions capabilities |
| External session store | `session: Mutex<Option<SessionId>>` is replaced by a Redis-backed store; any instance can serve any session | Full stateless scalability, but requires rewriting `AcpAsA2a`/`A2aAsAcp` against an external store — the most expensive option |

For the cloud MVP, **session affinity at the balancer** is recommended
(option 1) — it requires no changes in `core/` and is solved by infrastructure
configuration (`nginx.ingress.kubernetes.io/affinity: cookie` or
the equivalent of the cloud provider in use) [web:104][web:101].
ACP stdio agents are already bound to one process on one instance —
sticky routing simply guarantees the client lands on the same one.

### 2.5 Observability (what gets added beyond the current `tracing`)

The current `tracing`/`tracing-subscriber` writes only to the instance's stdout —
insufficient for multi-tenant diagnostics:

```
gatewayd/src/observability/
├── metrics.rs        # prometheus-compatible metrics: active sessions
│                      # per tenant, latency per direction (1-4), errors
├── audit_log.rs        # structured audit log (who/when/
│                      # which agent_id) — separate from debug logs
└── tenant_context.rs   # tracing::Span with tenant_id, to correlate
                        # one tenant's logs across all components
```

The "active sessions per tenant" metric directly feeds rate limiting
(§2.3) and billing — not a separate task, but shared data.

---

## 3. Final structure (what gets added to the repository tree)

```
gatewayd/src/
├── auth/                          # NEW — §2.1
│   ├── mod.rs
│   ├── static_token.rs             # old behavior preserved as one of the variants
│   ├── oauth2_jwt.rs
│   ├── token_cache.rs
│   └── token_exchange.rs           # optional, if OBO is needed
├── rate_limit.rs                  # NEW — §2.3
├── observability/                  # NEW — §2.5
│   ├── metrics.rs
│   ├── audit_log.rs
│   └── tenant_context.rs
├── registry.rs                    # EXTENDED — tenant_id in AgentEntry (§2.2)
├── transport_tcp.rs                # EXTENDED — middleware order Auth→Quota→RateLimit
├── transport_http.rs               # EXTENDED — same
└── transport_a2a_passthrough.rs    # EXTENDED — same
```

`core/` does not change at all — all cloud complexity lives in `gatewayd`,
which confirms the original architectural decision "the core knows nothing about
the transport and the authorization method".

---

## 4. Implementation order (not everything at once)

1. **OAuth2 (§2.1) first** — without it, multi-tenancy (§2.2) makes no
   sense, since there is no reliable way to determine the request's `tenant_id`.
2. **Multi-tenant routing (§2.2)** — right after auth; this is the foundation of isolation.
3. **Rate limiting (§2.3)** — protects against noisy neighbor between tenants,
   becomes critical once tenants are more than one paying client.
4. **Sticky routing (§2.4)** — needed only with horizontal
   scaling (>1 instance); with a single gatewayd instance in the cloud
   this step can be skipped entirely.
5. **Observability (§2.5)** — can be done in parallel with 1-3, does not
   block the rest, but the earlier it is done, the easier it is to diagnose
   problems with 1-4 during rollout.
