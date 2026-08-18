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
/// ДОБАВЛЕНО (Часть 1 роадмапа стриминга, задача G): накопитель чанков
/// заменён на канал. Раньше AgentMessageChunk копился в Vec и отдавался
/// целиком в Reply::Complete после завершения хода — клиент не видел
/// промежуточных чанков. Теперь каждый session/update уходит в
/// mpsc-канал сразу, а prompt() возвращает Reply::Streaming(rx).
type UpdatesMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<SessionUpdate>>>>;

pub struct StdioAcpAgent {
    child: Mutex<Child>,
    /// Arc, потому что писать в stdin должен теперь и reader-таск:
    /// на запросы агента к клиенту обязан приходить ответ.
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    updates: UpdatesMap,
    /// ДОБАВЛЕНО (аудит P2-11): было захардкожено `from_secs(60)`,
    /// что противоречило настраиваемому turn_lease_timeout_secs.
    call_timeout: std::time::Duration,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут ДО первого чанка
    /// стрима. Прокидывается из конфига (streaming.first_chunk_timeout_secs)
    /// через SpawnConfig; применяется в стрим-цикле prompt_streaming.
    first_chunk_timeout: std::time::Duration,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут МЕЖДУ чанками.
    /// Аналогично first_chunk_timeout — применяется в стрим-цикле.
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
        // ИСПРАВЛЕНО (аудит P2-9): без этого процесс агента переживал
        // адаптер и оставался сиротой (пустой impl Drop ничего не делал).
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
        // Хвост предыдущего хода не должен попасть в текущий ответ.
        self.updates.lock().await.remove(&session_key);

        // ДОБАВЛЕНО (Часть 1 роадмапа стриминга, задача G): канал, в
        // который reader-таск шлёт session/update-чанки сразу. Нестриминговый
        // prompt() читает его ПОСЛЕ завершения хода и собирает контент —
        // сохраняя старое поведение Reply::Complete (Р-20: дефолт не меняется).
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionUpdate>();
        self.updates.lock().await.insert(session_key.clone(), tx);

        let mut resp: PromptResponse = self.call("session/prompt", req).await?;

        // Собрать все чанки, ушедшие в канал за время хода.
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

    /// ДОБАВЛЕНО (Р-20, Часть 1 роадмапа стриминга): потоковый prompt().
    /// Возвращает Reply::Streaming(rx) СРАЗУ после записи session/prompt в
    /// stdin — канал отдаёт чанки по мере генерации (collect_session_update
    /// шлёт их в tx сразу). Финальный PromptResponse домаппливается фоновым
    /// таском в терминальный AgentMessageChunk, после чего tx дропается и
    /// канал закрывается.
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        let session_key = req.session_id.0.clone();
        // Хвост предыдущего хода не должен попасть в текущий ответ.
        self.updates.lock().await.remove(&session_key);

        let (tx, mut rx) = mpsc::unbounded_channel::<SessionUpdate>();
        self.updates
            .lock()
            .await
            .insert(session_key.clone(), tx.clone());

        // Отправляем session/prompt сами (без блокирующего call): канал
        // должен начать отдавать чанки клиенту немедленно. Регистрируем
        // pending-запись.
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

