//! core/src/convert.rs — финальная версия. lease_timeout передаётся
//! через конструктор, хардкода Duration::from_secs(30) нет.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use protocol::acp::{
    self, ContentBlock, McpServer, NewSessionRequest, PromptRequest, PromptResponse,
    SessionId, SessionUpdate, StopReason,
};
use protocol::a2a::{
    self, Artifact, ContextId, Message, MessageRole, Part, Task, TaskId, TaskState, TaskStatus,
};

use crate::agent::{A2aAgent, AcpAgent};
use crate::lease::TurnLease;
use crate::reply::Reply;
use crate::task_store::TaskStore;

// =========================================================================
// 1. Content mapping
// =========================================================================

pub fn content_block_to_part(cb: ContentBlock) -> Part {
    match cb {
        ContentBlock::Text { text } => Part::Text { text },
        ContentBlock::Image { mime_type, data, .. } => Part::File {
            file: a2a::FilePart { uri: None, bytes: Some(data), mime_type: Some(mime_type) },
        },
        ContentBlock::Audio { mime_type, data } => Part::File {
            file: a2a::FilePart { uri: None, bytes: Some(data), mime_type: Some(mime_type) },
        },
        ContentBlock::Resource { resource } => match resource {
            acp::EmbeddedResource::Text { text, .. } => Part::Text { text },
            acp::EmbeddedResource::Blob { blob, mime_type, .. } => {
                Part::File { file: a2a::FilePart { uri: None, bytes: Some(blob), mime_type } }
            }
        },
        ContentBlock::ResourceLink { uri, name, .. } => {
            Part::Text { text: format!("[resource: {name}]({uri})") }
        }
    }
}

pub fn part_to_content_block(p: Part) -> ContentBlock {
    match p {
        Part::Text { text } => ContentBlock::Text { text },
        Part::File { file } => ContentBlock::Image {
            mime_type: file.mime_type.unwrap_or_else(|| "application/octet-stream".into()),
            data: file.bytes.unwrap_or_default(),
            uri: file.uri,
        },
        Part::Data { data } => ContentBlock::Text { text: data.to_string() },
    }
}

fn message_to_prompt(session: SessionId, m: Message) -> PromptRequest {
    PromptRequest { session_id: session, prompt: m.parts.into_iter().map(part_to_content_block).collect() }
}

fn prompt_to_message(p: PromptRequest) -> Message {
    Message { role: MessageRole::User, parts: p.prompt.into_iter().map(content_block_to_part).collect(), message_id: None }
}

// =========================================================================
// 2. TaskState <-> StopReason
// =========================================================================

fn task_state_to_stop_reason(state: TaskState) -> anyhow::Result<StopReason> {
    match state {
        TaskState::Completed => Ok(StopReason::EndTurn),
        TaskState::Canceled => Ok(StopReason::Cancelled),
        TaskState::Failed | TaskState::Rejected => Ok(StopReason::Refusal),
        TaskState::InputRequired | TaskState::AuthRequired => {
            anyhow::bail!("input_required: A2A task ожидает ввода, ACP StopReason не применим")
        }
        TaskState::Submitted | TaskState::Working | TaskState::Unspecified => {
            anyhow::bail!("task ещё не завершена (state={state:?}), StopReason недоступен")
        }
    }
}

fn stop_reason_to_task_state(sr: StopReason) -> TaskState {
    match sr {
        StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => TaskState::Completed,
        StopReason::Refusal => TaskState::Failed,
        StopReason::Cancelled => TaskState::Canceled,
    }
}

// =========================================================================
// 3. AcpAsA2a
// =========================================================================

pub struct AcpAsA2a<T: AcpAgent> {
    inner: T,
    lease: TurnLease,
    lease_timeout: Duration,
    default_cwd: String,
    session: tokio::sync::Mutex<Option<SessionId>>,
    tasks: TaskStore,
}

impl<T: AcpAgent> AcpAsA2a<T> {
    pub fn new(
        inner: T,
        default_cwd: String,
        task_store_dir: impl Into<PathBuf>,
        lease_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            lease: TurnLease::default(),
            lease_timeout,
            default_cwd,
            session: tokio::sync::Mutex::new(None),
            tasks: TaskStore::new(task_store_dir),
        }
    }

    async fn ensure_session(&self) -> anyhow::Result<SessionId> {
        let mut guard = self.session.lock().await;
        if let Some(s) = &*guard {
            return Ok(s.clone());
        }
        let resp = self
            .inner
            .new_session(NewSessionRequest {
                cwd: self.default_cwd.clone(),
                mcp_servers: Vec::<McpServer>::new(),
                additional_directories: Vec::new(),
            })
            .await?;
        *guard = Some(resp.session_id.clone());
        Ok(resp.session_id)
    }
}

