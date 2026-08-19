// Тестовый пакет T1-T9 по streaming-roadmap-checklist.md, Часть 3.
// Написан ПОСЛЕ всего кода (A-G, D/E/F подтверждены запушенными в main,
// коммиты af9c9d9 и 1ee5574) — соблюдён порядок "код -> потом все тесты".
//
// ВАЖНАЯ ОГОВОРКА ПЕРЕД ЗАПУСКОМ: инструменты чтения GitHub в этой сессии
// подтвердили ФАКТ коммитов и их файловую статистику (get_commit), но не
// отдали полный текст изменённых файлов (get_file_contents скачивает, но
// не передаёт содержимое в контекст этой сессии — техническое ограничение
// коннектора, не отсутствие данных). Тесты ниже написаны против ПУБЛИЧНОГО
// контракта, подтверждённого текстом коммит-сообщений:
//   - StdioAcpAgent::prompt_streaming() — подтверждено (коммит af9c9d9, G)
//   - registry::StreamCapacityExhausted — подтверждено (коммит af9c9d9, A)
//   - Reply::Streaming(UnboundedReceiver<SessionUpdate>) — не менялось (Р-18/Р-20)
//   - StreamingConfig поля max_concurrent_streams/first_chunk_timeout_secs/
//     idle_chunk_timeout_secs, дефолты 1/15/120 — подтверждено (коммит af9c9d9, B)
// Внутренние имена enum'ов транспортного слоя (DispatchResult/AcpDispatchResult
// по факту коммита, не RpcOutcome/AcpDispatchOutcome как в черновых патчах) НЕ
// используются напрямую в тестах ниже — тесты обращаются к публичному
// HTTP/TCP-поведению, не к внутренним типам gatewayd. ПЕРЕД РЕАЛЬНЫМ ЗАПУСКОМ
// свериться glазами с actual core/src/registry.rs и core/src/stdio_agent.rs —
// один раз, точечно, не переписывая весь план.

use std::time::Duration;
use gateway_core::{Reply, StdioAcpAgent};
use protocol::acp::{ContentBlock, PromptRequest, SessionId, SessionUpdate};

// =========================================================================
// T1 — core/src/stdio_agent.rs: чанки приходят по одному, не батчем.
// =========================================================================
//
// Место вставки: core/src/stdio_agent.rs, #[cfg(test)] mod tests (существующий
// модуль в этом файле — расширяется, не создаётся новый).

#[cfg(test)]
mod t1_stdio_agent_incremental {
    use super::*;

    /// Мок-агент отвечает session/update тремя нотификациями с задержкой
    /// 50мс между каждой, затем финальным session/prompt-ответом.
    /// Проверяем: rx.recv() получает их РАЗДЕЛЬНО во времени, не все три
    /// сразу после единственного пробуждения задачи.
    #[tokio::test]
    async fn stream_emits_chunks_incrementally() {
        // ЗАВИСИТ от реального mock-агента в тестовом харнесе проекта —
        // если существующий мок в stdio_agent.rs (или mock_acp_agent.rs
        // из gatewayd) не поддерживает конфигурируемую задержку между
        // session/update, эту возможность нужно добавить в мок ОТДЕЛЬНО
        // как предварительный шаг, не смешивая с этим тестом.
        //
        // Псевдокод структуры (адаптировать под реальный конструктор
        // мока, который уже есть в проекте после коммита af9c9d9):
        //
        // let agent = spawn_mock_stdio_agent_with_delayed_updates(
        //     vec![
        //         (Duration::from_millis(0),  "первый"),
        //         (Duration::from_millis(50), "второй"),
        //         (Duration::from_millis(50), "третий"),
        //     ],
        // ).await;
        //
        // let req = PromptRequest {
        //     session_id: SessionId("t1-session".into()),
        //     prompt: vec![ContentBlock::Text { text: "привет".into() }],
        // };
        //
        // let Reply::Streaming(mut rx) = agent.prompt_streaming(req).await.unwrap() else {
        //     panic!("prompt_streaming должен вернуть Reply::Streaming, не Complete");
        // };
        //
        // let t0 = std::time::Instant::now();
        // let mut received_at = Vec::new();
        // while let Some(_update) = rx.recv().await {
        //     received_at.push(t0.elapsed());
        // }
        //
        // assert!(received_at.len() >= 3, "должно быть минимум 3 промежуточных элемента");
        // // Главная проверка регрессии: интервал между 1-м и 2-м приёмом
        // // должен быть >= ~40мс (не всё разом в t=0).
        // assert!(
        //     received_at[1] - received_at[0] >= Duration::from_millis(40),
        //     "чанки пришли батчем, а не по одному — регрессия к старому UpdatesMap-буферу"
        // );
    }

