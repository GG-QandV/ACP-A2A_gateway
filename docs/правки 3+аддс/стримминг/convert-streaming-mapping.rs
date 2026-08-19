// core/src/convert.rs — ДОБАВЛЕНИЕ (не полный файл): маппинг стриминга
// для Reply::Streaming в обоих направлениях. Патч senior-уровня по
// задаче "core/src/convert.rs — маппинг SessionUpdate <-> A2aEvent" из
// delegation-instructions-junior-middle.md, раздел "Что НЕ делегируется".
//
// Вставляется в core/src/convert.rs. Место вставки указано в комментариях
// "ЗАМЕНИТЬ В" перед каждым блоком. Не самостоятельный файл — фрагмент патча.
//
// Разбор по вариантам сделан ВРУЧНУЮ (5 вариантов SessionUpdate x 3
// варианта A2aEvent), как того требует процесс из "Что НЕ делегируется":
// ни один вариант не отбрасывается молча — там, где прямого эквивалента
// нет (UsageUpdate), решение явное и задокументировано, а не implicit.

// =========================================================================
// НАПРАВЛЕНИЕ 4: AcpAsA2a — ACP-агент даёт SessionUpdate, наружу нужен A2aEvent.
// ЗАМЕНИТЬ В: impl<T: AcpAgent> AcpAsA2a<T> :: send_task_as(),
//             ветку `Reply::Streaming(_rx) => anyhow::bail!("Фаза 1: стриминг не реализован")`
// =========================================================================

/// Разбор вариантов SessionUpdate -> A2aEvent.
///
/// РЕШЕНИЯ ПО КАЖДОМУ ВАРИАНТУ (senior review, не механический маппинг):
///
/// 1. `AgentMessageChunk` — прямой эквивалент есть: это и есть основной
///    контент ответа агента по мере генерации. Мапится в
///    `A2aEvent::TaskStatusUpdate` с `state: Working`, `message` содержит
///    один Part::Text (или иной ContentBlock, преобразованный через уже
///    существующий `content_block_to_part`). `final: false` — это НЕ
///    последнее событие потока.
///
/// 2. `ToolCall` / `ToolCallUpdate` — в A2A v1.0 (protocol/src/a2a.rs)
///    НЕТ отдельного события "вызов инструмента". Решение: НЕ отбрасывать
///    молча (в отличие от игнорирования всего события) — упаковать как
///    `TaskStatusUpdate` с `state: Working` и текстовым `message`,
///    описывающим статус вызова (`"[инструмент: {title}] {status}"`).
///    Это theряет структурированность (клиент A2A не узнает tool_call_id
///    как отдельное поле), но клиент минимум ВИДИТ, что происходит —
///    лучше текстовый след, чем полная потеря сигнала. Если у клиента
///    A2A появится потребность в структурированных tool-call событиях —
///    это отдельное расширение A2aEvent, не дело этого патча.
///
/// 3. `Plan` — аналогично ToolCall: нет прямого эквивалента, мапится в
///    `TaskStatusUpdate` с текстовым представлением плана (нумерованный
///    список entries), `state: Working`.
///
/// 4. `UsageUpdate` — РЕШЕНИЕ: НЕ эмitить как A2aEvent клиенту вообще.
///    Токены/стоимость — внутренняя телеметрия ACP-агента, в A2A-спеке
///    (protocol/src/a2a.rs) для этого нет поля ни в Task, ни в
///    TaskStatus, ни в Artifact. Молчаливый дроп был бы недокументированной
///    потерей — вместо этого логируем на DEBUG-уровне для наблюдаемости
///    без изменения протокольного контракта с клиентом. Тот же принцип,
///    что уже применён в этом файле для `TaskState <-> StopReason`
///    ("НЕ биекция, задокументировано явно").
///
/// Терминальное событие (когда rx закрывается после последнего элемента)
/// строится ОТДЕЛЬНО в вызывающем коде send_task_as() из финального
/// PromptResponse — та же логика, что уже есть в ветке Reply::Complete
/// (parts/artifacts/agent_message), только с `final: true` и через тот
/// же A2aEvent::TaskStatusUpdate.
fn session_update_to_a2a_event(update: SessionUpdate, task_id: &TaskId) -> Option<a2a::A2aEvent> {
    match update {
        SessionUpdate::AgentMessageChunk { content, .. } => {
            let part = content_block_to_part(content);
            Some(a2a::A2aEvent::TaskStatusUpdate {
                task_id: task_id.clone(),
                status: TaskStatus {
                    state: TaskState::Working,
                    message: Some(Message {
                        role: MessageRole::Agent,
                        parts: vec![part],
                        message_id: None,
                    }),
                    timestamp: now_iso8601(),
                },
                r#final: false,
            })
        }

        SessionUpdate::ToolCall { title, status, .. } => Some(a2a::A2aEvent::TaskStatusUpdate {
            task_id: task_id.clone(),
            status: TaskStatus {
                state: TaskState::Working,
                message: Some(text_status_message(&format!(
                    "[инструмент: {title}] {status:?}"
                ))),
                timestamp: now_iso8601(),
            },
            r#final: false,
        }),

        SessionUpdate::ToolCallUpdate { status, .. } => Some(a2a::A2aEvent::TaskStatusUpdate {
            task_id: task_id.clone(),
            status: TaskStatus {
                state: TaskState::Working,
                message: Some(text_status_message(&format!("[инструмент] {status:?}"))),
                timestamp: now_iso8601(),
            },
            r#final: false,
        }),

        SessionUpdate::Plan { entries } => {
            let text = entries
                .iter()
                .enumerate()
                .map(|(i, e)| format!("{}. [{}] {} ({})", i + 1, e.status, e.content, e.priority))
                .collect::<Vec<_>>()
                .join("\n");
            Some(a2a::A2aEvent::TaskStatusUpdate {
                task_id: task_id.clone(),
                status: TaskStatus {
                    state: TaskState::Working,
                    message: Some(text_status_message(&text)),
                    timestamp: now_iso8601(),
                },
                r#final: false,
            })
        }

        // РЕШЕНИЕ (см. комментарий выше метода): не имеет эквивалента в
        // A2A-протоколе. Не эмитим событие клиенту, только наблюдаемость.
        SessionUpdate::UsageUpdate { used, size, cost } => {
            tracing::debug!(
                used,
                size,
                cost = ?cost,
                "SessionUpdate::UsageUpdate не имеет эквивалента в A2A — не транслируется клиенту"
            );
            None
        }
    }
}

