//! core/src/agent.rs — trait'ы под реальные Request/Response типы протоколов.

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

    /// ДОБАВЛЕНО (Р-20, Часть 1 роадмапа стриминга): потоковый вариант
    /// prompt(). Дефолт = обычный prompt() (обратная совместимость: любой
    /// код, зовущий prompt_streaming() на агенте без реализации стриминга,
    /// получает старое поведение Reply::Complete). Агент, умеющий стримить,
    /// переопределяет метод и возвращает Reply::Streaming(rx).
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        self.prompt(req).await
    }

    /// ACP-канон: session/cancel — notification, без ответа. Поэтому
    /// сигнатура возвращает (), а не структуру с результатом.
    async fn cancel(&self, session: SessionId) -> anyhow::Result<()>;

    /// ДОБАВЛЕНО (найдено live-тестом P2-10): привести агента в рабочее
    /// состояние ДО того, как кто-то прочитает generation().
    ///
    /// Без этого respawn был ленивым: смерть процесса обнаруживалась
    /// только внутри prompt(), то есть уже ПОСЛЕ сверки поколений.
    /// Первый запрос со старым contextId успевал уйти в свежий процесс
    /// со старым sessionId и получал от агента «Invalid params» вместо
    /// честного ContextLost; пометка срабатывала лишь со второго раза.
    async fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }

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
