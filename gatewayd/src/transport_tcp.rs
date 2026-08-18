//! gatewayd/src/transport_tcp.rs
//! Направления 1 (ACP<->ACP passthrough) и 3 (ACP-клиент -> A2A-агент).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gateway_core::{A2aAsAcp, AcpAgent, HttpA2aAgent};
use protocol::acp::{InitializeRequest, NewSessionRequest, PromptRequest, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use crate::registry::{Registry, Transport};

/// ДОБАВЛЕНО (аудит P1-5): read_line читал строку неограниченной длины —
/// один клиент мог выесть память процесса одной строкой без \n.
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
/// ДОБАВЛЕНО (аудит P1-5): молчащее соединение висело вечно (slowloris).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// read_line с потолком. Семантика возврата та же: Ok(0) на EOF.
async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut String,
) -> anyhow::Result<usize> {
    let n = {
        let mut limited = reader.take(MAX_LINE_BYTES);
        limited.read_line(line).await?
    };
    if n as u64 >= MAX_LINE_BYTES {
        anyhow::bail!("line exceeds {MAX_LINE_BYTES} bytes limit");
    }
    Ok(n)
}

#[derive(Debug, Deserialize)]
struct Handshake {
    token: String,
    agent_id: String,
}

pub async fn serve(
    listen_addr: &str,
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    tracing::info!(%listen_addr, "tcp transport listening");

    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let registry = registry.clone();
        let task_store_dir = task_store_dir.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, registry, task_store_dir, lease_timeout).await {
                tracing::warn!(%peer_addr, error = %e, "connection closed with error");
            }
        });
    }
}

async fn handle_connection(
    mut socket: TcpStream,
    registry: Arc<Registry>,
    task_store_dir: PathBuf,
    lease_timeout: Duration,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = socket.split();
    let mut reader = BufReader::new(read_half);

    let mut handshake_line = String::new();
    tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line_limited(&mut reader, &mut handshake_line))
        .await
        .map_err(|_| anyhow::anyhow!("handshake timeout"))??;
    let handshake: Handshake = serde_json::from_str(handshake_line.trim())
        .map_err(|e| anyhow::anyhow!("invalid handshake: {e}"))?;

    if !registry.check_token(&handshake.token) {
        write_half.write_all(b"{\"error\":\"invalid token\"}\n").await?;
        anyhow::bail!("token rejected for agent_id={}", handshake.agent_id);
    }

    let entry = registry
        .lookup(&handshake.agent_id)
        .ok_or_else(|| anyhow::anyhow!("unknown agent_id: {}", handshake.agent_id))?
        .clone();

    match entry.transport {
        Transport::Stdio { command, cwd, env } => {
            handle_stdio_passthrough(reader, write_half, &command, &cwd, &env).await
        }
        Transport::Http { url, push_token } => {
            handle_http_target(
                reader,
                write_half,
                HttpTargetParams {
                    url,
                    push_token,
                    _task_store_dir: task_store_dir,
                    lease_timeout,
                    registry,
                    agent_id: handshake.agent_id,
                },
            )
            .await
        }
    }
}

async fn handle_stdio_passthrough(
    mut reader: BufReader<tokio::net::tcp::ReadHalf<'_>>,
    mut writer: tokio::net::tcp::WriteHalf<'_>,
    command: &[String],
    cwd: &Option<String>,
    env: &HashMap<String, String>,
) -> anyhow::Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command in agent config"))?;

    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.envs(env);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    // ИСПРАВЛЕНО (аудит P2-9): без kill_on_drop процесс агента переживал
    // соединение и оставался сиротой при панике/отмене таска.
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let mut child_stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    let child_stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let mut child_stdout_reader = BufReader::new(child_stdout);

    let socket_to_child = async {
        let mut line = String::new();
        loop {
            line.clear();
            let n = read_line_limited(&mut reader, &mut line).await?;
            if n == 0 {
                break;
            }
            child_stdin.write_all(line.as_bytes()).await?;
        }
        anyhow::Ok(())
    };

    let child_to_socket = async {
        let mut line = String::new();
        loop {
            line.clear();
            let n = read_line_limited(&mut child_stdout_reader, &mut line).await?;
            if n == 0 {
                break;
            }
            writer.write_all(line.as_bytes()).await?;
        }
        anyhow::Ok(())
    };

    tokio::select! {
        res = socket_to_child => res?,
        res = child_to_socket => res?,
    }

    let _ = child.kill().await;
    Ok(())
}

/// ИСПРАВЛЕНО (аудит P2-3): поле id было обязательным, поэтому
/// JSON-RPC notification не парсился и клиент получал parse error.
/// В ACP session/cancel — именно notification (см. core/src/agent.rs).
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcOk<'a, R> {
    jsonrpc: &'a str,
    id: Value,
    result: R,
}

#[derive(Debug, Serialize)]
struct JsonRpcErr<'a> {
    jsonrpc: &'a str,
    id: Value,
    error: JsonRpcErrBody,
}

#[derive(Debug, Serialize)]
struct JsonRpcErrBody {
    code: i64,
    message: String,
}

/// Параметры TCP-обработчика A2A-агента (задача E): registry и agent_id
/// нужны для лимита параллельных стримов (try_acquire_stream) — отдельной
/// структурой, чтобы не раздувать сигнатуру до 8 аргументов.
struct HttpTargetParams {
    url: String,
    push_token: Option<String>,
    _task_store_dir: PathBuf,
    lease_timeout: Duration,
    registry: Arc<Registry>,
    agent_id: String,
}

