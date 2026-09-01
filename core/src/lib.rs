//! core/src/lib.rs — final version for MVP: all core modules are in place.

pub mod agent;
pub mod convert;
pub mod http_agent;
pub mod lease;
pub mod owner;
pub mod reply;
pub mod stdio_agent;
pub mod supervisor;
pub mod task_store;

pub use agent::{A2aAgent, AcpAgent};
pub use convert::{A2aAsAcp, AcpAsA2a, DEFAULT_SESSION_TTL};
pub use owner::Owner;
pub use http_agent::HttpA2aAgent;
pub use lease::{TurnGuard, TurnLease, TurnLeaseTimeoutError};
pub use reply::Reply;
pub use stdio_agent::StdioAcpAgent;
pub use supervisor::{ContextLost, SpawnConfig, SupervisedStdioAgent, DEFAULT_RESPAWN_BACKOFF};
pub use task_store::TaskStore;
