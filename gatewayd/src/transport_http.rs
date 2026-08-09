//! gatewayd/src/transport_http.rs
//! Направление 4: A2A-клиент -> ACP-агент.
//! Эндпоинты: GET /agents/:id/.well-known/agent.json, POST /agents/:id/rpc

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use gateway_core::{
    A2aAgent, AcpAsA2a, ContextLost, Owner, SpawnConfig, SupervisedStdioAgent,
};
use protocol::a2a::{Task, TaskId};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::registry::{Registry, Transport};

pub struct HttpState {
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
    /// Внешний адрес шлюза для AgentCard.url (аудит P2-12).
    public_url: String,
    /// ДОБАВЛЕНО (аудит P2-11): таймаут RPC к stdio-агенту из конфига.
    call_timeout: Duration,
    adapters: tokio::sync::Mutex<HashMap<String, Arc<AcpAsA2a<SupervisedStdioAgent>>>>,
}

pub fn router(
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
    call_timeout: Duration,
    public_url: String,
) -> Router {
    let state = Arc::new(HttpState {
        registry,
        task_store_dir,
        lease_timeout,
        public_url,
        call_timeout,
        adapters: tokio::sync::Mutex::new(HashMap::new()),
    });

    Router::new()
        .route("/agents/:agent_id/.well-known/agent.json", get(agent_card))
        .route("/agents/:agent_id/rpc", post(rpc_handler))
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
    })
    .await
    .map_err(|source| {
        // Логируем причину целиком: клиенту уходит короткий текст,
        // оператору нужен полный контекст отказа.
        tracing::error!(agent_id, error = ?source, "не удалось поднять агента");
        AdapterError::Unavailable { agent_id: agent_id.to_string(), source }
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
        None => return (StatusCode::UNAUTHORIZED, Json(json!({"error": "missing token"}))).into_response(),
    };
    if !state.registry.check_token(&token) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid token"}))).into_response();
    }

    match get_or_spawn_adapter(&state, &agent_id).await {
        Ok(adapter) => match adapter.card().await {
            Ok(card) => Json(card).into_response(),
            Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response(),
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
        None => return rpc_error(request.id, StatusCode::UNAUTHORIZED, -32000, "missing token"),
    };
    if !state.registry.check_token(&token) {
        return rpc_error(request.id, StatusCode::UNAUTHORIZED, -32000, "invalid token");
    }

    let adapter = match get_or_spawn_adapter(&state, &agent_id).await {
        Ok(a) => a,
        Err(e) => return rpc_error(request.id, e.status(), e.rpc_code(), &e.to_string()),
    };

    // ИСПРАВЛЕНО (аудит P1-1): владелец разговора выводится из токена
    // клиента, иначе адаптер не может отличить одного клиента от другого.
    let owner = Owner::from_token(&token);

    let result = dispatch_a2a_method(&adapter, owner, &request).await;
    match result {
        Ok(value) => Json(json!({ "jsonrpc": "2.0", "id": request.id, "result": value })).into_response(),
        // ДОБАВЛЕНО (аудит P2-10): потеря контекста — не «что-то пошло
        // не так». Клиент должен отличить её от прочих ошибок, чтобы
        // начать разговор заново, а не молча продолжать в пустоту.
        Err(e) if e.downcast_ref::<ContextLost>().is_some() => {
            rpc_error(request.id, StatusCode::CONFLICT, CONTEXT_LOST_CODE, &e.to_string())
        }
        Err(e) => rpc_error(request.id, StatusCode::OK, -32000, &e.to_string()),
    }
}

/// Код ошибки для потерянного контекста. Диапазон -32000..-32099
/// отведён JSON-RPC под ошибки приложения.
const CONTEXT_LOST_CODE: i64 = -32010;

fn rpc_error(id: Value, status: StatusCode, code: i64, message: &str) -> axum::response::Response {
    (status, Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))).into_response()
}

async fn dispatch_a2a_method(
    adapter: &Arc<AcpAsA2a<SupervisedStdioAgent>>,
    owner: Owner,
    request: &JsonRpcRequest,
) -> anyhow::Result<Value> {
    match request.method.as_str() {
        "message/send" => {
            let task: Task = build_task_from_send_params(&request.params)?;
            match adapter.send_task_as(owner, task).await? {
                gateway_core::Reply::Complete(t) => Ok(serde_json::to_value(t)?),
                gateway_core::Reply::Streaming(_) => {
                    anyhow::bail!("Фаза 1: streaming не реализован для A2A->ACP направления")
                }
            }
        }
        "tasks/get" => {
            let id = request.params.get("id").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/get: id обязателен"))?;
            let task = adapter.get_task_as(owner, TaskId(id.to_string())).await?;
            Ok(serde_json::to_value(task)?)
        }
        "tasks/cancel" => {
            let id = request.params.get("id").and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tasks/cancel: id обязателен"))?;
            let task = adapter.cancel_task_as(owner, TaskId(id.to_string())).await?;
            Ok(serde_json::to_value(task)?)
        }
        other => anyhow::bail!("method_not_found: {other}"),
    }
}

fn build_task_from_send_params(params: &Value) -> anyhow::Result<Task> {
    let message_value = params.get("message")
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
