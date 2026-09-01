//! core/src/agent.rs — traits for the real protocol Request/Response types.

use async_trait::async_trait;
use protocol::a2a::{A2aEvent, AgentCard, Task, TaskId};
use protocol::acp::{
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionId, SessionUpdate,
};

use crate::reply::Reply;

#[async_trait]
pub trait AcpAgent: Send + Sync {
    async fn initialize(&self, req: InitializeRequest) -> anyhow::Result<InitializeResponse>;
    async fn new_session(&self, req: NewSessionRequest) -> anyhow::Result<NewSessionResponse>;

    async fn prompt(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>>;

    /// ADDED (P-20, Part 1 of the streaming roadmap): streaming variant of
    /// prompt(). Default = plain prompt() (backward compatibility: any
    /// code calling prompt_streaming() on an agent without a streaming
    /// implementation gets the old Reply::Complete behavior). An agent that
    /// can stream overrides the method and returns Reply::Streaming(rx).
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        self.prompt(req).await
    }

    /// ACP canon: session/cancel — a notification, no response. That is why
    /// the signature returns (), not a struct with a result.
    async fn cancel(&self, session: SessionId) -> anyhow::Result<()>;

    /// ADDED (found by live test P2-10): bring the agent into a working
    /// state BEFORE anyone reads generation().
    ///
    /// Without this, respawn was lazy: process death was detected
    /// only inside prompt(), i.e. already AFTER the generation check.
    /// The first request with an old contextId could reach the fresh process
    /// with the old sessionId and got "Invalid params" from the agent instead
    /// of an honest ContextLost; the marking only kicked in on the second try.
    async fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// ADDED (audit P2-10): generation number of the agent process.
    /// Changes on restart — the converter uses it to realize that
    /// ACP sessions started earlier no longer exist.
    /// Implementations without restarts return a constant.
    async fn generation(&self) -> u64 {
        0
    }

    /// Whether the agent is alive right now. Used by the adapter cache so as
    /// not to hand out a connection to a dead process.
    async fn is_alive(&self) -> bool {
        true
    }
}

#[async_trait]
pub trait A2aAgent: Send + Sync {
    async fn card(&self) -> anyhow::Result<AgentCard>;

    async fn send_task(&self, task: Task) -> anyhow::Result<Reply<Task, A2aEvent>>;
    async fn get_task(&self, id: TaskId) -> anyhow::Result<Task>;

    /// A2A canon: task/cancel MUST return a Task (not a notification).
    async fn cancel_task(&self, id: TaskId) -> anyhow::Result<Task>;
}