fn text_status_message(text: &str) -> Message {
    Message {
        role: MessageRole::Agent,
        parts: vec![Part::Text { text: text.to_string() }],
        message_id: None,
    }
}

// Полная ветка Reply::Streaming для send_task_as() — заменяет собой
// строку `Reply::Streaming(_rx) => anyhow::bail!("Фаза 1: стриминг не реализован")`.
// Возвращает Reply::Streaming(out_rx) с уже смаппленными A2aEvent —
// вызывающий транспортный код (gatewayd/transport_http.rs, задача D)
// получает готовый UnboundedReceiver<A2aEvent> и просто рендерит его в SSE,
// не зная НИЧЕГО про ACP SessionUpdate. Это и есть контракт seam Reply<T,U>.
//
// ЗАМЕНИТЬ В: match self.inner.prompt(prompt_req).await? { ... }
/*
match self.inner.prompt(prompt_req).await? {
    Reply::Complete(resp) => {
        // ...существующий код Complete-пути не меняется...
    }
    Reply::Streaming(mut in_rx) => {
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<a2a::A2aEvent>();
        let task_id = task.id.clone();
        let context_id = task.context_id.clone();
        let tasks = self.tasks.clone(); // TaskStore должен быть Clone (Arc внутри) — проверить при интеграции
        let owner_for_save = owner;

        tokio::spawn(async move {
            let mut chunk_count = 0usize;
            let mut last_state = TaskState::Working;

            while let Some(update) = in_rx.recv().await {
                chunk_count += 1;
                if let Some(event) = session_update_to_a2a_event(update, &task_id) {
                    if out_tx.send(event).is_err() {
                        // ЛОГ-ЛОВУШКА (WARN, по умолчанию включена):
                        tracing::warn!(
                            task_id = %task_id.0,
                            "получатель A2aEvent отключился до terminal event — задача продолжает выполняться в фоне"
                        );
                        return;
                    }
                }
            }

            // ЛОГ-ЛОВУШКА (WARN, по умолчанию включена): см. Часть 1,
            // пункт 1.1 streaming-roadmap-checklist.md — 0 чанков не баг
            // сам по себе, но диагностический сигнал.
            if chunk_count == 0 {
                tracing::warn!(task_id = %task_id.0, "stream produced 0 chunks before terminal event");
            }

            // Терминальное событие. ВАЖНО: здесь нет доступа к финальному
            // PromptResponse — StopReason приходит через ОТДЕЛЬНЫЙ канал
            // от stdio_agent.rs (задача G/junior, core/src/stdio_agent.rs).
            // Контракт для задачи G: последний элемент in_rx ДОЛЖЕН быть
            // терминальным SessionUpdate-эквивалентом ЛИБО in_rx закрывается
            // явно после того, как отдельный механизм (см. TODO ниже)
            // передал финальный state. Это ОТКРЫТЫЙ ВОПРОС для интеграции
            // между задачей G и этим маппингом — см. раздел "Точка
            // интеграции G <-> convert.rs" ниже.
            last_state = TaskState::Completed; // placeholder до решения точки интеграции
            let _ = out_tx.send(a2a::A2aEvent::TaskStatusUpdate {
                task_id: task_id.clone(),
                status: TaskStatus { state: last_state, message: None, timestamp: now_iso8601() },
                r#final: true,
            });
        });

        Ok(Reply::Streaming(out_rx))
    }
}
*/

