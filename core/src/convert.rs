// core/src/convert.rs — final version. lease_timeout is passed through
// the constructor; there is no hardcoded Duration::from_secs(30) anywhere — the timeout
// is configured by the calling code (main.rs -> transport_*.rs), which
// reads it from config.yaml (turn_lease_timeout_secs).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use protocol::a2a::{
    self, Artifact, ContextId, Message, MessageRole, Part, Task, TaskId, TaskState, TaskStatus,
};
use protocol::acp::{
    self, ContentBlock, McpServer, NewSessionRequest, PromptRequest, PromptResponse, SessionId,
    SessionUpdate, StopReason,
};

use crate::agent::{A2aAgent, AcpAgent};
use crate::lease::TurnLease;
use crate::owner::Owner;
use crate::reply::Reply;
use crate::supervisor::ContextLost;
use crate::task_store::{OwnedTask, TaskStore};

// =========================================================================
// 1. Content mapping
// =========================================================================

pub fn content_block_to_part(cb: ContentBlock) -> Part {
    match cb {
        ContentBlock::Text { text } => Part::Text { text },
        ContentBlock::Image {
            mime_type, data, ..
        } => Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some(data),
                mime_type: Some(mime_type),
            },
        },
        ContentBlock::Audio { mime_type, data } => Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some(data),
                mime_type: Some(mime_type),
            },
        },
        ContentBlock::Resource { resource } => match resource {
            acp::EmbeddedResource::Text { text, .. } => Part::Text { text },
            acp::EmbeddedResource::Blob {
                blob, mime_type, ..
            } => Part::File {
                file: a2a::FilePart {
                    uri: None,
                    bytes: Some(blob),
                    mime_type,
                },
            },
        },
        ContentBlock::ResourceLink { uri, name, .. } => Part::Text {
            text: format!("[resource: {name}]({uri})"),
        },
    }
}

pub fn part_to_content_block(p: Part) -> ContentBlock {
    match p {
        Part::Text { text } => ContentBlock::Text { text },
        // FIXED (audit P2-13): previously ANY File became an Image,
        // including audio and PDF. The type is chosen by mime.
        Part::File { file } => {
            let mime = file
                .mime_type
                .unwrap_or_else(|| "application/octet-stream".into());
            let data = file.bytes.unwrap_or_default();
            if mime.starts_with("image/") {
                ContentBlock::Image {
                    mime_type: mime,
                    data,
                    uri: file.uri,
                }
            } else if mime.starts_with("audio/") {
                ContentBlock::Audio {
                    mime_type: mime,
                    data,
                }
            } else {
                ContentBlock::Resource {
                    resource: acp::EmbeddedResource::Blob {
                        uri: file.uri.unwrap_or_default(),
                        blob: data,
                        mime_type: Some(mime),
                    },
                }
            }
        }
        Part::Data { data } => ContentBlock::Text {
            text: data.to_string(),
        },
    }
}

fn message_to_prompt(session: SessionId, m: Message) -> PromptRequest {
    PromptRequest {
        session_id: session,
        prompt: m.parts.into_iter().map(part_to_content_block).collect(),
    }
}

fn prompt_to_message(p: PromptRequest) -> Message {
    Message {
        role: MessageRole::User,
        parts: p.prompt.into_iter().map(content_block_to_part).collect(),
        message_id: None,
    }
}

// =========================================================================
// 2. TaskState <-> StopReason - not a bijection; documented explicitly.
// =========================================================================

fn task_state_to_stop_reason(state: TaskState) -> anyhow::Result<StopReason> {
    match state {
        TaskState::Completed => Ok(StopReason::EndTurn),
        TaskState::Canceled => Ok(StopReason::Cancelled),
        TaskState::Failed | TaskState::Rejected => Ok(StopReason::Refusal),
        // FIXED (audit P2-6): was bail! — the whole prompt died on
        // the routine 'agent requests input' scenario. This is a normal turn end:
        // control returns to the client, which sends the next prompt.
        TaskState::InputRequired | TaskState::AuthRequired => Ok(StopReason::EndTurn),
        TaskState::Submitted | TaskState::Working | TaskState::Unspecified => {
            anyhow::bail!("task ещё не завершена (state={state:?}), StopReason недоступен")
        }
    }
}

fn stop_reason_to_task_state(sr: StopReason) -> TaskState {
    match sr {
        StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => {
            TaskState::Completed
        }
        StopReason::Refusal => TaskState::Failed,
        StopReason::Cancelled => TaskState::Canceled,
    }
}

// =========================================================================
// 3. AcpAsA2a - an A2A client sees an ACP agent.
// =========================================================================

struct SessionEntry {
    session_id: SessionId,
    owner: Owner,
    last_used: std::time::Instant,
    /// ADDED (audit P2-10): the agent-process generation in which
    /// this ACP session was created. If the process has been
    /// restarted since, the session no longer exists — and the client should learn
    /// about it explicitly, not keep talking into the void.
    generation: u64,
}

/// Cap on the number of concurrent conversations per agent. Without it
/// a client with a valid token could create contexts indefinitely.
const MAX_SESSIONS_PER_AGENT: usize = 256;

pub struct AcpAsA2a<T: AcpAgent> {
    inner: T,
    lease: TurnLease,
    lease_timeout: Duration,
    default_cwd: String,
    /// ADDED (audit P2-12): the external address at which this agent
    /// is visible from outside. Previously the AgentCard carried an empty url — the card
    /// is invalid per the A2A spec, and agent.json is the entry point for external
    /// clients, i.e. the first thing they read.
    public_url: String,
    /// FIXED (audit P1-1): was `Mutex<Option<SessionId>>` — ONE
    /// ACP session shared by all of the agent's clients, i.e. any two A2A clients
    /// ended up in the same conversation and saw each other's context.
    /// Now a session is created per A2A contextId and belongs to the client.
    sessions: tokio::sync::Mutex<HashMap<ContextId, SessionEntry>>,
    /// Idle sessions are evicted, otherwise the HashMap grows without bound
    /// (the same defect as P2-8 in TurnLease).
    session_ttl: Duration,
    tasks: TaskStore,
}

