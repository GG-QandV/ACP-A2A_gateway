//! gatewayd/src/transport_http.rs
//! Direction 4: A2A client -> ACP agent.
//! Endpoints: GET /agents/:id/.well-known/agent.json, POST /agents/:id/rpc

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

/// ADDED (Phase 3.2, T4/resubscribe live): per-task broadcast hub.
/// live stream events are published here with a seq (see spawn_stream_relay);
/// resubscribe, after replaying history, subscribes to this channel and
/// continues receiving live. Writing to the hub always happens BEFORE the
/// live send to the resubscriber, so under Lagged/catch-up the source of
/// truth stays the durable event_log (broadcast events are only a last-N cache).
#[derive(Default)]
pub struct StreamHub {
    senders: tokio::sync::Mutex<HashMap<String, broadcast::Sender<(u64, protocol::a2a::A2aEvent)>>>,
}

impl StreamHub {
    /// Subscribe to a task's live events. None = no active stream for the
    /// task (relay hasn't started or already closed) — resubscribe
    /// returns only history from the durable event_log.
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

    /// Publish a live event. Creates the task's channel on the first
    /// event (the relay owns the lifecycle: closes it on completion).
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
        // Client may have dropped off — not a failure, just normal: broadcast drops it.
        let _ = tx.send((seq, event));
    }

    /// The relay closes the task's channel when it finishes — resubscribers
    /// get Closed and know there will be no more live tail.
    pub async fn close(&self, task_id: &str) {
        self.senders.lock().await.remove(task_id);
    }
}

/// Broadcast buffer capacity per task. Must not break the stream on
/// overflow: a Lagged gap is closed by re-reading the event_log.
const HUB_CAPACITY: usize = 1024;

pub struct HttpState {
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
    /// External gateway address for AgentCard.url (audit P2-12).
    public_url: String,
    /// ADDED (audit P2-11): RPC timeout to the stdio agent, from config.
    call_timeout: Duration,
    adapters: tokio::sync::Mutex<HashMap<String, Arc<AcpAsA2a<SupervisedStdioAgent>>>>,
    /// ADDED (Phase 2/3 of the buffer config): durable buffer of stream
    /// events. None = the event_log section is disabled in config — streams
    /// work as before (ephemeral channel, no seq on the wire).
    event_log: Option<Arc<EventLog>>,
    /// ADDED (Phase 3.2): per-task broadcast hub for the live continuation
    /// of tasks/resubscribe after history replay.
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
        // ADDED: SDK format a2a-rs. a2a-server accepts both
        // POST /message:send (primary) and POST /message/send (legacy
        // alias, a2a-server/src/rest.rs:24,68) — both route to one
        // handler. In matchit 0.7 a `:` inside a segment starts a
        // parameter, i.e. the route "/agents/{id}/message{suffix}"
        // has TWO parameters — hence its own handler with a
        // tuple extractor (suffix is ignored). The legacy path below is
        // one static segment, it uses a plain Path<String>.
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

/// FIXED: previously this path returned a single anonymous anyhow error,
/// and the caller answered 404 / -32601 identically for "no such agent",
/// "process failed to start" and "handshake failed". The message lied about
/// the nature of the problem: an agent refusal looked like a typo in
/// agent_id, which sent log- and status-based diagnostics in the wrong
/// direction. In prod this is the cost of every incident, not of one
/// debug cycle.
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
    /// 404 — about addressing: the agent isn't in the registry, and
    /// retrying the request changes nothing.
    /// 503 — about availability: the agent is configured but isn't
    /// starting now; retrying the request later makes sense.
    fn status(&self) -> StatusCode {
        match self {
            Self::UnknownAgent(_) => StatusCode::NOT_FOUND,
            Self::NotAcpAgent(_) => StatusCode::BAD_REQUEST,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// -32601 (method not found) is appropriate only for a nonexistent
    /// agent. A spawn refusal is an application error, not addressing.
    fn rpc_code(&self) -> i64 {
        match self {
            Self::UnknownAgent(_) => -32601,
            Self::NotAcpAgent(_) => -32602,
            Self::Unavailable { .. } => AGENT_UNAVAILABLE_CODE,
        }
    }
}

/// Code for "agent is configured but won't start". Kept separate so the
/// client can tell a temporary refusal from a permanent one and decide to retry.
const AGENT_UNAVAILABLE_CODE: i64 = -32020;

async fn get_or_spawn_adapter(
    state: &Arc<HttpState>,
    agent_id: &str,
) -> Result<Arc<AcpAsA2a<SupervisedStdioAgent>>, AdapterError> {
    let mut adapters = state.adapters.lock().await;
    // FIXED (audit P2-10): the cache used to hand out an adapter without
    // caring whether the process behind it was alive. Now a dead process
    // is respawned by the adapter itself (via SupervisedStdioAgent), and
    // the cache stays valid — no need to rebuild it anymore.
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
        // ADDED (streaming roadmap Part 2): the agent's separate stream
        // timeouts come from config (streaming. section).
        first_chunk_timeout: entry.first_chunk_timeout,
        idle_chunk_timeout: entry.idle_chunk_timeout,
    })
    .await
    .map_err(|source| {
        // Log the cause in full: the client gets a short text,
        // the operator needs the complete failure context.
        tracing::error!(agent_id, error = ?source, "не удалось поднять агента");
        AdapterError::Unavailable {
            agent_id: agent_id.to_string(),
            source,
        }
    })?;

