//! core/src/lib.rs — финальная версия для MVP: все модули ядра на месте.

pub mod agent;
pub mod convert;
pub mod http_agent;
pub mod lease;
pub mod reply;
pub mod stdio_agent;
pub mod task_store;

pub use agent::{A2aAgent, AcpAgent};
pub use convert::{A2aAsAcp, AcpAsA2a, Owner, DEFAULT_SESSION_TTL};
pub use http_agent::HttpA2aAgent;
pub use lease::{TurnGuard, TurnLease, TurnLeaseTimeoutError};
pub use reply::Reply;
pub use stdio_agent::StdioAcpAgent;
pub use task_store::TaskStore;
