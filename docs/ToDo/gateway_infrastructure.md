# Gateway Service Infrastructure (enterprise)

> **Language:** English · [Русская версия](gateway_infrastructure-ru.md)

**Version**: 1.0  
**Date**: 2026-08-18  
**Status**: For the future (for the enterprise version)

---

## 1. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                       Gateway (gatewayd)                     │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────────┐  │
│  │ Auto-router  │  │    Guard     │  │  Token generator   │  │
│  │ (Local/      │  │  - Verify    │  │  (Token Worker)    │  │
│  │  Server)     │  │  - Rotation  │  │  - Hash collection │  │
│  │              │  │  - Blocking  │  │  - Token issuing   │  │
│  │              │  │              │  │  - Agent binding   │  │
│  └──────────────┘  └──────────────┘  └────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
         │                    │                    │
         ▼                    ▼                    ▼
┌───────────────────┐ ┌───────────────────┐ ┌───────────────────┐
│      Agent        │ │      Journal      │ │  Token storage    │
│ (t-dev-*,         │ │ (who, when,       │ │ (tokens.json)     │
│  t-prod-*,        │ │  status)          │ │  - Token          │
│  mTLS)            │ │                   │ │  - Agent hash     │
│                   │ │                   │ │  - TTL            │
│                   │ │                   │ │  - Status         │
└───────────────────┘ └───────────────────┘ └───────────────────┘
```

---

## 2. Components

### 2.1 Auto-router

**Task**: Determines the mode (LOCAL/SERVER) by token, IP, mTLS.

**Logic**:
```rust
pub fn detect_mode(
    token: &str,
    ip: &str,
    has_client_cert: bool,
) -> anyhow::Result<GatewayMode> {
    // 1. Check the token
    if !token.starts_with("t-dev-") && !token.starts_with("t-prod-") {
        anyhow::bail!("Unknown token type: {}", token);
    }

    // 2. Check whether the token has expired
    if is_token_expired(token) {
        anyhow::bail!("Token expired: {}", token);
    }

    // 3. mTLS: no certificate → deny
    if !has_client_cert {
        anyhow::bail!("No client certificate (mTLS)");
    }

    // 4. Determine the mode
    let mode = if token.starts_with("t-dev-") && is_local_ip(ip) {
        GatewayMode::Local
    } else if token.starts_with("t-prod-") {
        GatewayMode::Server
    } else {
        anyhow::bail!("Unknown token/IP combination");
    };

    Ok(mode)
}
```

---

### 2.2 Guard

**Task**: Verification, rotation, blocking of tokens.

**Token structure**:
```rust
pub struct Token {
    pub id: String,
    pub agent_hash: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: TokenStatus,
}

pub enum TokenStatus {
    Active,
    Expired,
    Revoked,
}
```

**Verification**:
```rust
pub fn verify(&self, token_id: &str, agent_hash: &str) -> anyhow::Result<()> {
    // 1. Look up the token
    let token = self.tokens
        .iter()
        .find(|t| t.id == token_id)
        .ok_or_else(|| anyhow::anyhow!("Token not found: {}", token_id))?;

    // 2. Check the status
    if token.status != TokenStatus::Active {
        anyhow::bail!("Token is not active: {} (status: {:?})", token_id, token.status);
    }

    // 3. Check whether it has expired
    if Utc::now() > token.expires_at {
        anyhow::bail!("Token expired: {}", token_id);
    }

    // 4. Check the agent hash (binding the token to the agent)
    if token.agent_hash != agent_hash {
        anyhow::bail!("Agent hash mismatch (token: {}, agent: {})", 
                     token_id, agent_hash);
    }

    Ok(())
}
```

**Rotation**:
```rust
pub fn rotate_expired(&mut self) {
    for token in &mut self.tokens {
        if Utc::now() > token.expires_at {
            token.status = TokenStatus::Expired;
        }
    }
}
```

**Blocking**:
```rust
pub fn revoke(&mut self, token_id: &str) {
    if let Some(token) = self.tokens.iter_mut().find(|t| t.id == token_id) {
        token.status = TokenStatus::Revoked;
    }
}
```

---

### 2.3 Token generator (Token Worker)

**Task**: Hash collection, token issuing, binding to the agent.

**Hash collection**:
```rust
pub fn collect_agent_hashes(&self) -> anyhow::Result<Vec<String>> {
    let mut hashes = Vec::new();
    let agents = list_agents()?;
    
    for agent in agents {
        // Hash = SHA256(UUID + CPU_ID + MAC)
        let hash = compute_agent_hash(&agent.uuid, &agent.cpu_id, &agent.mac)?;
        hashes.push(hash);
    }

    Ok(hashes)
}

