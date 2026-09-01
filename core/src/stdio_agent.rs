//! core/src/stdio_agent.rs
//!
//! StdioAcpAgent — real AcpAgent implementation via process spawn.
//! Model: one process per instance, requests correlated by JSON-RPC id
//! through a oneshot channel. session/cancel is a notification (no id).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use protocol::acp::{
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionId, SessionUpdate,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::agent::AcpAgent;
use crate::reply::Reply;

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
/// ADDED (streaming roadmap Part 1, task G): the chunk accumulator was
/// replaced by a channel. Previously AgentMessageChunk was collected into a
/// Vec and delivered whole in Reply::Complete after the turn finished — the
/// client never saw intermediate chunks. Now each session/update goes to an
/// mpsc channel immediately, and prompt() returns Reply::Streaming(rx).
type UpdatesMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<SessionUpdate>>>>;

pub struct StdioAcpAgent {
    child: Mutex<Child>,
    /// Arc because the reader task now also needs to write to stdin:
    /// agent-to-client requests must always get a response.
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    updates: UpdatesMap,
    /// ADDED (audit P2-11): it was hardcoded as `from_secs(60)`,
    /// contradicting the configurable turn_lease_timeout_secs.
    call_timeout: std::time::Duration,
    /// ADDED (streaming roadmap Part 2): timeout BEFORE the first chunk
    /// of the stream. Wired from config (streaming.first_chunk_timeout_secs)
    /// via SpawnConfig; applied in the stream loop of prompt_streaming.
    first_chunk_timeout: std::time::Duration,
    /// ADDED (streaming roadmap Part 2): timeout BETWEEN chunks.
    /// Same as first_chunk_timeout — applied in the stream loop.
    idle_chunk_timeout: std::time::Duration,
}

