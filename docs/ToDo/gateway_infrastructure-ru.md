# Инфраструктура сервиса Gateway (корпоратив)


> **Язык:** русский · [English version](./gateway_infrastructure.md)
**Версия**: 1.0  
**Дата**: 2026-08-18  
**Статус**: На будущее (для корпоративной версии)

---

## 1. Архитектура

```
┌─────────────────────────────────────────────────────────────┐
│                     Шлюз (gatewayd)                         │
│                                                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │ Авто-роутер │  │   Сторож    │  │ Генератор токенов   │ │
│  │ (Local/     │  │ (Guard)     │  │ (Token Worker)      │ │
│  │  Server)    │  │ - Верифика- │  │ - Сбор хешей        │ │
│  │             │  │   ция       │  │ - Выдача токенов    │ │
│  │             │  │ - Ротация   │  │ - Привязка к агенту │ │
│  │             │  │ - Блокиров- │  │                     │ │
│  │             │  │   ка        │  │                     │ │
│  └─────────────┘  └─────────────┘  └─────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
         │                    │                    │
         │                    │                    │
         ▼                    ▼                    ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐
│   Агент         │ │   Журнал        │ │   Токен-хранилище│
│ (t-dev-*,       │ │ (кто, когда,    │ │ (tokens.json)   │
│  t-prod-*,      │ │  статус)        │ │                 │
│  mTLS)          │ │                 │ │ - Токен         │
└─────────────────┘ └─────────────────┘ │ - Хеш агента    │
                                        │ - TTL           │
                                        │ - Статус        │
                                        └─────────────────┘
```

---

## 2. Компоненты

### 2.1 Авто-роутер

**Задача**: Определяет режим (LOCAL/SERVER) по токену, IP, mTLS.

**Логика**:
```rust
pub fn detect_mode(
    token: &str,
    ip: &str,
    has_client_cert: bool,
) -> anyhow::Result<GatewayMode> {
    // 1. Проверяем токен
    if !token.starts_with("t-dev-") && !token.starts_with("t-prod-") {
        anyhow::bail!("Неизвестный тип токена: {}", token);
    }

    // 2. Проверяем, устарел ли токен
    if is_token_expired(token) {
        anyhow::bail!("Токен устарел: {}", token);
    }

    // 3. mTLS: нет сертификата → запрещаем
    if !has_client_cert {
        anyhow::bail!("Нет клиентского сертификата (mTLS)");
    }

    // 4. Определяем режим
    let mode = if token.starts_with("t-dev-") && is_local_ip(ip) {
        GatewayMode::Local
    } else if token.starts_with("t-prod-") {
        GatewayMode::Server
    } else {
        anyhow::bail!("Неизвестная комбинация токена и IP");
    };

    Ok(mode)
}
```

---

### 2.2 Сторож (Guard)

**Задача**: Верификация, ротация, блокировка токенов.

**Структура токена**:
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

**Верификация**:
```rust
pub fn verify(&self, token_id: &str, agent_hash: &str) -> anyhow::Result<()> {
    // 1. Ищем токен
    let token = self.tokens
        .iter()
        .find(|t| t.id == token_id)
        .ok_or_else(|| anyhow::anyhow!("Токен не найден: {}", token_id))?;

    // 2. Проверяем статус
    if token.status != TokenStatus::Active {
        anyhow::bail!("Токен не активен: {} (статус: {:?})", token_id, token.status);
    }

    // 3. Проверяем, не истёк ли
    if Utc::now() > token.expires_at {
        anyhow::bail!("Токен истёк: {}", token_id);
    }

    // 4. Проверяем хеш агента (привязка токена к агенту)
    if token.agent_hash != agent_hash {
        anyhow::bail!("Хеш агента не совпадает (токен: {}, агент: {})", 
                     token_id, agent_hash);
    }

    Ok(())
}
```

**Ротация**:
```rust
pub fn rotate_expired(&mut self) {
    for token in &mut self.tokens {
        if Utc::now() > token.expires_at {
            token.status = TokenStatus::Expired;
        }
    }
}
```