fn compute_agent_hash(uuid: &str, cpu_id: &str, mac: &str) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(uuid.as_bytes());
    hasher.update(cpu_id.as_bytes());
    hasher.update(mac.as_bytes());
    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}
```

**Issuing a token**:
```rust
pub fn issue_token(&self, agent_hash: &str) -> anyhow::Result<Token> {
    let token_id = format!("t-dev-{}", uuid::Uuid::new_v4());
    let issued_at = Utc::now();
    let expires_at = issued_at + Duration::days(30); // TTL 30 days

    let token = Token {
        id: token_id,
        agent_hash: agent_hash.to_string(),
        issued_at,
        expires_at,
        status: TokenStatus::Active,
    };

    self.save_token(&token)?;
    Ok(token)
}
```

---

### 2.4 mTLS (client certificates)

**Task**: Protection against spoofing as a local agent.

**Certificate check**:
```rust
pub fn check_client_cert(cert: Option<&x509::X509>) -> bool {
    cert.map_or(false, |cert| {
        is_trusted_ca(cert) &&
        !cert.not_after().diff(Utc::now()).unwrap().is_negative()
    })
}

fn is_trusted_ca(cert: &x509::X509) -> bool {
    let trusted_cas = load_trusted_cas();
    trusted_cas.iter().any(|ca| cert.issuer() == ca.subject())
}
```

---

### 2.5 Journal for the user

**Format** (journal.jsonl):
```jsonl
{"timestamp": "2026-08-18T14:30:00Z", "event": "agent_connected", "agent": "claurst-main", "token": "t-dev-local-001", "mode": "LOCAL", "status": "allowed", "ip": "192.168.1.100", "mac": "aa:bb:cc:dd:ee:ff"}
{"timestamp": "2026-08-18T14:35:00Z", "event": "token_rotated", "old_token": "t-dev-abc123", "new_token": "t-dev-def456", "agent": "claurst-main"}
{"timestamp": "2026-08-18T14:40:00Z", "event": "agent_connected", "agent": "unknown-device", "token": "t-dev-local-001", "mode": "LOCAL", "status": "denied", "ip": "192.168.1.100", "mac": "11:22:33:44:55:66", "reason": "MAC mismatch"}
```

**Writing to the journal**:
```rust
pub fn log_connection(agent: &str, token: &str, mode: &str, status: &str, ip: &str, mac: &str) -> anyhow::Result<()> {
    let entry = JournalEntry {
        timestamp: Utc::now().to_rfc3339(),
        event: "agent_connected".to_string(),
        agent: agent.to_string(),
        token: token.to_string(),
        mode: mode.to_string(),
        status: status.to_string(),
        ip: ip.to_string(),
        mac: mac.to_string(),
    };

    let line = serde_json::to_string(&entry)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("journal.jsonl")?;
    
    writeln!(file, "{}", line)?;
    Ok(())
}
```

---

### 2.6 Logs for the dev

**Format** (gatewayd.log):
```
2026-08-18T14:30:00Z INFO  agent connected: claurst-main
2026-08-18T14:30:00Z DEBUG token verified: t-dev-local-001
2026-08-18T14:30:00Z TRACE connection details: ip=192.168.1.100, port=8348, mac=aa:bb:cc:dd:ee:ff
2026-08-18T14:30:00Z DEBUG mTLS certificate verified: CN=claurst-main
2026-08-18T14:30:00Z DEBUG agent hash matched: sha256=abc123...
```

**Logging levels**:
- `ERROR`: critical errors (the gateway does not start)
- `WARN`: warnings (token expired, approval needed)
- `INFO`: regular events (an agent connected)
- `DEBUG`: debugging (token verification, mTLS)
- `TRACE`: details (IP, MAC, hashes)

---

## 3. Scenarios

### 3.1 Local agent (plugged in directly)

```
1. Agent: t-dev-* + local IP + mTLS → LOCAL mode
2. Guard: token valid, hash matches → ✓
3. Journal: "claurst-main connected (LOCAL)"
4. Logs: "token verified, mTLS verified, hash matched"
```

### 3.2 Remote agent (approval needed)

```
1. Agent: t-prod-* + public IP → SERVER mode
2. Guard: token valid → ✓
3. First time? → user approval
4. Journal: "agent-xyz connected (SERVER, y/n)"
5. User: y → ✓
6. Logs: "token verified, user approved"
```

### 3.3 Attacker (spoofing)

```
1. Agent: t-dev-* (stolen) + local IP
2. mTLS: no certificate → ❌
3. Guard: token valid, but no mTLS → ❌
4. Journal: "unknown-device denied (no mTLS)"
5. Logs: "mTLS certificate missing, connection denied"
```

### 3.4 Token expired

```
1. Guard: token expired (TTL 30 days) → ❌
2. Generator: issues a new token
3. Journal: "t-dev-abc123 → t-dev-def456 (rotation)"
4. Logs: "token expired, rotated to t-dev-def456"
5. Agent: updates the token → ✓
```

---

## 4. Security

### 4.1 Token storage

**Option 1**: File encryption
```rust
// tokens.json.enc (AES-256-GCM)
let key = load_encryption_key();
let ciphertext = aes_gcm_encrypt(&tokens_json, &key)?;
std::fs::write("tokens.json.enc", ciphertext)?;
```

**Option 2**: HSM/TEE
```rust
// Tokens are stored in a secure enclave
let tokens = hsm_load_tokens()?;
```

### 4.2 Token rotation

- **TTL**: 30 days (automatic rotation)
- **Manual**: `gatewayd --revoke-token t-dev-abc123`
- **Automatic**: on suspicious activity (too many attempts)

### 4.3 mTLS

- **CA**: internal CA (issues certificates to agents)
- **Rotation**: 1 year (automatic)
- **Revocation**: CRL/OCSP (upon compromise)

---

## 5. Deployment

### 5.1 Locally (personal user)

```bash
# 1. Installation
$ cargo install gatewayd

