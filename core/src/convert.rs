//! core/src/convert.rs — финальная версия. lease_timeout передаётся через
//! конструктор, хардкода Duration::from_secs(30) нигде нет — таймаут
//! настраивается вызывающим кодом (main.rs -> transport_*.rs), который
//! читает его из config.yaml (turn_lease_timeout_secs).

use std::collections::HashMap;
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
        // ИСПРАВЛЕНО (аудит P2-13): раньше ЛЮБОЙ File становился Image,
        // включая аудио и PDF. Тип выбирается по mime.
        Part::File { file } => {
            let mime = file.mime_type.unwrap_or_else(|| "application/octet-stream".into());
            let data = file.bytes.unwrap_or_default();
            if mime.starts_with("image/") {
                ContentBlock::Image { mime_type: mime, data, uri: file.uri }
            } else if mime.starts_with("audio/") {
                ContentBlock::Audio { mime_type: mime, data }
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
// 2. TaskState <-> StopReason — НЕ биекция, задокументировано явно.
// =========================================================================

fn task_state_to_stop_reason(state: TaskState) -> anyhow::Result<StopReason> {
    match state {
        TaskState::Completed => Ok(StopReason::EndTurn),
        TaskState::Canceled => Ok(StopReason::Cancelled),
        TaskState::Failed | TaskState::Rejected => Ok(StopReason::Refusal),
        // ИСПРАВЛЕНО (аудит P2-6): было bail! — весь prompt падал на
        // штатном сценарии "агент просит ввод". Это нормальное завершение
        // хода: управление возвращается клиенту, он шлёт следующий prompt.
        TaskState::InputRequired | TaskState::AuthRequired => Ok(StopReason::EndTurn),
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
// 3. AcpAsA2a — A2A-клиент видит ACP-агента.
// =========================================================================

/// Владелец разговора. Хранится как хеш токена, а не сам токен: для
/// проверки «тот же клиент?» достаточно равенства, а держать секреты в
/// памяти дольше необходимого незачем.
///
/// `Anonymous` — вызовы через голый трейт `A2aAgent`, без транспортного
/// контекста. Это отдельная корзина: анонимные вызовы изолированы от
/// токенных и друг с другом делят контекст только по явному contextId.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Anonymous,
    Token(u64),
}

impl Owner {
    pub fn from_token(token: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        token.hash(&mut hasher);
        Owner::Token(hasher.finish())
    }
}

struct SessionEntry {
    session_id: SessionId,
    owner: Owner,
    last_used: std::time::Instant,
}

/// Потолок числа одновременных разговоров на одного агента. Без него
/// клиент с валидным токеном может создавать контексты бесконечно.
const MAX_SESSIONS_PER_AGENT: usize = 256;

pub struct AcpAsA2a<T: AcpAgent> {
    inner: T,
    lease: TurnLease,
    lease_timeout: Duration,
    default_cwd: String,
    /// ИСПРАВЛЕНО (аудит P1-1): было `Mutex<Option<SessionId>>` — ОДНА
    /// ACP-сессия на всех клиентов агента, то есть любые два A2A-клиента
    /// оказывались в одном разговоре и видели контекст друг друга.
    /// Теперь сессия заводится на A2A contextId и принадлежит клиенту.
    sessions: tokio::sync::Mutex<HashMap<ContextId, SessionEntry>>,
    /// Простаивающие сессии выселяются, иначе HashMap растёт без предела
    /// (тот же дефект, что P2-8 у TurnLease).
    session_ttl: Duration,
    tasks: TaskStore,
}

impl<T: AcpAgent> AcpAsA2a<T> {
    pub fn new(
        inner: T,
        default_cwd: String,
        task_store_dir: impl Into<PathBuf>,
        lease_timeout: Duration,
    ) -> Self {
        Self::with_session_ttl(inner, default_cwd, task_store_dir, lease_timeout, DEFAULT_SESSION_TTL)
    }

    pub fn with_session_ttl(
        inner: T,
        default_cwd: String,
        task_store_dir: impl Into<PathBuf>,
        lease_timeout: Duration,
        session_ttl: Duration,
    ) -> Self {
        Self {
            inner,
            lease: TurnLease::default(),
            lease_timeout,
            default_cwd,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            session_ttl,
            tasks: TaskStore::new(task_store_dir),
        }
    }

    /// Сессия для конкретного разговора. Первое обращение с новым
    /// contextId заводит новую ACP-сессию на том же процессе агента —
    /// плодить процессы не требуется, sessionId для того и существует.
    async fn ensure_session(
        &self,
        context: &ContextId,
        owner: Owner,
    ) -> anyhow::Result<SessionId> {
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
                // Освобождаем и запись в TurnLease, иначе выселение
                // сессии оставляло бы там мусор.
                self.lease.forget(&entry.session_id).await;
            }
        }

        if let Some(entry) = sessions.get_mut(context) {
            // Владелец разговора зафиксирован при создании: чужой
            // contextId не даёт подключиться к чужой сессии.
            if entry.owner != owner {
                anyhow::bail!("contextId принадлежит другому клиенту");
            }
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
            SessionEntry { session_id: resp.session_id.clone(), owner, last_used: now },
        );
        Ok(resp.session_id)
    }

    /// Сессия существующего разговора без создания новой. Используется
    /// отменой: отменять нечего, если разговора не было.
    async fn lookup_session(
        &self,
        context: &ContextId,
        owner: Owner,
    ) -> anyhow::Result<SessionId> {
        let sessions = self.sessions.lock().await;
        let entry = sessions
            .get(context)
            .ok_or_else(|| anyhow::anyhow!("нет активной сессии для contextId {}", context.0))?;
        if entry.owner != owner {
            anyhow::bail!("contextId принадлежит другому клиенту");
        }
        Ok(entry.session_id.clone())
    }

    /// Число живых разговоров — для тестов и диагностики.
    pub async fn active_sessions(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Чтение задачи с проверкой владельца её разговора.
    ///
    /// ЧАСТИЧНО закрывает аудит P1-2 (IDOR): чужую задачу не отдадим,
    /// пока её разговор жив. После выселения сессии по TTL владелец
    /// неизвестен — полное решение требует поля владельца в TaskStore
    /// и вынесено в отдельный пункт.
    pub async fn get_task_as(&self, owner: Owner, id: TaskId) -> anyhow::Result<Task> {
        let task = self.tasks.load(&id).await?;
        self.assert_owns(&task.context_id, owner).await?;
        Ok(task)
    }

    /// Отмена с проверкой владельца. Отменяется сессия того разговора,
    /// которому принадлежит задача, а не «текущая» сессия адаптера.
    pub async fn cancel_task_as(&self, owner: Owner, id: TaskId) -> anyhow::Result<Task> {
        // ИСПРАВЛЕНО (аудит P2-4): возвращалась пустышка с пустым
        // context_id, а сохранённая задача затиралась ею же.
        let mut result = self.tasks.load(&id).await?;
        self.assert_owns(&result.context_id, owner).await?;

        let session = self.lookup_session(&result.context_id, owner).await?;
        self.inner.cancel(session).await?;

        result.status.state = TaskState::Canceled;
        result.status.timestamp = now_iso8601();
        self.tasks.save(&result).await?;
        Ok(result)
    }

    /// Разговор либо принадлежит этому владельцу, либо уже забыт
    /// (выселен по TTL) — во втором случае претендовать не на что и
    /// отказывать не за что.
    async fn assert_owns(&self, context: &ContextId, owner: Owner) -> anyhow::Result<()> {
        let sessions = self.sessions.lock().await;
        match sessions.get(context) {
            Some(entry) if entry.owner != owner => {
                anyhow::bail!("задача принадлежит другому клиенту")
            }
            _ => Ok(()),
        }
    }

    /// Отправка с указанием владельца разговора. Транспорт, который
    /// знает токен клиента, должен звать именно этот метод — трейтовый
    /// `send_task` владельца не несёт и работает как Anonymous.
    pub async fn send_task_as(
        &self,
        owner: Owner,
        task: Task,
    ) -> anyhow::Result<Reply<Task, a2a::A2aEvent>> {
        let session = self.ensure_session(&task.context_id, owner).await?;

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
                // ИСПРАВЛЕНО (аудит P2-1): ответ агента выбрасывался, и
                // A2A-клиент получал Task вообще без Part'ов. Теперь
                // PromptResponse.content уходит в artifacts и в
                // status.message с ролью Agent.
                let parts: Vec<Part> =
                    resp.content.into_iter().map(content_block_to_part).collect();
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
                    status: TaskStatus { state, message: agent_message, timestamp: now_iso8601() },
                    history: None,
                    artifacts,
                    metadata: None,
                };
                self.tasks.save(&result).await?;
                Ok(Reply::Complete(result))
            }
            // ИСПРАВЛЕНО (аудит P2-7): unreachable! = паника воркер-таска
            // в сетевом сервисе. Теперь обычная ошибка.
            Reply::Streaming(_rx) => anyhow::bail!("Фаза 1: стриминг не реализован"),
        }
    }

}

