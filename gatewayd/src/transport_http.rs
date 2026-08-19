//! gatewayd/src/transport_http.rs
//! Направление 4: A2A-клиент -> ACP-агент.
//! Эндпоинты: GET /agents/:id/.well-known/agent.json, POST /agents/:id/rpc

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use gateway_core::{A2aAgent, AcpAsA2a, ContextLost, Owner, SpawnConfig, SupervisedStdioAgent};
use protocol::a2a::{Task, TaskId};
use protocol::a2a_sdk_compat::{normalize_message, render_task_sdk};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use tokio::sync::broadcast;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::event_log::{EventLog, EventRecord};
use crate::registry::{Registry, Transport};

/// ДОБАВЛЕНО (Фаза 3.2, T4/resubscribe live): per-task broadcast-hub.
/// live-события стрима публикуются сюда с seq (см. spawn_stream_relay);
/// resubscribe после replay истории подписывается на этот канал и
/// продолжает приём вживую. Запись в hub — всегда ДО live-отправки
/// resubscriber'у, поэтому при Lacad/catch-up источник истины остаётся
/// durable event_log (события в broadcast — только кэш последних).
#[derive(Default)]
pub struct StreamHub {
    senders: tokio::sync::Mutex<HashMap<String, broadcast::Sender<(u64, protocol::a2a::A2aEvent)>>>,
}

impl StreamHub {
    /// Подписка на live-события задачи. None = для задачи нет активного
    /// стрима (релей ещё не стартовал или уже закрылся) — resubscribe
    /// отдаёт только историю из durable event_log.
    pub async fn subscribe(
        &self,
        task_id: &str,
    ) -> Option<broadcast::Receiver<(u64, protocol::a2a::A2aEvent)>> {
        self.senders
            .lock()
            .await
            .get(task_id)
            .map(broadcast::Sender::subscribe)
    }

    /// Публикация live-события. Создаёт канал для задачи при первом
    /// событии (релей владеет жизненным циклом: close на завершении).
    pub async fn publish(&self, task_id: &str, seq: u64, event: protocol::a2a::A2aEvent) {
        let mut guard = self.senders.lock().await;
        let tx = match guard.get(task_id) {
            Some(tx) => tx.clone(),
            None => {
                let (tx, _rx) = broadcast::channel(HUB_CAPACITY);
                guard.insert(task_id.to_string(), tx.clone());
                tx
            }
        };
        // Клиент мог отвалиться — не сбой, а норма: broadcast дропает.
        let _ = tx.send((seq, event));
    }

    /// Релей закрывает канал задачи на своём завершении — resubscriber'ы
    /// получают Closed и понимают, что live-хвоста больше не будет.
    pub async fn close(&self, task_id: &str) {
        self.senders.lock().await.remove(task_id);
    }
}

/// Ёмкость broadcast-буфера на задачу. Не должен ломать стрим при
/// переполнении: Lacad-пропуск закрывается повторным чтением event_log.
const HUB_CAPACITY: usize = 1024;

pub struct HttpState {
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
    /// Внешний адрес шлюза для AgentCard.url (аудит P2-12).
    public_url: String,
    /// ДОБАВЛЕНО (аудит P2-11): таймаут RPC к stdio-агенту из конфига.
    call_timeout: Duration,
    adapters: tokio::sync::Mutex<HashMap<String, Arc<AcpAsA2a<SupervisedStdioAgent>>>>,
    /// ДОБАВЛЕНО (Фаза 2/3 буферного конфига): durable-буфер событий
    /// стрима. None = секция event_log выключена в конфиге — стримы идут
    /// как раньше (эфемерный канал, без seq на wire).
    event_log: Option<Arc<EventLog>>,
    /// ДОБАВЛЕНО (Фаза 3.2): per-task broadcast-hub для live-продолжения
    /// tasks/resubscribe после replay истории.
    stream_hub: Arc<StreamHub>,
}

pub fn router(
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
    call_timeout: Duration,
    public_url: String,
    event_log: Option<Arc<EventLog>>,
) -> Router {
    let state = Arc::new(HttpState {
        registry,
        task_store_dir,
        lease_timeout,
        public_url,
        call_timeout,
        adapters: tokio::sync::Mutex::new(HashMap::new()),
        event_log,
        stream_hub: Arc::new(StreamHub::default()),
    });

    Router::new()
        .route("/agents/:agent_id/.well-known/agent.json", get(agent_card))
        .route("/agents/:agent_id/rpc", post(rpc_handler))
        // ДОБАВЛЕНО: SDK-формат a2a-rs. a2a-server принимает и
        // POST /message:send (основной), и POST /message/send (легаси-
        // алиас, a2a-server/src/rest.rs:24,68) — оба ведём на один
        // хендлер. В matchit 0.7 `:` в середине сегмента трактуется как
        // начало параметра, т.е. маршрут "/agents/{id}/message{suffix}"
        // имеет ДВА параметра — поэтому у него отдельный хендлер с
        // tuple-экстрактором (suffix игнорируется). Легаси-путь ниже —
        // один статический сегмент, у него обычный Path<String>.
        .route(
            "/agents/:agent_id/message:send",
            post(rest_send_message_handler_sdk),
        )
        .route(
            "/agents/:agent_id/message/send",
            post(rest_send_message_handler),
        )
        .with_state(state)
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string)
}

