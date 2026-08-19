# Инструкция для делегирования: стриминг в ACP-A2A_gateway

Для агента/разработчика уровня junior–middle. Основано на `streaming-roadmap-checklist.md`.
Соблюдай стиль проекта: маркеры `ИСПРАВЛЕНО`/`ДОБАВЛЕНО` в комментариях, правило seam из
`docs/04-architecture-guide-extending.md` ("новая возможность = новый файл или точечная правка,
не переписывание"), русскоязычные комментарии в том же тоне, что уже есть в коде.

## Главное правило: КОД → ПОТОМ ВСЕ ТЕСТЫ

Не пиши тесты параллельно с кодом и не переключайся между задачами. Порядок обязателен:

1. Реализуй **весь** код по назначенным тебе пунктам (раздел "Что делегируется" ниже).
2. Только после того как весь код готов и `cargo check --workspace` проходит без ошибок —
   переходи к тестам (раздел "Тесты" в конце, это отдельный, финальный шаг).
3. Тесты пишутся по критериям готовности, которые уже сформулированы для каждого пункта — не
   придумывай свои критерии, используй данные.
4. После каждого нового теста сделай negative control: закомментируй/откати свою правку и убедись,
   что тест покраснел. Если не покраснел — тест неправильный, перепиши его. Это конвенция проекта
   (см. `docs/decisions.md`, раздел "Как это писалось") — без этого шага задача не считается закрытой.

Не помечай задачу как готовую, пока `cargo clippy --workspace --all-targets -- -D warnings` не
проходит чисто.

---

## Что делегируется полностью (можно делать самостоятельно, без согласования)

### Задача A — `gatewayd/src/registry.rs`: лимит параллельных стримов на агента

Файл сейчас содержит `AgentEntry { transport: Transport }` — одно поле. Добавь:

```rust
pub struct AgentEntry {
    pub transport: Transport,
    pub stream_permits: std::sync::Arc<tokio::sync::Semaphore>,
}
```

- Ёмкость семафора приходит из конфига (см. Задачу B) — прокидывается через конструктор
  `AgentEntry::new(transport, max_concurrent_streams)`, не хардкодь число.
- Добавь метод `Registry::try_acquire_stream(&self, agent_id: &str) -> Result<tokio::sync::OwnedSemaphorePermit, StreamCapacityExhausted>` —
  свой тип ошибки через `thiserror`, по образцу `TurnLeaseTimeoutError` в `core/src/lease.rs`.
- Лог при отказе: `tracing::warn!(agent_id, active_streams, limit, "agent stream capacity exhausted — запрос отклонён fail-closed");` —
  вставь прямо в место, где `try_acquire` возвращает `Err`.
- Не трогай `check_token`/`lookup` — они не связаны с этой задачей.

### Задача B — `gatewayd/src/main.rs`: конфиг секции `streaming:`

Файл уже содержит паттерн `default_agent_call_timeout_secs()` / `default_task_retention_days()` —
копируй этот же паттерн, не изобретай новый способ задавать дефолты.

- В `RawAgentEntry` (оба варианта, `Stdio` и `Http`) добавь опциональное поле:
  ```rust
  #[serde(default)]
  streaming: StreamingConfig,
  ```
- Новая структура рядом с существующими:
  ```rust
  #[derive(Debug, Deserialize, Default)]
  struct StreamingConfig {
      #[serde(default = "default_max_concurrent_streams")]
      max_concurrent_streams: usize,
      #[serde(default = "default_first_chunk_timeout_secs")]
      first_chunk_timeout_secs: u64,
      #[serde(default = "default_idle_chunk_timeout_secs")]
      idle_chunk_timeout_secs: u64,
  }
  fn default_max_concurrent_streams() -> usize { 1 }
  fn default_first_chunk_timeout_secs() -> u64 { 15 }
  fn default_idle_chunk_timeout_secs() -> u64 { 120 }
  ```
- В `build_registry()`: если `max_concurrent_streams == 0` — `anyhow::bail!(...)`, по образцу уже
  существующей проверки на пустой токен. Формулировка ошибки: `"agent {id}: streaming.max_concurrent_streams не может быть 0"`.
- Прокинь эти три значения в `AgentEntry::new(...)` из Задачи A.

### Задача C — `core/src/supervisor.rs` + `core/src/stdio_agent.rs`: раздельные таймауты

`SpawnConfig` в `supervisor.rs` сейчас содержит одно поле `call_timeout: Duration`. Добавь два новых:

```rust
pub struct SpawnConfig {
    // ...существующие поля не трогать...
    pub first_chunk_timeout: Duration,
    pub idle_chunk_timeout: Duration,
}
```

- Прокинь их в `StdioAcpAgent::spawn(...)` аналогично тому, как уже прокидывается `call_timeout`.
- В стрим-цикле (появится после Задачи D от другого исполнителя — если её ещё нет, оставь `TODO`
  с ссылкой на неё и не блокируй свою часть): первый `rx.recv()` — таймаут `first_chunk_timeout`,
  каждый следующий — `idle_chunk_timeout`. Используй тот же `tokio::time::timeout(...)`, что уже
  применён в `StdioAcpAgent::call()` — не выдумывай новый механизм.
- Лог при срабатывании idle-таймаута: `tracing::warn!(session_id = %key, elapsed = ?elapsed, "idle_chunk_timeout сработал — агент не присылал чанков дольше лимита, поток закрыт");`

### Задача D — `gatewayd/src/transport_http.rs`: SSE-обёртка (3 места)

В файле есть три идентичные заглушки — найди все вхождения строки `"Фаза 1: streaming не реализован"`
(в `dispatch_a2a_method` для методов `message/send` и `SendMessage`, и в `rest_send_message_core`).

- Не переписывай каждое место отдельно — вынеси одну общую функцию:
  ```rust
  fn stream_to_sse(rx: tokio::sync::mpsc::UnboundedReceiver<protocol::a2a::A2aEvent>)
      -> axum::response::sse::Sse<impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
  ```
  Внутри — `tokio_stream::wrappers::UnboundedReceiverStream::new(rx)`, `.map(...)` в `Event::default().json_data(...)`.
- Добавь `tokio-stream = "0.1"` в `gatewayd/Cargo.toml` (единственная новая зависимость этой задачи).
- Замени все три вызова заглушки на вызов `stream_to_sse(rx)`.
- Не трогай логику самого маппинга `SessionUpdate ↔ A2aEvent` — она в `core/src/convert.rs`, это
  отдельная задача (см. раздел "Что НЕ делегируется" ниже). Твоя задача только про транспортный слой:
  взять уже готовый `rx: UnboundedReceiver<A2aEvent>` и отрендерить его в HTTP SSE-ответ.
- Лог при разрыве соединения клиентом: `tracing::warn!(agent_id, task_id = %id, "SSE клиент отключился до terminal event — задача продолжает выполняться в фоне");`

### Задача E — `gatewayd/src/transport_tcp.rs`: построчный стрим

- Обработай ветку `Reply::Streaming` — как только конвертер (задача другого исполнителя) отдаёт
  `rx: UnboundedReceiver<SessionUpdate>`, каждый элемент сериализуется как ACP-нотификация
  `session/update` и пишется newline-delimited в тот же TCP-сокет, которым уже пишутся ответы —
  переиспользуй существующий writer, не открывай новый.
- Лог при ошибке записи: `tracing::error!(session_id = %session.0, error = %e, "не удалось записать session/update в TCP-сокет клиента — соединение будет закрыто");`

### Задача F — логирование: `tracing-appender` в `gatewayd/src/main.rs`

- Добавь `tracing-appender = "0.2"` в `gatewayd/Cargo.toml`.
- Расширь `tracing_subscriber_init()`: если в конфиге `logging.output` равно `"file"` или `"both"` —
  добавь `tracing_appender::rolling::Builder` с `.max_log_files(N)` (число из `logging.file.max_files`).
  Не убирай существующий stdout-слой при `"both"` — регистрируй два слоя одновременно через
  `tracing_subscriber::registry()`, не через `fmt()` напрямую.
- Полная схема конфига — в `streaming-roadmap-checklist.md`, раздел 4.3. Скопируй структуру оттуда
  буквально, не меняй имена полей.
- Добавь два лога-ловушки (объём каталога логов раз в час, фоновая задача по образцу уже
  существующего `sweeper` в `main.rs`):
  - `tracing::warn!(current_size_mb, limit_mb, "лог-каталог приближается к max_total_size_mb (>80%)...")`
  - `tracing::error!(current_size_mb, limit_mb, "лог-каталог превысил max_total_size_mb — принудительное удаление старейших файлов")`

---

## Что делегируется с обязательным чек-поинтом (нужен review ПЕРЕД мержем, не после)

### Задача G — `core/src/stdio_agent.rs`: канал вместо буфера чанков

Это самая рискованная задача из всего роадмапа — трогает существующую конкурентную модель
(`Mutex<HashMap>`), и ошибка здесь тихо теряет чанки ответа агента, а не падает с понятной ошибкой.

- Реализуй по плану из `streaming-roadmap-checklist.md`, пункт 1.1: `UpdatesMap` заменяется на
  `HashMap<SessionId, mpsc::UnboundedSender<SessionUpdate>>`, `collect_session_update()` шлёт сразу,
  `prompt()` возвращает `Reply::Streaming(rx)`.
- **Прежде чем открывать PR**: покажи диф старшему разработчику/архитектору проекта и явно укажи —
  (1) как закрывается канал при завершении хода (кто зовёт `drop(tx)` и когда), (2) что происходит,
  если `session/prompt` от агента упадёт с ошибкой ПОСЛЕ того, как в канал уже ушли чанки — получатель
  должен узнать об ошибке, а не увидеть просто закрытый канал без объяснения.
- Лог-ловушки (обе ERROR/WARN, вставляй сразу, не после review):
  - `tracing::error!(session_id = %session_key, "stream channel closed before terminal event — возможная утечка ресурсов агента");`
  - `tracing::warn!(session_id = %session_key, chunk_count, "stream produced 0 chunks before terminal event");`

---

## Что НЕ делегируется на этом этапе (делает архитектор/senior)

### `core/src/convert.rs` — маппинг `SessionUpdate ↔ A2aEvent`

Не бери эту задачу без явного одобрения. Причина: ошибка в маппинге не является багом, который
проявится сразу — это семантическая потеря данных (например, `ToolCallUpdate` замаплен неправильно,
и клиент видит некорректный статус инструмента, но тестов, которые бы это поймали механически, писать
джуниору сложно без глубокого понимания обоих протоколов). Если тебе назначили эту задачу — сначала
пройди по всем 5 вариантам `SessionUpdate` и всем 3 вариантам `A2aEvent` вручную с наставником, и
только после этого пиши код.

---

## Тесты — строго после всего кода выше

Пиши в этом порядке, по одному файлу за раз, не вперемешку с кодом:

1. `core/src/stdio_agent.rs` (`#[cfg(test)]`, юнит) — чанки приходят по одному, не батчем (T1).
2. `core/src/convert.rs` (`#[cfg(test)]`, юнит) — каждый вариант enum маппится без паники (T2, только
   если тебе поручена задача G/маппинг).
3. Новый файл `gatewayd/tests/streaming_http.rs` — реальный SSE-клиент получает несколько событий до
   `final: true` (T3).
4. Новый файл `gatewayd/tests/streaming_tcp.rs` — TCP-клиент получает построчные нотификации (T4).
5. Прогони **все существующие тесты** на нестриминговый путь (`Reply::Complete`) — они не должны
   ломаться (T5, регрессия).
6. Для каждого написанного теста — откати свою правку и убедись, что тест покраснел (T6, negative
   control, обязательно для каждого теста без исключений).
7. `gatewayd/src/registry.rs` (`#[cfg(test)]`) — семафор отклоняет запрос сверх лимита (T7).
8. `gatewayd/src/main.rs` (`#[cfg(test)]`) — конфиг без секции `streaming:` использует дефолты (T8).
9. `core/src/stdio_agent.rs` (`#[cfg(test)]`) — `idle_chunk_timeout` закрывает зависший стрим (T9).

Финальная проверка перед сдачей: `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings`
— оба зелёные. Если что-то красное — это твоя задача, не передавай дальше с известными красными тестами.
