//! gatewayd/src/event_log.rs
//!
//! Durable event log buffer for the stream (Phase 2, closing T4/resubscribe).
//!
//! The model is "source of truth = durable history" (as in agent-connector
//! `driver_http_sse.rs`, decisions P-22/P-23): every A2aEvent sent
//! to the client is additionally persisted with a monotonic seq (per task).
//! After a connection drop the client can ask for the last processed
//! seq and request `events_after(after_seq)` — stream continuation.
//!
//! Implementation — a single background writer task that OWNS the
//! `rusqlite::Connection` (sync SQLite in the async world): all operations
//! (append/events_after/last_seq/cleanup) go through a single mpsc channel and
//! are executed sequentially. There are no races by design, no write_guard needed.
//!
//! Self-cleanup: the writer checks the DB size (page_count * page_size) and on
//! exceeding `max_size_mb` removes the oldest events by seq. 0 = no limit.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

pub struct EventLog {
    tx: mpsc::Sender<Cmd>,
}

pub struct EventRecord {
    pub task_id: String,
    pub seq: u64,
    pub event_json: String,
}

enum Cmd {
    Append {
        task_id: String,
        event_json: String,
        reply: oneshot::Sender<anyhow::Result<u64>>,
    },
    EventsAfter {
        task_id: String,
        after_seq: u64,
        limit: u64,
        reply: oneshot::Sender<anyhow::Result<Vec<EventRecord>>>,
    },
    LastSeq {
        task_id: String,
        reply: oneshot::Sender<anyhow::Result<u64>>,
    },
}

const CHANNEL_CAPACITY: usize = 1024;
/// DB size check after every N appends (cheap, page_count is 1 query).
const SIZE_CHECK_EVERY_N_APPENDS: u64 = 64;

impl EventLog {
    /// Opens the DB and spins up the writer task. `max_size_mb == 0` = no limit.
    pub fn spawn(path: PathBuf, max_size_mb: u64) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("event_log: каталог {}: {e}", parent.display()))?;
            }
        }
        let conn = Connection::open(&path)
            .map_err(|e| anyhow::anyhow!("event_log: открыть {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS task_events (
                 task_id  TEXT NOT NULL,
                 seq      INTEGER NOT NULL,
                 event_kind_json TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 PRIMARY KEY (task_id, seq)
             );
             CREATE INDEX IF NOT EXISTS ix_task_events_replay ON task_events(task_id, seq);",
        )
        .map_err(|e| anyhow::anyhow!("event_log: миграция {}: {e}", path.display()))?;

        let (tx, rx) = mpsc::channel::<Cmd>(CHANNEL_CAPACITY);
        let writer = Writer {
            conn,
            rx,
            max_size_mb,
            appends_since_check: 0,
        };
        tokio::spawn(writer.run());
        tracing::info!(path = %path.display(), max_size_mb, "event_log writer запущен");
        Ok(Arc::new(Self { tx }))
    }

    /// Appends an event and returns its monotonic seq for the task.
    pub async fn append(&self, task_id: &str, event_json: &str) -> anyhow::Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Append {
                task_id: task_id.to_string(),
                event_json: event_json.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!("event_log: writer отключился"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("event_log: writer не ответил"))?
    }

    /// Returns events with seq > after_seq, in ascending seq order.
    pub async fn events_after(
        &self,
        task_id: &str,
        after_seq: u64,
        limit: u64,
    ) -> anyhow::Result<Vec<EventRecord>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::EventsAfter {
                task_id: task_id.to_string(),
                after_seq,
                limit,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!("event_log: writer отключился"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("event_log: writer не ответил"))?
    }

    /// The last seq for a task (0 if there are no events). The client asks
    /// "what is the last marker" before reconnect.
    pub async fn last_seq(&self, task_id: &str) -> anyhow::Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::LastSeq {
                task_id: task_id.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!("event_log: writer отключился"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("event_log: writer не ответил"))?
    }
}

struct Writer {
    conn: Connection,
    rx: mpsc::Receiver<Cmd>,
    max_size_mb: u64,
    appends_since_check: u64,
}