# 2. Config
$ cat config.yaml
agents:
  - name: claurst-main
    url: http://localhost:8348
    token: t-dev-local-001

# 3. Startup
$ ./gatewayd config.yaml
```

### 5.2 Enterprise (cluster)

```bash
# 1. Installation
$ helm install gateway ./charts/gateway

# 2. Config (ConfigMap)
$ kubectl apply -f gateway-config.yaml

# 3. Startup
$ kubectl rollout restart deployment/gatewayd

# 4. Token generator (CronJob)
$ kubectl apply -f token-worker-cron.yaml
```

---

## 6. Monitoring

### 6.1 Metrics (Prometheus)

```
gateway_agents_connected_total{mode="local"} 5
gateway_agents_connected_total{mode="server"} 10
gateway_tokens_issued_total 15
gateway_tokens_expired_total 3
gateway_tokens_revoked_total 1
gateway_connections_denied_total 2
```

### 6.2 Alerts (Alertmanager)

```yaml
# alertrules.yaml
groups:
  - name: gateway
    rules:
      - alert: HighConnectionDenials
        expr: rate(gateway_connections_denied_total[5m]) > 10
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High number of denied connections"
      
      - alert: TokensExpiringSoon
        expr: gateway_tokens_expiring_in_24h > 5
        for: 1h
        labels:
          severity: info
        annotations:
          summary: "Tokens expire within 24 hours"
```

---

## 7. Risks and decisions

| Risk | Decision |
|---|---|
| **Hash = MAC (spoofable)** | Use UUID + CPU_ID + MAC |
| **Generator requires root** | Run as the user, read only accessible files |
| **Tokens in a file (theft)** | Encryption (AES-256-GCM) or HSM |
| **Rotation breaks agents** | Agents update tokens automatically (API) |
| **mTLS + tokens (double work)** | mTLS for local, tokens for remote |
| **Guard = single point of failure** | Redundancy (2+ Guards), token cache |
| **User does not understand** | Clear errors, documentation, UI |

---

## 8. Roadmap

### Phase 1: Personal users (now)

- [ ] Single config (config.yaml)
- [ ] Token/key/password
- [ ] Approval (first time)
- [ ] Journal for the user (journal.jsonl)
- [ ] Logs for the dev (gatewayd.log)

### Phase 2: Enterprise (future)

- [ ] Auto-router (LOCAL/SERVER)
- [ ] Guard: verification, rotation, blocking
- [ ] Token generator: hashes, issuing, binding
- [ ] mTLS: certificates, CA, rotation
- [ ] Token encryption (AES-256-GCM)
- [ ] Monitoring (Prometheus, Grafana)
- [ ] Alerts (Alertmanager)
- [ ] Kubernetes (Helm, ConfigMap, CronJob)

---

## 9. Appendices

### 9.1 Project structure

```
gatewayd/
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── guard.rs
│   ├── token_worker.rs
│   ├── tls.rs
│   └── journal.rs
├── config.yaml
├── config.loc.buffer.yaml
├── config.serv.buffer.yaml
├── tokens.json.enc
├── journal.jsonl
└── gatewayd.log
```

### 9.2 Dependencies (Cargo.toml)

```toml
[dependencies]
anyhow = "1.0"
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1.0", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1.0"
sha2 = "0.10"
uuid = { version = "1.0", features = ["v4"] }
x509 = "0.1"
aes-gcm = "0.10"
tracing = "0.1"
tracing-subscriber = "0.3"
```

---

**End of document**
