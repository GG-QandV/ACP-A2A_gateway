//! core/src/stdio_agent.rs
//!
//! StdioAcpAgent — реальная реализация AcpAgent через spawn процесса.
//! Модель: один процесс на инстанс, запросы коррелируются по JSON-RPC id
//! через oneshot-канал. session/cancel — notification (без id).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use protocol::acp::{
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionUpdate,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

use crate::agent::AcpAgent;
use crate::reply::Reply;

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
/// ДОБАВЛЕНО (аудит P2-1): накопитель AgentMessageChunk по sessionId.
/// Раньше reader-таск отбрасывал все нотификации агента (`continue`),
/// то есть весь текст ответа терялся.
type UpdatesMap = Arc<Mutex<HashMap<String, Vec<protocol::acp::ContentBlock>>>>;

pub struct StdioAcpAgent {
    child: Mutex<Child>,
    /// Arc, потому что писать в stdin теперь должен и reader-таск:
    /// на запросы агента к клиенту обязан приходить ответ.
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    updates: UpdatesMap,
    /// ДОБАВЛЕНО (аудит P2-11): было захардкожено `from_secs(60)`,
    /// что противоречило настраиваемому turn_lease_timeout_secs.
    call_timeout: std::time::Duration,
}

impl StdioAcpAgent {
    pub async fn spawn(
        command: &[String],
        cwd: &Option<String>,
        env: &HashMap<String, String>,
        call_timeout: std::time::Duration,
    ) -> anyhow::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty command"))?;

        let mut cmd = Command::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.envs(env);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());
        // ИСПРАВЛЕНО (аудит P2-9): без этого процесс агента переживал
        // адаптер и оставался сиротой (пустой impl Drop ничего не делал).
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let updates: UpdatesMap = Arc::new(Mutex::new(HashMap::new()));
        spawn_reader_task(
            BufReader::new(stdout),
            pending.clone(),
            updates.clone(),
            stdin.clone(),
        );

        Ok(Self {
            child: Mutex::new(child),
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            updates,
            call_timeout,
        })
    }

    /// Публичная проверка живости — нужна супервизору и кэшу адаптеров.
    pub async fn is_alive(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    async fn ensure_alive(&self) -> anyhow::Result<()> {
        let mut child = self.child.lock().await;
        match child.try_wait()? {
            Some(status) => anyhow::bail!("agent process exited (status={status:?})"),
            None => Ok(()),
        }
    }

    async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        self.ensure_alive().await?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&line).await.map_err(|e| {
                anyhow::anyhow!("write to agent stdin failed (process likely dead): {e}")
            })?;
        }

        let waited = tokio::time::timeout(self.call_timeout, rx).await;

        // ИСПРАВЛЕНО (аудит P2-14): при таймауте запись оставалась в
        // pending навсегда — утечка на каждый истёкший запрос.
        let Ok(received) = waited else {
            self.pending.lock().await.remove(&id);
            anyhow::bail!("agent did not respond to {method} within {:?}", self.call_timeout);
        };

        let result = received
            .map_err(|_| anyhow::anyhow!("agent reader task dropped (process likely dead)"))?;

        match result {
            Ok(value) => Ok(serde_json::from_value(value)?),
            Err(err_msg) => anyhow::bail!("agent returned error for {method}: {err_msg}"),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        self.ensure_alive().await?;
        let notification = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut line = serde_json::to_vec(&notification)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        Ok(())
    }
}