    // The address of this very agent, not the whole gateway: the card describes
    // the endpoint to which the client will send message/send.
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

    // FIXED (audit P1-1): the conversation owner is derived from the client
    // token; otherwise the adapter can't tell one client from another.
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
        // ADDED (Phase 3): tasks/resubscribe renders as an SSE stream
        // (history + live) — the client sees the same id:s as in the live stream,
        // and can continue after the last one received.
        Ok(DispatchResult::Resubscribe(stream)) => Sse::new(stream).into_response(),
        // ADDED (task D): the streaming response renders as SSE,
        // not as a JSON-RPC envelope — the client reads an A2aEvent stream.
        // ADDED (Part 2, task A): parallel-stream limit —
        // the permit lives until the SSE stream closes, fail-closed.
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
        // ADDED (audit P2-10): lost context is not "something went
        // wrong". The client must distinguish it from other errors to
        // restart the conversation, not silently continue into a void.
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => rpc_error(
            request.id,
            StatusCode::CONFLICT,
            CONTEXT_LOST_CODE,
            &e.to_string(),
        ),
        Err(e) => rpc_error(request.id, StatusCode::OK, -32000, &e.to_string()),
    }
}

/// Error code for lost context. The -32000..-32099 range is
/// reserved by JSON-RPC for application errors.
const CONTEXT_LOST_CODE: i64 = -32010;

fn rpc_error(id: Value, status: StatusCode, code: i64, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })),
    )
        .into_response()
}

// =========================================================================
// Direction 4 (SDK client): REST POST /agents/:id/message:send
// =========================================================================
//
// Response contract — SendMessageResponse from a2a-rs, NOT JSON-RPC:
//   success:   200  { "task": { ... } }                 (render_task_sdk)
//   error:  <http-status>  { "error": { "code", "status", "message", "details" } }
// Envelope shape confirmed by a2a-server/src/rest.rs:470, task wrapper —
// a2a-server/src/rest.rs:1264 (the test reads send_resp["task"]["id"]).
// HTTP status carries the machine error code; "status" — gRPC-style name,
// "code" — internal application code (-32010 for ContextLost etc.).

