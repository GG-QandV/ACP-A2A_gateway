// core/src/convert.rs и gatewayd/src/transport_http.rs — задача D
// (SSE-обёртка, направление 4). Написано senior заранее, ПАРАЛЛЕЛЬНО с
// задачами A/Б (registry.rs, main.rs), которые на момент написания
// этого патча ЕЩЁ НЕ ЗАПУШЕНЫ в GitHub (последний коммит в
// ACP-A2A_gateway — 9c37554, не содержит правок registry.rs/main.rs
// под streaming). Это ОЖИДАЕМО при параллельной работе — задача D не
// должна ждать A/Б, она зависит только от Reply::Streaming<A2aEvent>,
// который уже определён в reply.rs и уже смаппен в convert.rs
// (см. convert-streaming-mapping.rs, отданный ранее).
//
// ЯВНЫЙ КОНТРАКТ, который эта задача ОЖИДАЕТ от A/Б при интеграции —
// если реальные сигнатуры разойдутся, чинить нужно ТОЛЬКО точку
// вызова acquire_stream_permit() ниже, остальной код не зависит от
// деталей Registry/AgentEntry:
//
//   trait/метод (любое из двух — уточнить при мерже с A):
//     registry.try_acquire_stream(agent_id) -> Result<OwnedSemaphorePermit, StreamCapacityExhausted>
//   ИЛИ
//     agent_entry.stream_permits.clone().try_acquire_owned()  (Arc<Semaphore> прямо в AgentEntry)
//
// Ниже реализовано через второй вариант (прямой доступ к полю), потому
// что он не требует знания о новом типе ошибки из Registry — если A
// реализовал первый вариант, замена тривиальна (один вызов функции).

use std::sync::Arc;
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use protocol::a2a::A2aEvent;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

// =========================================================================
// 1. Общая функция рендеринга — заменяет ВСЕ ТРИ заглушки одним вызовом.
// =========================================================================