impl Writer {
    async fn run(mut self) {
        // Connection is not Send — we keep it in this task forever.
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                Cmd::Append {
                    task_id,
                    event_json,
                    reply,
                } => {
                    let res = self.do_append(&task_id, &event_json);
                    let _ = reply.send(res);
                }
                Cmd::EventsAfter {
                    task_id,
                    after_seq,
                    limit,
                    reply,
                } => {
                    let res = self.do_events_after(&task_id, after_seq, limit);
                    let _ = reply.send(res);
                }
                Cmd::LastSeq { task_id, reply } => {
                    let res = self.do_last_seq(&task_id);
                    let _ = reply.send(res);
                }
            }
        }
        tracing::warn!("event_log writer завершился: канал команд закрыт");
    }

    fn do_append(&mut self, task_id: &str, event_json: &str) -> anyhow::Result<u64> {
        let seq = self.next_seq(task_id)?;
        let created_at = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO task_events (task_id, seq, event_kind_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![task_id, seq as i64, event_json, created_at],
            )
            .map_err(|e| anyhow::anyhow!("event_log: insert: {e}"))?;

        self.appends_since_check += 1;
        if self.max_size_mb > 0 && self.appends_since_check >= SIZE_CHECK_EVERY_N_APPENDS {
            self.appends_since_check = 0;
            self.cleanup_if_oversized();
        }
        Ok(seq)
    }

    fn next_seq(&self, task_id: &str) -> anyhow::Result<u64> {
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM task_events WHERE task_id = ?1",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("event_log: max seq: {e}"))?;
        Ok((seq + 1) as u64)
    }

    fn do_events_after(
        &self,
        task_id: &str,
        after_seq: u64,
        limit: u64,
    ) -> anyhow::Result<Vec<EventRecord>> {
        let limit = limit.min(10_000);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, seq, event_kind_json
                 FROM task_events
                 WHERE task_id = ?1 AND seq > ?2
                 ORDER BY seq
                 LIMIT ?3",
            )
            .map_err(|e| anyhow::anyhow!("event_log: prepare: {e}"))?;
        let rows = stmt
            .query_map(
                rusqlite::params![task_id, after_seq as i64, limit as i64],
                |row| {
                    Ok(EventRecord {
                        task_id: row.get(0)?,
                        seq: row.get::<_, i64>(1)? as u64,
                        event_json: row.get(2)?,
                    })
                },
            )
            .map_err(|e| anyhow::anyhow!("event_log: query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("event_log: read: {e}"))
    }

    fn do_last_seq(&self, task_id: &str) -> anyhow::Result<u64> {
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM task_events WHERE task_id = ?1",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("event_log: max seq: {e}"))?;
        Ok(seq as u64)
    }

    /// If the DB outgrew the limit — remove the oldest events (by seq) until the
    /// size drops back under the threshold. WAL files are counted too: page_count
    /// includes the write-ahead log.
    fn cleanup_if_oversized(&mut self) {
        let size_bytes: i64 = self
            .conn
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .unwrap_or(-1);
        if size_bytes <= 0 || size_bytes as u64 <= self.max_size_mb * 1024 * 1024 {
            return;
        }
        let deleted = self
            .conn
            .execute(
                "DELETE FROM task_events
                 WHERE (task_id, seq) IN (
                     SELECT task_id, seq FROM task_events
                     ORDER BY seq
                     LIMIT ?1
                 )",
                rusqlite::params![self.max_size_mb.min(100_000) as i64],
            )
            .unwrap_or(0);
        tracing::warn!(
            max_size_mb = self.max_size_mb,
            deleted,
            "event_log: БД превысила лимит — удалены старейшие события"
        );
    }
}

fn now_iso8601() -> String {
    // Time as in core/src/util: local time without external crates.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}

/// Returns true when there is no longer a gap between events — whether the
/// client needs deduplication. Helper utility for Phase 3 (catch-up).
pub fn is_contiguous(prev: u64, next: u64) -> bool {
    prev == 0 || next == prev + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn append_assigns_monotonic_seq_per_task() {
        let dir = tempdir().unwrap();
        let log = EventLog::spawn(dir.path().join("ev.db"), 0).unwrap();

        let s1 = log.append("task-1", r#"{"k":1}"#).await.unwrap();
        let s2 = log.append("task-1", r#"{"k":2}"#).await.unwrap();
        let s3 = log.append("task-1", r#"{"k":3}"#).await.unwrap();
        assert_eq!((s1, s2, s3), (1, 2, 3));

        // seqs are numbered independently for another task.
        let other = log.append("task-2", r#"{"k":1}"#).await.unwrap();
        assert_eq!(other, 1);

        assert_eq!(log.last_seq("task-1").await.unwrap(), 3);
        assert_eq!(log.last_seq("task-2").await.unwrap(), 1);
        assert_eq!(log.last_seq("nope").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn events_after_replays_in_order_exclusive_of_after_seq() {
        let dir = tempdir().unwrap();
        let log = EventLog::spawn(dir.path().join("ev.db"), 0).unwrap();
        for i in 1..=5 {
            log.append("t", &format!(r#"{{"n":{i}}}"#)).await.unwrap();
        }

        let evs = log.events_after("t", 2, 100).await.unwrap();
        let seqs: Vec<u64> = evs.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);

        let tail = log.events_after("t", 5, 100).await.unwrap();
        assert!(tail.is_empty(), "после последнего seq ничего нет");

        let limited = log.events_after("t", 0, 2).await.unwrap();
        let seqs: Vec<u64> = limited.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2], "limit соблюдается");
    }

    #[tokio::test]
    async fn cleanup_removes_oldest_events_when_over_limit() {
        let dir = tempdir().unwrap();
        // max_size_mb=1 — a 1 MB threshold; we append enough events
        // for the DB size to exceed it.
        let log = EventLog::spawn(dir.path().join("ev.db"), 1).unwrap();
        let big = format!(r#"{{"payload":"{}"}}"#, "x".repeat(8 * 1024));
        for _i in 1..=300 {
            log.append("t", &big).await.unwrap();
        }

        // After cleanup, not all 300 events should remain in the DB.
        let all = log.events_after("t", 0, 10_000).await.unwrap();
        assert!(
            all.len() < 300,
            "самоочистка должна была удалить старейшие, осталось {}",
            all.len()
        );
        // Order is preserved: seq strictly increases from the surviving minimum.
        let seqs: Vec<u64> = all.iter().map(|e| e.seq).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
    }
}