/// ИСПРАВЛЕНО: раньше на этом пути возвращалась одна безликая
/// anyhow-ошибка, и вызывающий код отвечал 404 / -32601 одинаково на
/// «нет такого агента», «процесс не поднялся» и «рукопожатие
/// провалилось». Сообщение врало о природе проблемы: отказ агента
/// выглядел как опечатка в agent_id, из-за чего диагностика по логам и
/// статусам вела не туда. В проде это цена каждого инцидента, а не
/// одного отладочного цикла.
#[derive(Debug, thiserror::Error)]
enum AdapterError {
    #[error("unknown agent_id: {0}")]
    UnknownAgent(String),

    #[error("agent_id={0} не является stdio/ACP агентом (для A2A-целей используйте /a2a-proxy)")]
    NotAcpAgent(String),

    #[error("агент {agent_id} недоступен: {source}")]
    Unavailable {
        agent_id: String,
        #[source]
        source: anyhow::Error,
    },
}

impl AdapterError {
    /// 404 — про адресацию: такого агента в реестре нет, и повтор
    /// запроса ничего не изменит.
    /// 503 — про доступность: агент настроен, но сейчас не поднимается;
    /// запрос имеет смысл повторить позже.
    fn status(&self) -> StatusCode {
        match self {
            Self::UnknownAgent(_) => StatusCode::NOT_FOUND,
            Self::NotAcpAgent(_) => StatusCode::BAD_REQUEST,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// -32601 (method not found) уместен только для несуществующего
    /// агента. Отказ запуска — ошибка приложения, а не адресации.
    fn rpc_code(&self) -> i64 {
        match self {
            Self::UnknownAgent(_) => -32601,
            Self::NotAcpAgent(_) => -32602,
            Self::Unavailable { .. } => AGENT_UNAVAILABLE_CODE,
        }
    }
}

/// Код для «агент настроен, но не поднимается». Отдельный, чтобы клиент
/// мог отличить временный отказ от постоянного и решить, повторять ли.
const AGENT_UNAVAILABLE_CODE: i64 = -32020;

async fn get_or_spawn_adapter(
    state: &Arc<HttpState>,
    agent_id: &str,
) -> Result<Arc<AcpAsA2a<SupervisedStdioAgent>>, AdapterError> {
    let mut adapters = state.adapters.lock().await;
    // ИСПРАВЛЕНО (аудит P2-10): раньше кэш отдавал адаптер, не
    // интересуясь, жив ли за ним процесс. Теперь мёртвый процесс
    // поднимает сам адаптер (через SupervisedStdioAgent), а кэш
    // остаётся валидным — пересоздавать его больше не нужно.
    if let Some(existing) = adapters.get(agent_id) {
        return Ok(existing.clone());
    }

    let entry = state
        .registry
        .lookup(agent_id)
        .ok_or_else(|| AdapterError::UnknownAgent(agent_id.to_string()))?
        .clone();

    let Transport::Stdio { command, cwd, env } = entry.transport else {
        return Err(AdapterError::NotAcpAgent(agent_id.to_string()));
    };

    let default_cwd = cwd.clone().unwrap_or_else(|| ".".to_string());
    let supervised = SupervisedStdioAgent::spawn(SpawnConfig {
        command,
        cwd,
        env,
        call_timeout: state.call_timeout,
        protocol_version: SpawnConfig::DEFAULT_PROTOCOL_VERSION,
        // ДОБАВЛЕНО (Часть 2 роадмапа стриминга): раздельные таймауты
        // стрима агента берутся из конфига (streaming. секция).
        first_chunk_timeout: entry.first_chunk_timeout,
        idle_chunk_timeout: entry.idle_chunk_timeout,
    })
    .await
    .map_err(|source| {
        // Логируем причину целиком: клиенту уходит короткий текст,
        // оператору нужен полный контекст отказа.
        tracing::error!(agent_id, error = ?source, "не удалось поднять агента");
        AdapterError::Unavailable {
            agent_id: agent_id.to_string(),
            source,
        }
    })?;

    // Адрес именно этого агента, а не шлюза целиком: карточка описывает
    // конечную точку, по которой клиент будет слать message/send.
    let agent_url = format!(
        "{}/agents/{agent_id}/rpc",
        state.public_url.trim_end_matches('/')
    );

    let adapter = Arc::new(AcpAsA2a::new(
        supervised,
        default_cwd,
        state.task_store_dir.join(agent_id),
        state.lease_timeout,
        agent_url,
    ));

    adapters.insert(agent_id.to_string(), adapter.clone());
    Ok(adapter)
}

async fn agent_card(
    State(state): State<Arc<HttpState>>,
    AxumPath(agent_id): AxumPath<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "missing token"})),
            )
                .into_response()
        }
    };
    if !state.registry.check_token(&token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid token"})),
        )
            .into_response();
    }

    match get_or_spawn_adapter(&state, &agent_id).await {
        Ok(adapter) => match adapter.card().await {
            Ok(card) => Json(card).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": e.to_string()})),
            )
                .into_response(),
        },
        Err(e) => (e.status(), Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn rpc_handler(
    State(state): State<Arc<HttpState>>,
    AxumPath(agent_id): AxumPath<String>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => {
            return rpc_error(
                request.id,
                StatusCode::UNAUTHORIZED,
                -32000,
                "missing token",
            )
        }
    };
    if !state.registry.check_token(&token) {
        return rpc_error(
            request.id,
            StatusCode::UNAUTHORIZED,
            -32000,
            "invalid token",
        );
    }

    let adapter = match get_or_spawn_adapter(&state, &agent_id).await {
        Ok(a) => a,
        Err(e) => return rpc_error(request.id, e.status(), e.rpc_code(), &e.to_string()),
    };

    // ИСПРАВЛЕНО (аудит P1-1): владелец разговора выводится из токена
    // клиента, иначе адаптер не может отличить одного клиента от другого.
    let owner = Owner::from_token(&token);

    let result = dispatch_a2a_method(
        &adapter,
        owner,
        &request,
        state.event_log.clone(),
        state.stream_hub.clone(),
    )
    .await;
    match result {
        Ok(DispatchResult::Json(value)) => {
            Json(json!({ "jsonrpc": "2.0", "id": request.id, "result": value })).into_response()
        }
        // ДОБАВЛЕНО (Фаза 3): tasks/resubscribe рендерится как SSE-поток
        // (история + live) — клиент видит те же id:, что и в живом стриме,
        // и может продолжить после последнего полученного.
        Ok(DispatchResult::Resubscribe(stream)) => Sse::new(stream).into_response(),
        // ДОБАВЛЕНО (задача D): стриминговый ответ рендерится как SSE,
        // а не как JSON-RPC конверт — клиент читает поток A2aEvent.
        // ДОБАВЛЕНО (Часть 2, задача A): лимит параллельных стримов —
        // permit живёт до закрытия SSE-потока, fail-closed.
        Ok(DispatchResult::Streaming(rx)) => {
            let permit = match state.registry.try_acquire_stream(&agent_id) {
                Ok(p) => p,
                Err(e) => {
                    return rpc_error(
                        request.id,
                        StatusCode::SERVICE_UNAVAILABLE,
                        -32000,
                        &e.to_string(),
                    )
                }
            };
            let relay = spawn_stream_relay(rx, state.event_log.clone(), state.stream_hub.clone());
            stream_to_sse(relay, permit).into_response()
        }
        // ДОБАВЛЕНО (аудит P2-10): потеря контекста — не «что-то пошло
        // не так». Клиент должен отличить её от прочих ошибок, чтобы
        // начать разговор заново, а не молча продолжать в пустоту.
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => rpc_error(
            request.id,
            StatusCode::CONFLICT,
            CONTEXT_LOST_CODE,
            &e.to_string(),
        ),
        Err(e) => rpc_error(request.id, StatusCode::OK, -32000, &e.to_string()),
    }
}

