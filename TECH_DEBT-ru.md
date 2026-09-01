# TECH_DEBT


> **Язык:** русский · [English version](./TECH_DEBT.md)
## Открытые

### 2026-08-19: юнит-покрытие 5 вариантов `SessionUpdate` (критерий 1.2) — 0 тестов
- **Что**: маппинг `SessionUpdate → A2aEvent` в конвертерах не покрыт юнит-тестами на каждый из 5 вариантов enum (`AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `UsageUpdate`) — untested variant = молчаливая потеря информации при расширении протокола.
- **Impact**: low-medium — стриминг работает на happy path, но регрессия маппинга не ловится тестами.
- **Fix**: юнит-тесты в `core/src/convert.rs` по каждому варианту (см. `docs/streaming-roadmap-checklist-ru.md`, критерий 1.2).

### 2026-08-19: `tasks/resubscribe` — только HTTP, TCP line-протокол без RPC
- **Что**: `tasks/resubscribe` и `tasks/get-last-seq` реализованы для HTTP (направление 4, `transport_http.rs`); TCP line-протокол (направление 3) не имеет resubscribe RPC — клиент, отвалившийся от TCP-стрима, переподключается только новым `session/prompt`.
- **Impact**: low — resubscribe нужен HTTP-клиентам; TCP-направление работает без него.
- **Fix**: по запросу — добавить `tasks/resubscribe` в TCP-транспорт (опционально).

## Закрыто

### 2026-08-19: tasks/resubscribe не реализован (Фаза 2.1 → реализован Phase 3.2)
- **Закрыто**: durable event buffer (`gatewayd/src/event_log.rs`, монотонный per-task `seq`), `tasks/get-last-seq` + `tasks/resubscribe` в `transport_http.rs` (replay из event log как SSE-стрим) — клиент, отвалившийся посреди стрима, переподключается к идущей задаче через HTTP. См. коммит b9c0b8b.

### 2026-08-18: хеш токена — HMAC-SHA256 (коммит a970dcd)
- **Закрыто**: `RandomState` заменён на HMAC-SHA256 с ключом из `{env:GATEWAY_HMAC_KEY}` (дефолт `default-dev-key-do-not-use-in-prod` для разработки). Криптографический хеш, формат `Owner::Token { hash: u64 }` не изменился — `StoredTask` без миграции. Прода: обязательно задать ключ через env.

### 2026-08-18: T4 — TCP-стрим, направление 3 (коммит a970dcd)
- **Закрыто**: `HttpA2aAgent::send_task` при `Content-Type: text/event-stream` возвращает `Reply::Streaming` (SSE-клиент `sse_to_a2a_events`), иначе `Reply::Complete`. `blocking: false` для стрим-запросов. Тесты: юнит `send_task_returns_streaming_on_sse_response` + интеграционный `streaming_tcp.rs` (TCP-клиент получает `session/update` построчно). Mock-серверы генерируют SSE через реальную сериализацию `A2aEvent` (как прод `stream_to_sse`), без ручного JSON-хардкода.

### 2026-08-18: continue по contextId таймаутит (направление 4) (коммит 9cde4e6)
- **Закрыто**: `ensure_session` уже возвращал существующую сессию (аудиты P1-1/P2-10), добавлен интеграционный тест `second_message_send_same_context_returns_same_session`.

### 2026-08-18: стриминг в конвертерах — Фаза 2.0 (коммиты af9c9d9, 1ee5574, 36745ac, 1e2de5d, da3749f, a970dcd)
- **Закрыто**: `Reply::Streaming` реализован через `prompt_streaming()` (P-20/P-21). Транспорт: SSE (HTTP, направление 4) + построчный TCP (направление 3, SSE-клиент — см. T4). Лимит `max_concurrent_streams` (Semaphore per-agent, try_acquire_stream в HTTP+TCP, fail-closed). Раздельные first/idle_chunk_timeout в стрим-цикле. Логирование с ротацией (tracing-appender). Тесты T1-T9 + negative control + P-23/P-24 + hash HMAC. 151 тест, clippy -D warnings чисто. `tasks/resubscribe` закрыт отдельной записью выше.

### 2026-08-09: сессии без session/new копились в HashMap (P2-8)
- **Закрыто**: сессия только через `session/new`, `prompt` отклоняет неизвестный sessionId до acquire, `cancel` освобождает лиз, TTL-выселение, потолок `MAX_SESSIONS_PER_CONNECTION = 256`.

### 2026-08-09: AgentCard.url пустой (P2-12)
- **Закрыто**: url = `config.public_url` + `/agents/<id>/rpc`.

### 2026-08-09: файлы задач копились бесконечно
- **Закрыто**: `sweep_expired(ttl)` + фоновая уборка раз в час по mtime файла (`.json.tmp` не трогаются).
