# ACP-A2A_gateway

Прослойка-шлюз между **ACP-агентами** (claurst, hermes, opencode) и **A2A-клиентами** (внешние сервисы, другие агенты). Поднимает два порта (TCP + HTTP), обслуживает четыре направления, раздаёт токены, изолирует разговоры между клиентами. Включает стриминг с durable-буфером событий, журнал, health-мониторинг и апрув агентов через CLI.

## Направления

| # | Входящая сторона | Агент | Транспорт | Порт/путь |
|---|---|---|---|---|
| 1 | ACP-клиент | ACP (stdio) | TCP passthrough | `listen` |
| 2 | A2A-клиент | A2A (HTTP) | HTTP reverse-proxy, без конвертера | `http_listen` → `/a2a-proxy/:id/*path` |
| 3 | ACP-клиент | A2A (HTTP) | TCP, конвертер ACP→A2A | `listen` |
| 4 | A2A-клиент | ACP (stdio) | HTTP, конвертер A2A→ACP | `http_listen` → `/agents/:id/rpc` |

Направления 2 и 4 поддерживают **стриминг**: HTTP-клиент получает SSE-поток
событий (`text/event-stream`) до терминального `final: true`. Каждое событие
персистится в durable `event_log` (SQLite), поэтому потерянный клиент может
продолжить через `tasks/resubscribe`.

## Диаграмма архитектуры

Полная схема шлюза (клиенты, транспорты, core, стриминг, хранилища, базовые модули):

<img src="docs/diagram_gateway.drawio.svg" alt="Gateway architecture diagram" width="100%">

Исходник (drawio): [`docs/diagram_gateway.drawio.svg`](docs/diagram_gateway.drawio.svg)

## Структура

- `protocol/` — типы (ACP + A2A), ноль бизнес-логики
- `core/` — ядро: конвертеры, сессии, владелец, TaskStore, stdio/http агенты, supervisor, reply
- `gatewayd/` — бинарник: конфиг, Registry, TCP/HTTP транспорты, стриминг (SSE-релей, StreamHub, resubscribe), event_log, журнал, health, approvals, фоновая уборка задач

## Сборка и тесты

```bash
cargo build --workspace
cargo check --workspace --all-targets
cargo test --workspace          # 155 тестов (юнит + интеграционные + T1–T10 стриминга)
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` коммитится (решение от 2026-08-19) — зависимости зафиксированы
и не должны расходиться с `Cargo.toml` на CI.

## Запуск

```bash
cp config.example.yaml config.yaml   # поправить токены, агентов, public_url
./target/debug/gatewayd config.yaml
```

- TCP `listen` (по умолч. `0.0.0.0:8347`): первая строка — handshake `{"token":"...","agent_id":"..."}`.
- HTTP `http_listen` (по умолч. `0.0.0.0:8348`): Bearer-токен в `Authorization`.
- `public_url` — внешний адрес шлюза, уходит в `AgentCard.url` (`agent.json`).
- Фоновая уборка завершённых задач: `task_retention_days` (по умолч. 7), раз в час.

### Секции конфига

| Секция | Назначение |
|---|---|
| `streaming:` (на агента) | `max_concurrent_streams`, `first_chunk_timeout_secs`, `idle_chunk_timeout_secs` |
| `event_log:` | durable-буфер событий стрима (SQLite) — источник истины для `tasks/resubscribe` |
| `task_store:` | durable-хранилище задач (SQLite); без секции — файловое хранилище |
| `journal:` | durable-журнал для пользователя (health-алерты, обрывы стримов, апрувы), retention_days |
| `health:` | периодическая проверка размеров БД и занятости слотов стримов |
| `approvals:` | человеческий апрув агентов (pending/approved/rejected) |
| `logging:` | уровень/вывод, файловая ротация, `compress_rotated`, `debug_ttl_minutes` |

### Логирование и диагностика (Часть 4)

```yaml
logging:
  level: "info"              # info | debug | trace | warn | error | off
  output: "stdout"           # stdout | file | both
  debug_ttl_minutes: 60      # время жизни временно расширенного уровня
  file:
    path: "/var/log/acp-a2a-gateway/gateway.log"
    max_file_size_mb: 100
    max_files: 10
    max_total_size_mb: 1000
    compress_rotated: true
```

- `level: "off"` — аварийный клапан: полностью отключает фильтр (стартовое предупреждение печатается в stderr до отключения).
- Ротация: `tracing-appender` + фоновая чистка каталога раз в час (реальное удаление старейших файлов и gzip при превышении `max_total_size_mb`).
- **Горячая смена уровня без рестарта**:
  - `GET /debug/level` — текущий уровень;
  - `POST /debug/level` с телом `{"level":"debug"}` и `Authorization: Bearer <token>` — установить уровень;
  - уровни `debug|trace` автоматически откатываются к `info` через `debug_ttl_minutes`.

## CLI-команды (Rust-модуль, без внешнего sqlite3)

- `gatewayd --journal [--limit N] [--level info|warn|error] [--category NAME] [--since 10m|6h|1d|2w|1mo] [--db PATH]`
  — просмотр durable-журнала (health-алерты, обрывы стримов, апрувы) ASCII-таблицей.
- `gatewayd --approvals` — статусы агентов (pending/approved/rejected) + fingerprint.
- `gatewayd --approve <name>` / `gatewayd --reject <name>` — человеческий апрув агентов
  (секция `approvals:` в конфиге; неодобренный агент не обслуживается и попадает в журнал).
- `gatewayd --setup` — интерактивный мастер генерации конфига.

Полный гайд: [`docs/06-gateway-guide.md`](docs/06-gateway-guide.md) (RU) · [`docs/06-gateway-guide-en.md`](docs/06-gateway-guide-en.md) (EN) · [`docs/06-gateway-guide-uk.md`](docs/06-gateway-guide-uk.md) (UK).

## Стриминг (кратко)

- Направления 2/4 возвращают SSE; `spawn_stream_relay` персистит события в `event_log`
  и публикует их в per-task `StreamHub`.
- `tasks/resubscribe` — durable-подписка: клиент получает историю из `event_log`,
  затем live-события из `StreamHub` (дедуп по `seq`).
- Детали и чек-лист: [`docs/streaming-roadmap-checklist.md`](docs/streaming-roadmap-checklist.md).

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
