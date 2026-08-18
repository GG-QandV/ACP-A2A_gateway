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
    ClientCapabilities, Implementation, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionId, SessionUpdate,
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
    /// Версия ACP, заявляемая шлюзом при рукопожатии. Число, как в
    /// протоколе — раньше была строка, и шлюз слал агенту "1".
    pub protocol_version: protocol::acp::ProtocolVersion,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут ДО первого чанка
    /// стрима (эквивалент call_timeout для уже начавшегося потока).
    pub first_chunk_timeout: Duration,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут МЕЖДУ чанками
    /// уже начатого стрима — не блокирует долгий, но живой поток.
    pub idle_chunk_timeout: Duration,
}

impl SpawnConfig {
    pub const DEFAULT_PROTOCOL_VERSION: protocol::acp::ProtocolVersion =
        protocol::acp::DEFAULT_PROTOCOL_VERSION;
}

struct Current {
    agent: Arc<StdioAcpAgent>,
    /// Ответ на initialize текущего процесса. Рукопожатие делается ОДИН
    /// раз на процесс, сразу после спавна, и его результат переиспользуется.
    init: InitializeResponse,
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
        let (agent, init) = Self::spawn_and_handshake(&config).await?;

        Ok(Self {
            config,
            current: Mutex::new(Current {
                agent,
                init,
                generation: 1,
                spawned_at: Instant::now(),
            }),
            generation: AtomicU64::new(1),
            respawn_backoff,
        })
    }

    /// ИСПРАВЛЕНО (найдено при разборе live-теста): initialize вызывался
    /// ТОЛЬКО из card(), то есть при запросе agent.json. Клиент, идущий
    /// сразу в message/send, доводил агента до session/new без
    /// рукопожатия — прямое нарушение ACP. После респавна свежий процесс
    /// не получал initialize вообще никогда, даже если исходный получил.
    ///
    /// Теперь рукопожатие — часть подъёма процесса: каждый живой
    /// процесс инициализирован ровно один раз, и первый, и любой
    /// последующий.
    async fn spawn_and_handshake(
        config: &SpawnConfig,
    ) -> anyhow::Result<(Arc<StdioAcpAgent>, InitializeResponse)> {
        let agent = StdioAcpAgent::spawn(
            &config.command,
            &config.cwd,
            &config.env,
            config.call_timeout,
            config.first_chunk_timeout,
            config.idle_chunk_timeout,
        )
        .await?;

        let init = agent
            .initialize(InitializeRequest {
                protocol_version: config.protocol_version,
                // Клиентские возможности шлюза пусты сознательно: fs,
                // терминал и запрос разрешений он не реализует, а значит
                // не должен их заявлять. Корректный агент, увидев это,
                // не станет слать встречные запросы.
                client_capabilities: ClientCapabilities::default(),
                client_info: Some(Implementation {
                    name: "acp-a2a-gateway".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                }),
            })
            .await?;

        Ok((Arc::new(agent), init))
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

        let (fresh, init) = Self::spawn_and_handshake(&self.config).await?;

        current.agent = fresh;
        current.init = init;
        current.generation += 1;
        current.spawned_at = Instant::now();
        self.generation.store(current.generation, Ordering::SeqCst);

        Ok(current.agent.clone())
    }
}

#[async_trait]
impl AcpAgent for SupervisedStdioAgent {
    /// Рукопожатие уже сделано при подъёме процесса — повторно слать
    /// initialize нельзя, ACP этого не предполагает. Отдаём сохранённый
    /// ответ живого процесса.
    async fn initialize(&self, _req: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        self.healthy().await?;
        Ok(self.current.lock().await.init.clone())
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

    /// ДОБАВЛЕНО (Р-20): делегирует prompt_streaming() живому процессу.
    /// Без этого дефолтный метод трейта вызвал бы self.prompt() (Complete)
    /// — стриминг через супервизор не работал бы.
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        self.healthy().await?.prompt_streaming(req).await
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
