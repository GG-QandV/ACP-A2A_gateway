# TECH_DEBT

## Открытые

### 2026-08-09: continue по contextId таймаутит (направление 4)
- **Что**: второй `message/send` в ту же сессию через `/agents/:id/rpc` не получает ответа до `agent_call_timeout_secs`. Воспроизводится на claurst и hermes; прямой `session/prompt` в ту же сессию отвечает за ~3с — дефект в конвертере/адаптере шлюза, не в агентах.
- **Почему**: диагностика не завершена; репро в `docs/06-gateway-guide.md` §7.
- **Impact**: high
- **Fix**: разобрать жизненный цикл ACP-сессии в `AcpAsA2a` при втором `message/send` (вероятно, потеря sessionId/TurnLease между запросами).

### 2026-08-09: хеш токена — `std::hash::DefaultHasher`
- **Что**: не криптографический хеш токена.
- **Почему**: для сравнения на равенство достаточно; подбор не является текущей моделью угроз.
- **Impact**: low
- **Fix**: заменить на HMAC при усилении модели угроз.

### 2026-08-09: стриминг в конвертерах не реализован
- **Что**: `Reply::Streaming` падает «Фаза 1: стриминг не реализован» в обоих конвертерах (A2A→ACP и ACP→A2A). Направление 2 (reverse-proxy) SSE-стрим передаёт как есть — здесь проблемы нет.
- **Почему**: оставлено на Фазу 2.
- **Impact**: medium
- **Fix**: реализовать `tasks/resubscribe` ↔ `session/update` маппинг.
- **Статус**: **в разработке** — роадмап `docs/streaming-roadmap-checklist.md`, план `docs/stream-rollout-plan.md`, инструкция `docs/правки 3+аддс/стримминг/delegation-instructions-junior-middle.md`. Baseline зафиксирован тегом `pre-streaming-baseline` (Gate 0, 2026-08-18).

## Закрыто

### 2026-08-09: сессии без session/new копились в HashMap (P2-8)
- **Закрыто**: сессия только через `session/new`, `prompt` отклоняет неизвестный sessionId до acquire, `cancel` освобождает лиз, TTL-выселение, потолок `MAX_SESSIONS_PER_CONNECTION = 256`.

### 2026-08-09: AgentCard.url пустой (P2-12)
- **Закрыто**: url = `config.public_url` + `/agents/<id>/rpc`.

### 2026-08-09: файлы задач копились бесконечно
- **Закрыто**: `sweep_expired(ttl)` + фоновая уборка раз в час по mtime файла (`.json.tmp` не трогаются).
