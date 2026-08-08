# Черновик: гейтвей в облаке

Текущая архитектура (Фаза 1) — однопроцессный, статический токен,
in-process `Registry`, файловый `TaskStore` на локальном диске. Этот
документ фиксирует, что **добавляется** к архитектуре для облачного
многопользовательского (multi-tenant) развёртывания — не переписывание,
а слой поверх существующего ядра.

---

## 1. Что остаётся без изменений

`core/` (agent.rs, convert.rs, lease.rs, reply.rs) — протокольная логика
не знает о тенантах, облаке, авторизации по OAuth2. Это осознанное
архитектурное решение из предыдущих этапов треда: `core` работает с
одним агентом за раз, независимо от того, как вызывающий код его нашёл
и авторизовал. Это свойство напрямую упрощает облачный переход — не
нужно протаскивать `tenant_id` через весь конвертер.

---

## 2. Новые модули для облака

### 2.1 Auth: OAuth2 вместо статического токена

Статический `tokens: [...]` в config.yaml не масштабируется на множество
клиентов/организаций — нет revoke без редеплоя, нет разграничения прав
между тенантами, нет аудита "кто именно сделал запрос" [web:106].

**Решение — Client Credentials Grant** (server-to-server, без
пользовательского браузера) как основной flow для агентов/сервисов, и
Authorization Code + PKCE для человеко-управляемых клиентов (Zed,
VS Code) [web:106]:

```
gatewayd/src/auth/
├── mod.rs               # trait TokenValidator (заменяет Registry::check_token)
├── static_token.rs      # StaticTokenValidator — текущее поведение, для dev/on-prem
├── oauth2_jwt.rs         # OAuth2JwtValidator — валидирует JWT от внешнего IdP
└── token_cache.rs        # короткоживущий кэш валидированных токенов (избежать
                          # похода в IdP на каждый запрос)
```

```rust
#[async_trait]
pub trait TokenValidator: Send + Sync {
    /// Возвращает identity (tenant_id + scopes), а не просто bool —
    /// это и есть расширение точки, о которой писал архитектурный гайд.
    async fn validate(&self, token: &str) -> anyhow::Result<Identity>;
}

pub struct Identity {
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub subject: String, // sub claim из JWT, для аудита
}
```

Поток для JWT-варианта, по паттерну AgentCore Gateway [web:93]: клиент
получает JWT от Authorization Server (Okta/Auth0/Cognito) через
client_credentials, шлёт его в `Authorization: Bearer <jwt>`, gatewayd
валидирует подпись и claims **сам** (через JWKS endpoint IdP, с кэшем
публичных ключей) — без похода в IdP на каждый запрос, только при
первой валидации или истечении кэша ключей.

**On-behalf-of (OBO) для мульти-агентных сценариев**: если gateway сам
должен обратиться к третьему сервису от имени пользователя (не просто
проксировать), нужен token exchange — входящий JWT обменивается на новый,
audience-bound токен для конкретного downstream-агента, без передачи
исходного токена пользователя дальше по цепочке [web:93]. Это отдельный
модуль `auth/token_exchange.rs`, нужен только если downstream-агенты сами
проверяют audience claim.

### 2.2 Multi-tenant роутинг

Текущий `Registry` — плоский `HashMap<String, AgentEntry>`, общий для
всех клиентов. В облаке агенты принадлежат тенантам, и один тенант не
должен видеть/использовать агентов другого:

```rust
// registry.rs — расширение, не замена
pub struct AgentEntry {
    pub transport: Transport,
    pub tenant_id: String,        // НОВОЕ
}

impl Registry {
    // БЫЛО: pub fn lookup(&self, agent_id: &str) -> Option<&AgentEntry>
    // СТАЛО: tenant_id обязателен в lookup — агент из чужого тенанта
    //        не найдётся, даже если agent_id совпадает по строке.
    pub fn lookup(&self, tenant_id: &str, agent_id: &str) -> Option<&AgentEntry> {
        self.agents.get(agent_id).filter(|e| e.tenant_id == tenant_id)
    }
}
```

`tenant_id` берётся из `Identity`, возвращённого `TokenValidator` — не
из тела запроса клиента (нельзя доверять клиенту заявлять "я тенант X").

**Хранилище реестра**: на этом масштабе плоский YAML-файл уже не
подходит — нужна БД (Postgres, учитывая уже используемый в других
проектах пользователя стек) с таблицей `agents(tenant_id, agent_id,
transport_config)`, и `Registry` становится тонкой обёрткой над
запросом к БД с кэшем (TTL 30-60s, чтобы не бить БД на каждый запрос).

### 2.3 Rate limiting — per-tenant, не per-IP

Ключевое правило из практики multi-tenant SaaS: **бакет должен быть
keyed по tenant_id, не по IP** — корпоративные клиенты часто шарят
egress IP, и per-IP лимит наказывает не того клиента [web:102][web:103].
Рекомендуемая иерархия — три слоя лимитов [web:103]:

```
Layer 1: глобальный инфраструктурный лимит   (circuit breaker, last resort)
Layer 2: per-tenant лимит                     (основная граница изоляции)
Layer 3: per-agent (или per-API-key) сублимит (внутри тенанта)
```