/// Primary SDK path POST /agents/:id/message:send.
/// In matchit 0.7 a `:` inside a segment starts a parameter — the route carries
/// two ({id}, {suffix}); the second is unneeded and discarded.
async fn rest_send_message_handler_sdk(
    State(state): State<Arc<HttpState>>,
    AxumPath((agent_id, _suffix)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    rest_send_message_core(state, &agent_id, &headers, &body).await
}

/// Legacy alias POST /agents/:id/message/send — one parameter, plain
/// extractor. Same processing path as message:send.
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

    // Validate the body BEFORE spawning the adapter: a broken request
    // shouldn't spawn an agent process just to be rejected.
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
        // The {task: ...} wrapper is mandatory: the SDK client unwraps
        // SendMessageResponse itself (render_task_sdk builds it).
        Ok(gateway_core::Reply::Complete(t)) => Json(render_task_sdk(&t)).into_response(),
        // ADDED (task D): a stub replaced by an SSE stream of A2aEvent.
        // ADDED (Part 2, task A): parallel-stream limit —
        // fail-closed, the permit lives until the stream closes.
        Ok(gateway_core::Reply::Streaming(rx)) => {
            let permit = match state.registry.try_acquire_stream(agent_id) {
                Ok(p) => p,
                Err(e) => {
                    return rest_sdk_error(StatusCode::SERVICE_UNAVAILABLE, -32000, &e.to_string())
                }
            };
            tracing::info!(
                agent_id,
                "SDK REST-клиент перешёл в стриминговый режим (SSE)"
            );
            let relay = spawn_stream_relay(rx, state.event_log.clone(), state.stream_hub.clone());
            stream_to_sse(relay, permit).into_response()
        }
        // Same contract as /rpc: lost context — 409 +
        // ABORTED, not an abstract internal error.
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => {
            rest_sdk_error(StatusCode::CONFLICT, CONTEXT_LOST_CODE, &e.to_string())
        }
        Err(e) => rest_sdk_error(StatusCode::INTERNAL_SERVER_ERROR, -32000, &e.to_string()),
    }
}

/// SDK REST error envelope: {"code", "status", "message", "details"}.
/// "jsonrpc" and "id" fields are absent on purpose — this is not a JSON-RPC reply.
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

/// gRPC-style name from the HTTP status (mapping like grpc-status). Every
/// code rest_sdk_error actually returns must be here —
/// regression is caught by test http_status_to_sdk_name_covers_all_used_codes.
/// Public so the integration test checks exactly this code, not
/// its own copy of the mapping.
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

/// One stream item after the relay: seq (Some = event persisted in
/// event_log and continuable via resubscribe) plus the event itself.
struct StreamItem {
    seq: Option<u64>,
    event: protocol::a2a::A2aEvent,
}

/// ADDED (Phase 3.2, T4/resubscribe): relay task that separates stream
/// lifetime from the client connection's lifetime. Reads A2aEvents from the agent
/// channel BEFORE sending them to the client:
/// 1. persists the event with task_id into event_log (source of truth for
///    resubscribe); on failure — doesn't break the stream, sends without seq;
/// 2. publishes (seq, event) to the per-task hub for live continuation
///    of resubscribers (BEFORE the client — a subscriber won't miss the event);
/// 3. hands the event to the current connection's client.
///    Client dropped — the relay keeps running: the agent isn't cancelled, events
///    keep being written to the durable buffer and hub; resubscribe will catch up.
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
                                // To the hub — BEFORE the client: a resubscriber that has
                                // subscribed to the task must not miss this event.
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
            // Client may have dropped off — ignore the send error, the relay continues.
            let _ = client_tx.send(StreamItem {
                seq: persisted,
                event,
            });
        }
        // The agent channel closed (stream finished) — resubscribers get
        // Closed from the hub and know there will be no more live tail.
        if let Some(task_id) = known_task {
            hub.close(&task_id).await;
        }
    });
    client_rx
}

