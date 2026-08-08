//! gatewayd/src/transport_tcp.rs — направления 1 (ACP<->ACP passthrough)
//! и 3 (ACP-клиент -> A2A-агент через конвертер).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use core::{A2aAsAcp, AcpAgent, HttpA2aAgent};
use protocol::acp::{InitializeRequest, NewSessionRequest, PromptRequest, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use crate::registry::{Registry, Transport};

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
    reader.read_line(&mut handshake_line).await?;
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
            handle_http_target(reader, write_half, url, push_token, task_store_dir, lease_timeout).await
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

    let mut child = cmd.spawn()?;
    let mut child_stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    let child_stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let mut child_stdout_reader = BufReader::new(child_stdout);

    let socket_to_child = async {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
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
            let n = child_stdout_reader.read_line(&mut line).await?;
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

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Value,
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

async fn handle_http_target(
    mut reader: BufReader<tokio::net::tcp::ReadHalf<'_>>,
    mut writer: tokio::net::tcp::WriteHalf<'_>,
    url: String,
    push_token: Option<String>,
    _task_store_dir: PathBuf,
    lease_timeout: Duration,
) -> anyhow::Result<()> {
    let http_agent = HttpA2aAgent::new(url, push_token);
    let adapter = A2aAsAcp::new(http_agent, lease_timeout);

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
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
        match response {
            Ok(result) => write_ok(&mut writer, request.id, result).await?,
            Err(e) => write_error(&mut writer, request.id, -32000, &e.to_string()).await?,
        }
    }

    Ok(())
}

async fn dispatch_acp_method(
    adapter: &A2aAsAcp<HttpA2aAgent>,
    request: &JsonRpcRequest,
) -> anyhow::Result<Value> {
    match request.method.as_str() {
        "initialize" => {
            let req: InitializeRequest = serde_json::from_value(request.params.clone())?;
            let resp = adapter.initialize(req).await?;
            Ok(serde_json::to_value(resp)?)
        }
        "session/new" => {
            let req: NewSessionRequest = serde_json::from_value(request.params.clone())?;
            let resp = adapter.new_session(req).await?;
            Ok(serde_json::to_value(resp)?)
        }
        "session/prompt" => {
            let req: PromptRequest = serde_json::from_value(request.params.clone())?;
            match adapter.prompt(req).await? {
                core::Reply::Complete(resp) => Ok(serde_json::to_value(resp)?),
                core::Reply::Streaming(_) => {
                    anyhow::bail!("Фаза 1: стриминг для ACP->A2A направления не реализован")
                }
            }
        }
        "session/cancel" => {
            let session_id_raw = request
                .params
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("session/cancel: sessionId обязателен"))?;
            adapter.cancel(SessionId(session_id_raw.to_string())).await?;
            Ok(json!({}))
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