#[async_trait]
impl<T: AcpAgent + Send + Sync> A2aAgent for AcpAsA2a<T> {
    async fn card(&self) -> anyhow::Result<a2a::AgentCard> {
        let init = self
            .inner
            .initialize(acp::InitializeRequest {
                protocol_version: "1".into(),
                client_capabilities: Default::default(),
                client_info: None,
            })
            .await?;
        Ok(a2a::AgentCard {
            name: init.agent_info.as_ref().map(|i| i.name.clone()).unwrap_or_default(),
            description: None,
            version: init.protocol_version,
            url: String::new(),
            capabilities: a2a::AgentCardCapabilities { streaming: false, push_notifications: false },
            skills: Vec::new(),
        })
    }

    async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, a2a::A2aEvent>> {
        let session = self.ensure_session().await?;

        let _guard = self.lease.acquire(&session, self.lease_timeout).await?;

        let incoming_message = task
            .status
            .message
            .clone()
            .ok_or_else(|| anyhow::anyhow!("task.status.message обязателен для send_task в MVP"))?;
        let prompt_req = message_to_prompt(session.clone(), incoming_message);

        match self.inner.prompt(prompt_req).await? {
            Reply::Complete(resp) => {
                let state = stop_reason_to_task_state(resp.stop_reason);
                let result = Task {
                    id: task.id,
                    context_id: task.context_id,
                    status: TaskStatus { state, message: None, timestamp: now_iso8601() },
                    history: None,
                    artifacts: None,
                    metadata: None,
                };
                self.tasks.save(&result).await?;
                Ok(Reply::Complete(result))
            }
            Reply::Streaming(_rx) => unreachable!("Фаза 1: стрим не реализован"),
        }
    }

    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task> {
        self.tasks.load(&id).await
    }

    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task> {
        let session = self.session.lock().await.clone()
            .ok_or_else(|| anyhow::anyhow!("нет активной сессии для cancel"))?;
        self.inner.cancel(session).await?;

        let result = Task {
            id,
            context_id: ContextId(String::new()),
            status: TaskStatus { state: TaskState::Canceled, message: None, timestamp: now_iso8601() },
            history: None,
            artifacts: None,
            metadata: None,
        };
        self.tasks.save(&result).await?;
        Ok(result)
    }
}

// =========================================================================
// 4. A2aAsAcp
// =========================================================================

pub struct A2aAsAcp<T: A2aAgent> {
    inner: T,
    lease: TurnLease,
    lease_timeout: Duration,
}

impl<T: A2aAgent> A2aAsAcp<T> {
    pub fn new(inner: T, lease_timeout: Duration) -> Self {
        Self { inner, lease: TurnLease::default(), lease_timeout }
    }
}

#[async_trait]
impl<T: A2aAgent + Send + Sync> AcpAgent for A2aAsAcp<T> {
    async fn initialize(&self, _req: acp::InitializeRequest) -> anyhow::Result<acp::InitializeResponse> {
        let card = self.inner.card().await?;
        Ok(acp::InitializeResponse {
            protocol_version: card.version,
            agent_capabilities: acp::AgentCapabilities {
                load_session: false,
                prompt_capabilities: acp::PromptCapabilities { image: true, audio: false, embedded_context: false },
                mcp_capabilities: Default::default(),
                session_capabilities: Default::default(),
            },
            agent_info: Some(acp::Implementation { name: card.name, version: String::new() }),
            auth_methods: Vec::new(),
        })
    }

    async fn new_session(&self, _req: NewSessionRequest) -> anyhow::Result<acp::NewSessionResponse> {
        Ok(acp::NewSessionResponse { session_id: SessionId(new_session_id()) })
    }

    async fn prompt(&self, req: PromptRequest) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        let _guard = self.lease.acquire(&req.session_id, self.lease_timeout).await?;

        let message = prompt_to_message(req.clone());
        let task = Task {
            id: TaskId(req.session_id.0.clone()),
            context_id: ContextId(req.session_id.0),
            status: TaskStatus { state: TaskState::Submitted, message: Some(message), timestamp: now_iso8601() },
            history: None,
            artifacts: None,
            metadata: None,
        };

        match self.inner.send_task(task).await? {
            Reply::Complete(t) => {
                let stop_reason = task_state_to_stop_reason(t.status.state)?;
                Ok(Reply::Complete(PromptResponse { stop_reason }))
            }
            Reply::Streaming(_rx) => unreachable!("Фаза 1: стрим не реализован"),
        }
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        self.inner.cancel_task(TaskId(session.0)).await?;
        Ok(())
    }
}

fn now_iso8601() -> Option<String> {
    Some(chrono::Utc::now().to_rfc3339())
}

fn new_session_id() -> String {
    format!("sess-{}", uuid_v4_stub())
}

fn uuid_v4_stub() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!("{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos())
}