/// Сутки простоя: разговор живёт между сообщениями клиента, но не вечно.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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
// 4. A2aAsAcp — ACP-клиент видит A2A-агента.
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
        // ИСПРАВЛЕНО (аудит P2-5): TaskId брался равным session_id, из-за
        // чего все ходы одной сессии имели один id — перезапись в store и
        // отказ upstream на дубликат. Уникальный id на ход, контекст
        // остаётся сессионным.
        let task = Task {
            id: TaskId(format!("{}-{}", req.session_id.0, unique_suffix())),
            context_id: ContextId(req.session_id.0),
            status: TaskStatus { state: TaskState::Submitted, message: Some(message), timestamp: now_iso8601() },
            history: None,
            artifacts: None,
            metadata: None,
        };

        match self.inner.send_task(task).await? {
            Reply::Complete(t) => {
                let stop_reason = task_state_to_stop_reason(t.status.state)?;
                // ИСПРАВЛЕНО (аудит P2-2): контент Task выбрасывался,
                // ACP-клиент получал только stop_reason без текста.
                let mut content: Vec<ContentBlock> = Vec::new();
                if let Some(msg) = t.status.message {
                    content.extend(msg.parts.into_iter().map(part_to_content_block));
                }
                for artifact in t.artifacts.unwrap_or_default() {
                    content.extend(artifact.parts.into_iter().map(part_to_content_block));
                }
                Ok(Reply::Complete(PromptResponse { stop_reason, content }))
            }
            Reply::Streaming(_rx) => anyhow::bail!("Фаза 1: стриминг не реализован"),
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
    format!("sess-{}", unique_suffix())
}