impl StdioAcpAgent {
    pub async fn spawn(
        command: &[String],
        cwd: &Option<String>,
        env: &HashMap<String, String>,
        call_timeout: std::time::Duration,
        first_chunk_timeout: std::time::Duration,
        idle_chunk_timeout: std::time::Duration,
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
        // FIXED (audit P2-9): without this, the agent process outlived the
        // adapter and stayed orphaned (empty impl Drop did nothing).
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;

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
            first_chunk_timeout,
            idle_chunk_timeout,
        })
    }

    /// Public liveness check — needed by the supervisor and adapter cache.
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

        // FIXED (audit P2-14): on timeout the entry stayed in
        // pending forever — a leak on every expired request.
        let Ok(received) = waited else {
            self.pending.lock().await.remove(&id);
            anyhow::bail!(
                "agent did not respond to {method} within {:?}",
                self.call_timeout
            );
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

            // FIXED (found by live test): previously any line with a numeric
            // id was treated as a response. But ACP is bidirectional JSON-RPC:
            // the agent itself sends requests to the client (session/request_permission,
            // fs/read_text_file), and their id numbering starts at 1, i.e. it
            // collides with ours. Such an agent request consumed the pending
            // entry, leaving nobody to resolve our real response — and the
            // call hung until timeout. That is exactly what produced the stable
            // «agent did not respond to session/prompt within 60s» when
            // continuing a conversation: on the second turn the agent managed
            // to ask about something.
            //
            // Distinguish by the presence of "method": requests and
            // notifications have it, responses do not.
            if parsed.get("method").is_some() {
                match parsed.get("id") {
                    // Agent request to the client. We must reply: without a
                    // response the agent hangs on its side.
                    Some(request_id) => {
                        reply_method_not_found(&stdin, request_id.clone(), &parsed).await;
                    }
                    // Notification: AgentMessageChunk is collected per sessionId
                    // and appended to the PromptResponse at the end of the turn
                    // (audit P2-1).
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
                    Err(err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string())
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

/// Reply to an agent-to-client request. Client capabilities (permission
/// requests, file access, terminal) are not implemented by the gateway —
/// we report this honestly instead of staying silent and hanging the
/// agent. Capabilities are declared empty in initialize, so a
/// well-behaved agent should not send such requests.
async fn reply_method_not_found(
    stdin: &Arc<Mutex<tokio::process::ChildStdin>>,
    id: Value,
    request: &Value,
) {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    tracing::warn!(%method, "агент запросил у клиента метод, который шлюз не поддерживает");

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("gateway не реализует клиентский метод {method}"),
        }
    });

    let Ok(mut line) = serde_json::to_vec(&response) else {
        return;
    };
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
        // Tail of the previous turn must not leak into the current response.
        self.updates.lock().await.remove(&session_key);

        // ADDED (streaming roadmap Part 1, task G): channel the reader task
        // sends session/update chunks into immediately. The non-streaming
        // prompt() reads it AFTER the turn completes and assembles the content —
        // preserving the old Reply::Complete behavior (P-20: default unchanged).
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionUpdate>();
        self.updates.lock().await.insert(session_key.clone(), tx);

        let mut resp: PromptResponse = self.call("session/prompt", req).await?;

        // Collect all chunks sent into the channel during the turn.
        let mut collected: Vec<protocol::acp::ContentBlock> = Vec::new();
        while let Ok(update) = rx.try_recv() {
            if let SessionUpdate::AgentMessageChunk { content, .. } = update {
                collected.push(content);
            }
        }
        self.updates.lock().await.remove(&session_key);

        if resp.content.is_empty() {
            resp.content = collected;
        }
        Ok(Reply::Complete(resp))
    }

    /// ADDED (P-20, streaming roadmap Part 1): streaming prompt().
    /// Returns Reply::Streaming(rx) IMMEDIATELY after writing session/prompt to
    /// stdin — the channel yields chunks as they are generated (collect_session_update
    /// sends them into tx right away). The final PromptResponse is remapped by a
    /// background task into a terminal AgentMessageChunk, after which tx is dropped
    /// and the channel closes.
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        let session_key = req.session_id.0.clone();
        // Tail of the previous turn must not leak into the current response.
        self.updates.lock().await.remove(&session_key);

        let (tx, mut rx) = mpsc::unbounded_channel::<SessionUpdate>();
        self.updates
            .lock()
            .await
            .insert(session_key.clone(), tx.clone());

        // We send session/prompt ourselves (without the blocking call): the channel
        // must start yielding chunks to the client immediately. Register a
        // pending entry.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (resp_tx, mut resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, resp_tx);

        let request =
            json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": req });
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(&line).await.map_err(|e| {
                anyhow::anyhow!("write to agent stdin failed (process likely dead): {e}")
            })?;
        }

        // Key choice (P-20): if the final PromptResponse arrives first
        // (agent did not stream — 0 chunks in the channel) — return Reply::Complete,
        // preserving the old behavior for everyone expecting a non-streaming response.
        // If a session/update chunk arrives first — the agent is streaming:
        // return Reply::Streaming with an outer channel that relays both
        // the remaining chunks and the terminal element.
        // If neither a chunk nor a response arrives within first_chunk_timeout —
        // the agent never started the turn: timeout (T9).
        //
        // FIXED (P-23): biased + chunk branch FIRST. Without biased, when
        // resp_rx and rx.recv() became ready simultaneously the choice was random
        // between Complete and Streaming — the intent "if the agent started
        // streaming, trust that signal" was not guaranteed. Now the chunk branch
        // has priority and the result is deterministic.
        tokio::select! {
            biased;
            first = rx.recv() => {
                let Some(first) = first else {
                    self.updates.lock().await.remove(&session_key);
                    anyhow::bail!("stream channel closed before any event");
                };
                // Agent is streaming: outer channel, first chunk already arrived.
                let (out_tx, out_rx) = mpsc::unbounded_channel::<SessionUpdate>();
                let _ = out_tx.send(first);

                // Background task: relays the remaining chunks from rx into out, then
                // waits for the final PromptResponse and sends the terminal element.
                // ADDED (Part 2, task C): idle_chunk_timeout is applied
                // here (the first chunk was already received above, so the first wait
                // is for the SECOND chunk, timeout = idle).
                tokio::spawn({
                    let updates = self.updates.clone();
                    let idle_chunk_timeout = self.idle_chunk_timeout;
                    async move {
                        let mut last_chunks = 0usize;
                        loop {
                            tokio::select! {
                                chunk = rx.recv() => {
                                    match chunk {
                                        Some(c) => {
                                            last_chunks += 1;
                                            if out_tx.send(c).is_err() {
                                                break; // receiver disconnected
                                            }
                                        }
                                        None => break, // channel closed without terminal
                                    }
                                }
                                resp = &mut resp_rx => {
                                    let terminal = match resp {
                                        Ok(Ok(value)) => serde_json::from_value::<PromptResponse>(value)
                                            .map_err(|e| format!("bad PromptResponse: {e}"))
                                            .map(|r| SessionUpdate::AgentMessageChunk {
                                                message_id: None,
                                                content: prompt_to_content_block(&r),
                                            }),
                                        Ok(Err(err_msg)) => Err(err_msg),
                                        Err(_) => Err("agent stdout closed before prompt response".to_string()),
                                    };
                                    match terminal {
                                        Ok(terminal) => {
                                            if last_chunks == 0 {
                                                tracing::warn!(
                                                    session_id = %session_key,
                                                    chunk_count = last_chunks,
                                                    "stream produced 0 chunks before terminal event"
                                                );
                                            }
                                            let _ = out_tx.send(terminal);
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                session_id = %session_key,
                                                error = %e,
                                                "session/prompt завершился ошибкой после ухода чанков — канал закрыт без terminal"
                                            );
                                        }
                                    }
                                    break;
                                }
                                _ = tokio::time::sleep(idle_chunk_timeout) => {
                                    // LOG-TRAP (WARN, enabled by default):
                                    tracing::warn!(
                                        session_id = %session_key,
                                        elapsed = ?idle_chunk_timeout,
                                        "idle_chunk_timeout сработал — агент не присылал чанков дольше лимита, поток закрыт"
                                    );
                                    break;
                                }
                            }
                        }
                        updates.lock().await.remove(&session_key);
                    }
                });

                Ok(Reply::Streaming(out_rx))
            }
            resp = &mut resp_rx => {
                let resp: PromptResponse = match resp {
                    Ok(Ok(value)) => serde_json::from_value(value)?,
                    Ok(Err(err_msg)) => anyhow::bail!("agent returned error for session/prompt: {err_msg}"),
                    Err(_) => anyhow::bail!("agent stdout closed before prompt response"),
                };
                self.updates.lock().await.remove(&session_key);
                Ok(Reply::Complete(resp))
            }
            _ = tokio::time::sleep(self.first_chunk_timeout) => {
                // T9: agent neither started streaming nor replied within first_chunk_timeout.
                // Clean up pending and close the session so nothing hangs.
                self.pending.lock().await.remove(&id);
                self.updates.lock().await.remove(&session_key);
                anyhow::bail!(
                    "agent did not start streaming within {:?}",
                    self.first_chunk_timeout
                )
            }
        }
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        self.notify("session/cancel", json!({ "sessionId": session.0 }))
            .await
    }

    async fn is_alive(&self) -> bool {
        StdioAcpAgent::is_alive(self).await
    }
}