/// Код ошибки для потерянного контекста. Диапазон -32000..-32099
/// отведён JSON-RPC под ошибки приложения.
const CONTEXT_LOST_CODE: i64 = -32010;

fn rpc_error(id: Value, status: StatusCode, code: i64, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })),
    )
        .into_response()
}

// =========================================================================
// Направление 4 (SDK-клиент): REST POST /agents/:id/message:send
// =========================================================================
//
// Контракт ответа — SendMessageResponse из a2a-rs, НЕ JSON-RPC:
//   успех:   200  { "task": { ... } }                 (render_task_sdk)
//   ошибка:  <http-status>  { "error": { "code", "status", "message", "details" } }
// Форма конверта подтверждена a2a-server/src/rest.rs:470, обёртка task —
// a2a-server/src/rest.rs:1264 (тест читает send_resp["task"]["id"]).
// HTTP-статус несёт машинный код ошибки; "status" — gRPC-style имя,
// "code" — внутренний код приложения (-32010 для ContextLost и т.д.).

/// Основной SDK-путь POST /agents/:id/message:send.
/// В matchit 0.7 `:` внутри сегмента начинает параметр — маршрут несёт
/// два ({id}, {suffix}), второй не нужен и отбрасывается.
async fn rest_send_message_handler_sdk(
    State(state): State<Arc<HttpState>>,
    AxumPath((agent_id, _suffix)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    rest_send_message_core(state, &agent_id, &headers, &body).await
}

/// Легаси-алиас POST /agents/:id/message/send — один параметр, обычный
/// экстрактор. Тот же путь обработки, что и у message:send.
async fn rest_send_message_handler(
    State(state): State<Arc<HttpState>>,
    AxumPath(agent_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    rest_send_message_core(state, &agent_id, &headers, &body).await
}

async fn rest_send_message_core(
    state: Arc<HttpState>,
    agent_id: &str,
    headers: &HeaderMap,
    body: &Value,
) -> axum::response::Response {
    let token = match extract_bearer(headers) {
        Some(t) => t,
        None => return rest_sdk_error(StatusCode::UNAUTHORIZED, -32000, "missing token"),
    };
    if !state.registry.check_token(&token) {
        return rest_sdk_error(StatusCode::UNAUTHORIZED, -32000, "invalid token");
    }

    // Валидируем тело ДО подъёма адаптера: битый запрос не должен
    // спавнить процесс агента ради того, чтобы быть отвергнутым.
    let task = match build_task_from_send_params_sdk(body) {
        Ok(t) => t,
        Err(e) => return rest_sdk_error(StatusCode::BAD_REQUEST, -32602, &e.to_string()),
    };

    let adapter = match get_or_spawn_adapter(&state, agent_id).await {
        Ok(a) => a,
        Err(e) => return rest_sdk_error(e.status(), e.rpc_code(), &e.to_string()),
    };

    let owner = Owner::from_token(&token);

    match adapter.send_task_as(owner, task).await {
        // Обёртка {task: ...} обязательна: SDK-клиент разворачивает
        // SendMessageResponse сам (render_task_sdk её и строит).
        Ok(gateway_core::Reply::Complete(t)) => Json(render_task_sdk(&t)).into_response(),
        // ДОБАВЛЕНО (задача D): вместо заглушки — SSE-поток A2aEvent.
        // ДОБАВЛЕНО (Часть 2, задача A): лимит параллельных стримов —
        // fail-closed, permit живёт до закрытия потока.
        Ok(gateway_core::Reply::Streaming(rx)) => {
            let permit = match state.registry.try_acquire_stream(agent_id) {
                Ok(p) => p,
                Err(e) => {
                    return rest_sdk_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        -32000,
                        &e.to_string(),
                    )
                }
            };
            tracing::info!(
                agent_id,
                "SDK REST-клиент перешёл в стриминговый режим (SSE)"
            );
            let relay = spawn_stream_relay(rx, state.event_log.clone(), state.stream_hub.clone());
            stream_to_sse(relay, permit).into_response()
        }
        // Тот же контракт, что и у /rpc: потерянный контекст — 409 +
        // ABORTED, а не абстрактная внутренняя ошибка.
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => {
            rest_sdk_error(StatusCode::CONFLICT, CONTEXT_LOST_CODE, &e.to_string())
        }
        Err(e) => rest_sdk_error(StatusCode::INTERNAL_SERVER_ERROR, -32000, &e.to_string()),
    }
}

/// REST-конверт ошибки SDK: {"code", "status", "message", "details"}.
/// Поля "jsonrpc" и "id" отсутствуют намеренно — это не JSON-RPC ответ.
fn rest_sdk_error(status: StatusCode, code: i64, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "status": http_status_to_sdk_name(status),
                "message": message,
                "details": [],
            }
        })),
    )
        .into_response()
}