impl<T: AcpAgent> AcpAsA2a<T> {
    pub fn new(
        inner: T,
        default_cwd: String,
        task_store_dir: impl Into<PathBuf>,
        lease_timeout: Duration,
        public_url: String,
    ) -> Self {
        Self::with_session_ttl(
            inner,
            default_cwd,
            task_store_dir,
            lease_timeout,
            public_url,
            DEFAULT_SESSION_TTL,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_session_ttl(
        inner: T,
        default_cwd: String,
        task_store_dir: impl Into<PathBuf>,
        lease_timeout: Duration,
        public_url: String,
        session_ttl: Duration,
    ) -> Self {
        Self {
            inner,
            lease: TurnLease::default(),
            lease_timeout,
            default_cwd,
            public_url,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            session_ttl,
            tasks: TaskStore::new(task_store_dir),
        }
    }

    /// Session for a specific conversation. The first request with a new
    /// contextId spawns a fresh ACP session on the same agent process —
    /// no need to multiply processes, that is what sessionId exists for.
    async fn ensure_session(&self, context: &ContextId, owner: Owner) -> anyhow::Result<SessionId> {
        let mut sessions = self.sessions.lock().await;

        let now = std::time::Instant::now();
        let ttl = self.session_ttl;
        let expired: Vec<ContextId> = sessions
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_used) > ttl)
            .map(|(ctx, _)| ctx.clone())
            .collect();
        for ctx in expired {
            if let Some(entry) = sessions.remove(&ctx) {
                // Also release the TurnLease entry, otherwise evicting the
                // session would leave garbage there.
                self.lease.forget(&entry.session_id).await;
            }
        }

        // FIXED (found by live test): first bring the agent to
        // a ready state (with a restart if needed), and
        // only then read the generation. Otherwise the comparison used the old
        // number, the prompt went to the fresh process with the old sessionId,
        // and the client got 'Invalid params' from the agent instead of ContextLost.
        self.inner.ensure_ready().await?;
        let generation = self.inner.generation().await;

        if let Some(entry) = sessions.get(context) {
            // The conversation owner is fixed at creation: a foreign
            // contextId does not allow attaching to someone else's session.
            if entry.owner != owner {
                anyhow::bail!("contextId принадлежит другому клиенту");
            }

            // FIXED (audit P2-10): an entry that survived an agent restart
            // points at a nonexistent ACP session. Report the context loss
            // and drop the entry — a repeated request
            // with the same contextId starts the conversation anew.
            if entry.generation != generation {
                let previous = entry.generation;
                let stale = sessions
                    .remove(context)
                    .expect("запись только что читалась");
                self.lease.forget(&stale.session_id).await;
                return Err(ContextLost {
                    previous,
                    current: generation,
                }
                .into());
            }

            let entry = sessions
                .get_mut(context)
                .expect("запись только что читалась");
            entry.last_used = now;
            return Ok(entry.session_id.clone());
        }

        if sessions.len() >= MAX_SESSIONS_PER_AGENT {
            anyhow::bail!(
                "достигнут потолок одновременных разговоров на агента ({MAX_SESSIONS_PER_AGENT})"
            );
        }

        let resp = self
            .inner
            .new_session(NewSessionRequest {
                cwd: self.default_cwd.clone(),
                mcp_servers: Vec::<McpServer>::new(),
                additional_directories: Vec::new(),
            })
            .await?;

        sessions.insert(
            context.clone(),
            SessionEntry {
                session_id: resp.session_id.clone(),
                owner,
                last_used: now,
                generation,
            },
        );
        Ok(resp.session_id)
    }

    /// Session of an existing conversation without creating a new one. Used
    /// by cancel: there is nothing to cancel if the conversation never existed.
    async fn lookup_session(&self, context: &ContextId, owner: Owner) -> anyhow::Result<SessionId> {
        let sessions = self.sessions.lock().await;
        let entry = sessions
            .get(context)
            .ok_or_else(|| anyhow::anyhow!("нет активной сессии для contextId {}", context.0))?;
        if entry.owner != owner {
            anyhow::bail!("contextId принадлежит другому клиенту");
        }
        Ok(entry.session_id.clone())
    }

    /// Number of live conversations — for tests and diagnostics.
    pub async fn active_sessions(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Read a task with a check of its conversation's owner.
    ///
    /// PARTIALLY closes audit P1-2 (IDOR): a foreign task is not handed out
    /// while its conversation is alive. After TTL session eviction the
    /// owner is unknown — the full fix requires an owner field in TaskStore
    /// and is tracked as a separate item.
    pub async fn get_task_as(&self, owner: Owner, id: TaskId) -> anyhow::Result<Task> {
        let stored = self.tasks.load_owned(&id).await?;
        assert_owner_matches(&stored, owner)?;
        // Second line of defense: if the task is in the old format (owner not
        // recorded) but its conversation is still alive — ask the session registry.
        self.assert_owns(&stored.task.context_id, owner).await?;
        Ok(stored.task)
    }

    /// Cancel with an owner check. The session of the conversation that owns
    /// the task is cancelled, not the adapter's 'current' session.
    pub async fn cancel_task_as(&self, owner: Owner, id: TaskId) -> anyhow::Result<Task> {
        // FIXED (audit P2-4): it returned a stub with an empty
        // context_id, and the stored task was overwritten by that stub.
        let stored = self.tasks.load_owned(&id).await?;
        assert_owner_matches(&stored, owner)?;
        let mut result = stored.task;
        self.assert_owns(&result.context_id, owner).await?;

        let session = self.lookup_session(&result.context_id, owner).await?;
        self.inner.cancel(session).await?;

        result.status.state = TaskState::Canceled;
        result.status.timestamp = now_iso8601();
        self.tasks.save(&result, owner).await?;
        Ok(result)
    }

    /// A conversation either belongs to this owner or has already been forgotten
    /// (evicted by TTL). Previously 'forgotten' meant 'let it through' — that
    /// was the P1-2 hole. Now the main check is done by attribution in
    /// the task store, and this remains a second line of defense for old-format
    /// entries whose owner is not recorded.
    async fn assert_owns(&self, context: &ContextId, owner: Owner) -> anyhow::Result<()> {
        let sessions = self.sessions.lock().await;
        match sessions.get(context) {
            Some(entry) if entry.owner != owner => {
                anyhow::bail!("задача принадлежит другому клиенту")
            }
            _ => Ok(()),
        }
    }

    /// Send with the conversation owner specified. A transport that
    /// knows the client token must call exactly this method — the trait-level
    /// `send_task` carries no owner and behaves as Anonymous.
    pub async fn send_task_as(
        &self,
        owner: Owner,
        task: Task,
    ) -> anyhow::Result<Reply<Task, a2a::A2aEvent>> {
        let session = self.ensure_session(&task.context_id, owner).await?;

        let _guard = self.lease.acquire(&session, self.lease_timeout).await?;

        let incoming_message =
            task.status.message.clone().ok_or_else(|| {
                anyhow::anyhow!("task.status.message обязателен для send_task в MVP")
            })?;
        let prompt_req = message_to_prompt(session.clone(), incoming_message);

        // ADDED (P-20): send_task_as uses prompt_streaming() —
        // the calling transport layer (gatewayd, direction 4) is ready
        // to handle Reply::Streaming and render it as SSE.
        match self.inner.prompt_streaming(prompt_req).await? {
            Reply::Complete(resp) => {
                let state = stop_reason_to_task_state(resp.stop_reason);
                // FIXED (audit P2-1): the agent reply was thrown away, and
                // the A2A client got a Task with no Parts at all. Now
                // PromptResponse.content goes into artifacts and into
                // status.message with role Agent.
                let parts: Vec<Part> = resp
                    .content
                    .into_iter()
                    .map(content_block_to_part)
                    .collect();
                let agent_message = (!parts.is_empty()).then(|| Message {
                    role: MessageRole::Agent,
                    parts: parts.clone(),
                    message_id: None,
                });
                let artifacts = (!parts.is_empty()).then(|| {
                    vec![Artifact {
                        artifact_id: format!("{}-response", task.id.0),
                        name: Some("response".into()),
                        description: None,
                        parts,
                        metadata: None,
                    }]
                });
                let result = Task {
                    id: task.id,
                    context_id: task.context_id,
                    status: TaskStatus {
                        state,
                        message: agent_message,
                        timestamp: now_iso8601(),
                    },
                    history: None,
                    artifacts,
                    metadata: None,
                };
                self.tasks.save(&result, owner).await?;
                Ok(Reply::Complete(result))
            }
            // FIXED (audit P2-7): unreachable! = worker-task panic
            // in a network service. Now a regular error.
            // ADDED (P-20, diff convert-streaming-mapping.rs): the real
            // stream — SessionUpdate -> A2aEvent is translated by a background
            // task until the channel closes; the terminal event
            // (final: true) is sent at the end.
            Reply::Streaming(mut in_rx) => {
                let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<a2a::A2aEvent>();
                let task_id = task.id.clone();

                tokio::spawn(async move {
                    let mut chunk_count = 0usize;

                    while let Some(update) = in_rx.recv().await {
                        chunk_count += 1;
                        if let Some(event) = session_update_to_a2a_event(update, &task_id) {
                            if out_tx.send(event).is_err() {
                                // LOG-TRAP (WARN, enabled by default):
                                tracing::warn!(
                                    task_id = %task_id.0,
                                    "получатель A2aEvent отключился до terminal event — задача продолжает выполняться в фоне"
                                );
                                return;
                            }
                        }
                    }

                    // LOG-TRAP (WARN, enabled by default): 0 chunks —
                    // not a bug per se, but a diagnostic signal.
                    if chunk_count == 0 {
                        tracing::warn!(
                            task_id = %task_id.0,
                            "stream produced 0 chunks before terminal event"
                        );
                    }

                    // Terminal event (integration point G<->convert,
                    // decision (c) from convert-streaming-mapping.rs): terminal
                    // state is always Completed if the channel closed without error.
                    // The Cancelled/Refusal distinction is lost on the streaming path —
                    // a deliberate trade-off to preserve the seam (on the
                    // Complete path the distinction exists via
                    // stop_reason_to_task_state).
                    let _ = out_tx.send(a2a::A2aEvent::TaskStatusUpdate {
                        task_id: task_id.clone(),
                        status: TaskStatus {
                            state: TaskState::Completed,
                            message: None,
                            timestamp: now_iso8601(),
                        },
                        r#final: true,
                    });
                });

                Ok(Reply::Streaming(out_rx))
            }
        }
    }
}

/// ADDED (P-21, diff convert-streaming-mapping.rs, senior-level):
/// mapping of SessionUpdate -> A2aEvent variants. The mapping of all 5 variants
/// is written, but with the current collect_session_update() filter (only
/// agent_message_chunk) only AgentMessageChunk is reachable — the other
/// variants are readiness for the next parsing iteration.
///
/// DECISIONS FOR EACH VARIANT:
/// 1. AgentMessageChunk -> TaskStatusUpdate(state: Working, final: false).
/// 2. ToolCall/ToolCallUpdate -> TaskStatusUpdate with a textual description
///    (no direct equivalent in A2A; a textual trace beats losing the signal).
/// 3. Plan -> TaskStatusUpdate with a numbered text list.
/// 4. UsageUpdate -> NOT emitted to the client (no field in A2A), DEBUG log only.
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

