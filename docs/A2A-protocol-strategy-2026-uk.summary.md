# Стратегія A2A-протоколу 2026 — Резюме (UK)

**Продукти:** `ACP-A2A_gateway` (шлюз) · `agent-connector` (адаптер).

**Рішення в один рядок:** базовий діалект A2A = **SDK (v1.0, ProtoJSON)**; **Spec (pre-1.0)** як fallback для старих клієнтів; **ACP** як deep fallback для успадкованих інсталяцій. ANP (W3C DID) — окрема ніша, поза scope.

## Чому

- Екосистема агентних протоколів консолідувалася у 2026: **A2A переміг** у агент↔агент (Linux Foundation, v1.0, TSC з 8 вендорів: AWS, Cisco, Google, IBM Research, Microsoft, Salesforce, SAP, ServiceNow, 150+ організацій). **MCP** — комплементарний шар агент↔інструменти. **ACP** згорнуто та влито в A2A (репозиторій заархівовано 2025-08-29).
- У **v1.0 стався breaking change**: канонічним wire став **ProtoJSON** — PascalCase-методи (`SendMessage`, `GetTask`, `CancelTask`), `SCREAMING_SNAKE_CASE`-енуми, єдиний `Part`. Це **SDK-діалект** = наша база.
- Старий wire (`message/send`, `tasks/get`, lowercase) — **pre-1.0 legacy** = **Spec-діалект** = наш fallback, щоб клієнти на старому діалекті продовжували працювати.
- Офіційні SDK мають opt-in `legacyCompat`-шар — ми слідуємо механізму самого стандарту, не винаходимо свій.

## Пріоритет

| # | Діалект | Роль |
|---|---|---|
| 1 | **A2A SDK (v1.0)** | база |
| 2 | **A2A Spec (pre-1.0)** | fallback |
| 3 | **ACP** | deep fallback |
| — | **ANP (W3C DID)** | поза scope |

Старі діалекти підтримуємо **визначений період** — міграція/рефакторинг завжди має вартість для клієнтів.

## Ключова підзадача

**Діалект-зонд** — один ідемпотентний первинний запит (`GetTask`/`tasks/get` з неіснуючим `task_id`), який одразу показує, яким діалектом говорить клієнт. Agent Card `protocolVersion` — пріоритетніше; результат кешується на ендпоінт; при неоднозначності — SDK. Деталі: §9.2.

## Посилання

- **Повна стратегія (UK):** [A2A-protocol-strategy-2026-uk.md](A2A-protocol-strategy-2026-uk.md)
- Версія EN: [A2A-protocol-strategy-2026-en.md](A2A-protocol-strategy-2026-en.md)
- Версія RU: [A2A-protocol-strategy-2026.md](A2A-protocol-strategy-2026.md)
- Уніфіковане ТЗ (шлюз / адаптер / зонд): [TZ-a2a-dialects-gateway-adapter.md](TZ-a2a-dialects-gateway-adapter.md) (у репо шлюзу)