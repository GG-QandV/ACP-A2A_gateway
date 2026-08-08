//! core/src/lease.rs
//!
//! TurnLease: сериализация promptов на одну сессию. Без него два
//! одновременных session/prompt (или task/send) к одному ACP-процессу
//! перемешивают его stdin/stdout поток.
//!
//! Fail-closed: если лиз не получен за timeout, вызывающий код НЕ входит
//! в критическую секцию (в отличие от тихого зависания).

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

/// RAII guard: лиз освобождается автоматически при Drop, даже если
/// вызывающий код вернётся через `?` на середине обработки.
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

    /// Число сессий, за которыми сейчас числится лиз. Нужно, чтобы
    /// утечку можно было измерить в тесте, а не только рассуждать о ней.
    pub async fn tracked_sessions(&self) -> usize {
        self.locks.lock().await.len()
    }

    /// Вызывается при закрытии сессии, чтобы не накапливать записи в HashMap.
    pub async fn forget(&self, session: &SessionId) {
        self.locks.lock().await.remove(session);
    }
}

// ИСПРАВЛЕНО (найдено компилятором, E0119): ручной
// `impl From<TurnLeaseTimeoutError> for anyhow::Error` конфликтовал с
// blanket-impl из anyhow (`impl<E: StdError + Send + Sync> From<E>`).
// TurnLeaseTimeoutError уже реализует StdError через thiserror, поэтому
// конверсия через `?` работает и без этого impl — он просто лишний.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Тесты из docs/03-dev-guide-testing.md — в репо отсутствовали.
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

        // Fail-closed: ошибка, а не паника и не тихий проход в критическую секцию.
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