```rust
gatewayd/src/rate_limit.rs

pub struct RateLimiter {
    // token bucket per tenant_id, с fast-path в памяти процесса и
    // периодической сверкой с центральным store (Redis) — hybrid-паттерн,
    // компромисс между latency (in-memory) и точностью в multi-instance
    // развёртывании (Redis) [web:107].
    local_buckets: DashMap<String, TokenBucket>,
    redis: Option<redis::Client>, // None = single-instance режим, без Redis
}

impl RateLimiter {
    pub async fn check(&self, tenant_id: &str) -> Result<(), RateLimitExceeded>;
}
```

Порядок middleware в запросе — фиксированный, это важно для корректности
биллинга [web:102]: **Auth → Billing quota → Rate limit → Dispatch**.
Проверка токена должна произойти раньше проверки лимита (не наоборот) —
иначе анонимные/невалидные запросы будут расходовать бюджет лимитов.

### 2.4 Sticky routing для stateful-сессий

`AcpAsA2a`/`A2aAsAcp` держат состояние в памяти процесса (`session:
Mutex<Option<SessionId>>`, `adapters: HashMap` в `transport_http.rs`).
При горизонтальном масштабировании (>1 инстанс gatewayd за балансером)
это создаёт проблему: если сессия создана на инстансе A, а следующий
запрос балансер отправит на инстанс B — сессия не найдётся.

Три варианта решения, от простого к сложному:

| Вариант | Механизм | Когда достаточно |
|---|---|---|
| Session affinity на балансере | L4/L7 sticky sessions по cookie/client-IP [web:95][web:104] | Небольшое число инстансов, WebSocket/долгие TCP-соединения — самый простой путь для Фазы облака-MVP |
| Consistent hashing по session_id | Балансер/ingress хэширует `session_id` из запроса в инстанс | Если нужно масштабировать за пределы возможностей sticky sessions балансера |
| Внешнее хранилище сессий | `session: Mutex<Option<SessionId>>` заменяется на Redis-backed store, любой инстанс может обслужить любую сессию | Полная stateless-масштабируемость, но требует переписать `AcpAsA2a`/`A2aAsAcp` под внешнее хранилище — самый дорогой вариант |

Для облачного MVP рекомендуется **session affinity на балансере**
(вариант 1) — не требует изменений в `core/`, решается конфигурацией
инфраструктуры (`nginx.ingress.kubernetes.io/affinity: cookie` или
аналог у используемого облачного провайдера) [web:104][web:101].
ACP stdio-агенты и так привязаны к одному процессу на одном инстансе —
sticky routing просто гарантирует, что клиент попадёт туда же.

### 2.5 Наблюдаемость (что добавляется сверх текущего `tracing`)

Текущий `tracing`/`tracing-subscriber` пишет только в stdout инстанса —
недостаточно для multi-tenant диагностики:

```
gatewayd/src/observability/
├── metrics.rs        # prometheus-совместимые метрики: активные сессии
│                      # per tenant, latency per direction (1-4), ошибки
├── audit_log.rs        # структурированный лог для аудита (кто/когда/
│                      # какой agent_id) — отдельно от debug-логов
└── tenant_context.rs   # tracing::Span с tenant_id, для корреляции
                        # логов одного тенанта через все compоненты
```

Метрика "активные сессии per tenant" напрямую питает rate limiting
(§2.3) и биллинг — не отдельная задача, а совместно используемые данные.

---

## 3. Итоговая структура (что добавляется к дереву репозитория)

```
gatewayd/src/
├── auth/                          # НОВОЕ — §2.1
│   ├── mod.rs
│   ├── static_token.rs             # старое поведение сохранено как один из вариантов
│   ├── oauth2_jwt.rs
│   ├── token_cache.rs
│   └── token_exchange.rs           # опционально, если нужен OBO
├── rate_limit.rs                  # НОВОЕ — §2.3
├── observability/                  # НОВОЕ — §2.5
│   ├── metrics.rs
│   ├── audit_log.rs
│   └── tenant_context.rs
├── registry.rs                    # РАСШИРЕН — tenant_id в AgentEntry (§2.2)
├── transport_tcp.rs                # РАСШИРЕН — middleware order Auth→Quota→RateLimit
├── transport_http.rs               # РАСШИРЕН — то же
└── transport_a2a_passthrough.rs    # РАСШИРЕН — то же
```

`core/` не меняется вообще — вся облачная сложность живёт в `gatewayd`,
что подтверждает изначальное архитектурное решение "ядро не знает про
транспорт и способ авторизации".

---

## 4. Порядок внедрения (не всё сразу)

1. **OAuth2 (§2.1) первым** — без него multi-tenant (§2.2) не имеет
   смысла, т.к. нет надёжного способа определить `tenant_id` запроса.
2. **Multi-tenant роутинг (§2.2)** — сразу за auth, это основа изоляции.
3. **Rate limiting (§2.3)** — защищает от noisy neighbor между тенантами,
   становится критичным, когда тенантов больше одного платящего клиента.
4. **Sticky routing (§2.4)** — нужен только при горизонтальном
   масштабировании (>1 инстанс); при одном инстансе gatewayd в облаке
   можно пропустить этот пункт полностью.
5. **Observability (§2.5)** — можно делать параллельно с 1-3, не
   блокирует остальное, но чем раньше, тем проще диагностировать
   проблемы 1-4 по ходу внедрения.