/// ADDED (streaming roadmap Part 1, task D): renders a ready A2aEvent
/// stream into an HTTP SSE response (text/event-stream). Seam contract
/// Reply<T,U>: the transport knows nothing about ACP SessionUpdate — it receives
/// already-mapped A2aEvents and just serializes them into SSE frames.
/// Each event is a separate `data: {...}\n\n`.
///
/// ADDED (Part 2, task A): `permit` holds one of the agent's parallel-stream
/// slots until the stream closes (RAII pattern, like TurnGuard) —
/// the map closure captures the permit by move, and it drops together
/// with the stream.
///
/// ADDED (Phase 3.2): the source is the relay task (spawn_stream_relay), not
/// the agent channel directly. The seq from persistence goes to the client in
/// the SSE `id:` field: the client remembers the last processed id and on
/// reconnect sends it as after_seq. Events without seq (not persisted) go
/// without id.
fn stream_to_sse(
    rx: tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = UnboundedReceiverStream::new(rx).map(move |item| {
        // the permit is kept alive in this closure for the whole stream.
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

/// task_id from an A2aEvent for persistence into event_log. Message carries no
/// task_id — such events aren't buffered.
fn event_task_id(event: &protocol::a2a::A2aEvent) -> Option<String> {
    match event {
        protocol::a2a::A2aEvent::TaskStatusUpdate { task_id, .. }
        | protocol::a2a::A2aEvent::TaskArtifactUpdate { task_id, .. } => Some(task_id.0.clone()),
        protocol::a2a::A2aEvent::Message(_) => None,
    }
}

/// ADDED (Phase 3.2, T4/resubscribe live): SSE stream for stream
/// continuation. Two phases:
/// 1. History — events with seq > after_seq from the durable event_log (replay).
/// 2. Live — once history is exhausted, subscribe to the per-task hub
///    (spawn_stream_relay publishes every event here). Serve only
///    events with seq > the last one served (dedup: history and live may
///    overlap at the boundary). broadcast overflowed (Lagged) — re-read the
///    durable history from the last seq (catch-up) and continue live.
///    Agent channel closed (stream finished) — hub.close, the subscriber
///    gets Closed and the stream ends.
async fn resubscribe_stream(
    log: Arc<EventLog>,
    hub: Arc<StreamHub>,
    task_id: String,
    after_seq: u64,
) -> anyhow::Result<futures_util::stream::BoxStream<'static, Result<Event, Infallible>>> {
    // State of the unfold machine. Phases:
    //  queue non-empty -> serve history (from durable event_log, seq > after_seq);
    //  queue empty     -> switch to the hub live subscription (don't touch it
    //                     until history is exhausted);
    //  live closed     -> stream finished, close the stream.
    struct State {
        queue: std::collections::VecDeque<EventRecord>,
        live: Option<broadcast::Receiver<(u64, protocol::a2a::A2aEvent)>>,
        last_seq: u64,
        hub: Arc<StreamHub>,
        log: Arc<EventLog>,
        task_id: String,
    }

    // History is read BEFORE building the stream — the first item is served
    // without a latent subscription, and the live phase doesn't start before history exists.
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
            // 1) History: while records remain, serve them (source of truth).
            if let Some(rec) = st.queue.pop_front() {
                st.last_seq = rec.seq;
                return Some((Ok(event_from_record(&rec)), st));
            }
            // 2) Live: history exhausted — subscribe to the hub once.
            if st.live.is_none() {
                st.live = st.hub.subscribe(&st.task_id).await;
                // hub.subscribe == None: no active stream for the task
                // (relay not started or closed) — there will be no live tail.
                st.live.as_ref()?;
            }
            // 3) Live events with seq dedup (the history/live boundary may
            //    overlap) and catch-up on Lagged.
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
                // Duplicate at the history/live boundary — skip.
                Ok(_) => {}
                // broadcast overflowed — catch-up via durable history:
                // read everything we lost, then continue live.
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
                // Stream finished (relay closed the task channel) — end of the stream.
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(stream.boxed())
}

/// Serializes an EventRecord from the durable event_log into an SSE frame with id=seq.
/// A corrupt event doesn't crash the stream — an explicit error marker is emitted.
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

/// Dispatcher result: either synchronous JSON (Complete), an SSE stream
/// (Streaming), or replay+live from event_log/hub (Resubscribe).
enum DispatchResult {
    Json(Value),
    Streaming(tokio::sync::mpsc::UnboundedReceiver<protocol::a2a::A2aEvent>),
    /// ADDED (Phase 3): ready-made SSE stream for tasks/resubscribe —
    /// first history from the durable event_log (seq > after_seq), then
    /// live continuation from the per-task hub (Phase 3.2).
    Resubscribe(futures_util::stream::BoxStream<'static, Result<Event, Infallible>>),
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
                // ADDED (task D): a stub replaced by an SSE stream.
                gateway_core::Reply::Streaming(rx) => Ok(DispatchResult::Streaming(rx)),
            }
        }

        // ADDED: SDK format a2a-rs (SendMessage method, camelCase/proto
        // fields). The response renders via render_task_sdk — {task} wrapper,
        // TASK_STATE_*, ROLE_*. The semantic branch above is unchanged.
        "SendMessage" => {
            let task: Task = build_task_from_send_params_sdk(&request.params)?;
            match adapter.send_task_as(owner, task).await? {
                gateway_core::Reply::Complete(t) => Ok(DispatchResult::Json(render_task_sdk(&t))),
                // ADDED (task D): a stub replaced by an SSE stream.
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

        // ADDED: SDK alias for tasks/get. The "name" parameter in SDK JSON-RPC
        // carries a path like "tasks/<id>" — extract the id from the tail, or,
        // if the client sent a flat "id" (not per SDK spec, but lenient
        // parsing), take it directly.
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

        // ADDED (buffer-config Phase 3, T4/resubscribe): stream
        // continuation after a connection drop. The client passes the last
        // processed seq (after_seq). First the server serves history from the
        // durable event_log (events with seq > after_seq, ascending),
        // then — ADDED (Phase 3.2) — live continuation from the per-task hub:
        // if the task's stream is still alive, new events arrive live right
        // after replay. If event_log is disabled in config — error:
        // there is nothing to restore from; an honest refusal beats a silent void.
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
            let stream =
                resubscribe_stream(log.clone(), stream_hub.clone(), id.to_string(), after_seq)
                    .await?;
            Ok(DispatchResult::Resubscribe(stream))
        }

        // ADDED (Phase 3): before reconnect the client can query the
        // task's last marker. Response — { "seq": N, "task_id": "..." }.
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

/// SDK GetTask/CancelTask params carry either {"name": "tasks/<id>"} (per
/// the SDK spec) or a flat {"id": "<id>"} (lenient parsing — some
/// clients send it the same way as in the semantic format). Try both.
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

    // FIXED (audit P1-1): contextId used to be equated with task_id,
    // i.e. every message started a new conversation, while on the adapter
    // side they all landed in one shared session anyway. Now the client's
    // contextId is respected (a standard A2A field), and if it's missing —
    // a new one is issued and returned to the client in Task.contextId.
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

/// SDK variant of build_task_from_send_params: uses normalize_message
/// instead of direct serde_json::from_value::<Message>, to accept
/// ROLE_USER/{text} without a kind field. contextId is read from camelCase (SDK)
/// with a snake_case fallback (for mixed clients).
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

/// FIXED (audit P1-3): it was a bare nanosecond timestamp —
/// a predictable, enumerable task ID, which together with the missing
/// owner check in tasks/get exposed other parties' tasks. Also removed
/// the unwrap() on system time.
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

    /// Regression: previously all three causes returned 404 / -32601, and an agent
    /// spawn failure looked in logs like a typo in agent_id.
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

    /// The refusal cause must reach the error text: the operator reads
    /// it in the response and in the log, and "unavailable" without a cause is useless.
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
