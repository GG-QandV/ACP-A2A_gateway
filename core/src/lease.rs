//! core/src/lease.rs
//!
//! TurnLease: serializes prompts for one session. Without it, two
//! concurrent session/prompt (or task/send) calls to one ACP process
//! interleave its stdin/stdout stream.
//!
//! Fail-closed: if the lease is not acquired within the timeout, the caller does
//! NOT enter the critical section (as opposed to hanging silently).

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

/// RAII guard: the lease is released automatically on Drop, even if
/// the caller returns via `?` in the middle of processing.
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

    /// Number of sessions currently tracked with a lease. Needed so the
    /// leak can be measured in a test, not just reasoned about.
    pub async fn tracked_sessions(&self) -> usize {
        self.locks.lock().await.len()
    }

    /// Called when a session closes, so HashMap entries do not accumulate.
    pub async fn forget(&self, session: &SessionId) {
        self.locks.lock().await.remove(session);
    }
}

// FIXED (caught by the compiler, E0119): a manual
// `impl From<TurnLeaseTimeoutError> for anyhow::Error` conflicted with
// the blanket impl from anyhow (`impl<E: StdError + Send + Sync> From<E>`).
// TurnLeaseTimeoutError already implements StdError via thiserror, so
// the `?` conversion works without this impl — it was simply redundant.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Tests from docs/03-dev-guide-testing.md — missing in the repo.
    #[tokio::test]
    async fn second_acquire_waits_for_first_release() {
        let lease = TurnLease::default();
        let session = SessionId("s-1".into());

        let guard = lease.acquire(&session, Duration::from_secs(1)).await.unwrap();
        let start = Instant::now();

        let lease_ref = &lease;
        let session_ref = &session;
        let holder = async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(guard);
        };
        let waiter = async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            lease_ref.acquire(session_ref, Duration::from_secs(1)).await
        };

        let (_, second) = tokio::join!(holder, waiter);
        assert!(second.is_ok());
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn acquire_times_out_instead_of_hanging() {
        let lease = TurnLease::default();
        let session = SessionId("s-2".into());

        let _held = lease.acquire(&session, Duration::from_secs(1)).await.unwrap();
        let result = lease.acquire(&session, Duration::from_millis(50)).await;

        // Fail-closed: an error, not a panic and not a silent pass into the critical section.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn different_sessions_do_not_block_each_other() {
        let lease = TurnLease::default();
        let _a = lease.acquire(&SessionId("a".into()), Duration::from_millis(50)).await.unwrap();
        let b = lease.acquire(&SessionId("b".into()), Duration::from_millis(50)).await;
        assert!(b.is_ok());
    }

    #[tokio::test]
    async fn forget_removes_session_entry() {
        let lease = TurnLease::default();
        let session = SessionId("s-3".into());
        drop(lease.acquire(&session, Duration::from_millis(50)).await.unwrap());
        lease.forget(&session).await;
        assert!(lease.locks.lock().await.is_empty());
    }
}