/// gRPC-style имя из HTTP-статуса (маппинг как у grpc-status). Каждый
/// код, который реально отдаёт rest_sdk_error, обязан быть здесь —
/// регрессию ловит тест http_status_to_sdk_name_covers_all_used_codes.
/// Публичен, чтобы интеграционный тест проверял именно этот код, а не
/// свою копию маппинга.
pub fn http_status_to_sdk_name(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "INVALID_ARGUMENT",
        StatusCode::UNAUTHORIZED => "UNAUTHENTICATED",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "ABORTED",
        StatusCode::SERVICE_UNAVAILABLE => "UNAVAILABLE",
        StatusCode::NOT_IMPLEMENTED => "UNIMPLEMENTED",
        StatusCode::OK => "OK",
        _ => "UNKNOWN",
    }
}

/// Один элемент потока после relay: seq (Some = событие персистено в
/// event_log и может быть продолжено через resubscribe) и само событие.
struct StreamItem {
    seq: Option<u64>,
    event: protocol::a2a::A2aEvent,
}

/// ДОБАВЛЕНО (Фаза 3.2, T4/resubscribe): relay-таск, отделяющий жизнь
/// стрима от жизни клиентского соединения. Читает A2aEvent из канала
/// агента ДО их отправки клиенту:
/// 1. персистит событие с task_id в event_log (источник истины для
///    resubscribe), в случае сбоя — не ломает стрим, шлёт без seq;
/// 2. публикует (seq, event) в per-task hub для live-продолжения
///    resubscriber'ов (ДО клиента — подписчик не пропустит событие);
/// 3. отдаёт событие клиенту текущего соединения.
/// Клиент отвалился — relay живёт дальше: агент не канселится, события
/// продолжают писаться в durable-буфер и hub, resubscribe нагонит.
fn spawn_stream_relay(
    rx: tokio::sync::mpsc::UnboundedReceiver<protocol::a2a::A2aEvent>,
    event_log: Option<Arc<EventLog>>,
    hub: Arc<StreamHub>,
) -> tokio::sync::mpsc::UnboundedReceiver<StreamItem> {
    let (client_tx, client_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut rx = rx;
        let mut known_task: Option<String> = None;
        while let Some(event) = rx.recv().await {
            let tid = event_task_id(&event);
            if let Some(t) = tid.as_deref() {
                known_task = Some(t.to_string());
            }
            let persisted = match (event_log.as_ref(), tid) {
                (Some(log), Some(task_id)) => {
                    match serde_json::to_string(&event) {
                        Ok(json) => match log.append(&task_id, &json).await {
                            Ok(seq) => {
                                // В hub — ДО клиента: resubscriber, подписавшийся
                                // на задачу, не должен пропустить это событие.
                                hub.publish(&task_id, seq, event.clone()).await;
                                Some(seq)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    task_id = %task_id,
                                    error = %e,
                                    "event_log: не удалось персистить — шлю без seq"
                                );
                                None
                            }
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, "event_log: сериализация события");
                            None
                        }
                    }
                }
                _ => None,
            };
            // Клиент мог отвалиться — игнорируем send-ошибку, relay продолжает.
            let _ = client_tx.send(StreamItem { seq: persisted, event });
        }
        // Канал агента закрылся (стрим завершён) — resubscriber'ы получают
        // Closed из hub и понимают, что live-хвоста больше не будет.
        if let Some(task_id) = known_task {
            hub.close(&task_id).await;
        }
    });
    client_rx
}

