//! core/src/supervisor.rs
//!
//! FIXED (audit P2-10): the adapter cache in transport_http did not check
//! process liveness. A dead agent stayed in the cache forever, and every
//! request to it returned an error until a manual gateway restart. With
//! contextId-based conversations the cost grew: it is not one client that breaks,
//! but all of that agent's conversations at once.
//!
//! Here the process is respawned, but — and this is the key point — the respawn is not
//! hidden from the client. Each live process has a generation number;
//! sessions created in a previous generation are marked lost, and
//! the first access to such a conversation receives an explicit
//! `ContextLost` error, not a quiet continuation in an empty session.
//!
//! Without the marker, the client cannot tell "the agent remembers the conversation" from
//! "the agent restarted and does not remember" — for a gateway that trades in
//! conversation continuity, that is unacceptable.

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

/// The conversation broke because the agent process was restarted.
/// A separate type, not a string: the transport must distinguish this from other
/// errors and tell the client the context was lost, not "something went wrong".
#[derive(Debug, thiserror::Error)]
#[error("контекст потерян: процесс агента был перезапущен (поколение {previous} -> {current})")]
pub struct ContextLost {
    pub previous: u64,
    pub current: u64,
}

/// Launch parameters sufficient for a respawn.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub call_timeout: Duration,
    /// The ACP version the gateway declares during the handshake. A number, as in
    /// the protocol — it used to be a string, and the gateway sent "1" to the agent.
    pub protocol_version: protocol::acp::ProtocolVersion,
    /// ADDED (Streaming roadmap Part 2): timeout BEFORE the first chunk of a
    /// stream (call_timeout equivalent for an already-started stream).
    pub first_chunk_timeout: Duration,
    /// ADDED (Streaming roadmap Part 2): timeout BETWEEN chunks of an already
    /// started stream — does not block a long but live stream.
    pub idle_chunk_timeout: Duration,
}

impl SpawnConfig {
    pub const DEFAULT_PROTOCOL_VERSION: protocol::acp::ProtocolVersion =
        protocol::acp::DEFAULT_PROTOCOL_VERSION;
}

struct Current {
    agent: Arc<StdioAcpAgent>,
    /// The initialize response of the current process. The handshake is done ONCE
    /// per process, right after spawn, and its result is reused.
    init: InitializeResponse,
    generation: u64,
    /// Moment of the last spawn — the backoff is held against it, so that an
    /// agent crashing at startup is not restarted in a loop.
    spawned_at: Instant,
}

/// Keeps a live ACP-agent process, respawning it after death.
pub struct SupervisedStdioAgent {
    config: SpawnConfig,
    current: Mutex<Current>,
    generation: AtomicU64,
    respawn_backoff: Duration,
}

/// Minimum pause between spawn attempts. Any shorter, and an agent crashing at
/// startup turns into a restart loop.
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

    /// FIXED (found while dissecting the live test): initialize was called
    /// ONLY from card(), i.e. on an agent.json request. A client going
    /// straight to message/send drove the agent to session/new without a
    /// handshake — a direct ACP violation. After a respawn the fresh process
    /// never got initialize at all, even if the original one did.
    ///
    /// Now the handshake is part of bringing the process up: every live
    /// process is initialized exactly once — the first one, and any
    /// subsequent one.
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
                // The gateway's client capabilities are deliberately empty: it does
                // not implement fs, terminal, or permission requests, so it
                // must not declare them. A correct agent, seeing this,
                // will not send counter-requests.
                client_capabilities: ClientCapabilities::default(),
                client_info: Some(Implementation {
                    name: "acp-a2a-gateway".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                }),
            })
            .await?;

        Ok((Arc::new(agent), init))
    }

    /// Live agent. If the process died — spawns a new one and increments the
    /// generation number; the calling code learns of it via `generation()`.
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
    /// The handshake is already done when the process comes up — sending
    /// initialize again is not allowed, ACP does not assume it. Return the saved
    /// response of the live process.
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

    /// ADDED (P-20): delegates prompt_streaming() to the live process.
    /// Without this, the trait's default method would call self.prompt() (Complete)
    /// — streaming through the supervisor would not work.
    async fn prompt_streaming(
        &self,
        req: PromptRequest,
    ) -> anyhow::Result<Reply<PromptResponse, SessionUpdate>> {
        self.healthy().await?.prompt_streaming(req).await
    }

    async fn cancel(&self, session: SessionId) -> anyhow::Result<()> {
        self.healthy().await?.cancel(session).await
    }

    /// Forces a liveness check and, if needed, a restart —
    /// so the generation() called right after is already current.
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