fn spawn_reader_task(
    mut reader: BufReader<tokio::process::ChildStdout>,
    pending: PendingMap,
    updates: UpdatesMap,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
) {
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }

            let parsed: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // ИСПРАВЛЕНО (найдено live-тестом): раньше ответом считалась
            // ЛЮБАЯ строка с числовым id. Но ACP — двусторонний JSON-RPC:
            // агент сам шлёт клиенту запросы (session/request_permission,
            // fs/read_text_file), и нумерация их id начинается с 1, то есть
            // сталкивается с нашей. Такой запрос агента съедал запись из
            // pending, наш настоящий ответ разрешать было уже некому — и
            // вызов висел до таймаута. Именно это давало стабильный
            // «agent did not respond to session/prompt within 60s» при
            // продолжении разговора: на втором ходу агент успевал о
            // чём-нибудь спросить.
            //
            // Различаем по наличию "method": оно есть у запросов и
            // нотификаций и отсутствует у ответов.
            if parsed.get("method").is_some() {
                match parsed.get("id") {
                    // Запрос агента к клиенту. Отвечать обязаны: без
                    // ответа агент виснет на своей стороне.
                    Some(request_id) => {
                        reply_method_not_found(&stdin, request_id.clone(), &parsed).await;
                    }
                    // Нотификация: AgentMessageChunk копится по sessionId
                    // и прикладывается к PromptResponse в конце хода
                    // (аудит P2-1).
                    None => collect_session_update(&parsed, &updates).await,
                }
                continue;
            }

            let Some(id) = parsed.get("id").and_then(Value::as_u64) else {
                continue;
            };

            let sender = pending.lock().await.remove(&id);
            if let Some(tx) = sender {
                let outcome = if let Some(err) = parsed.get("error") {
                    Err(err.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string())
                } else {
                    Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(outcome);
            }
        }

        let mut pending = pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err("agent stdout closed (process exited)".to_string()));
        }
    });
}


/// Ответ на запрос агента к клиенту. Клиентские возможности (запрос
/// разрешений, доступ к файлам, терминал) шлюзом не реализованы —
/// сообщаем об этом честно, вместо того чтобы молчать и подвешивать
/// агента. Возможности заявлены пустыми в initialize, так что
/// корректный агент такие запросы слать не должен.
async fn reply_method_not_found(
    stdin: &Arc<Mutex<tokio::process::ChildStdin>>,
    id: Value,
    request: &Value,
) {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("<unknown>");
    tracing::warn!(%method, "агент запросил у клиента метод, который шлюз не поддерживает");

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("gateway не реализует клиентский метод {method}"),
        }
    });

    let Ok(mut line) = serde_json::to_vec(&response) else { return };
    line.push(b'\n');
    let _ = stdin.lock().await.write_all(&line).await;
}

#[async_trait]
impl AcpAgent for StdioAcpAgent {
    async fn initialize(&self, req: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        self.call("initialize", req).await
    }

    async fn new_session(&self, req: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
        self.call("session/new", req).await
    }

    async fn prompt(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        let session_key = req.session_id.0.clone();
        // Хвост предыдущего хода не должен попасть в текущий ответ.
        self.updates.lock().await.remove(&session_key);

        let mut resp: PromptResponse = self.call("session/prompt", req).await?;

        let collected = self.updates.lock().await.remove(&session_key).unwrap_or_default();
        if resp.content.is_empty() {
            resp.content = collected;
        }
        Ok(Reply::Complete(resp))
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        self.notify("session/cancel", json!({ "sessionId": session.0 })).await
    }

    async fn is_alive(&self) -> bool {
        StdioAcpAgent::is_alive(self).await
    }
}

// ИСПРАВЛЕНО (аудит P2-9): пустой impl Drop удалён — он только
// маскировал утечку процессов. Его роль выполняет kill_on_drop(true),
// выставленный в spawn().

/// Извлекает контентный чанк из session/update и складывает в накопитель.
/// Служебные апдейты (tool_call, plan, usage) и мысли агента в ответ
/// не попадают — это не текст ответа модели.
async fn collect_session_update(parsed: &Value, updates: &UpdatesMap) {
    if parsed.get("method").and_then(Value::as_str) != Some("session/update") {
        return;
    }
    let Some(params) = parsed.get("params") else { return };
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else { return };
    let Some(update) = params.get("update") else { return };

    if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk") {
        return;
    }
    let Some(content) = update.get("content") else { return };
    let Ok(block) = serde_json::from_value::<protocol::acp::ContentBlock>(content.clone()) else {
        return;
    };

    updates.lock().await.entry(session_id.to_string()).or_default().push(block);
}