**Блокировка**:
```rust
pub fn revoke(&mut self, token_id: &str) {
    if let Some(token) = self.tokens.iter_mut().find(|t| t.id == token_id) {
        token.status = TokenStatus::Revoked;
    }
}
```

---

### 2.3 Генератор токенов (Token Worker)

**Задача**: Сбор хешей, выдача токенов, привязка к агенту.

**Сбор хешей**:
```rust
pub fn collect_agent_hashes(&self) -> anyhow::Result<Vec<String>> {
    let mut hashes = Vec::new();
    let agents = list_agents()?;
    
    for agent in agents {
        // Хеш = SHA256(UUID + CPU_ID + MAC)
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

**Выдача токена**:
```rust
pub fn issue_token(&self, agent_hash: &str) -> anyhow::Result<Token> {
    let token_id = format!("t-dev-{}", uuid::Uuid::new_v4());
    let issued_at = Utc::now();
    let expires_at = issued_at + Duration::days(30); // TTL 30 дней

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

### 2.4 mTLS (клиентские сертификаты)

**Задача**: Защита от маскировки под локального агента.

**Проверка сертификата**:
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

### 2.5 Журнал для юзера

**Формат** (journal.jsonl):
```jsonl
{"timestamp": "2026-08-18T14:30:00Z", "event": "agent_connected", "agent": "claurst-main", "token": "t-dev-local-001", "mode": "LOCAL", "status": "allowed", "ip": "192.168.1.100", "mac": "aa:bb:cc:dd:ee:ff"}
{"timestamp": "2026-08-18T14:35:00Z", "event": "token_rotated", "old_token": "t-dev-abc123", "new_token": "t-dev-def456", "agent": "claurst-main"}
{"timestamp": "2026-08-18T14:40:00Z", "event": "agent_connected", "agent": "unknown-device", "token": "t-dev-local-001", "mode": "LOCAL", "status": "denied", "ip": "192.168.1.100", "mac": "11:22:33:44:55:66", "reason": "MAC не совпадает"}
```

**Запись в журнал**:
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

### 2.6 Логи для дева

**Формат** (gatewayd.log):
```
2026-08-18T14:30:00Z INFO  agent connected: claurst-main
2026-08-18T14:30:00Z DEBUG token verified: t-dev-local-001
2026-08-18T14:30:00Z TRACE connection details: ip=192.168.1.100, port=8348, mac=aa:bb:cc:dd:ee:ff
2026-08-18T14:30:00Z DEBUG mTLS certificate verified: CN=claurst-main
2026-08-18T14:30:00Z DEBUG agent hash matched: sha256=abc123...
```

**Уровни логирования**:
- `ERROR`: Критические ошибки (шлюз не запускается)
- `WARN`: Предупреждения (токен устарел, апрув нужен)
- `INFO`: Обычные события (агент подключился)
- `DEBUG`: Отладка (верификация токена, mTLS)
- `TRACE`: Детали (IP, MAC, хеши)

---

## 3. Сценарии

### 3.1 Локальный агент (втыкается сразу)

```
1. Агент: t-dev-* + локальный IP + mTLS → LOCAL режим
2. Guard: токен валидный, хеш совпадает → ✓
3. Журнал: "claurst-main подключён (LOCAL)"
4. Логи: "token verified, mTLS verified, hash matched"
```

### 3.2 Удалённый агент (нужен апрув)

```
1. Агент: t-prod-* + публичный IP → SERVER режим
2. Guard: токен валидный → ✓
3. Первый раз? → апрув юзера
4. Журнал: "agent-xyz подключён (SERVER, y/n)"
5. Юзер: y → ✓
6. Логи: "token verified, user approved"
```

### 3.3 Злоумышленник (маскируется)

```
1. Агент: t-dev-* (украден) + локальный IP
2. mTLS: нет сертификата → ❌
3. Guard: токен валидный, но нет mTLS → ❌
4. Журнал: "unknown-device запрещён (нет mTLS)"
5. Логи: "mTLS certificate missing, connection denied"
```

### 3.4 Токен устарел

```
1. Guard: токен истёк (TTL 30 дней) → ❌
2. Генератор: выдаёт новый токен
3. Журнал: "t-dev-abc123 → t-dev-def456 (ротация)"
4. Логи: "token expired, rotated to t-dev-def456"
5. Агент: обновляет токен → ✓
```

---

## 4. Безопасность

### 4.1 Хранение токенов

**Вариант 1**: Шифрование файла
```rust
// tokens.json.enc (AES-256-GCM)
let key = load_encryption_key();
let ciphertext = aes_gcm_encrypt(&tokens_json, &key)?;
std::fs::write("tokens.json.enc", ciphertext)?;
```

**Вариант 2**: HSM/TEE
```rust
// Токены хранятся в secure enclave
let tokens = hsm_load_tokens()?;
```

### 4.2 Ротация токенов

- **TTL**: 30 дней (автоматическая ротация)
- **Ручная**: `gatewayd --revoke-token t-dev-abc123`
- **Автоматическая**: при подозрительной активности (слишком много попыток)

### 4.3 mTLS

- **CA**: Внутренний CA (выпускает сертификаты агентам)
- **Ротация**: 1 год (автоматическая)
- **Отзыв**: CRL/OCSP (при компрометации)

---

## 5. Развёртывание

### 5.1 Локально (персональный юзер)

```bash
# 1. Установка
$ cargo install gatewayd

# 2. Конфиг
$ cat config.yaml
agents:
  - name: claurst-main
    url: http://localhost:8348
    token: t-dev-local-001

# 3. Запуск
$ ./gatewayd config.yaml
```

### 5.2 Корпоратив (кластер)

```bash
# 1. Установка
$ helm install gateway ./charts/gateway

# 2. Конфиг (ConfigMap)
$ kubectl apply -f gateway-config.yaml

# 3. Запуск
$ kubectl rollout restart deployment/gatewayd

# 4. Генератор токенов (CronJob)
$ kubectl apply -f token-worker-cron.yaml
```

---

## 6. Мониторинг

### 6.1 Метрики (Prometheus)

```
gateway_agents_connected_total{mode="local"} 5
gateway_agents_connected_total{mode="server"} 10
gateway_tokens_issued_total 15
gateway_tokens_expired_total 3
gateway_tokens_revoked_total 1
gateway_connections_denied_total 2
```

### 6.2 Алерты (Alertmanager)

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
          summary: "Много запрещённых подключений"
      
      - alert: TokensExpiringSoon
        expr: gateway_tokens_expiring_in_24h > 5
        for: 1h
        labels:
          severity: info
        annotations:
          summary: "Токены истекают через 24 часа"
```

---

## 7. Риски и решения

| Риск | Решение |
|---|---|
| **Хеш = MAC (подделка)** | Использовать UUID + CPU_ID + MAC |
| **Генератор требует root** | Запуск от юзера, читать только доступные файлы |
| **Токены в файле (кража)** | Шифрование (AES-256-GCM) или HSM |
| **Ротация ломает агентов** | Агенты обновляют токены автоматически (API) |
| **mTLS + токены (двойная работа)** | mTLS для локальных, токены для удалённых |
| **Guard = точка отказа** | Redundancy (2+ Guard), кэш токенов |
| **Юзер не понимает** | Понятные ошибки, документация, UI |

---

## 8. Roadmap

### Фаза 1: Персональные юзеры (сейчас)

- [ ] Один конфиг (config.yaml)
- [ ] Токен/ключ/пасс
- [ ] Апрув (первый раз)
- [ ] Журнал для юзера (journal.jsonl)
- [ ] Логи для дева (gatewayd.log)

### Фаза 2: Корпоратив (будущее)

- [ ] Авто-роутер (LOCAL/SERVER)
- [ ] Сторож (Guard): верификация, ротация, блокировка
- [ ] Генератор токенов: хеши, выдача, привязка
- [ ] mTLS: сертификаты, CA, ротация
- [ ] Шифрование токенов (AES-256-GCM)
- [ ] Мониторинг (Prometheus, Grafana)
- [ ] Алерты (Alertmanager)
- [ ] Kubernetes (Helm, ConfigMap, CronJob)

---

## 9. Приложения

### 9.1 Структура проекта

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

### 9.2 Зависимости (Cargo.toml)

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

**Конец документа**