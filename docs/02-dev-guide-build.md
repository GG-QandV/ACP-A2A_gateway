# Дев-гайд: сборка

## Предварительные требования

- Rust stable (edition 2021), `cargo` в PATH.
- Для реальных ACP-агентов: бинарник агента в PATH (`claurst`, `opencode`
  и т.п.) — нужен только для интеграционных тестов и локального запуска,
  не для `cargo build`.
- Не требуется: Docker, внешние сервисы, сеть (сборка полностью офлайн
  после `cargo fetch`).

## Сборка с нуля

```bash
git clone <repo-url> gateway && cd gateway

# Скачать все зависимости заранее (удобно при нестабильной сети)
cargo fetch

# Собрать весь workspace одной командой
cargo build --workspace

# Релизная сборка (оптимизации, медленнее компилируется)
cargo build --workspace --release
```

Бинарник после сборки: `target/debug/gatewayd` (или `target/release/gatewayd`).

## Сборка отдельных crate'ов

Полезно при работе над одним модулем — не пересобирает весь workspace:

```bash
cargo build -p protocol   # только типы, самая быстрая сборка
cargo build -p core       # ядро + protocol
cargo build -p gatewayd   # всё целиком (зависит от core и protocol)
```

## Проверка без полной сборки (быстрее в разработке)

```bash
cargo check --workspace       # проверка типов без генерации бинарника
cargo clippy --workspace       # линтер — обязателен перед коммитом
cargo clippy --workspace -- -D warnings   # fail on any warning (CI-режим)
```

Критерий приёмки из исходного ТЗ: **`cargo check --workspace` и `clippy`
без warnings** — это должно проходить на каждом коммите, не только перед
релизом.

## Первый запуск

```bash
cp config.example.yaml config.yaml
# отредактировать config.yaml: путь к реальному ACP-агенту, токены,
# task_store_dir

export OPENMODEL_API_KEY="..."   # если агент требует ключ (см. env: в конфиге)

cargo run -p gatewayd -- config.yaml
```

При успешном старте в логах (уровень `info`, управляется `RUST_LOG`):

```
starting acp-a2a gateway (dual transport, 3 directions)
tcp transport listening (listen_addr=0.0.0.0:8347)
```

Управление уровнем логов:

```bash
RUST_LOG=debug cargo run -p gatewayd -- config.yaml
RUST_LOG=core=trace,gatewayd=info cargo run -p gatewayd -- config.yaml
```

## Частые проблемы сборки

| Симптом | Причина | Решение |
|---|---|---|
| `error: linking with cc failed` | Нет системного линкера (Linux) | `apt install build-essential` / `xcode-select --install` (macOS) |
| `failed to select a version for reqwest` | Конфликт версий TLS-бэкенда | Проверить, что `reqwest` версия одинакова в `core/Cargo.toml` и `gatewayd/Cargo.toml` |
| Долгая первая сборка (>2 мин) | `axum`+`reqwest`+`tokio` тянут много транзитивных зависимостей | Нормально для первой сборки; последующие — инкрементальные, секунды |
| `cargo clippy` падает на `unreachable!()` в `convert.rs` | Это ожидаемо — Reply::Streaming веткой в Фазе 1 недостижима намеренно | Не баг, см. архитектурный гайд §"seam для стриминга" |

## CI-минимум (если настраивается GitHub Actions/аналог)

```yaml
# .github/workflows/ci.yml (минимальный, без деплоя)
steps:
  - run: cargo check --workspace
  - run: cargo clippy --workspace -- -D warnings
  - run: cargo test --workspace
```

Это ровно тот же набор команд, что и локальная разработка — специальной
CI-инфраструктуры для этого проекта на этапе MVP не требуется.
