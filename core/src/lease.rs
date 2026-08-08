//! core/src/lease.rs
//!
//! TurnLease: сериализация promptов на одну сессию. Fail-closed: если
//! лиз не получен за timeout, вызывающий код НЕ входит в критическую
//! секцию (в отличие от тихого зависания).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use protocol::acp::SessionId;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::timeout;

#[derive(Debug, thiserror::Error)]
#[error("turn lease timeout for session {0:?} after {1:?}")]
pub struct TurnLeaseTimeoutError(pub SessionId, pub Duration);

#[derive(Default)]
pub struct TurnLease {
    locks: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
}

pub struct TurnGuard(#[allow(dead_code)] OwnedMutexGuard<()>);

impl TurnLease {
    pub async fn acquire(
        &self,
        session: &SessionId,
        wait_budget: Duration,
    ) -> Result<TurnGuard, TurnLeaseTimeoutError> {
        let per_session_lock = {
            let mut locks = self.locks.lock().await;
            locks.entry(session.clone()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
        };

        match timeout(wait_budget, per_session_lock.lock_owned()).await {
            Ok(guard) => Ok(TurnGuard(guard)),
            Err(_) => Err(TurnLeaseTimeoutError(session.clone(), wait_budget)),
        }
    }

    pub async fn forget(&self, session: &SessionId) {
        self.locks.lock().await.remove(session);
    }
}

impl From<TurnLeaseTimeoutError> for anyhow::Error {
    fn from(e: TurnLeaseTimeoutError) -> Self {
        anyhow::anyhow!(e)
    }
}