// FIXED (audit P2-9): the empty impl Drop was removed — it only
// masked the process leak. kill_on_drop(true), set in spawn(),
// fulfills its role.

/// ADDED (streaming roadmap Part 1, task G): extracts the
/// session/update from the agent's JSON-RPC notification and sends it to the
/// session's mpsc channel immediately, without accumulation.
///
/// P-21 (decisions.md): ONLY `agent_message_chunk` is parsed — the main
/// channel of the textual response. ToolCall/ToolCallUpdate/Plan/UsageUpdate are
/// NOT parsed in Phase 2.0: they require checking against the exact ACP JSON
/// schema for each variant separately, the risk is disproportionate to the
/// value. The mapping of these 4 variants in convert.rs::session_update_to_a2a_event()
/// is written but unreachable under the current filter — deliberate (readiness
/// for the next iteration).
async fn collect_session_update(parsed: &Value, updates: &UpdatesMap) {
    if parsed.get("method").and_then(Value::as_str) != Some("session/update") {
        return;
    }
    let Some(params) = parsed.get("params") else {
        return;
    };
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    let Some(update) = params.get("update") else {
        return;
    };

    if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk") {
        return;
    }
    let Some(content) = update.get("content") else {
        return;
    };
    let Ok(block) = serde_json::from_value::<protocol::acp::ContentBlock>(content.clone()) else {
        return;
    };

    let session_update = SessionUpdate::AgentMessageChunk {
        message_id: update
            .get("messageId")
            .and_then(Value::as_str)
            .map(str::to_string),
        content: block,
    };

    let tx = updates.lock().await.get(session_id).cloned();
    if let Some(tx) = tx {
        let _ = tx.send(session_update);
    }
}