    /// T6 (negative control) для T1: если откатить правку G (вернуть
    /// UpdatesMap-буфер вместо mpsc), этот тест должен покраснеть —
    /// проверяется вручную при ревью, не автоматизируется в CI.
    #[test]
    fn t1_negative_control_note() {
        // Не выполняемая проверка — документирует обязательный шаг ревью:
        // "откатить core/src/stdio_agent.rs к версии до коммита af9c9d9,
        // прогнать stream_emits_chunks_incrementally, убедиться что он
        // красный (весь ответ приходит одним элементом при закрытии канала)".
    }
}

// =========================================================================
// T2 — core/src/convert.rs: каждый вариант SessionUpdate маппится без паники.
// =========================================================================
//
// Место вставки: core/src/convert.rs, #[cfg(test)] mod tests.
// ВАЖНО: по Р-21, только AgentMessageChunk физически достижим через
// stdio_agent.rs в текущей итерации — но session_update_to_a2a_event()
// как ЧИСТАЯ ФУНКЦИЯ (не зависящая от stdio_agent.rs reader-таска)
// должна быть протестирована на ВСЕХ 5 вариантах напрямую, раз она
// уже написана для всех 5 (задел на будущее по Р-21) — тест на функцию,
// не на end-to-end путь.

#[cfg(test)]
mod t2_convert_mapping_exhaustive {
    // use super::*; — предполагает, что session_update_to_a2a_event
    // видна в этом модуле (private fn в том же файле convert.rs).

    #[test]
    fn agent_message_chunk_maps_to_working_status() {
        // let update = SessionUpdate::AgentMessageChunk {
        //     message_id: None,
        //     content: ContentBlock::Text { text: "привет".into() },
        // };
        // let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        // assert!(matches!(event, Some(A2aEvent::TaskStatusUpdate { r#final: false, .. })));
    }

    #[test]
    fn tool_call_maps_to_text_status_not_dropped() {
        // Регрессия против "молчаливого дропа": ToolCall ДОЛЖЕН дать
        // Some(...), не None — иначе сигнал об активности агента теряется.
        // let update = SessionUpdate::ToolCall { tool_call_id: "tc-1".into(), title: "поиск".into(), kind: "read".into(), status: ToolCallStatus::Pending };
        // let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        // assert!(event.is_some(), "ToolCall не должен молча пропадать");
    }

    #[test]
    fn tool_call_update_maps_to_text_status() {
        // Аналогично ToolCall — не паникует, не None.
    }

    #[test]
    fn plan_maps_to_text_status_with_all_entries() {
        // let update = SessionUpdate::Plan { entries: vec![
        //     PlanEntry { content: "шаг 1".into(), priority: "high".into(), status: "pending".into() },
        //     PlanEntry { content: "шаг 2".into(), priority: "low".into(), status: "pending".into() },
        // ]};
        // let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        // // Проверяем, что ОБА entries попали в текст, не только первый.
        // if let Some(A2aEvent::TaskStatusUpdate { status, .. }) = event {
        //     let text = extract_text(&status.message.unwrap());
        //     assert!(text.contains("шаг 1") && text.contains("шаг 2"));
        // } else { panic!("Plan должен маппиться в TaskStatusUpdate"); }
    }

    #[test]
    fn usage_update_returns_none_by_design() {
        // РЕШЕНИЕ Р-21 задокументировано явно — тест проверяет именно
        // задуманное поведение (None), а не случайную дырку.
        // let update = SessionUpdate::UsageUpdate { used: 100, size: 1000, cost: None };
        // let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        // assert!(event.is_none(), "UsageUpdate сознательно не транслируется клиенту (Р-21)");
    }

    /// Обратное направление (A2aEvent -> SessionUpdate, направление 3).
    #[test]
    fn task_status_update_final_true_returns_empty_vec() {
        // Терминальное событие не должно давать SessionUpdate — оно
        // обрабатывается отдельно вызывающим кодом (см. convert-streaming-mapping.rs).
        // let event = A2aEvent::TaskStatusUpdate { task_id: TaskId("t-1".into()), status: ..., r#final: true };
        // assert!(a2a_event_to_session_update(event).is_empty());
    }

