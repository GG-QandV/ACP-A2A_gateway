# ACP-A2A_gateway

Прослойка-шлюз между **ACP-агентами** (claurst, hermes, opencode) и **A2A-клиентами** (внешние сервисы, другие агенты). Поднимает два порта (TCP + HTTP), обслуживает четыре направления, раздаёт токены, изолирует разговоры между клиентами.

## Направления

| # | Входящая сторона | Агент | Транспорт | Порт/путь |
|---|---|---|---|---|
| 1 | ACP-клиент | ACP (stdio) | TCP passthrough | `listen` |
| 2 | A2A-клиент | A2A (HTTP) | HTTP reverse-proxy, без конвертера | `http_listen` → `/a2a-proxy/:id/*path` |
| 3 | ACP-клиент | A2A (HTTP) | TCP, конвертер ACP→A2A | `listen` |
| 4 | A2A-клиент | ACP (stdio) | HTTP, конвертер A2A→ACP | `http_listen` → `/agents/:id/rpc` |

## Структура

- `protocol/` — типы (ACP + A2A), ноль бизнес-логики
- `core/` — ядро: конвертеры, сессии, владелец, TaskStore, stdio/http агенты
- `gatewayd/` — бинарник: конфиг, Registry, TCP/HTTP транспорты, фоновая уборка задач

## Сборка и тесты

```bash
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 69 тестов
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` из окружения сборки **не коммитить** (зависимости искусственно понижены под старый компилятор в чужих артефактах).

## Запуск

```bash
cp config.example.yaml config.yaml   # поправить токены, агентов, public_url
./target/debug/gatewayd config.yaml
```

- TCP `listen` (по умолч. `0.0.0.0:8347`): первая строка — handshake `{"token":"...","agent_id":"..."}`.
- HTTP `http_listen` (по умолч. `0.0.0.0:8348`): Bearer-токен в `Authorization`.
- `public_url` — внешний адрес шлюза, уходит в `AgentCard.url` (`agent.json`).
- Фоновая уборка завершённых задач: `task_retention_days` (по умолч. 7), раз в час.

Полный гайд: [`docs/06-gateway-guide.md`](docs/06-gateway-guide.md).

## Стратегия A2A-протокола 2026

Стратегия диалектов A2A для шлюза и адаптера (SDK v1.0 = база, Spec pre-1.0 =
fallback, ACP = deep fallback, ANP — вне scope). Язык: **EN** ·
[UA](docs/A2A-protocol-strategy-2026-uk.summary.md) ·
[RU](docs/A2A-protocol-strategy-2026-ru.summary.md) —
каждый открывает краткое резюме со ссылкой на полную версию стратегии
на этом языке.

- **EN:** [A2A-protocol-strategy-2026-en.summary.md](docs/A2A-protocol-strategy-2026-en.summary.md)
- **UA:** [A2A-protocol-strategy-2026-uk.summary.md](docs/A2A-protocol-strategy-2026-uk.summary.md)
- **RU:** [A2A-protocol-strategy-2026-ru.summary.md](docs/A2A-protocol-strategy-2026-ru.summary.md)