        // DECISION: no equivalent in the A2A protocol. Do not emit an event
        // to the client, observability only.
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
        parts: vec![Part::Text {
            text: text.to_string(),
        }],
        message_id: None,
    }
}

/// One day of idleness: a conversation lives between client messages, but not forever.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[async_trait]
impl<T: AcpAgent + Send + Sync> A2aAgent for AcpAsA2a<T> {
    async fn card(&self) -> anyhow::Result<a2a::AgentCard> {
        let init = self
            .inner
            .initialize(acp::InitializeRequest {
                protocol_version: acp::DEFAULT_PROTOCOL_VERSION,
                client_capabilities: Default::default(),
                client_info: None,
            })
            .await?;
        Ok(a2a::AgentCard {
            name: init
                .agent_info
                .as_ref()
                .map(|i| i.name.clone())
                .unwrap_or_default(),
            description: None,
            // A2A AgentCard.version is a string, ACP protocolVersion is a number.
            version: init.protocol_version.to_string(),
            url: self.public_url.clone(),
            capabilities: a2a::AgentCardCapabilities {
                streaming: false,
                push_notifications: false,
            },
            skills: Vec::new(),
        })
    }

    async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, a2a::A2aEvent>> {
        self.send_task_as(Owner::Anonymous, task).await
    }

    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.get_task_as(Owner::Anonymous, id).await
    }

    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.cancel_task_as(Owner::Anonymous, id).await
    }
}

// =========================================================================
// 4. A2aAsAcp - an ACP client sees an A2A agent.
// =========================================================================

/// Session cap per ACP connection. A second line of defense after the
/// 'session created via session/new' check: even a well-intentioned client
/// must not be able to open an unlimited number of them.
const MAX_SESSIONS_PER_CONNECTION: usize = 256;

pub struct A2aAsAcp<T: A2aAgent> {
    inner: T,
    lease: TurnLease,
    lease_timeout: Duration,
    /// FIXED (audit P2-8): previously there was no notion of sessions here
    /// at all. `prompt` took sessionId straight from the client request and
    /// keyed TurnLease by it, and `forget` was never called — i.e. any
    /// sessionId sent by a client permanently added an entry
    /// to the lease HashMap. This is not just a leak: a client with a valid token
    /// could stuff the gateway's memory by generating identifiers on the fly.
    ///
    /// Now a session exists only if created via session/new,
    /// and is removed on session/cancel together with the lease entry.
    sessions: tokio::sync::Mutex<HashMap<SessionId, std::time::Instant>>,
    session_ttl: Duration,
}

impl<T: A2aAgent> A2aAsAcp<T> {
    pub fn new(inner: T, lease_timeout: Duration) -> Self {
        Self::with_session_ttl(inner, lease_timeout, DEFAULT_SESSION_TTL)
    }

    pub fn with_session_ttl(inner: T, lease_timeout: Duration, session_ttl: Duration) -> Self {
        Self {
            inner,
            lease: TurnLease::default(),
            lease_timeout,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            session_ttl,
        }
    }

    /// Registers a session created via session/new.
    async fn register_session(&self, session: SessionId) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        self.evict_expired(&mut sessions).await;

        if sessions.len() >= MAX_SESSIONS_PER_CONNECTION {
            anyhow::bail!(
                "достигнут потолок сессий на соединение ({MAX_SESSIONS_PER_CONNECTION}); \
                 закройте ненужные через session/cancel"
            );
        }

