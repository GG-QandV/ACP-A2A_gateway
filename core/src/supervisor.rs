//! core/src/supervisor.rs
//!
//! ИСПРАВЛЕНО (аудит P2-10): кэш адаптеров в transport_http не проверял
//! живость процесса. Умерший агент оставался в кэше навсегда, и все
//! запросы к нему возвращали ошибку до ручного рестарта шлюза. С
//! появлением разговоров на contextId цена выросла: падает не один
//! клиент, а все разговоры этого агента разом.
//!
//! Здесь процесс переспавнивается, но — и это главное — переспавн не
//! прячется от клиента. Каждый живой процесс имеет номер поколения;
//! сессии, заведённые в прошлом поколении, помечаются потерянными, и
//! первое обращение к такому разговору получает явную ошибку
//! `ContextLost`, а не тихое продолжение в пустой сессии.
//!
//! Без пометки клиент не может отличить «агент помнит разговор» от
//! «агент перезапустился и не помнит» — для шлюза, который торгует
//! непрерывностью разговора, это неприемлемо.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use protocol::acp::{
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, SessionId, SessionUpdate,
};
use tokio::sync::Mutex;

use crate::agent::AcpAgent;
use crate::reply::Reply;
use crate::stdio_agent::StdioAcpAgent;

/// Разговор оборвался, потому что процесс агента был перезапущен.
/// Отдельный тип, а не строка: транспорт обязан отличить это от прочих
/// ошибок и сказать клиенту, что контекст потерян, а не «что-то пошло не так».
#[derive(Debug, thiserror::Error)]
#[error("контекст потерян: процесс агента был перезапущен (поколение {previous} -> {current})")]
pub struct ContextLost {
    pub previous: u64,
    pub current: u64,
}

/// Параметры запуска, которых достаточно для повторного спавна.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub call_timeout: Duration,
}

struct Current {
    agent: Arc<StdioAcpAgent>,
    generation: u64,
    /// Момент последнего спавна — по нему выдерживается backoff, чтобы
    /// агент, падающий на старте, не перезапускался в цикле.
    spawned_at: Instant,
}

/// Держит живой процесс ACP-агента, переспавнивая его после смерти.
pub struct SupervisedStdioAgent {
    config: SpawnConfig,
    current: Mutex<Current>,
    generation: AtomicU64,
    respawn_backoff: Duration,
}

/// Минимальная пауза между попытками спавна. Меньше — и падающий на
/// старте агент превращается в цикл перезапусков.
pub const DEFAULT_RESPAWN_BACKOFF: Duration = Duration::from_secs(5);

impl SupervisedStdioAgent {
    pub async fn spawn(config: SpawnConfig) -> anyhow::Result<Self> {
        Self::spawn_with_backoff(config, DEFAULT_RESPAWN_BACKOFF).await
    }

    pub async fn spawn_with_backoff(
        config: SpawnConfig,
        respawn_backoff: Duration,
    ) -> anyhow::Result<Self> {
        let agent = StdioAcpAgent::spawn(
            &config.command,
            &config.cwd,
            &config.env,
            config.call_timeout,
        )
        .await?;

        Ok(Self {
            config,
            current: Mutex::new(Current {
                agent: Arc::new(agent),
                generation: 1,
                spawned_at: Instant::now(),
            }),
            generation: AtomicU64::new(1),
            respawn_backoff,
        })
    }

    /// Живой агент. Если процесс умер — поднимает новый и увеличивает
    /// номер поколения; вызывающий код узнаёт об этом по `generation()`.
    async fn healthy(&self) -> anyhow::Result<Arc<StdioAcpAgent>> {
        let mut current = self.current.lock().await;

        if current.agent.is_alive().await {
            return Ok(current.agent.clone());
        }

        let since_spawn = Instant::now().duration_since(current.spawned_at);
        if since_spawn < self.respawn_backoff {
            anyhow::bail!(
                "процесс агента мёртв, повторный запуск не раньше чем через {:?}",
                self.respawn_backoff - since_spawn
            );
        }

        tracing::warn!(
            generation = current.generation,
            "процесс агента мёртв, перезапуск; разговоры прошлого поколения будут помечены потерянными"
        );

        let fresh = StdioAcpAgent::spawn(
            &self.config.command,
            &self.config.cwd,
            &self.config.env,
            self.config.call_timeout,
        )
        .await?;

        current.agent = Arc::new(fresh);
        current.generation += 1;
        current.spawned_at = Instant::now();
        self.generation.store(current.generation, Ordering::SeqCst);

        Ok(current.agent.clone())
    }
}

#[async_trait]
impl AcpAgent for SupervisedStdioAgent {
    async fn initialize(&self, req: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        self.healthy().await?.initialize(req).await
    }

    async fn new_session(&self, req: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
        self.healthy().await?.new_session(req).await
    }

    async fn prompt(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        self.healthy().await?.prompt(req).await
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        self.healthy().await?.cancel(session).await
    }

    /// Форсирует проверку живости и, при необходимости, перезапуск —
    /// поэтому вызванный следом generation() уже актуален.
    async fn ensure_ready(&self) -> anyhow::Result<()> {
        self.healthy().await.map(|_| ())
    }

    async fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    async fn is_alive(&self) -> bool {
        self.current.lock().await.agent.is_alive().await
    }
}
