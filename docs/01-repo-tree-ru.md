# Дерево репозитория


> **Язык:** русский · [English version](./01-repo-tree.md)
Итоговая структура на конец треда — 3 crate'а, 4 из 4 направлений
(1: ACP↔ACP, 2: A2A↔A2A, 3: ACP-клиент→A2A-агент, 4: A2A-клиент→ACP-агент).

```
gateway/
├── Cargo.toml                        # workspace, resolver = "2"
├── config.example.yaml               # шаблон конфига, копируется в config.yaml
│
├── protocol/                         # ТИПЫ. Ноль бизнес-логики.
│   ├── Cargo.toml                     # deps: serde, serde_json
│   └── src/
│       ├── lib.rs                      # pub use acp::*; pub use a2a::*;
│       ├── acp.rs                      # SessionId, ContentBlock, PromptRequest,
│       │                               # StopReason, SessionUpdate (Фаза 2)
│       └── a2a.rs                      # TaskId, Task, TaskState (8 состояний),
│                                       # Message, Part, AgentCard, A2aEvent (Фаза 2)
│
├── core/                             # ЯДРО. Вся содержательная сложность здесь.
│   ├── Cargo.toml                     # deps: protocol, tokio, async-trait, anyhow,
│   │                                   # thiserror, chrono, reqwest, serde_json
│   │                                   # dev-deps: tempfile
│   └── src/
│       ├── lib.rs                      # pub mod agent/convert/http_agent/lease/
│       │                               # reply/stdio_agent/task_store
│       ├── reply.rs                    # enum Reply<T,U> — seam для стриминга
│       ├── agent.rs                    # trait AcpAgent, trait A2aAgent
│       ├── convert.rs                  # AcpAsA2a, A2aAsAcp — универсальный
│       │                               # конвертер (ContentBlock↔Part,
│       │                               # TaskState↔StopReason)
│       ├── lease.rs                    # TurnLease — сериализация promptов
│       │                               # на сессию, fail-closed timeout
│       ├── task_store.rs               # TaskStore — файловое хранилище Task
│       │                               # (JSON, atomic write, для get_task)
│       ├── stdio_agent.rs              # StdioAcpAgent — spawn процесса,
│       │                               # request/response по JSON-RPC id
│       └── http_agent.rs               # HttpA2aAgent — HTTP JSON-RPC клиент
│                                       # к внешнему A2A-агенту
│
└── gatewayd/                         # БИНАРНИК. Только проводка (I/O).
    ├── Cargo.toml                     # deps: protocol, core, tokio, axum,
    │                                   # reqwest, serde_yaml, tracing-subscriber
    └── src/
        ├── main.rs                      # читает config.yaml, строит Registry,
        │                               # поднимает TCP+HTTP параллельно,
        │                               # фоновая уборка задач (sweep_expired)
        ├── registry.rs                  # Registry: token set + agent map
        │                               # (Transport::Stdio | Transport::Http)
        ├── transport_tcp.rs             # Направления 1 и 3: ACP-клиент как
        │                               # входящая сторона (TCP, ndjson)
        ├── transport_http.rs            # Направление 4: A2A-клиент → ACP-агент
        │                               # (axum: /agents/:id/rpc + agent.json)
        └── transport_a2a_passthrough.rs # Направление 2: A2A-клиент → A2A-агент
                                        # (reverse-proxy, /a2a-proxy/:id/*path)
```

## Зачем именно такое разбиение (не больше и не меньше файлов)

`protocol` отделён от `core`, потому что типы тестируются и переиспользуются
независимо от логики конвертации — если завтра появится второй gateway
на других транспортах, `protocol` крейт используется без изменений.

`core` — единственный крейт, где стоит писать unit-тесты изолированно от
сети и процессов (кроме `stdio_agent.rs`, который сам по себе требует
реального subprocess). Здесь сосредоточена вся содержательная логика:
маппинг протоколов, надёжность (`lease.rs`), персистентность (`task_store.rs`).

`gatewayd` — чистая проводка: парсинг конфига, обвязка сети, dispatch по
`agent_id`. Ни один файл здесь не должен разрастаться до "God module" —
если `transport_http.rs` начнёт содержать бизнес-логику (не связанную с
HTTP-фреймингом), это сигнал, что она должна переехать в `core`.