/// ДОБАВЛЕНО (Часть 1 роадмапа стриминга, задача D): рендерит готовый
/// поток A2aEvent в HTTP SSE-ответ (text/event-stream). Контракт seam
/// Reply<T,U>: транспорт не знает про ACP SessionUpdate — он получает
/// уже смаппленные A2aEvent и просто сериализует их в SSE-фреймы.
/// Каждое событие — отдельный `data: {...}\n\n`.
///
/// ДОБАВЛЕНО (Часть 2, задача A): `permit` удерживает слот параллельных
/// стримов агента до закрытия потока (RAII-паттерн, как у TurnGuard) —
/// map-замыкание захватывает permit по move, и он дропается вместе со
/// стримом.
///
/// ДОБАВЛЕНО (Фаза 3.2): источник — relay-таск (spawn_stream_relay), а не
/// канал агента напрямую. seq из персистенции уходит клиенту в SSE
/// `id:`-поле: клиент запоминает последний обработанный id и при
/// reconnect шлёт его как after_seq. События без seq (не персистены) идут
/// без id.
fn stream_to_sse(
    rx: tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = UnboundedReceiverStream::new(rx).map(move |item| {
        // permit удерживается живым в этом замыкании на весь стрим.
        let _ = &permit;
        let mut ev = Event::default()
            .json_data(item.event)
            .expect("A2aEvent сериализуется");
        if let Some(seq) = item.seq {
            ev = ev.id(seq.to_string());
        }
        Ok(ev)
    });
    Sse::new(stream)
}

/// task_id из A2aEvent для персистенции в event_log. Message не несёт
/// task_id — такие события не буферизуются.
fn event_task_id(event: &protocol::a2a::A2aEvent) -> Option<String> {
    match event {
        protocol::a2a::A2aEvent::TaskStatusUpdate { task_id, .. }
        | protocol::a2a::A2aEvent::TaskArtifactUpdate { task_id, .. } => {
            Some(task_id.0.clone())
        }
        protocol::a2a::A2aEvent::Message(_) => None,
    }
}