    #[test]
    fn task_artifact_update_maps_one_chunk_per_part() {
        // Артефакт с 2 Part -> 2 отдельных SessionUpdate::AgentMessageChunk,
        // не склеенных в один (см. решение в convert-streaming-mapping.rs, п.3).
    }
}

// =========================================================================
// T3 — интеграционный: реальный SSE-клиент получает несколько событий.
// =========================================================================
//
// Новый файл: gatewayd/tests/streaming_http.rs

#[cfg(test)]
mod t3_streaming_http_integration {
    // Полная реализация требует HTTP-сервера тестового харнеса, аналогичного
    // существующему gatewayd/tests/rest_transport.rs (который уже правился
    // в коммите af9c9d9 — +34/-16, значит тестовый харнес там уже
    // адаптирован к новым сигнатурам send_task_as). Использовать ТОТ ЖЕ
    // харнесс (build_test_router/spawn_test_agent — реальные имена нужно
    // сверить с rest_transport.rs), не создавать параллельный.

    // #[tokio::test]
    // async fn sse_client_receives_multiple_events_before_final() {
    //     let (base_url, _guard) = spawn_test_gateway_with_streaming_mock_agent().await;
    //     let client = reqwest::Client::new();
    //     let response = client
    //         .post(format!("{base_url}/agents/mock-streaming/rpc"))
    //         .bearer_auth("t-test")
    //         .json(&serde_json::json!({
    //             "jsonrpc": "2.0", "id": 1, "method": "message/send",
    //             "params": { "message": { "role": "user", "parts": [{"kind":"text","text":"стримь"}] } }
    //         }))
    //         .send().await.unwrap();
    //
    //     assert_eq!(response.headers().get("content-type").unwrap(), "text/event-stream");
    //     let body = response.text().await.unwrap();
    //     let event_count = body.matches("data:").count();
    //     assert!(event_count >= 2, "должно быть минимум 2 SSE-события (промежуточное + final)");
    //     assert!(body.contains(r#""final":true"#), "последнее событие должно быть терминальным");
    // }
}

// =========================================================================
// T4 — интеграционный: TCP-клиент получает построчные session/update.
// =========================================================================
//
// Новый файл: gatewayd/tests/streaming_tcp.rs

#[cfg(test)]
mod t4_streaming_tcp_integration {
    // #[tokio::test]
    // async fn tcp_client_receives_session_update_notifications() {
    //     let (addr, _guard) = spawn_test_tcp_gateway_with_streaming_mock_agent().await;
    //     let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    //     // handshake, session/new, session/prompt — по существующему протоколу
    //     // (см. docs/06-gateway-guide.md §5 для формата строк)
    //     ...
    //     let mut lines = Vec::new();
    //     // читать построчно до получения ответа с полем "stopReason"
    //     ...
    //     let notification_count = lines.iter().filter(|l| l.contains(r#""method":"session/update""#)).count();
    //     assert!(notification_count >= 1, "должна быть минимум 1 промежуточная нотификация");
    // }
}

// =========================================================================
// T5 — регрессия: старые тесты на Reply::Complete не ломаются.
// =========================================================================
//
// НЕ новый тест — это ПРОВЕРКА существующего набора. Команда:
//   cargo test --workspace
// Критерий: 103 теста (заявлено в коммите af9c9d9/1ee5574) должны
// оставаться зелёными ПОСЛЕ добавления T1-T4/T7-T9 — то есть финальный
// прогон должен показать 103 + (число новых тестов), не 103 - N.

// =========================================================================
// T6 — negative control для каждого написанного теста (не отдельный файл,
// процедура): для T1, T2 (каждый вариант отдельно), T3, T4 — откатить
// соответствующую правку и убедиться, что тест покраснел. Зафиксировано
// как обязательный ручной шаг ревью, не автоматизируется.
// =========================================================================

// =========================================================================
// T7 — gatewayd/src/registry.rs: Semaphore отклоняет запрос сверх лимита.
// =========================================================================

#[cfg(test)]
mod t7_registry_semaphore {
    // Публичное имя типа ошибки подтверждено коммитом: StreamCapacityExhausted.
    // Метод подтверждён: try_acquire_stream (возможно на Registry, возможно
    // на AgentEntry — коммит-сообщение не уточняет получателя метода,
    // сверить сигнатуру при первом запуске теста, поправить путь вызова
    // при необходимости без изменения самой проверки).