/// ИСПРАВЛЕНО (аудит P1-3): был голый наносекундный таймстамп —
/// предсказуемый, перечислимый и коллизионный при конкурентных вызовах,
/// плюс unwrap() на системном времени. Теперь время + 96 бит энтропии.
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
        // Деградация без паники: монотонный счётчик вместо энтропии.
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
    use protocol::a2a::{ContextId, TaskState};
    use protocol::acp::{InitializeResponse, NewSessionResponse};

    /// Фейковый ACP-агент: отвечает фиксированным текстом и считает,
    /// сколько ACP-сессий у него запросили.
    #[derive(Default)]
    struct EchoAcpAgent {
        sessions_created: std::sync::atomic::AtomicUsize,
        last_prompt_session: std::sync::Mutex<Option<SessionId>>,
    }

    #[async_trait]
    impl AcpAgent for EchoAcpAgent {
        async fn initialize(
            &self,
            _req: acp::InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            Ok(InitializeResponse {
                protocol_version: "1".into(),
                agent_capabilities: Default::default(),
                agent_info: None,
                auth_methods: Vec::new(),
            })
        }

        async fn new_session(&self, _req: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
            // Счётчик: каждая новая сессия получает свой id — иначе
            // изоляцию разговоров нечем отличить от её отсутствия.
            let n = self.sessions_created.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(NewSessionResponse { session_id: SessionId(format!("sess-{n}")) })
        }

        async fn prompt(
            &self,
            _req: PromptRequest,
        ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
            *self.last_prompt_session.lock().unwrap() = Some(_req.session_id.clone());
            Ok(Reply::Complete(PromptResponse {
                stop_reason: StopReason::EndTurn,
                content: vec![ContentBlock::Text { text: "ответ агента".into() }],
            }))
        }

        async fn cancel(&self, _session: SessionId) -> anyhow::Result<()> {
            Ok(())
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

    /// Регрессия на аудит P2-1: ответ агента раньше выбрасывался и
    /// A2A-клиент получал Task вообще без Part'ов.
    #[tokio::test]
    async fn send_task_carries_agent_content_back() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::new(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
        );

        let reply = adapter.send_task(task_with_text("t-1", "привет")).await.unwrap();
        let Reply::Complete(task) = reply else { panic!("ожидался Complete") };

        assert_eq!(task.status.state, TaskState::Completed);

        let artifacts = task.artifacts.expect("артефакт с ответом должен присутствовать");
        assert_eq!(artifacts.len(), 1);
        assert!(matches!(&artifacts[0].parts[0], Part::Text { text } if text == "ответ агента"));

        let message = task.status.message.expect("status.message должен содержать ответ");
        assert!(matches!(message.role, MessageRole::Agent));
    }

    /// Регрессия на аудит P2-4: cancel_task возвращал пустышку с пустым
    /// context_id и затирал сохранённую задачу.
    #[tokio::test]
    async fn cancel_task_preserves_original_task() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::new(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
        );

        adapter.send_task(task_with_text("t-2", "привет")).await.unwrap();
        let canceled = adapter.cancel_task(TaskId("t-2".into())).await.unwrap();

        assert_eq!(canceled.status.state, TaskState::Canceled);
        assert_eq!(canceled.context_id.0, "ctx", "context_id не должен теряться");
    }

    /// Регрессия на аудит P2-13: любой File превращался в Image.
    #[test]
    fn non_image_file_part_is_not_forced_to_image() {
        let pdf = Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some("JVBERi0=".into()),
                mime_type: Some("application/pdf".into()),
            },
        };
        assert!(matches!(part_to_content_block(pdf), ContentBlock::Resource { .. }));

        let wav = Part::File {
            file: a2a::FilePart {
                uri: None,
                bytes: Some("UklGRg==".into()),
                mime_type: Some("audio/wav".into()),
            },
        };
        assert!(matches!(part_to_content_block(wav), ContentBlock::Audio { .. }));
    }

    /// Регрессия на аудит P2-6: input-required ронял весь ход.
    #[test]
    fn input_required_does_not_fail_the_turn() {
        assert!(task_state_to_stop_reason(TaskState::InputRequired).is_ok());
        assert!(task_state_to_stop_reason(TaskState::AuthRequired).is_ok());
    }

    /// Регрессия на аудит P1-3: id больше не голый таймстамп.
    #[test]
    fn unique_suffix_is_unique_and_not_bare_timestamp() {
        let a = unique_suffix();
        let b = unique_suffix();
        assert_ne!(a, b);
        assert!(a.contains('-'));
        assert!(a.len() > 24);
    }

    // ---------------------------------------------------------------
    // Регрессии на аудит P1-1: изоляция разговоров
    // ---------------------------------------------------------------

    fn adapter_for_test(dir: &std::path::Path) -> AcpAsA2a<EchoAcpAgent> {
        AcpAsA2a::new(EchoAcpAgent::default(), ".".into(), dir, Duration::from_secs(5))
    }

    /// Главная регрессия: раньше `session` была одна на весь адаптер,
    /// и два клиента разговаривали в общей ACP-сессии.
    #[tokio::test]
    async fn different_contexts_get_different_acp_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let bob = Owner::from_token("token-bob");

        adapter.send_task_as(alice, task_in_context("t-a", "ctx-alice", "привет")).await.unwrap();
        let session_alice = adapter.inner.last_prompt_session.lock().unwrap().clone().unwrap();

        adapter.send_task_as(bob, task_in_context("t-b", "ctx-bob", "привет")).await.unwrap();
        let session_bob = adapter.inner.last_prompt_session.lock().unwrap().clone().unwrap();

        assert_ne!(session_alice, session_bob, "разговоры не должны делить ACP-сессию");
        assert_eq!(adapter.active_sessions().await, 2);
    }

    /// Тот же контекст того же клиента — та же сессия, разговор
    /// продолжается, а не начинается заново на каждое сообщение.
    #[tokio::test]
    async fn same_context_reuses_session() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter.send_task_as(owner, task_in_context("t-1", "ctx-1", "раз")).await.unwrap();
        let first = adapter.inner.last_prompt_session.lock().unwrap().clone().unwrap();

        adapter.send_task_as(owner, task_in_context("t-2", "ctx-1", "два")).await.unwrap();
        let second = adapter.inner.last_prompt_session.lock().unwrap().clone().unwrap();

        assert_eq!(first, second);
        assert_eq!(adapter.active_sessions().await, 1);
        assert_eq!(adapter.inner.sessions_created.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Угаданный чужой contextId не подключает к чужому разговору.
    #[tokio::test]
    async fn foreign_owner_cannot_join_context() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter.send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна")).await.unwrap();

        let attempt = adapter
            .send_task_as(mallory, task_in_context("t-2", "ctx-secret", "подсяду"))
            .await;
        assert!(attempt.is_err(), "чужой contextId должен отклоняться");
    }

    /// Чужую задачу нельзя прочитать, пока её разговор жив.
    #[tokio::test]
    async fn foreign_owner_cannot_read_task() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());

        let alice = Owner::from_token("token-alice");
        let mallory = Owner::from_token("token-mallory");

        adapter.send_task_as(alice, task_in_context("t-1", "ctx-secret", "тайна")).await.unwrap();

        assert!(adapter.get_task_as(alice, TaskId("t-1".into())).await.is_ok());
        assert!(adapter.get_task_as(mallory, TaskId("t-1".into())).await.is_err());
        assert!(adapter.cancel_task_as(mallory, TaskId("t-1".into())).await.is_err());
    }

    /// Анонимные вызовы (голый трейт) — своя корзина, не сливаются
    /// с разговорами токенных клиентов.
    #[tokio::test]
    async fn anonymous_is_isolated_from_token_owners() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let alice = Owner::from_token("token-alice");

        adapter.send_task_as(alice, task_in_context("t-1", "ctx-1", "привет")).await.unwrap();
        let via_trait = adapter.send_task(task_in_context("t-2", "ctx-1", "привет")).await;

        assert!(via_trait.is_err());
    }

    /// Простаивающие разговоры выселяются, иначе HashMap растёт вечно.
    #[tokio::test]
    async fn idle_sessions_are_evicted_by_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AcpAsA2a::with_session_ttl(
            EchoAcpAgent::default(),
            ".".into(),
            dir.path(),
            Duration::from_secs(5),
            Duration::from_millis(50),
        );
        let owner = Owner::from_token("token-alice");

        adapter.send_task_as(owner, task_in_context("t-1", "ctx-old", "раз")).await.unwrap();
        assert_eq!(adapter.active_sessions().await, 1);

        tokio::time::sleep(Duration::from_millis(80)).await;

        // Обращение с другим контекстом заодно прогоняет выселение.
        adapter.send_task_as(owner, task_in_context("t-2", "ctx-new", "два")).await.unwrap();
        assert_eq!(adapter.active_sessions().await, 1, "просроченный разговор должен быть выселен");
    }

    /// Отмена работает по разговору задачи, а не по «текущей» сессии.
    #[tokio::test]
    async fn cancel_resolves_session_by_task_context() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_for_test(dir.path());
        let owner = Owner::from_token("token-alice");

        adapter.send_task_as(owner, task_in_context("t-a", "ctx-a", "раз")).await.unwrap();
        adapter.send_task_as(owner, task_in_context("t-b", "ctx-b", "два")).await.unwrap();

        let canceled = adapter.cancel_task_as(owner, TaskId("t-a".into())).await.unwrap();
        assert_eq!(canceled.context_id.0, "ctx-a");
        assert_eq!(canceled.status.state, TaskState::Canceled);
    }
}