        sessions.insert(session, std::time::Instant::now());
        Ok(())
    }

    /// Checks that the session exists and extends it.
    ///
    /// Per ACP the client must call session/new before session/prompt.
    /// This was not checked, and an arbitrary sessionId was considered
    /// valid — which was the root of the leak.
    async fn touch_session(&self, session: &SessionId) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        self.evict_expired(&mut sessions).await;

        match sessions.get_mut(session) {
            Some(last_used) => {
                *last_used = std::time::Instant::now();
                Ok(())
            }
            None => anyhow::bail!(
                "неизвестный sessionId {}: сессию нужно создать через session/new",
                session.0
            ),
        }
    }

    /// Forgets a session and releases its lease entry.
    async fn drop_session(&self, session: &SessionId) {
        self.sessions.lock().await.remove(session);
        self.lease.forget(session).await;
    }

    async fn evict_expired(&self, sessions: &mut HashMap<SessionId, std::time::Instant>) {
        let now = std::time::Instant::now();
        let ttl = self.session_ttl;
        let expired: Vec<SessionId> = sessions
            .iter()
            .filter(|(_, last_used)| now.duration_since(**last_used) > ttl)
            .map(|(session, _)| session.clone())
            .collect();

        for session in expired {
            sessions.remove(&session);
            // Without this the entry would stay in TurnLease forever —
            // exactly the leak this is all meant to fix.
            self.lease.forget(&session).await;
        }
    }

    /// Number of live sessions — for tests and diagnostics.
    pub async fn active_sessions(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// How many sessions are tracked by TurnLease. Should match the number
    /// of live sessions: a mismatch is the leak.
    pub async fn leased_sessions(&self) -> usize {
        self.lease.tracked_sessions().await
    }
}

#[async_trait]
impl<T: A2aAgent + Send + Sync> AcpAgent for A2aAsAcp<T> {
    async fn initialize(
        &self,
        _req: acp::InitializeRequest,
    ) -> anyhow::Result<acp::InitializeResponse> {
        let card = self.inner.card().await?;
        Ok(acp::InitializeResponse {
            // Reverse conversion: A2A card string -> ACP number.
            // A bogus version must not kill the handshake — take
            // the major part, fall back to the default on failure.
            protocol_version: card
                .version
                .split('.')
                .next()
                .and_then(|major| major.trim().parse().ok())
                .unwrap_or(acp::DEFAULT_PROTOCOL_VERSION),
            agent_capabilities: acp::AgentCapabilities {
                load_session: false,
                prompt_capabilities: acp::PromptCapabilities {
                    image: true,
                    audio: false,
                    embedded_context: false,
                },
                mcp_capabilities: Default::default(),
                session_capabilities: Default::default(),
            },
            agent_info: Some(acp::Implementation {
                name: card.name,
                version: String::new(),
            }),
            auth_methods: Vec::new(),
        })
    }

    async fn new_session(
        &self,
        _req: NewSessionRequest,
    ) -> anyhow::Result<acp::NewSessionResponse> {
        let session_id = SessionId(new_session_id());
        self.register_session(session_id.clone()).await?;
        Ok(acp::NewSessionResponse { session_id })
    }

    async fn prompt(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        // Check BEFORE acquire: otherwise an unknown sessionId managed
        // to create a lease entry before it was rejected.
        self.touch_session(&req.session_id).await?;

        let _guard = self
            .lease
            .acquire(&req.session_id, self.lease_timeout)
            .await?;

        let message = prompt_to_message(req.clone());
        // FIXED (audit P2-5): TaskId was set equal to session_id, so
        // all turns of one session shared one id — overwrite in the store and
        // upstream duplicate rejection. A unique id per turn, the context
        // stays session-scoped.
        let task = Task {
            id: TaskId(format!("{}-{}", req.session_id.0, unique_suffix())),
            context_id: ContextId(req.session_id.0),
            status: TaskStatus {
                state: TaskState::Submitted,
                message: Some(message),
                timestamp: now_iso8601(),
            },
            history: None,
            artifacts: None,
            metadata: None,
        };

        match self.inner.send_task(task).await? {
            Reply::Complete(t) => {
                let stop_reason = task_state_to_stop_reason(t.status.state)?;
                // FIXED (audit P2-2): the Task content was thrown away,
                // the ACP client got only stop_reason without text.
                let mut content: Vec<ContentBlock> = Vec::new();
                if let Some(msg) = t.status.message {
                    content.extend(msg.parts.into_iter().map(part_to_content_block));
                }
                for artifact in t.artifacts.unwrap_or_default() {
                    content.extend(artifact.parts.into_iter().map(part_to_content_block));
                }
                Ok(Reply::Complete(PromptResponse {
                    stop_reason,
                    content,
                }))
            }
            // ADDED (P-20, diff convert-streaming-mapping.rs): A2aEvent
            // -> SessionUpdate is translated by a background task until the terminal
            // event (final: true) arrives — it closes the stream.
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
                                tracing::warn!(
                                    "получатель SessionUpdate отключился до terminal event (направление 3)"
                                );
                                return;
                            }
                        }
                        if is_final {
                            break; // terminal closes the stream
                        }
                    }
                    if chunk_count == 0 {
                        tracing::warn!(
                            "stream produced 0 chunks before terminal event (направление 3)"
                        );
                    }
                });

                Ok(Reply::Streaming(out_rx))
            }
        }
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        let result = self.inner.cancel_task(TaskId(session.0.clone())).await;

        // The session is forgotten regardless of the cancel outcome on the
        // agent side: the client considers it closed anyway, and there is no reason
        // to keep its lease entry.
        self.drop_session(&session).await;

        result.map(|_| ())
    }
}