/// Оборачивает уже смаппленный поток `A2aEvent` (пришедший из
/// `core/src/convert.rs::AcpAsA2a::send_task_as` — см. Reply::Streaming
/// ветку в convert-streaming-mapping.rs) в SSE-ответ axum.
///
/// НЕ содержит логики маппинга протоколов — только сериализация в
/// wire-формат SSE. Маппинг ACP SessionUpdate -> A2aEvent сделан ДО
/// вызова этой функции, в convert.rs. Это соответствует контракту seam:
/// транспортный слой не знает про ACP вообще.
pub fn stream_to_sse(
    rx: UnboundedReceiver<A2aEvent>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = UnboundedReceiverStream::new(rx).map(|event| {
        // A2aEvent уже Serialize (protocol/src/a2a.rs) — сериализуем как
        // JSON-данные одного SSE-кадра. serde_json::to_string не должен
        // паниковать на нашем собственном enum, но на случай будущих
        // непредвиденных полей (например, кто-то добавит не-Serialize
        // тип внутрь Artifact.metadata) — деградируем в текстовое
        // сообщение об ошибке внутри самого события, а не рвём поток:
        // клиент SSE не должен получить обрыв соединения из-за одного
        // плохого события, если можно вместо этого дать диагностику.
        let data = serde_json::to_string(&event).unwrap_or_else(|e| {
            tracing::error!(error = %e, "не удалось сериализовать A2aEvent для SSE — событие пропущено");
            serde_json::json!({"error": "serialization_failed"}).to_string()
        });
        Ok(Event::default().data(data))
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// =========================================================================
// 2. Точка вызова — замена ТРЁХ заглушек в dispatch_a2a_method() и
//    rest_send_message_core(). Показан набросок для message/send;
//    SendMessage и REST message:send делают то же самое, отличается
//    только построение Task на входе (уже готовая логика, не трогается).
// =========================================================================

/*
ЗАМЕНИТЬ В dispatch_a2a_method(), ветка "message/send":

БЫЛО:
    "message/send" => {
        let task: Task = build_task_from_send_params(&request.params)?;
        match adapter.send_task_as(owner, task).await? {
            gateway_core::Reply::Complete(t) => Ok(serde_json::to_value(t)?),
            gateway_core::Reply::Streaming(_) => {
                anyhow::bail!("Фаза 1: streaming не реализован для A2A->ACP направления")
            }
        }
    }

СТАНЕТ (внимание: сигнатура dispatch_a2a_method должна начать
возвращать не anyhow::Result<Value>, а enum, различающий JSON-ответ от
SSE-ответа — это ЕДИНСТВЕННАЯ структурная правка, которая выходит за
пределы одной функции; см. пункт 3 ниже):

    "message/send" => {
        let task: Task = build_task_from_send_params(&request.params)?;
        match adapter.send_task_as(owner, task).await? {
            gateway_core::Reply::Complete(t) => Ok(RpcOutcome::Json(serde_json::to_value(t)?)),
            gateway_core::Reply::Streaming(rx) => Ok(RpcOutcome::Sse(stream_to_sse(rx))),
        }
    }
*/

// =========================================================================
// 3. Структурная правка, вытекающая из наличия SSE-пути: dispatch_a2a_method
//    сейчас возвращает anyhow::Result<Value> и вызывающий код (rpc_handler)
//    оборачивает результат в JSON-RPC конверт. Ветка Streaming не может
//    вернуть Value (это не JSON, это открытый HTTP-стрим) — нужен общий
//    тип результата для обеих веток.
// =========================================================================

/// Замена типа возврата dispatch_a2a_method(). rpc_handler() должен
/// различать эти два варианта при построении HTTP-ответа: Json оборачивается
/// в JSON-RPC конверт {jsonrpc, id, result}, как сейчас; Sse отдаётся как есть
/// (SSE-ответ не оборачивается в JSON-RPC конверт — это отдельный HTTP-контракт,
/// клиент SSE ожидает text/event-stream, не JSON-тело).
pub enum RpcOutcome {
    Json(serde_json::Value),
    Sse(axum::response::Response),
}

/*
ЗАМЕНИТЬ В rpc_handler() — было:
    let result = dispatch_a2a_method(&adapter, owner, &request).await;
    match result {
        Ok(value) => Json(json!({ "jsonrpc": "2.0", "id": request.id, "result": value })).into_response(),
        ...
    }

СТАНЕТ:
    let result = dispatch_a2a_method(&adapter, owner, &request).await;
    match result {
        Ok(RpcOutcome::Json(value)) => Json(json!({ "jsonrpc": "2.0", "id": request.id, "result": value })).into_response(),
        Ok(RpcOutcome::Sse(response)) => response,  // уже готовый axum Response, не оборачиваем
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => { ... как сейчас ... }
        Err(e) => rpc_error(request.id, StatusCode::OK, -32000, &e.to_string()),
    }

stream_to_sse() возвращает Sse<...>, которая сама реализует IntoResponse —
для RpcOutcome::Sse нужно вызвать .into_response() на неё ДО того, как
она попадёт в enum (Sse<impl Stream<...>> не Send-safe для хранения без
конкретизации типа стрима через .into_response()):

    gateway_core::Reply::Streaming(rx) => Ok(RpcOutcome::Sse(stream_to_sse(rx).into_response())),
*/

// =========================================================================
// 4. Лог-ловушка на разрыв соединения клиентом (WARN, из роадмапа,
//    Часть 1 п.1.3) — ВАЖНО: axum::response::sse::Sse САМ не даёt простого
//    хука "клиент отключился". Отслеживание разрыва делается на уровне
//    исходного mpsc-канала в convert.rs (там, где out_tx.send(...).is_err()
//    уже ловит закрытие получателя — см. convert-streaming-mapping.rs,
//    комментарий "получатель A2aEvent отключился до terminal event").
//    Здесь, в transport_http.rs, дополнительная ловушка НЕ нужна и не
//    добавляется повторно — иначе одно и то же событие логировалось бы
//    дважды на двух разных уровнях (антипаттерн: расхождение источника
//    правды для одной и той же лог-записи).
// =========================================================================

// =========================================================================
// 5. Открытый вопрос для интеграции с A/Б (Semaphore) — ГДЕ вызывать
//    try_acquire? РЕШЕНИЕ: ДО вызова adapter.send_task_as(), не после
//    получения Reply::Streaming. Причина: acquire должен произойти
//    прежде, чем ACP-агент реально начнёт работу (prompt_streaming()
//    уже спавнит поток внутри stdio_agent.rs, задача G) — если ждать
//    Reply::Streaming для acquire, агент уже потратит ресурсы на запрос,
//    который потом придётся отбросить из-за лимита. Fail-closed должен
//    быть максимально ранним.
// =========================================================================

/*
ЗАМЕНИТЬ В rpc_handler() — ПЕРЕД вызовом dispatch_a2a_method(), сразу
после того, как adapter получен (get_or_spawn_adapter):

    // НОВОЕ: занимаем permit ДО отправки запроса агенту.
    let _stream_permit = match state.registry.lookup(&agent_id) {
        Some(entry) => match entry.stream_permits.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    "agent stream capacity exhausted — запрос отклонён fail-closed"
                );
                return rpc_error(
                    request.id,
                    StatusCode::SERVICE_UNAVAILABLE,
                    STREAM_CAPACITY_EXHAUSTED_CODE,
                    "agent stream capacity exhausted",
                );
            }
        },
        None => None, // agent_id не найден — дальше по коду уже есть проверка UnknownAgent
    };
    // _stream_permit должен жить до конца обработки запроса (Drop в
    // конце функции освобождает слот) — если Reply::Streaming, permit
    // нужно ПЕРЕМЕСТИТЬ внутрь tokio::spawn из convert.rs, иначе он
    // освободится сразу после return из rpc_handler, а не когда стрим
    // реально завершится. Это ТРЕБУЕТ передать permit в
    // send_task_as()/prompt_streaming() как дополнительный параметр —
    // ОТКРЫТЫЙ ВОПРОС для синхронизации между этой задачей и задачей A,
    // решить на созвоне ДО мержа обоих патчей, не в одностороннем порядке.

const STREAM_CAPACITY_EXHAUSTED_CODE: i64 = -32021;  // рядом с уже существующим AGENT_UNAVAILABLE_CODE = -32020
*/

// =========================================================================
// ИТОГ: что можно мержить сразу, что требует созвона с A/Б
// =========================================================================
//
// Готово к мержу независимо (не требует A/Б):
//   - stream_to_sse() — самодостаточная функция, зависит только от
//     UnboundedReceiver<A2aEvent> из convert.rs.
//   - RpcOutcome enum + правка сигнатуры dispatch_a2a_method/rpc_handler.
//
// ТРЕБУЕТ созвона перед мержем (пункт 5 выше):
//   - Момент и место acquire/release Semaphore-permit — нужно решить
//     совместно с исполнителем задачи A, где именно permit живёт на
//     всё время стрима (не только на время синхронной части запроса).
//     Если A уже сделал try_acquire_stream() как метод Registry (не
//     прямой доступ к полю AgentEntry) — код в пункте 5 адаптируется
//     тривиально, структура решения (acquire до вызова агента) не меняется.