/// ДОБАВЛЕНО (Фаза 3.2, T4/resubscribe live): SSE-поток продолжения
/// стрима. Две фазы:
/// 1. История — события с seq > after_seq из durable event_log (replay).
/// 2. Live — после исчерпания истории подписка на per-task hub
///    (spawn_stream_relay публикует сюда каждое событие). Отдаём только
///    события с seq > последнего отданного (дедуп: история и live могут
///    пересекаться на границе). broadcast переполнился (Lagged) — повторно
///    читаем durable-историю с последнего seq (catch-up) и продолжаем live.
/// Закрытие канала агента (стрим завершён) — hub.close, подписчик
/// получает Closed и поток завершается.
async fn resubscribe_stream(
    log: Arc<EventLog>,
    hub: Arc<StreamHub>,
    task_id: String,
    after_seq: u64,
) -> anyhow::Result<futures_util::stream::BoxStream<'static, Result<Event, Infallible>>> {
    // Состояние unfold-машины. Фазы:
    //  queue непуст  -> отдаём историю (из durable event_log, seq > after_seq);
    //  queue пуст    -> переключаемся на live-подписку hub (не трогаем её,
    //                   пока история не исчерпана);
    //  live закрыт   -> стрим завершён, поток закрываем.
    struct State {
        queue: std::collections::VecDeque<EventRecord>,
        live: Option<broadcast::Receiver<(u64, protocol::a2a::A2aEvent)>>,
        last_seq: u64,
        hub: Arc<StreamHub>,
        log: Arc<EventLog>,
        task_id: String,
    }

    // История читается ДО построения стрима — первый элемент отдаётся без
    // латентной подписки, и live-фаза не стартует раньше наличия истории.
    let history: Vec<EventRecord> = log.events_after(&task_id, after_seq, 10_000).await?;
    let last_seq = history.last().map(|r| r.seq).unwrap_or(after_seq);

    let state = State {
        queue: history.into_iter().collect(),
        live: None,
        last_seq,
        hub,
        log,
        task_id,
    };

    let stream = futures_util::stream::unfold(state, |mut st| async move {
        loop {
            // 1) История: пока есть записи, отдаём их (источник истины).
            if let Some(rec) = st.queue.pop_front() {
                st.last_seq = rec.seq;
                return Some((Ok(event_from_record(&rec)), st));
            }
            // 2) Live: история исчерпана — подписываемся на hub один раз.
            if st.live.is_none() {
                st.live = st.hub.subscribe(&st.task_id).await;
                // hub.subscribe == None: для задачи нет активного стрима
                // (релей не стартовал или закрылся) — live-хвоста не будет.
                if st.live.is_none() {
                    return None;
                }
            }
            // 3) События live с дедупом по seq (граница история/live может
            //    пересекаться) и catch-up при Lagged.
            let rx = st.live.as_mut().expect("live подписка есть");
            match rx.recv().await {
                Ok((seq, event)) if seq > st.last_seq => {
                    st.last_seq = seq;
                    return Some((
                        Ok(Event::default()
                            .json_data(event)
                            .expect("A2aEvent сериализуется")
                            .id(seq.to_string())),
                        st,
                    ));
                }
                // Дубликат на границе история/live — пропускаем.
                Ok(_) => {}
                // broadcast переполнен — catch-up через durable историю:
                // читаем всё, что потеряли, и продолжаем live.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::info!(
                        task_id = %st.task_id,
                        lagged = n,
                        "resubscribe: broadcast переполнен — catch-up из event_log"
                    );
                    match st.log.events_after(&st.task_id, st.last_seq, 10_000).await {
                        Ok(caught) => {
                            st.queue = caught.into_iter().collect();
                            st.live = None;
                        }
                        Err(e) => {
                            tracing::warn!(
                                task_id = %st.task_id,
                                error = %e,
                                "resubscribe: catch-up чтение event_log"
                            );
                            return None;
                        }
                    }
                }
                // Стрим завершён (релей закрыл канал задачи) — конец потока.
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(stream.boxed())
}

/// Сериализует EventRecord из durable event_log в SSE-фрейм с id=seq.
/// Битое событие не роняет поток — идёт явный маркер ошибки.
fn event_from_record(rec: &EventRecord) -> Event {
    match serde_json::from_str::<protocol::a2a::A2aEvent>(&rec.event_json) {
        Ok(ev) => Event::default()
            .json_data(ev)
            .expect("A2aEvent сериализуется")
            .id(rec.seq.to_string()),
        Err(e) => {
            tracing::warn!(
                task_id = %rec.task_id,
                seq = rec.seq,
                error = %e,
                "resubscribe: битое событие в event_log"
            );
            Event::default()
                .json_data(json!({ "error": "corrupt event in event_log" }))
                .expect("json сериализуется")
                .id(rec.seq.to_string())
        }
    }
}

/// Результат диспетчера: либо синхронный JSON (Complete), либо поток SSE
/// (Streaming), либо replay+live из event_log/hub (Resubscribe).
enum DispatchResult {
    Json(Value),
    Streaming(tokio::sync::mpsc::UnboundedReceiver<protocol::a2a::A2aEvent>),
    /// ДОБАВЛЕНО (Фаза 3): готовый SSE-поток для tasks/resubscribe —
    /// сначала история из durable event_log (seq > after_seq), затем
    /// live-продолжение из per-task hub (Фаза 3.2).
    Resubscribe(
        futures_util::stream::BoxStream<
            'static,
            Result<Event, Infallible>,
        >,
    ),
}

async fn dispatch_a2a_method(
    adapter: &Arc<AcpAsA2a<SupervisedStdioAgent>>,
    owner: Owner,
    request: &JsonRpcRequest,
    event_log: Option<Arc<EventLog>>,
    stream_hub: Arc<StreamHub>,
) -> anyhow::Result<DispatchResult> {
    match request.method.as_str() {
        "message/send" => {
            let task: Task = build_task_from_send_params(&request.params)?;
            match adapter.send_task_as(owner, task).await? {
                gateway_core::Reply::Complete(t) => {
                    Ok(DispatchResult::Json(serde_json::to_value(t)?))
                }
                // ДОБАВЛЕНО (задача D): вместо заглушки — SSE-поток.
                gateway_core::Reply::Streaming(rx) => Ok(DispatchResult::Streaming(rx)),
            }
        }

        // ДОБАВЛЕНО: SDK-формат a2a-rs (метод SendMessage, camelCase/proto
        // поля). Ответ рендерится через render_task_sdk — обёртка {task},
        // TASK_STATE_*, ROLE_*. Семантическая ветка выше не меняется.
        "SendMessage" => {
            let task: Task = build_task_from_send_params_sdk(&request.params)?;
            match adapter.send_task_as(owner, task).await? {
                gateway_core::Reply::Complete(t) => Ok(DispatchResult::Json(render_task_sdk(&t))),
                // ДОБАВЛЕНО (задача D): вместо заглушки — SSE-поток.
                gateway_core::Reply::Streaming(rx) => Ok(DispatchResult::Streaming(rx)),
            }
        }

        "tasks/get" => {
            let id = request
                .params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/get: id обязателен"))?;
            let task = adapter.get_task_as(owner, TaskId(id.to_string())).await?;
            Ok(DispatchResult::Json(serde_json::to_value(task)?))
        }

        // ДОБАВЛЕНО: SDK-алиас tasks/get. Параметр "name" в SDK JSON-RPC
        // содержит путь вида "tasks/<id>" — извлекаем id из хвоста, либо,
        // если клиент прислал плоское "id" (не по спеке SDK, но щадящий
        // разбор), берём его напрямую.
        "GetTask" => {
            let id = extract_sdk_task_id(&request.params)
                .ok_or_else(|| anyhow::anyhow!("GetTask: id/name обязателен"))?;
            let task = adapter.get_task_as(owner, TaskId(id)).await?;
            Ok(DispatchResult::Json(render_task_sdk(&task)))
        }

        "tasks/cancel" => {
            let id = request
                .params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/cancel: id обязателен"))?;
            let task = adapter
                .cancel_task_as(owner, TaskId(id.to_string()))
                .await?;
            Ok(DispatchResult::Json(serde_json::to_value(task)?))
        }

        // ДОБАВЛЕНО (Фаза 3 буферного конфига, T4/resubscribe): продолжение
        // стрима после разрыва соединения. Клиент передаёт последний
        // обработанный seq (after_seq). Сначала сервер отдаёт историю из
        // durable event_log (события с seq > after_seq, по возрастанию),
        // затем — ДОБАВЛЕНО (Фаза 3.2) — live-продолжение из per-task hub:
        // если стрим задачи ещё жив, новые события приходят вживую сразу
        // после replay. Если event_log выключен в конфиге — ошибка:
        // восстановить не из чего, лучше честный отказ, чем тихая пустота.
        "tasks/resubscribe" => {
            let log = event_log.as_ref().ok_or_else(|| {
                anyhow::anyhow!("tasks/resubscribe: event_log не включён в конфиге")
            })?;
            let id = request
                .params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/resubscribe: id обязателен"))?;
            let after_seq = request
                .params
                .get("after_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let stream = resubscribe_stream(
                log.clone(),
                stream_hub.clone(),
                id.to_string(),
                after_seq,
            )
            .await?;
            Ok(DispatchResult::Resubscribe(stream))
        }

        // ДОБАВЛЕНО (Фаза 3): клиент перед reconnect может спросить
        // последний маркер задачи. Ответ — { "seq": N, "task_id": "..." }.
        "tasks/get-last-seq" => {
            let log = event_log.as_ref().ok_or_else(|| {
                anyhow::anyhow!("tasks/get-last-seq: event_log не включён в конфиге")
            })?;
            let id = request
                .params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/get-last-seq: id обязателен"))?;
            let seq = log.last_seq(id).await?;
            Ok(DispatchResult::Json(json!({
                "task_id": id,
                "seq": seq,
            })))
        }

        "CancelTask" => {
            let id = extract_sdk_task_id(&request.params)
                .ok_or_else(|| anyhow::anyhow!("CancelTask: id/name обязателен"))?;
            let task = adapter.cancel_task_as(owner, TaskId(id)).await?;
            Ok(DispatchResult::Json(render_task_sdk(&task)))
        }

        other => anyhow::bail!("method_not_found: {other}"),
    }
}

/// SDK GetTask/CancelTask params несут либо {"name": "tasks/<id>"} (по
/// спеке SDK), либо плоское {"id": "<id>"} (щадящий разбор — некоторые
/// клиенты шлют так же, как в семантическом формате). Пробуем оба.
fn extract_sdk_task_id(params: &Value) -> Option<String> {
    if let Some(name) = params.get("name").and_then(Value::as_str) {
        return name.rsplit('/').next().map(str::to_string);
    }
    params.get("id").and_then(Value::as_str).map(str::to_string)
}

fn build_task_from_send_params(params: &Value) -> anyhow::Result<Task> {
    let message_value = params
        .get("message")
        .ok_or_else(|| anyhow::anyhow!("message/send: message обязателен"))?;
    let message: protocol::a2a::Message = serde_json::from_value(message_value.clone())?;

    let task_id = format!("task-{}", uuid_stub());

    // ИСПРАВЛЕНО (аудит P1-1): contextId раньше приравнивался к task_id,
    // то есть каждое сообщение начинало новый разговор, а на стороне
    // адаптера все они всё равно попадали в одну общую сессию. Теперь
    // contextId клиента уважается (штатное поле A2A), а если его нет —
    // выдаётся новый и возвращается клиенту в Task.contextId.
    let context_id = params
        .get("message")
        .and_then(|m| m.get("contextId"))
        .or_else(|| params.get("contextId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("ctx-{}", uuid_stub()));

    Ok(Task {
        id: TaskId(task_id),
        context_id: protocol::a2a::ContextId(context_id),
        status: protocol::a2a::TaskStatus {
            state: protocol::a2a::TaskState::Submitted,
            message: Some(message),
            timestamp: None,
        },
        history: None,
        artifacts: None,
        metadata: None,
    })
}

/// SDK-вариант build_task_from_send_params: использует normalize_message
/// вместо прямого serde_json::from_value::<Message>, чтобы принять
/// ROLE_USER/{text} без поля kind. contextId читается из camelCase (SDK)
/// с фоллбэком на snake_case (на случай смешанных клиентов).
fn build_task_from_send_params_sdk(params: &Value) -> anyhow::Result<Task> {
    let message_value = params
        .get("message")
        .ok_or_else(|| anyhow::anyhow!("SendMessage: message обязателен"))?;
    let message =
        normalize_message(message_value).map_err(|e| anyhow::anyhow!("SendMessage: {e}"))?;

    let task_id = format!("task-{}", uuid_stub());

    let context_id = params
        .get("message")
        .and_then(|m| m.get("contextId").or_else(|| m.get("context_id")))
        .or_else(|| params.get("contextId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("ctx-{}", uuid_stub()));

    Ok(Task {
        id: TaskId(task_id),
        context_id: protocol::a2a::ContextId(context_id),
        status: protocol::a2a::TaskStatus {
            state: protocol::a2a::TaskState::Submitted,
            message: Some(message),
            timestamp: None,
        },
        history: None,
        artifacts: None,
        metadata: None,
    })
}

/// ИСПРАВЛЕНО (аудит P1-3): был голый наносекундный таймстамп —
/// предсказуемый и перечислимый ID задачи, что вместе с отсутствием
/// проверки владельца в tasks/get открывало чужие задачи. Плюс убран
/// unwrap() на системном времени.
fn uuid_stub() -> String {
    use std::io::Read;
    use std::time::{SystemTime, UNIX_EPOCH};

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let mut buf = [0u8; 12];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !filled {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        buf[..8].copy_from_slice(&n.to_le_bytes());
    }
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("{millis:x}-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Регрессия: раньше все три причины отдавали 404 / -32601, и отказ
    /// запуска агента выглядел в логах как опечатка в agent_id.
    #[test]
    fn unknown_agent_is_addressing_error() {
        let e = AdapterError::UnknownAgent("нет-такого".into());
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        assert_eq!(e.rpc_code(), -32601);
    }

    #[test]
    fn failed_spawn_is_availability_error() {
        let e = AdapterError::Unavailable {
            agent_id: "claurst-main".into(),
            source: anyhow::anyhow!("рукопожатие не удалось"),
        };
        assert_eq!(
            e.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "агент настроен, но не поднялся — это 503, а не 404"
        );
        assert_eq!(e.rpc_code(), AGENT_UNAVAILABLE_CODE);
    }

    #[test]
    fn wrong_transport_is_client_error() {
        let e = AdapterError::NotAcpAgent("ops-agent".into());
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    /// Причина отказа должна доходить до текста ошибки: оператор читает
    /// её в ответе и в логе, и «недоступен» без причины бесполезно.
    #[test]
    fn unavailable_message_keeps_cause() {
        let e = AdapterError::Unavailable {
            agent_id: "claurst-main".into(),
            source: anyhow::anyhow!("protocolVersion: не разобрать версию"),
        };
        let text = e.to_string();
        assert!(text.contains("claurst-main"));
        assert!(text.contains("не разобрать версию"));
    }
}
