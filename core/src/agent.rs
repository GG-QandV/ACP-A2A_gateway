//! core/src/agent.rs — trait'ы под реальные Request/Response типы протоколов.

use async_trait::async_trait;
use protocol::acp::{
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionUpdate,
};
use protocol::a2a::{AgentCard, Task, TaskId, A2aEvent};

use crate::reply::Reply;

#[async_trait]
pub trait AcpAgent: Send + Sync {
    async fn initialize(&self, req: InitializeRequest) -> anyhow::Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> anyhow::Result<NewSessionResponse>;

    async fn prompt(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>>;

    /// ACP-канон: session/cancel — notification, без ответа. Поэтому
    /// сигнатура возвращает (), а не структуру с результатом.
    async fn cancel(&self, session: SessionId) -> anyhow::Result<()>;

    /// ДОБАВЛЕНО (аудит P2-10): номер поколения процесса агента.
    /// Меняется при перезапуске — по нему конвертер понимает, что
    /// заведённые раньше ACP-сессии больше не существуют.
    /// Реализации без перезапуска возвращают константу.
    async fn generation(&self) -> u64 {
        0
    }

    /// Жив ли агент прямо сейчас. Используется кэшом адаптеров, чтобы
    /// не отдавать соединение к мёртвому процессу.
    async fn is_alive(&self) -> bool {
        true
    }
}

#[async_trait]
pub trait A2aAgent: Send + Sync {
    async fn card(&self) -> anyhow::Result<AgentCard>;

    async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, A2aEvent>>;
    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task>;

    /// A2A-канон: task/cancel ДОЛЖЕН вернуть Task (не notification).
    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task>;
}