        // Ключевой выбор (Р-20): если первым приходит финальный PromptResponse
        // (агент не стримил — 0 чанков в канале) — возвращаем Reply::Complete,
        // сохраняя старое поведение для всех, кто ждёт не потоковый ответ.
        // Если первым приходит чанк session/update — агент стримит:
        // возвращаем Reply::Streaming с внешним каналом, в который
        // пересылаются и остаток чанков, и терминальный элемент.
        // Если за first_chunk_timeout не пришло ни чанка, ни ответа —
        // агент вообще не начал ход: таймаут (T9).
        //
        // ИСПРАВЛЕНО (Р-23): biased + chunk-ветка ПЕРВОЙ. Без biased при
        // одновременной готовности resp_rx и rx.recv() выбор был случайным
        // между Complete и Streaming — намерение "если агент начал стримить,
        // доверяем этому сигналу" не гарантировалось. Теперь chunk-ветка
        // имеет приоритет, результат детерминирован.
        tokio::select! {
            biased;
            first = rx.recv() => {
                let Some(first) = first else {
                    self.updates.lock().await.remove(&session_key);
                    anyhow::bail!("stream channel closed before any event");
                };
                // Агент стримит: внешний канал, первый чанк уже пришёл.
                let (out_tx, out_rx) = mpsc::unbounded_channel::<SessionUpdate>();
                let _ = out_tx.send(first);

                // Фоновый таск: пересылает остаток чанков из rx в out, затем
                // ждёт финальный PromptResponse и шлёт терминальный элемент.
                // ДОБАВЛЕНО (Часть 2, задача C): idle_chunk_timeout применяется
                // здесь (первый чанк уже получен выше, поэтому первый wait —
                // это ожидание ВТОРОГО чанка, таймаут = idle).
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
                                                break; // получатель отключился
                                            }
                                        }
                                        None => break, // канал закрыт без terminal
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
                                    // ЛОГ-ЛОВУШКА (WARN, по умолчанию включена):
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
                // Т9: агент не начал стримить и не ответил за first_chunk_timeout.
                // Чистим pending и закрываем сессию, чтобы не висели.
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

// ИСПРАВЛЕНО (аудит P2-9): пустой impl Drop удалён — он только
// маскировал утечку процессов. Его роль выполняет kill_on_drop(true),
// выставленный в spawn().

/// ДОБАВЛЕНО (Часть 1 роадмапа стриминга, задача G): извлекает
/// session/update из JSON-RPC нотификации агента и шлёт в mpsc-канал
/// сессии сразу, без накопления.
///
/// Р-21 (decisions.md): парсится ТОЛЬКО `agent_message_chunk` — основной
/// канал текстового ответа. ToolCall/ToolCallUpdate/Plan/UsageUpdate НЕ
/// парсятся в Фазе 2.0: требуют сверки с точной JSON-схемой ACP по
/// каждому варианту отдельно, риск непропорционален ценности. Маппинг
/// этих 4 вариантов в convert.rs::session_update_to_a2a_event() написан,
/// но недостижим при текущем фильтре — сознательно (готовность к
/// следующей итерации).
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

/// Финальный PromptResponse → терминальный SessionUpdate (текстовый чанк
/// с полным контентом ответа). Вызывается в prompt() после завершения хода.
fn prompt_to_content_block(resp: &PromptResponse) -> protocol::acp::ContentBlock {
    // Сливаем весь контент ответа в один текстовый блок: у ACP-агента
    // финальный ответ может состоять из нескольких ContentBlock, но
    // терминальный элемент стрима должен нести полный текст.
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

    /// Возвращает путь к тестовому mock-acp-агенту (бинарь gatewayd).
    /// Юнит-тесты core не получают CARGO_BIN_EXE_* (он задаётся только
    /// integration-тестам крейта с [[bin]]), поэтому используем явный env
    /// MOCK_AGENT_BIN либо фолбэк на target/debug/mock_acp_agent workspace.
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

    /// T1: чанки приходят по одному, не батчем в конце.
    /// Мок-агент шлёт 3 session/update с задержкой 50мс между ними;
    /// через prompt_streaming() каждый чанк ловится rx.recv() с реальным
    /// временным интервалом, а не скопом после завершения хода.
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

        // Первый чанк — ждём с достаточным запасом (первый идёт сразу).
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

        // Последующие — между ними реально проходит ~50мс (не батч в конце).
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

    /// T9: idle_chunk_timeout закрывает зависший стрим. Мок шлёт 1 чанк,
    /// затем не отвечает дольше idle (200мс) — стрим должен закрыться по
    /// таймауту (< 500мс), а не висеть до call_timeout.
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

        // Читаем до закрытия канала (idle-таймаут закроет после 1 чанка).
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

    /// T9: first_chunk_timeout срабатывает, если агент вообще не начинает
    /// стримить и не отвечает. Мок молчит дольше first (100мс) —
    /// prompt_streaming должен вернуть Err по таймауту, не ждать call_timeout.
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

    /// Р-23: при одновременной готовности чанка и финального ответа выбор
    /// должен быть детерминированным в пользу Streaming (biased + chunk-ветка
    /// первой). Мок шлёт 1 чанк и ответ без задержки — повторные прогоны
    /// не должны давать случайный Complete/Streaming.
    #[tokio::test]
    async fn simultaneous_response_and_chunk_prefers_streaming_path() {
        let agent = spawn_mock(&[
            ("MOCK_AGENT_STREAM_CHUNKS", "1"),
            ("MOCK_AGENT_FINAL_DELAY_MS", "50"),
        ])
        .await;
        let session = init_session(&agent).await;

        // Один вызов: 5 повторов на одной сессии вносят межходовую гонку
        // (фоновый таск предыдущего хода ещё жив) — это не тема Р-23.
        // Детерминированность выбора проверяется однократно.
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