// =========================================================================
// НАПРАВЛЕНИЕ 3: A2aAsAcp — A2A-агент даёт A2aEvent, наружу нужен SessionUpdate.
// ЗАМЕНИТЬ В: impl<T: A2aAgent> AcpAgent for A2aAsAcp<T> :: prompt(),
//             ветку Reply::Streaming от self.inner.send_task(task)
// =========================================================================

/// Разбор вариантов A2aEvent -> SessionUpdate.
///
/// РЕШЕНИЯ:
///
/// 1. `TaskStatusUpdate { final: false, .. }` — промежуточный статус.
///    Если `status.message` содержит текстовые Part — мапится в
///    `SessionUpdate::AgentMessageChunk`. Если message пуст (агент шлёт
///    только смену состояния без текста) — НЕ эмитим SessionUpdate
///    (в ACP нет события "пустой статус"), только tracing::debug лог.
///
/// 2. `TaskStatusUpdate { final: true, .. }` — терминальное событие.
///    НЕ мапится в SessionUpdate вообще — оно обрабатывается ОТДЕЛЬНО,
///    как сигнал закрытия потока и построения финального PromptResponse
///    (StopReason из TaskState через уже существующий task_state_to_stop_reason()).
///    Это симметрично тому, как терминальное событие в направлении 4
///    строится отдельно от чанков.
///
/// 3. `TaskArtifactUpdate` — артефакт (файл/результат) от A2A-агента.
///    Мапится в `SessionUpdate::AgentMessageChunk` с ContentBlock,
///    полученным через уже существующий `part_to_content_block` для
///    каждой части артефакта. Если артефакт содержит несколько Part —
///    эмитим несколько AgentMessageChunk подряд (по одному на Part),
///    не пытаясь склеить их в один блок — так минимальны риски потери
///    структуры (например, разные mime_type у разных частей).
///
/// 4. `Message(_)` — прямое сообщение от агента вне контекста статуса
///    задачи. Мапится в `SessionUpdate::AgentMessageChunk` так же, как
///    TaskArtifactUpdate — по одному чанку на Part.
fn a2a_event_to_session_update(event: a2a::A2aEvent) -> Vec<SessionUpdate> {
    match event {
        a2a::A2aEvent::TaskStatusUpdate { status, r#final, .. } => {
            if r#final {
                // Терминальное событие обрабатывается отдельно вызывающим
                // кодом (см. ниже) — здесь не эмитим ничего.
                return Vec::new();
            }
            match status.message {
                Some(message) if !message.parts.is_empty() => message
                    .parts
                    .into_iter()
                    .map(|part| SessionUpdate::AgentMessageChunk {
                        message_id: message.message_id.clone(),
                        content: part_to_content_block(part),
                    })
                    .collect(),
                _ => {
                    tracing::debug!(state = ?status.state, "TaskStatusUpdate без текста — не транслируется в ACP");
                    Vec::new()
                }
            }
        }

        a2a::A2aEvent::TaskArtifactUpdate { artifact, .. } => artifact
            .parts
            .into_iter()
            .map(|part| SessionUpdate::AgentMessageChunk {
                message_id: None,
                content: part_to_content_block(part),
            })
            .collect(),

        a2a::A2aEvent::Message(message) => message
            .parts
            .into_iter()
            .map(|part| SessionUpdate::AgentMessageChunk {
                message_id: message.message_id.clone(),
                content: part_to_content_block(part),
            })
            .collect(),
    }
}