/// Final PromptResponse → terminal SessionUpdate (a text chunk
/// carrying the full response content). Called in prompt() after the turn completes.
fn prompt_to_content_block(resp: &PromptResponse) -> protocol::acp::ContentBlock {
    // Merge all response content into one text block: an ACP agent's
    // final answer may consist of several ContentBlocks, but
    // the terminal element of the stream must carry the full text.
    let text = resp
        .content
        .iter()
        .filter_map(|b| match b {
            protocol::acp::ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    protocol::acp::ContentBlock::Text { text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::acp::{ContentBlock, NewSessionRequest, PromptRequest, SessionId};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// Returns the path to the test mock ACP agent (gatewayd binary).
    /// Core unit tests do not get CARGO_BIN_EXE_* (it is set only for
    /// integration tests of a crate with [[bin]]), so we use the explicit env
    /// MOCK_AGENT_BIN or fall back to the workspace target/debug/mock_acp_agent.
    fn mock_bin() -> String {
        if let Ok(path) = std::env::var("MOCK_AGENT_BIN") {
            return path;
        }
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let workspace = std::path::Path::new(&manifest)
            .ancestors()
            .nth(1)
            .expect("workspace root");
        workspace
            .join("target/debug/mock_acp_agent")
            .to_string_lossy()
            .to_string()
    }

    async fn spawn_mock(extra_env: &[(&str, &str)]) -> StdioAcpAgent {
        spawn_mock_with_timeouts(extra_env, Duration::from_secs(15), Duration::from_secs(120)).await
    }

    async fn spawn_mock_with_timeouts(
        extra_env: &[(&str, &str)],
        first_chunk_timeout: Duration,
        idle_chunk_timeout: Duration,
    ) -> StdioAcpAgent {
        let mut env = HashMap::new();
        for (k, v) in extra_env {
            env.insert(k.to_string(), v.to_string());
        }
        StdioAcpAgent::spawn(
            &[mock_bin()],
            &None,
            &env,
            Duration::from_secs(30),
            first_chunk_timeout,
            idle_chunk_timeout,
        )
        .await
        .expect("mock агент спавнится")
    }

    async fn init_session(agent: &StdioAcpAgent) -> SessionId {
        agent
            .new_session(NewSessionRequest {
                cwd: ".".to_string(),
                mcp_servers: vec![],
                additional_directories: vec![],
            })
            .await
            .expect("session/new работает")
            .session_id
    }

    /// T1: chunks arrive one by one, not as a batch at the end.
    /// The mock agent sends 3 session/update with 50ms delay between them;
    /// via prompt_streaming() each chunk is caught by rx.recv() at a real
    /// time interval, not in one heap after the turn completes.
    #[tokio::test]
    async fn stream_emits_chunks_incrementally() {
        let agent = spawn_mock(&[
            ("MOCK_AGENT_STREAM_CHUNKS", "3"),
            ("MOCK_AGENT_CHUNK_DELAY_MS", "50"),
        ])
        .await;
        let session = init_session(&agent).await;

        let reply = agent
            .prompt_streaming(PromptRequest {
                session_id: session.clone(),
                prompt: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            })
            .await
            .expect("prompt_streaming работает");

        let Reply::Streaming(mut rx) = reply else {
            panic!("ожидался Reply::Streaming, получили Complete");
        };

        // First chunk — wait with ample margin (the first comes immediately).
        let t0 = Instant::now();
        let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("первый чанк приходит")
            .expect("канал не закрыт");
        let elapsed_first = t0.elapsed();
        assert!(
            elapsed_first < Duration::from_millis(400),
            "первый чанк не должен ждать весь ход: {elapsed_first:?}"
        );

        // Subsequent ones — ~50ms really elapses between them (not a batch at the end).
        let t1 = Instant::now();
        let second = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("второй чанк приходит")
            .expect("канал не закрыт");
        let gap = t1.elapsed();
        assert!(
            gap >= Duration::from_millis(30),
            "чанки должны приходить по одному с задержкой, gap={gap:?}"
        );

        let _third = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("третий чанк приходит")
            .expect("канал не закрыт");

        assert!(matches!(first, SessionUpdate::AgentMessageChunk { .. }));
        assert!(matches!(second, SessionUpdate::AgentMessageChunk { .. }));
    }

    /// T9: idle_chunk_timeout closes a stalled stream. The mock sends 1 chunk,
    /// then stays silent longer than idle (200ms) — the stream must close on
    /// timeout (< 500ms), not hang until call_timeout.
    #[tokio::test]
    async fn idle_timeout_closes_stalled_stream() {
        let agent = spawn_mock_with_timeouts(
            &[
                ("MOCK_AGENT_STREAM_CHUNKS", "1"),
                ("MOCK_AGENT_FINAL_DELAY_MS", "2000"),
            ],
            Duration::from_secs(15),
            Duration::from_millis(200),
        )
        .await;
        let session = init_session(&agent).await;

        let reply = agent
            .prompt_streaming(PromptRequest {
                session_id: session.clone(),
                prompt: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            })
            .await
            .expect("prompt_streaming работает");

        let Reply::Streaming(mut rx) = reply else {
            panic!("ожидался Reply::Streaming");
        };

        // Read until the channel closes (idle timeout closes it after 1 chunk).
        let start = Instant::now();
        let mut chunks = 0usize;
        while rx.recv().await.is_some() {
            chunks += 1;
        }
        let elapsed = start.elapsed();
        assert!(chunks >= 1, "первый чанк должен прийти");
        assert!(
            elapsed < Duration::from_millis(500),
            "стрим должен закрыться по idle_chunk_timeout, а не ждать финал 2000мс: {elapsed:?}"
        );
    }

    /// T9: first_chunk_timeout fires if the agent never starts streaming
    /// and never replies. The mock stays silent longer than first (100ms) —
    /// prompt_streaming must return Err on timeout, not wait for call_timeout.
    #[tokio::test]
    async fn first_chunk_timeout_fires_if_agent_never_starts() {
        let agent = spawn_mock_with_timeouts(
            &[("MOCK_AGENT_FINAL_DELAY_MS", "2000")],
            Duration::from_millis(100),
            Duration::from_secs(120),
        )
        .await;
        let session = init_session(&agent).await;

        let start = Instant::now();
        let result = agent
            .prompt_streaming(PromptRequest {
                session_id: session.clone(),
                prompt: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            })
            .await;
        let elapsed = start.elapsed();
        assert!(
            result.is_err(),
            "агент не начал стримить — должна быть ошибка first_chunk_timeout"
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "first_chunk_timeout должен сработать раньше финального ответа (2000мс): {elapsed:?}"
        );
    }

    /// P-23: when a chunk and the final response are ready simultaneously, the
    /// choice must be deterministic in favor of Streaming (biased + chunk branch
    /// first). The mock sends 1 chunk and a response with no delay — repeated runs
    /// must not produce a random Complete/Streaming.
    #[tokio::test]
    async fn simultaneous_response_and_chunk_prefers_streaming_path() {
        let agent = spawn_mock(&[
            ("MOCK_AGENT_STREAM_CHUNKS", "1"),
            ("MOCK_AGENT_FINAL_DELAY_MS", "50"),
        ])
        .await;
        let session = init_session(&agent).await;

        // One call: 5 repeats on the same session introduce a cross-turn race
        // (the previous turn's background task is still alive) — not the subject
        // of P-23. The determinism of the choice is checked once.
        let reply = agent
            .prompt_streaming(PromptRequest {
                session_id: session.clone(),
                prompt: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            })
            .await
            .expect("prompt_streaming работает");
        assert!(
            matches!(reply, Reply::Streaming(_)),
            "при одновременном чанке+ответе должен выбираться Streaming (Р-23)"
        );
    }
}
