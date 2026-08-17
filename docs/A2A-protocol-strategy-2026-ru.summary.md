# Стратегия A2A-протокола 2026 — Резюме (RU)

**Продукты:** `ACP-A2A_gateway` (шлюз) · `agent-connector` (адаптер).

**Решение в одну строку:** базовый диалект A2A = **SDK (v1.0, ProtoJSON)**; **Spec (pre-1.0)** как fallback для старых клиентов; **ACP** как deep fallback для унаследованных инсталляций. ANP (W3C DID) — отдельная ниша, вне scope.

## Почему

- Экосистема агентных протоколов консолидировалась в 2026: **A2A победил** в агент↔агент (Linux Foundation, v1.0, TSC из 8 вендоров: AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, ServiceNow, 150+ организаций). **MCP** — комплементарный слой агент↔инструменты. **ACP** свёрнут и влит в A2A (репозиторий заархивирован 2025-08-29).
- В **v1.0 произошёл breaking change**: каноническим wire стал **ProtoJSON** — PascalCase-методы (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE`-энумы, единый `Part`. Это **SDK-диалект** = наша база.
- Старый wire (`message/send`, `tasks/get`, lowercase) — **pre-1.0 legacy** = **Spec-диалект** = наш fallback, чтобы клиенты на старом диалекте продолжали работать.
- Официальные SDK имеют opt-in `legacyCompat`-слой — мы следуем механизму самого стандарта, не изобретаем свой.

## Приоритет

| # | Диалект | Роль |
|---|---|---|
| 1 | **A2A SDK (v1.0)** | база |
| 2 | **A2A Spec (pre-1.0)** | fallback |
| 3 | **ACP** | deep fallback |
| — | **ANP (W3C DID)** | вне scope |

Старые диалекты поддерживаем **определённый период** — миграция/рефакторинг всегда имеет стоимость для клиентов.

## Ключевая подзадача

**Диалект-зонд** — один идемпотентный первичный запрос (`GetTask`/`tasks/get` с несуществующим `task_id`), который сразу показывает, каким диалектом говорит клиент. Agent Card `protocolVersion` — приоритетнее; результат кэшируется на эндпоинт; при неоднозначности — SDK. Детали: §9.2.

## Ссылки

- **Полная стратегия (RU):** [A2A-protocol-strategy-2026.md](A2A-protocol-strategy-2026.md)
- Версия EN: [A2A-protocol-strategy-2026-en.md](A2A-protocol-strategy-2026-en.md)
- Версия UK: [A2A-protocol-strategy-2026-uk.md](A2A-protocol-strategy-2026-uk.md)
- Унифицированное ТЗ (шлюз / адаптер / зонд): [TZ-a2a-dialects-gateway-adapter.md](TZ-a2a-dialects-gateway-adapter.md) (в репо шлюза)