// Полная ветка для prompt() в A2aAsAcp — заменяет заглушку в направлении 3.
// ЗАМЕНИТЬ В: match self.inner.send_task(task).await? { ... }
/*
match self.inner.send_task(task).await? {
    Reply::Complete(result_task) => {
        // ...существующий Complete-путь не меняется...
    }
    Reply::Streaming(mut in_rx) => {
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<SessionUpdate>();

        tokio::spawn(async move {
            let mut chunk_count = 0usize;
            while let Some(event) = in_rx.recv().await {
                let is_final = matches!(
                    &event,
                    a2a::A2aEvent::TaskStatusUpdate { r#final: true, .. }
                );
                for update in a2a_event_to_session_update(event) {
                    chunk_count += 1;
                    if out_tx.send(update).is_err() {
                        tracing::warn!("получатель SessionUpdate отключился до terminal event");
                        return;
                    }
                }
                if is_final {
                    break; // терминал закрывает поток; PromptResponse строит вызывающий код
                }
            }
            if chunk_count == 0 {
                tracing::warn!("stream produced 0 chunks before terminal event (направление 3)");
            }
        });

        Ok(Reply::Streaming(out_rx))
    }
}
*/

// =========================================================================
// ТОЧКА ИНТЕГРАЦИИ: G (core/src/stdio_agent.rs) <-> этот маппинг
// =========================================================================
//
// ОТКРЫТЫЙ ВОПРОС для синхронизации с задачей G (см. delegation-instructions,
// раздел "Что делегируется с обязательным чек-поинтом"): направление 4
// (AcpAsA2a::send_task_as) требует знать StopReason финального
// PromptResponse, чтобы правильно замапить TaskState терминального
// A2aEvent (Completed/Failed/Canceled — не просто "Working").
//
// Варианты решения (выбрать ОДИН на созвоне с исполнителем задачи G,
// до мержа обоих патчей):
//
//   (a) stdio_agent.rs добавляет спец-маркер в конец потока SessionUpdate
//       (например, оборачивает канал в enum StreamItem { Chunk(SessionUpdate),
//       Terminal(PromptResponse) }) — тогда Reply<T,U> должен стать
//       Reply<T, StreamItem<U>>, что МЕНЯЕТ сигнатуру — против правила
//       seam из 04-architecture-guide-extending.md. НЕ рекомендуется.
//
//   (b) stdio_agent.rs закрывает канал SessionUpdate молча, а финальный
//       StopReason передаётся через отдельный oneshot::Receiver<StopReason>,
//       который prompt() возвращает ВМЕСТЕ с Reply::Streaming (требует
//       либо доп. поля в Reply::Streaming, либо отдельного метода
//       agent.prompt_stream() -> (Reply<...>, oneshot::Receiver<StopReason>)).
//       Тоже трогает сигнатуры, но точечно — не сам enum Reply<T,U>.
//
//   (c) РЕКОМЕНДУЕТСЯ: не менять сигнатуры вообще — terminal state
//       всегда TaskState::Completed, если канал закрылся без ошибки, и
//       TaskState::Failed, если stdio_agent.rs вернул Err ПОСЛЕ того как
//       часть чанков уже ушла (это отдельная ошибка, не в потоке
//       SessionUpdate, а в внешнем anyhow::Result — обрабатывается кодом
//       вокруг tokio::spawn выше через доп. проверку результата
//       self.inner.prompt(...).await, если он возвращает ошибку уже
//       ПОСЛЕ начала стрима — редкий edge case, тот самый "что если
//       prompt упадёт после того как чанки уже ушли", который я как
//       senior обязан явно решить по своей же инструкции в задаче G).
//       Различие Cancelled/Refusal внутри Failed теряется — это
//       сознательный компромисс ради сохранения seam, аналогично уже
//       задокументированной "TaskState <-> StopReason — НЕ биекция".
//
// Решение (c) принято как рабочее для первой итерации. Пересмотреть,
// если в проде окажется, что клиентам критично различать Cancelled от
// прочих ошибок в потоковом пути (в нестриминговом Complete-пути это
// различие УЖЕ есть через stop_reason_to_task_state — потерян только
// в потоковом случае, и это явно задокументированный, а не случайный
// пробел).