/// ADDED (P-20, diff convert-streaming-mapping.rs): mapping of
/// A2aEvent -> SessionUpdate variants for direction 3 (A2A agent -> ACP client).
///
/// DECISIONS:
/// 1. TaskStatusUpdate { final: false } — if message contains text,
///    emit AgentMessageChunk, one per Part; empty status — DEBUG.
/// 2. TaskStatusUpdate { final: true } — terminal event, NOT mapped
///    into SessionUpdate (handled separately as a close signal).
/// 3. TaskArtifactUpdate — AgentMessageChunk, one per Part.
/// 4. Message(_) — AgentMessageChunk, one per Part.
fn a2a_event_to_session_update(event: a2a::A2aEvent) -> Vec<SessionUpdate> {
    match event {
        a2a::A2aEvent::TaskStatusUpdate {
            status, r#final, ..
        } => {
            if r#final {
                // The terminal event is handled separately by the calling
                // code — nothing is emitted here.
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

/// FIXED (audit P1-2): the owner is taken from the task store, so the
/// check survives TTL session eviction and gateway restart.
///
/// Tasks with no recorded owner (created before envelopes were introduced)
/// are not rejected by this line — otherwise a gateway upgrade would make
/// already accumulated tasks unavailable to their own owners.
fn assert_owner_matches(stored: &OwnedTask, owner: Owner) -> anyhow::Result<()> {
    match stored.owner {
        Some(recorded) if recorded != owner => {
            anyhow::bail!("задача принадлежит другому клиенту")
        }
        _ => Ok(()),
    }
}

fn now_iso8601() -> Option<String> {
    Some(chrono::Utc::now().to_rfc3339())
}

fn new_session_id() -> String {
    format!("sess-{}", unique_suffix())
}

/// FIXED (audit P1-3): was a bare nanosecond timestamp —
/// predictable, enumerable, collision-prone under concurrent calls,
/// plus unwrap() on system time. Now time + 96 bits of entropy.
pub(crate) fn unique_suffix() -> String {
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
        // Degradation without panic: a monotonic counter instead of entropy.
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
    use protocol::acp::{InitializeResponse, NewSessionResponse, PlanEntry, ToolCallStatus};

    /// T2 (Part 3 of the streaming roadmap): each SessionUpdate variant
    /// maps to an A2aEvent without panic and without silently dropping a signal.
    /// session_update_to_a2a_event is a pure function, tested directly on all
    /// 5 variants (per P-21, a stake in future parsing).
    #[test]
    fn agent_message_chunk_maps_to_working_status() {
        let update = SessionUpdate::AgentMessageChunk {
            message_id: None,
            content: ContentBlock::Text {
                text: "привет".into(),
            },
        };
        let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        assert!(
            matches!(
                event,
                Some(a2a::A2aEvent::TaskStatusUpdate { r#final: false, .. })
            ),
            "AgentMessageChunk должен маппиться в не-терминальный TaskStatusUpdate"
        );
    }

    #[test]
    fn tool_call_maps_to_text_status_not_dropped() {
        let update = SessionUpdate::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "поиск".into(),
            kind: "read".into(),
            status: ToolCallStatus::Pending,
        };
        let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        assert!(event.is_some(), "ToolCall не должен молча пропадать");
    }

    #[test]
    fn tool_call_update_maps_to_text_status() {
        let update = SessionUpdate::ToolCallUpdate {
            tool_call_id: "tc-2".into(),
            status: ToolCallStatus::InProgress,
            content: None,
        };
        let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        assert!(event.is_some(), "ToolCallUpdate не должен молча пропадать");
    }

    #[test]
    fn plan_maps_to_text_status_with_all_entries() {
        let update = SessionUpdate::Plan {
            entries: vec![
                PlanEntry {
                    content: "шаг 1".into(),
                    priority: "high".into(),
                    status: "pending".into(),
                },
                PlanEntry {
                    content: "шаг 2".into(),
                    priority: "low".into(),
                    status: "pending".into(),
                },
            ],
        };
        let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        if let Some(a2a::A2aEvent::TaskStatusUpdate { status, .. }) = event {
            let msg = status.message.expect("message присутствует");
            let text = msg
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.contains("шаг 1") && text.contains("шаг 2"),
                "план должен содержать оба шага: {text}"
            );
        } else {
            panic!("Plan должен маппиться в TaskStatusUpdate");
        }
    }

    #[test]
    fn usage_update_returns_none_by_design() {
        let update = SessionUpdate::UsageUpdate {
            used: 100,
            size: 1000,
            cost: None,
        };
        let event = session_update_to_a2a_event(update, &TaskId("t-1".into()));
        assert!(
            event.is_none(),
            "UsageUpdate сознательно не транслируется клиенту (Р-21)"
        );
    }

    #[test]
    fn task_status_update_final_true_returns_empty_vec() {
        let event = a2a::A2aEvent::TaskStatusUpdate {
            task_id: TaskId("t-1".into()),
            status: TaskStatus {
                state: TaskState::Completed,
                message: None,
                timestamp: None,
            },
            r#final: true,
        };
        assert!(
            a2a_event_to_session_update(event).is_empty(),
            "терминальное событие не должно давать SessionUpdate"
        );
    }

    #[test]
    fn task_artifact_update_maps_one_chunk_per_part() {
        let event = a2a::A2aEvent::TaskArtifactUpdate {
            task_id: TaskId("t-1".into()),
            artifact: Artifact {
                artifact_id: "a-1".into(),
                name: None,
                description: None,
                parts: vec![
                    Part::Text {
                        text: "один".into(),
                    },
                    Part::Text {
                        text: "два".into()
                    },
                ],
                metadata: None,
            },
            append: None,
        };
        let updates = a2a_event_to_session_update(event);
        assert_eq!(
            updates.len(),
            2,
            "2 Part -> 2 отдельных AgentMessageChunk, не склеены"
        );
    }

    /// Fake ACP agent: replies with fixed text and counts
    /// how many ACP sessions were requested of it.
    #[derive(Default)]
    struct EchoAcpAgent {
        sessions_created: std::sync::atomic::AtomicUsize,
        last_prompt_session: std::sync::Mutex<Option<SessionId>>,
        /// Simulated process restart: the generation grows, as in
        /// SupervisedStdioAgent after a respawn.
        generation: std::sync::atomic::AtomicU64,
        /// Process is killed and waits for a lazy restart in ensure_ready().
        dead: std::sync::atomic::AtomicBool,
        /// Whether the handshake happened. A protocol-strict agent must require
        /// initialize before session/new — the fake imitates this.
        initialized: std::sync::atomic::AtomicBool,
        initialize_calls: std::sync::atomic::AtomicUsize,
    }

    impl EchoAcpAgent {
        fn simulate_restart(&self) {
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        /// Process killed but not restarted yet — the generation
        /// bumps only inside ensure_ready(), like a real
        /// SupervisedStdioAgent with lazy respawn.
        fn simulate_kill(&self) {
            self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AcpAgent for EchoAcpAgent {
        async fn initialize(
            &self,
            _req: acp::InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            self.initialized
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.initialize_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(InitializeResponse {
                protocol_version: acp::DEFAULT_PROTOCOL_VERSION,
                agent_capabilities: Default::default(),
                agent_info: None,
                auth_methods: Vec::new(),
            })
        }

        async fn new_session(&self, _req: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
            if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("session/new без предшествующего initialize");
            }
            // Counter: each new session gets its own id — otherwise
            // conversation isolation is indistinguishable from its absence.
            let n = self
                .sessions_created
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(NewSessionResponse {
                session_id: SessionId(format!("sess-{n}")),
            })
        }

        async fn prompt(
            &self,
            _req: PromptRequest,
        ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
            *self.last_prompt_session.lock().unwrap() = Some(_req.session_id.clone());
            Ok(Reply::Complete(PromptResponse {
                stop_reason: StopReason::EndTurn,
                content: vec![ContentBlock::Text {
                    text: "ответ агента".into(),
                }],
            }))
        }

        async fn cancel(&self, _session: SessionId) -> anyhow::Result<()> {
            Ok(())
        }

        /// Lazy respawn exactly as in the supervisor: process death
        /// is detected here, and the generation grows here too.
        async fn ensure_ready(&self) -> anyhow::Result<()> {
            if self.dead.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // A fresh process is uninitialized: the handshake is part
                // of the process bring-up, as in SupervisedStdioAgent.
                self.initialized
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
                self.initialize(acp::InitializeRequest {
                    protocol_version: acp::DEFAULT_PROTOCOL_VERSION,
                    client_capabilities: Default::default(),
                    client_info: None,
                })
                .await?;
            }
            Ok(())
        }

        async fn generation(&self) -> u64 {
            self.generation.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn task_with_text(id: &str, text: &str) -> Task {
        task_in_context(id, "ctx", text)
    }

    fn task_in_context(id: &str, context: &str, text: &str) -> Task {
        Task {
            id: TaskId(id.into()),
            context_id: ContextId(context.into()),
            status: TaskStatus {
                state: TaskState::Submitted,
                message: Some(Message {
                    role: MessageRole::User,
                    parts: vec![Part::Text { text: text.into() }],
                    message_id: None,
                }),
                timestamp: None,
            },
            history: None,
            artifacts: None,
            metadata: None,
        }
    }

    /// Regression for audit P2-1: the agent reply was previously discarded and
    /// the A2A client got a Task with no Parts at all.
    #[tokio::test]
    async fn send_task_carries_agent_content_back() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::new(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
            "http://localhost:8348/agents/test/rpc".into(),
        );

        let reply = adapter
            .send_task(task_with_text("t-1", "привет"))
            .await
            .unwrap();
        let Reply::Complete(task) = reply else {
            panic!("ожидался Complete")
        };

        assert_eq!(task.status.state, TaskState::Completed);

        let artifacts = task
            .artifacts
            .expect("артефакт с ответом должен присутствовать");
        assert_eq!(artifacts.len(), 1);
        assert!(matches!(&artifacts[0].parts[0], Part::Text { text } if text == "ответ агента"));

        let message = task
            .status
            .message
            .expect("status.message должен содержать ответ");
        assert!(matches!(message.role, MessageRole::Agent));
    }

    /// Regression for audit P2-4: cancel_task returned a stub with an empty
    /// context_id and wiped the stored task.
    #[tokio::test]
    async fn cancel_task_preserves_original_task() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::new(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
            "http://localhost:8348/agents/test/rpc".into(),
        );

        adapter
            .send_task(task_with_text("t-2", "привет"))
            .await
            .unwrap();
        let canceled = adapter.cancel_task(TaskId("t-2".into())).await.unwrap();

        assert_eq!(canceled.status.state, TaskState::Canceled);
        assert_eq!(
            canceled.context_id.0, "ctx",
            "context_id не должен теряться"
        );
    }

    /// Regression for audit P2-13: any File was turned into an Image.
    #[test]
    fn non_image_file_part_is_not_forced_to_image() {
        let pdf = Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some("JVBERi0=".into()),
                mime_type: Some("application/pdf".into()),
            },
        };
        assert!(matches!(
            part_to_content_block(pdf),
            ContentBlock::Resource { .. }
        ));

        let wav = Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some("UklGRg==".into()),
                mime_type: Some("audio/wav".into()),
            },
        };
        assert!(matches!(
            part_to_content_block(wav),
            ContentBlock::Audio { .. }
        ));
    }

    /// Regression for audit P2-6: input-required took down the whole turn.
    #[test]
    fn input_required_does_not_fail_the_turn() {
        assert!(task_state_to_stop_reason(TaskState::InputRequired).is_ok());
        assert!(task_state_to_stop_reason(TaskState::AuthRequired).is_ok());
    }

    /// Regression for audit P1-3: the id is no longer a bare timestamp.
    #[test]
    fn unique_suffix_is_unique_and_not_bare_timestamp() {
        let a = unique_suffix();
        let b = unique_suffix();
        assert_ne!(a, b);
        assert!(a.contains('-'));
        assert!(a.len() > 24);
    }

    // ---------------------------------------------------------------
    // Regressions for audit P1-1: conversation isolation
    // ---------------------------------------------------------------

    fn adapter_for_test(dir: &std::path::Path) -> AcpAsA2a<EchoAcpAgent> {
        AcpAsA2a::new(
            EchoAcpAgent::default(),
            ".".into(),
            dir,
            Duration::from_secs(5),
            "http://localhost:8348/agents/test/rpc".into(),
        )
    }

    /// Main regression: previously `session` was single for the whole adapter,
    /// and two clients talked in one shared ACP session.
    #[tokio::test]
    async fn different_contexts_get_different_acp_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let bob = Owner::from_token("token-bob");

        adapter
            .send_task_as(alice, task_in_context("t-a", "ctx-alice", "привет"))
            .await
            .unwrap();
        let session_alice = adapter
            .inner
            .last_prompt_session
            .lock()
            .unwrap()
            .clone()
            .unwrap();

        adapter
            .send_task_as(bob, task_in_context("t-b", "ctx-bob", "привет"))
            .await
            .unwrap();
        let session_bob = adapter
            .inner
            .last_prompt_session
            .lock()
            .unwrap()
            .clone()
            .unwrap();

        assert_ne!(
            session_alice, session_bob,
            "разговоры не должны делить ACP-сессию"
        );
        assert_eq!(adapter.active_sessions().await, 2);
    }

    /// Same context of the same client — same session; the conversation
    /// continues rather than restarting on every message.
    #[tokio::test]
    async fn same_context_reuses_session() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "раз"))
            .await
            .unwrap();
        let first = adapter
            .inner
            .last_prompt_session
            .lock()
            .unwrap()
            .clone()
            .unwrap();

        adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-1", "два"))
            .await
            .unwrap();
        let second = adapter
            .inner
            .last_prompt_session
            .lock()
            .unwrap()
            .clone()
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(adapter.active_sessions().await, 1);
        assert_eq!(
            adapter
                .inner
                .sessions_created
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    /// A guessed foreign contextId does not attach to someone else's conversation.
    #[tokio::test]
    async fn foreign_owner_cannot_join_context() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна"))
            .await
            .unwrap();

        let attempt = adapter
            .send_task_as(mallory, task_in_context("t-2", "ctx-secret", "подсяду"))
            .await;
        assert!(attempt.is_err(), "чужой contextId должен отклоняться");
    }

    /// Someone else's task cannot be read while its conversation is alive.
    #[tokio::test]
    async fn foreign_owner_cannot_read_task() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна"))
            .await
            .unwrap();

        assert!(adapter
            .get_task_as(alice, TaskId("t-1".into()))
            .await
            .is_ok());
        assert!(adapter
            .get_task_as(mallory, TaskId("t-1".into()))
            .await
            .is_err());
        assert!(adapter
            .cancel_task_as(mallory, TaskId("t-1".into()))
            .await
            .is_err());
    }

    /// Anonymous calls (bare trait) — a separate bucket, not merged
    /// with conversations of token clients.
    #[tokio::test]
    async fn anonymous_is_isolated_from_token_owners() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let alice = Owner::from_token("token-alice");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-1", "привет"))
            .await
            .unwrap();
        let via_trait = adapter
            .send_task(task_in_context("t-2", "ctx-1", "привет"))
            .await;

        assert!(via_trait.is_err());
    }

    /// Idle conversations are evicted, otherwise the HashMap grows forever.
    #[tokio::test]
    async fn idle_sessions_are_evicted_by_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::with_session_ttl(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
            "http://localhost:8348/agents/test/rpc".into(),
            Duration::from_millis(50),
        );
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-old", "раз"))
            .await
            .unwrap();
        assert_eq!(adapter.active_sessions().await, 1);

        tokio::time::sleep(Duration::from_millis(80)).await;

        // A request with another context also runs the eviction pass.
        adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-new", "два"))
            .await
            .unwrap();
        assert_eq!(
            adapter.active_sessions().await,
            1,
            "просроченный разговор должен быть выселен"
        );
    }

    /// Cancel works by the task's conversation, not by the 'current' session.
    #[tokio::test]
    async fn cancel_resolves_session_by_task_context() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-a", "ctx-a", "раз"))
            .await
            .unwrap();
        adapter
            .send_task_as(owner, task_in_context("t-b", "ctx-b", "два"))
            .await
            .unwrap();

        let canceled = adapter
            .cancel_task_as(owner, TaskId("t-a".into()))
            .await
            .unwrap();
        assert_eq!(canceled.context_id.0, "ctx-a");
        assert_eq!(canceled.status.state, TaskState::Canceled);
    }

    // ---------------------------------------------------------------
    // Regressions for audit P1-2: attribution survives the session's lifetime
    // ---------------------------------------------------------------

    /// Main P1-2 regression: previously the owner check relied
    /// only on the live session registry, so after TTL eviction
    /// someone else's task became available to any token again.
    #[tokio::test]
    async fn foreign_owner_denied_after_session_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::with_session_ttl(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
            "http://localhost:8348/agents/test/rpc".into(),
            Duration::from_millis(50),
        );

        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна"))
            .await
            .unwrap();

        // Evict the conversation: a request with another context runs the TTL pass.
        tokio::time::sleep(Duration::from_millis(80)).await;
        adapter
            .send_task_as(mallory, task_in_context("t-2", "ctx-other", "своё"))
            .await
            .unwrap();

        // Alice's session is forgotten, but the task attribution remained in the store.
        assert!(
            adapter
                .get_task_as(mallory, TaskId("t-1".into()))
                .await
                .is_err(),
            "чужая задача не должна открываться после выселения сессии"
        );
        assert!(adapter
            .get_task_as(alice, TaskId("t-1".into()))
            .await
            .is_ok());
    }

    /// Attribution survives a process restart: a new adapter over the same
    /// task-store directory still distinguishes owners.
    #[tokio::test]
    async fn ownership_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        {
            let adapter = adapter_for_test(dir.path());
            adapter
                .send_task_as(alice, task_in_context("t-1", "ctx-1", "привет"))
                .await
                .unwrap();
        }

        // New adapter = empty session registry, as after a restart.
        let restarted = adapter_for_test(dir.path());
        assert_eq!(restarted.active_sessions().await, 0);

        assert!(restarted
            .get_task_as(mallory, TaskId("t-1".into()))
            .await
            .is_err());
        assert!(restarted
            .get_task_as(alice, TaskId("t-1".into()))
            .await
            .is_ok());
    }

    /// An anonymous call must not expose tasks owned by token clients.
    #[tokio::test]
    async fn anonymous_cannot_read_token_owned_task() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let alice = Owner::from_token("token-alice");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-1", "привет"))
            .await
            .unwrap();

        assert!(adapter.get_task(TaskId("t-1".into())).await.is_err());
    }

    // ---------------------------------------------------------------
    // Regressions for audit P2-10: agent restart marks conversations
    // ---------------------------------------------------------------

    /// Main regression: after an agent restart, a request to an old
    /// conversation must give an explicit context-lost error, not silently
    /// continue in an empty session where the agent remembers nothing.
    #[tokio::test]
    async fn restart_marks_old_context_as_lost() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "запомни 42"))
            .await
            .unwrap();

        adapter.inner.simulate_restart();

        let err = adapter
            .send_task_as(
                owner,
                task_in_context("t-2", "ctx-1", "что я просил запомнить?"),
            )
            .await
            .err()
            .expect("разговор прошлого поколения должен быть помечен потерянным");

        assert!(
            err.downcast_ref::<ContextLost>().is_some(),
            "ошибка должна быть типизированной, чтобы транспорт отличил её: {err}"
        );
    }

    /// The mark is one-shot: the client learned about the loss — the next request
    /// with the same contextId starts the conversation anew and works.
    #[tokio::test]
    async fn context_recovers_after_lost_notice() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "раз"))
            .await
            .unwrap();
        adapter.inner.simulate_restart();

        assert!(adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-1", "два"))
            .await
            .is_err());

        // The second call already creates a fresh session of the new generation.
        adapter
            .send_task_as(owner, task_in_context("t-3", "ctx-1", "три"))
            .await
            .unwrap();
        assert_eq!(adapter.active_sessions().await, 1);
    }

    /// The restart must not turn into a hole: a foreign contextId
    /// is rejected by owner before the mark kicks in.
    #[tokio::test]
    async fn restart_does_not_bypass_ownership_check() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter
            .send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна"))
            .await
            .unwrap();
        adapter.inner.simulate_restart();

        let err = adapter
            .send_task_as(mallory, task_in_context("t-2", "ctx-secret", "подсяду"))
            .await
            .err()
            .expect("чужой контекст должен отклоняться и после перезапуска");

        assert!(
            err.downcast_ref::<ContextLost>().is_none(),
            "чужаку нельзя сообщать даже факт существования контекста"
        );
    }

    /// Conversations created after the restart are not marked.
    #[tokio::test]
    async fn new_contexts_after_restart_are_not_marked() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter.inner.simulate_restart();
        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-new", "раз"))
            .await
            .unwrap();
        adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-new", "два"))
            .await
            .unwrap();

        assert_eq!(adapter.active_sessions().await, 1);
    }

    // ---------------------------------------------------------------
    // Regressions for defects found by the live test on claurst
    // ---------------------------------------------------------------

    /// Defect 1 from the live test: on lazy respawn the generation was read
    /// BEFORE the restart, the comparison passed, the prompt went to the fresh
    /// process with the old sessionId, and the client got 'Invalid params' from the agent.
    /// ContextLost triggered only on the second request.
    ///
    /// Here the agent is killed but not yet restarted — exactly the state
    /// in which the FIRST request after kill arrives.
    #[tokio::test]
    async fn first_request_after_kill_reports_context_lost() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "запомни 42"))
            .await
            .unwrap();

        // Process killed; restart will happen lazily, inside the call.
        adapter.inner.simulate_kill();

        let err = adapter
            .send_task_as(
                owner,
                task_in_context("t-2", "ctx-1", "что я просил запомнить?"),
            )
            .await
            .err()
            .expect("первый же запрос после смерти агента должен дать ContextLost");

        assert!(
            err.downcast_ref::<ContextLost>().is_some(),
            "ожидался ContextLost на ПЕРВОМ обращении, получено: {err}"
        );
    }

    /// And right after the mark the conversation recovers — one
    /// notification is enough for the client.
    #[tokio::test]
    async fn context_recovers_right_after_kill_notice() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "раз"))
            .await
            .unwrap();
        adapter.inner.simulate_kill();

        assert!(adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-1", "два"))
            .await
            .is_err());

        adapter
            .send_task_as(owner, task_in_context("t-3", "ctx-1", "три"))
            .await
            .unwrap();
        assert_eq!(adapter.active_sessions().await, 1);
    }

    /// Regression for a defect found while dissecting the live test: initialize
    /// was called only from card(), so a client going straight to
    /// message/send drove the agent to session/new without a handshake.
    #[tokio::test]
    async fn handshake_precedes_first_session() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        // card() was not called — the path is exactly as in message/send.
        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "привет"))
            .await
            .unwrap();

        assert!(adapter
            .inner
            .initialized
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    /// And, more importantly, after a restart the fresh process also gets
    /// the handshake — previously it never got one.
    #[tokio::test]
    async fn handshake_repeats_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter
            .send_task_as(owner, task_in_context("t-1", "ctx-1", "раз"))
            .await
            .unwrap();
        let before = adapter
            .inner
            .initialize_calls
            .load(std::sync::atomic::Ordering::SeqCst);

        adapter.inner.simulate_kill();
        // The first call reports context loss...
        assert!(adapter
            .send_task_as(owner, task_in_context("t-2", "ctx-1", "два"))
            .await
            .is_err());
        // ...but the handshake with the new process has already happened.
        let after = adapter
            .inner
            .initialize_calls
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(after > before, "свежий процесс должен получить initialize");

        adapter
            .send_task_as(owner, task_in_context("t-3", "ctx-1", "три"))
            .await
            .unwrap();
    }

    // ---------------------------------------------------------------
    // Regressions for audit P2-8: TurnLease leak via client sessionIds
    // ---------------------------------------------------------------

    /// Fake A2A agent to check the ACP side of the converter.
    struct StubA2aAgent;

    #[async_trait]
    impl A2aAgent for StubA2aAgent {
        async fn card(&self) -> anyhow::Result<a2a::AgentCard> {
            Ok(a2a::AgentCard {
                name: "stub".into(),
                description: None,
                version: "1".into(),
                url: String::new(),
                capabilities: Default::default(),
                skills: Vec::new(),
            })
        }

        async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, a2a::A2aEvent>> {
            let mut done = task;
            done.status.state = TaskState::Completed;
            Ok(Reply::Complete(done))
        }

        async fn get_task(&self, _id: TaskId) -> anyhow::Result<Task> {
            anyhow::bail!("не используется в тестах")
        }

        async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task> {
            Ok(Task {
                id: id.clone(),
                context_id: ContextId(id.0),
                status: TaskStatus {
                    state: TaskState::Canceled,
                    message: None,
                    timestamp: None,
                },
                history: None,
                artifacts: None,
                metadata: None,
            })
        }
    }

    fn new_session_req() -> NewSessionRequest {
        NewSessionRequest {
            cwd: ".".into(),
            mcp_servers: Vec::new(),
            additional_directories: Vec::new(),
        }
    }

    fn prompt_for(session: &SessionId) -> PromptRequest {
        PromptRequest {
            session_id: session.clone(),
            prompt: vec![ContentBlock::Text {
                text: "привет".into(),
            }],
        }
    }

    /// Main regression: previously prompt accepted ANY sessionId and
    /// created a lease entry for it. A client with a valid token could
    /// stuff the gateway's memory by generating identifiers on the fly.
    #[tokio::test]
    async fn prompt_with_unknown_session_is_rejected() {
        let adapter = A2aAsAcp::new(StubA2aAgent, Duration::from_secs(5));

        for i in 0..100 {
            let bogus = SessionId(format!("выдуманная-{i}"));
            assert!(
                adapter.prompt(prompt_for(&bogus)).await.is_err(),
                "sessionId без session/new должен отклоняться"
            );
        }

        assert_eq!(
            adapter.leased_sessions().await,
            0,
            "отклонённые идентификаторы не должны оставлять следа в лизе"
        );
    }

    /// session/cancel frees both the session and the lease entry.
    #[tokio::test]
    async fn cancel_releases_lease_entry() {
        let adapter = A2aAsAcp::new(StubA2aAgent, Duration::from_secs(5));

        let session = adapter
            .new_session(new_session_req())
            .await
            .unwrap()
            .session_id;
        adapter.prompt(prompt_for(&session)).await.unwrap();

        assert_eq!(adapter.active_sessions().await, 1);
        assert_eq!(adapter.leased_sessions().await, 1);

        adapter.cancel(session).await.unwrap();

        assert_eq!(adapter.active_sessions().await, 0);
        assert_eq!(
            adapter.leased_sessions().await,
            0,
            "лиз должен быть освобождён"
        );
    }

    /// A long connection with many closed sessions does not accumulate entries.
    #[tokio::test]
    async fn long_connection_does_not_accumulate_lease_entries() {
        let adapter = A2aAsAcp::new(StubA2aAgent, Duration::from_secs(5));

        for _ in 0..300 {
            let session = adapter
                .new_session(new_session_req())
                .await
                .unwrap()
                .session_id;
            adapter.prompt(prompt_for(&session)).await.unwrap();
            adapter.cancel(session).await.unwrap();
        }

        assert_eq!(adapter.active_sessions().await, 0);
        assert_eq!(adapter.leased_sessions().await, 0);
    }

    /// A client that does not close sessions hits the cap, not
    /// the gateway's memory.
    #[tokio::test]
    async fn open_sessions_hit_the_cap() {
        let adapter = A2aAsAcp::new(StubA2aAgent, Duration::from_secs(5));

        for _ in 0..MAX_SESSIONS_PER_CONNECTION {
            adapter.new_session(new_session_req()).await.unwrap();
        }

        assert!(
            adapter.new_session(new_session_req()).await.is_err(),
            "за потолком session/new должен отказывать"
        );
    }

    /// Idle sessions are evicted together with their lease entries.
    #[tokio::test]
    async fn idle_sessions_release_lease_entries() {
        let adapter = A2aAsAcp::with_session_ttl(
            StubA2aAgent,
            Duration::from_secs(5),
            Duration::from_millis(50),
        );

        let session = adapter
            .new_session(new_session_req())
            .await
            .unwrap()
            .session_id;
        adapter.prompt(prompt_for(&session)).await.unwrap();
        assert_eq!(adapter.leased_sessions().await, 1);

        tokio::time::sleep(Duration::from_millis(80)).await;

        // Any request runs the eviction pass.
        adapter.new_session(new_session_req()).await.unwrap();

        assert_eq!(
            adapter.active_sessions().await,
            1,
            "остаётся только свежая сессия"
        );
        assert_eq!(
            adapter.leased_sessions().await,
            0,
            "лиз просроченной сессии освобождён"
        );
    }

    /// Regression for audit P2-12: AgentCard.url was empty, making the
    /// card invalid per the A2A spec — and agent.json is the first thing
    /// an external client reads.
    #[tokio::test]
    async fn agent_card_carries_public_url() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let card = adapter.card().await.unwrap();

        assert!(!card.url.is_empty(), "url карточки не должен быть пустым");
        assert!(
            card.url.starts_with("http"),
            "url должен быть абсолютным: {}",
            card.url
        );
    }
}