async fn handle_http_target(
    mut reader: BufReader<tokio::net::tcp::ReadHalf<'_>>,
    mut writer: tokio::net::tcp::WriteHalf<'_>,
    params: HttpTargetParams,
) -> anyhow::Result<()> {
    let HttpTargetParams {
        url,
        push_token,
        _task_store_dir,
        lease_timeout,
        registry,
        agent_id,
    } = params;
    let http_agent = HttpA2aAgent::new(url, push_token);
    let adapter = A2aAsAcp::new(http_agent, lease_timeout);

    let mut line = String::new();
    loop {
        line.clear();
        let n = read_line_limited(&mut reader, &mut line).await?;
        if n == 0 {
            break;
        }

        let request: JsonRpcRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                write_error(&mut writer, Value::Null, -32700, &format!("parse error: {e}")).await?;
                continue;
            }
        };

        let response = dispatch_acp_method(&adapter, &request).await;

        // Notification (id отсутствует): по JSON-RPC ответ не отправляется.
        let Some(id) = request.id.clone() else {
            if let Err(e) = response {
                tracing::warn!(method = %request.method, error = %e, "notification failed");
            }
            continue;
        };

        match response {
            Ok(AcpDispatchResult::Json(result)) => write_ok(&mut writer, id, result).await?,
            // ДОБАВЛЕНО (задача E): поток SessionUpdate пишется в тот же
            // TCP-сокет построчно — каждая нотификация session/update,
            // до закрытия канала.
            // ДОБАВЛЕНО (Часть 2, задача A): лимит параллельных стримов —
            // permit живёт в scope до конца записи потока (RAII).
            Ok(AcpDispatchResult::Streaming(mut rx)) => {
                let _permit = match registry.try_acquire_stream(&agent_id) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(agent_id, error = %e, "TCP-стрим отклонён fail-closed");
                        write_error(&mut writer, id, -32000, &e.to_string()).await?;
                        continue;
                    }
                };
                let session_id = String::from("unknown");
                while let Some(update) = rx.recv().await {
                    let payload = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": update,
                        }
                    });
                    let mut bytes = serde_json::to_vec(&payload)?;
                    bytes.push(b'\n');
                    if let Err(e) = writer.write_all(&bytes).await {
                        // ЛОГ-ЛОВУШКА (ERROR, по умолчанию включена):
                        tracing::error!(
                            session_id = %session_id,
                            error = %e,
                            "не удалось записать session/update в TCP-сокет клиента — соединение будет закрыто"
                        );
                        return Err(e.into());
                    }
                }
            }
            Err(e) => write_error(&mut writer, id, -32000, &e.to_string()).await?,
        }
    }

    Ok(())
}

/// Результат ACP-диспетчера: синхронный JSON (Complete) либо поток
/// SessionUpdate (Streaming) — построчно пишется в TCP-сокет как
/// session/update-нотификации.
enum AcpDispatchResult {
    Json(Value),
    Streaming(tokio::sync::mpsc::UnboundedReceiver<protocol::acp::SessionUpdate>),
}

async fn dispatch_acp_method(
    adapter: &A2aAsAcp<HttpA2aAgent>,
    request: &JsonRpcRequest,
) -> anyhow::Result<AcpDispatchResult> {
    match request.method.as_str() {
        "initialize" => {
            let req: InitializeRequest = serde_json::from_value(request.params.clone())?;
            let resp = adapter.initialize(req).await?;
            Ok(AcpDispatchResult::Json(serde_json::to_value(resp)?))
        }
        "session/new" => {
            let req: NewSessionRequest = serde_json::from_value(request.params.clone())?;
            let resp = adapter.new_session(req).await?;
            Ok(AcpDispatchResult::Json(serde_json::to_value(resp)?))
        }
        "session/prompt" => {
            let req: PromptRequest = serde_json::from_value(request.params.clone())?;
            match adapter.prompt(req).await? {
                gateway_core::Reply::Complete(resp) => {
                    Ok(AcpDispatchResult::Json(serde_json::to_value(resp)?))
                }
                // ДОБАВЛЕНО (задача E): вместо заглушки — поток SessionUpdate,
                // который handle_http_target пишет построчно в TCP-сокет.
                gateway_core::Reply::Streaming(rx) => Ok(AcpDispatchResult::Streaming(rx)),
            }
        }
        "session/cancel" => {
            let session_id_raw = request
                .params
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("session/cancel: sessionId обязателен"))?;
            adapter.cancel(SessionId(session_id_raw.to_string())).await?;
            Ok(AcpDispatchResult::Json(json!({})))
        }
        other => anyhow::bail!("method_not_found: {other}"),
    }
}

async fn write_ok<W: AsyncWriteExt + Unpin>(writer: &mut W, id: Value, result: Value) -> anyhow::Result<()> {
    let payload = JsonRpcOk { jsonrpc: "2.0", id, result };
    let mut bytes = serde_json::to_vec(&payload)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}

async fn write_error<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    id: Value,
    code: i64,
    message: &str,
) -> anyhow::Result<()> {
    let payload = JsonRpcErr { jsonrpc: "2.0", id, error: JsonRpcErrBody { code, message: message.to_string() } };
    let mut bytes = serde_json::to_vec(&payload)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}