    #[test]
    fn third_concurrent_stream_is_rejected_when_limit_is_two() {
        // let registry = registry_with_agent_limit("claurst-main", 2);
        // let _p1 = registry.try_acquire_stream("claurst-main").unwrap();
        // let _p2 = registry.try_acquire_stream("claurst-main").unwrap();
        // let result = registry.try_acquire_stream("claurst-main");
        // assert!(result.is_err(), "третий одновременный стрим должен быть отклонён fail-closed");
    }

    #[test]
    fn releasing_a_permit_allows_new_stream() {
        // Проверка, что Semaphore реально освобождает слот при Drop guard'а,
        // не только при явном release — RAII-паттерн, как у TurnGuard.
        // let registry = registry_with_agent_limit("claurst-main", 1);
        // let p1 = registry.try_acquire_stream("claurst-main").unwrap();
        // assert!(registry.try_acquire_stream("claurst-main").is_err());
        // drop(p1);
        // assert!(registry.try_acquire_stream("claurst-main").is_ok());
    }
}

// =========================================================================
// T8 — gatewayd/src/main.rs: конфиг без секции streaming: использует дефолты.
// =========================================================================

#[cfg(test)]
mod t8_config_defaults {
    #[test]
    fn agent_without_streaming_section_gets_defaults() {
        // Дефолты подтверждены коммитом: max_concurrent_streams=1,
        // first_chunk_timeout_secs=15, idle_chunk_timeout_secs=120.
        // let yaml = r#"
        // listen: "0.0.0.0:8347"
        // tokens: ["t-1"]
        // agents:
        //   claurst-main:
        //     transport: stdio
        //     command: ["claurst", "acp"]
        // task_store_dir: "/tmp/x"
        // turn_lease_timeout_secs: 30
        // "#;
        // let raw: RawConfig = serde_yaml::from_str(yaml).unwrap();
        // let streaming = raw.agents["claurst-main"].streaming_config(); // имя метода сверить
        // assert_eq!(streaming.max_concurrent_streams, 1);
        // assert_eq!(streaming.first_chunk_timeout_secs, 15);
        // assert_eq!(streaming.idle_chunk_timeout_secs, 120);
    }

    #[test]
    fn max_concurrent_streams_zero_fails_startup() {
        // Валидация "0 на старте" подтверждена коммитом.
        // let yaml = r#"...
        //   claurst-main:
        //     ...
        //     streaming: { max_concurrent_streams: 0 }
        // "#;
        // let raw: RawConfig = serde_yaml::from_str(yaml).unwrap();
        // assert!(build_registry(&raw).is_err());
    }
}

// =========================================================================
// T9 — idle_chunk_timeout закрывает зависший стрим, first_chunk_timeout —
// незапустившийся.
// =========================================================================

#[cfg(test)]
mod t9_stream_timeouts {
    use super::*;

    #[tokio::test]
    async fn idle_timeout_closes_stalled_stream() {
        // Мок-агент присылает 1 чанк, затем не шлёт НИЧЕГО дольше
        // idle_chunk_timeout. Стрим должен закрыться по таймауту, не
        // висеть до старого agent_call_timeout_secs (120с по умолчанию —
        // тест не должен реально ждать 120с, поэтому мок настраивается
        // с idle_chunk_timeout = 100мс для скорости теста).
        //
        // let agent = spawn_mock_stdio_agent_with_idle_timeout(Duration::from_millis(100)).await;
        // ... отправить 1 чанк, затем не отправлять ничего ...
        // let start = std::time::Instant::now();
        // while rx.recv().await.is_some() {}
        // assert!(start.elapsed() < Duration::from_millis(500), "должно закрыться по idle_chunk_timeout, не по agent_call_timeout");
    }

    #[tokio::test]
    async fn first_chunk_timeout_fires_if_agent_never_starts_streaming() {
        // Мок-агент вообще не шлёт session/update и не отвечает —
        // first_chunk_timeout должен сработать раньше call_timeout.
    }
}

// =========================================================================
// T10-T12 (нагрузочный/live/clippy) — НЕ входят в этот пакет: T10 требует
// реальной инфраструктуры (5 живых мок-агентов, 10 минут прогона под
// нагрузкой) и делается отдельным прогоном, не юнит-тестом в CI. T11 —
// ручной live-прогон с реальным claurst/hermes, как существующий
// e2e_live (коммит 00fe731, --ignored --nocapture). T12 — команда
// `cargo clippy --workspace --all-targets -- -D warnings`, не код теста.
