//! gatewayd/src/journal.rs
//!
//! Durable-журнал событий для пользователя (Фаза 5): health-алерты,
//! обрывы стримов, апрувы. Пишется одним фоновым writer-таском, который
//! владеет `rusqlite::Connection` (sync SQLite в async-мире) — та же
//! модель, что в `event_log.rs`: все операции идут через единый
//! mpsc-канал и исполняются последовательно, гонок нет.
//!
//! Очистка: по `retention_days` (удаляются события старше TTL) и по
//! `max_size_mb` (если размер БД перерос лимит — удаляются старейшие).
//! 0 = соответствующий лимит отключён.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

/// Уровень события журнала — зеркалит уровни tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

pub struct Journal {
    tx: mpsc::Sender<Cmd>,
}

/// Запись журнала. `seq` — монотонный глобальный номер (по журналу в целом).
pub struct JournalRecord {
    pub seq: u64,
    pub ts: String,
    pub level: String,
    pub category: String,
    pub message: String,
}

enum Cmd {
    Append {
        level: Level,
        category: String,
        message: String,
        reply: oneshot::Sender<anyhow::Result<u64>>,
    },
    Recent {
        limit: u64,
        reply: oneshot::Sender<anyhow::Result<Vec<JournalRecord>>>,
    },
}

const CHANNEL_CAPACITY: usize = 1024;

impl Journal {
    /// Открывает БД и поднимает writer-таск. `max_size_mb == 0` и
    /// `retention_days == 0` — соответствующие лимиты отключены.
    pub fn spawn(
        path: PathBuf,
        max_size_mb: u64,
        retention_days: u64,
    ) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("journal: каталог {}: {e}", parent.display()))?;
            }
        }
        let conn = Connection::open(&path)
            .map_err(|e| anyhow::anyhow!("journal: открыть {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS journal_events (
                 id       INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts       TEXT NOT NULL,
                 level    TEXT NOT NULL,
                 category TEXT NOT NULL,
                 message  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS ix_journal_ts ON journal_events(ts);",
        )
        .map_err(|e| anyhow::anyhow!("journal: миграция {}: {e}", path.display()))?;

        let (tx, rx) = mpsc::channel::<Cmd>(CHANNEL_CAPACITY);
        let mut writer = Writer {
            conn,
            rx,
            max_size_mb,
            retention_days,
        };
        // При старте сразу чистим (могло переполниться за время простоя).
        writer.cleanup();
        tokio::spawn(writer.run());
        tracing::info!(path = %path.display(), max_size_mb, retention_days, "journal writer запущен");
        Ok(Arc::new(Self { tx }))
    }

    /// Дописывает событие и возвращает его монотонный seq.
    pub async fn append(
        &self,
        level: Level,
        category: &str,
        message: &str,
    ) -> anyhow::Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Append {
                level,
                category: category.to_string(),
                message: message.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!("journal: writer отключился"))?;
        rx.await.map_err(|_| anyhow::anyhow!("journal: writer не ответил"))?
    }

    /// Последние N записей, по убыванию seq (для CLI-просмотра).
    pub async fn recent(&self, limit: u64) -> anyhow::Result<Vec<JournalRecord>> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Recent {
                limit: limit.min(1000),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!("journal: writer отключился"))?;
        rx.await.map_err(|_| anyhow::anyhow!("journal: writer не ответил"))?
    }
}

/// Фильтры запроса журнала для CLI-просмотра. Все опциональны.
#[derive(Debug, Default, Clone)]
pub struct JournalFilter {
    pub limit: Option<u64>,
    pub level: Option<String>,
    pub category: Option<String>,
    /// Только записи с ts >= (now - since_secs).
    pub since_secs: Option<u64>,
}

/// Read-only запрос журнала напрямую из файла БД (не через writer-таск).
/// Используется CLI-просмотром (`gatewayd --journal`), когда основной
/// gateway может быть и остановлен. Открывает отдельное соединение.
pub fn query_recent(path: &Path, filter: &JournalFilter) -> anyhow::Result<Vec<JournalRecord>> {
    let conn = Connection::open(path)
        .map_err(|e| anyhow::anyhow!("journal: открыть {}: {e}", path.display()))?;
    // Читаем только; никогда не пишем в БД работающего gateway.
    conn.pragma_update(None, "query_only", true)?;

    let mut sql = String::from(
        "SELECT id, ts, level, category, message FROM journal_events WHERE 1=1",
    );
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(level) = &filter.level {
        sql.push_str(" AND level = ?");
        params.push(level.clone().into());
    }
    if let Some(category) = &filter.category {
        sql.push_str(" AND category = ?");
        params.push(category.clone().into());
    }
    if let Some(since) = filter.since_secs {
        let cutoff = now_unix().saturating_sub(since);
        sql.push_str(" AND CAST(ts AS INTEGER) >= ?");
        params.push((cutoff as i64).into());
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    params.push((filter.limit.unwrap_or(20).clamp(1, 1000) as i64).into());

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| anyhow::anyhow!("journal: prepare: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(JournalRecord {
                seq: row.get::<_, i64>(0)? as u64,
                ts: row.get(1)?,
                level: row.get(2)?,
                category: row.get(3)?,
                message: row.get(4)?,
            })
        })
        .map_err(|e| anyhow::anyhow!("journal: query: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("journal: read: {e}"))
}

struct Writer {
    conn: Connection,
    rx: mpsc::Receiver<Cmd>,
    max_size_mb: u64,
    retention_days: u64,
}

impl Writer {
    async fn run(mut self) {
        // Connection не Send — остаётся в этом таске навсегда.
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                Cmd::Append {
                    level,
                    category,
                    message,
                    reply,
                } => {
                    let res = self.do_append(level, &category, &message);
                    let _ = reply.send(res);
                }
                Cmd::Recent { limit, reply } => {
                    let res = self.do_recent(limit);
                    let _ = reply.send(res);
                }
            }
        }
        tracing::warn!("journal writer завершился: канал команд закрыт");
    }

    fn do_append(&mut self, level: Level, category: &str, message: &str) -> anyhow::Result<u64> {
        let ts = now_unix_secs();
        self.conn
            .execute(
                "INSERT INTO journal_events (ts, level, category, message)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![ts, level.as_str(), category, message],
            )
            .map_err(|e| anyhow::anyhow!("journal: insert: {e}"))?;

        // Каждый аппенд после вставки — быстрая проверка лимитов
        // (2 лёгких запроса; аппенды в журнал редкие: алерты раз в минуты).
        self.cleanup();
        self.last_seq()
    }

    fn last_seq(&self) -> anyhow::Result<u64> {
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM journal_events",
                [],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("journal: max id: {e}"))?;
        Ok(seq as u64)
    }

    fn do_recent(&self, limit: u64) -> anyhow::Result<Vec<JournalRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, ts, level, category, message
                 FROM journal_events
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(|e| anyhow::anyhow!("journal: prepare: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(JournalRecord {
                    seq: row.get::<_, i64>(0)? as u64,
                    ts: row.get(1)?,
                    level: row.get(2)?,
                    category: row.get(3)?,
                    message: row.get(4)?,
                })
            })
            .map_err(|e| anyhow::anyhow!("journal: query: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("journal: read: {e}"))
    }

    /// TTL-очистка (retention_days) затем, при переполнении размера —
    /// удаление старейших. WAL-файлы учитываются: page_count включает
    /// журнал активных записей.
    fn cleanup(&mut self) {
        if self.retention_days > 0 {
            let cutoff = now_unix() - self.retention_days * 24 * 60 * 60;
            match self.conn.execute(
                "DELETE FROM journal_events WHERE ts < ?1",
                rusqlite::params![cutoff],
            ) {
                Ok(deleted) if deleted > 0 => {
                    tracing::info!(deleted, retention_days = self.retention_days, "journal: удалены события старше TTL");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "journal: TTL-очистка не удалась"),
            }
        }
        if self.max_size_mb == 0 {
            return;
        }
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
                "DELETE FROM journal_events
                 WHERE id IN (
                     SELECT id FROM journal_events
                     ORDER BY id
                     LIMIT ?1
                 )",
                rusqlite::params![self.max_size_mb.min(100_000) as i64],
            )
            .unwrap_or(0);
        tracing::warn!(
            max_size_mb = self.max_size_mb,
            deleted,
            "journal: БД превысила лимит — удалены старейшие события"
        );
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_unix_secs() -> String {
    format!("{}", now_unix())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn query_recent_filters_by_level_category_and_since() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("j.db");
        let journal = Journal::spawn(db_path.clone(), 0, 0).unwrap();
        journal.append(Level::Info, "health", "summary1").await.unwrap();
        journal.append(Level::Warn, "health", "warn1").await.unwrap();
        journal.append(Level::Error, "stream", "err1").await.unwrap();

        let no_filter = query_recent(&db_path, &JournalFilter::default()).unwrap();
        assert_eq!(no_filter.len(), 3);

        let errors = query_recent(
            &db_path,
            &JournalFilter {
                level: Some("error".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].category, "stream");

        let stream = query_recent(
            &db_path,
            &JournalFilter {
                category: Some("stream".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stream.len(), 1);

        // Old entry (200 days back) inserted directly via a second connection;
        // writer holds the only Connection, but WAL allows parallel reads.
        let old_ts = now_unix() - 200 * 24 * 60 * 60;
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO journal_events (ts, level, category, message)
                 VALUES (?1, 'info', 'health', 'old')",
                rusqlite::params![old_ts.to_string()],
            )
            .unwrap();
        }

        // since=100 days keeps the 3 fresh entries, drops the 200-day-old one.
        let since_far = query_recent(
            &db_path,
            &JournalFilter {
                since_secs: Some(100 * 24 * 60 * 60),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(since_far.len(), 3, "old entry must be filtered by since");

        // since=1 hour also keeps the fresh entries (they were just written).
        let since_hour = query_recent(
            &db_path,
            &JournalFilter {
                since_secs: Some(3600),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(since_hour.len(), 3);

        let limit = query_recent(
            &db_path,
            &JournalFilter {
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(limit.len(), 2);
    }

    #[tokio::test]
    async fn append_assigns_monotonic_seq() {
        let dir = tempdir().unwrap();
        let journal = Journal::spawn(dir.path().join("j.db"), 0, 0).unwrap();

        let s1 = journal.append(Level::Info, "health", "ok").await.unwrap();
        let s2 = journal.append(Level::Warn, "health", "warn").await.unwrap();
        let s3 = journal.append(Level::Error, "stream", "err").await.unwrap();
        assert_eq!((s1, s2, s3), (1, 2, 3));

        let recents = journal.recent(10).await.unwrap();
        assert_eq!(recents.len(), 3);
        assert_eq!(recents[0].seq, 3, "recent возвращает по убыванию seq");
        assert_eq!(recents[0].level, "error");
        assert_eq!(recents[0].category, "stream");
    }

    #[tokio::test]
    async fn recent_honors_limit() {
        let dir = tempdir().unwrap();
        let journal = Journal::spawn(dir.path().join("j.db"), 0, 0).unwrap();
        for _ in 0..10 {
            journal.append(Level::Info, "health", "x").await.unwrap();
        }
        let recents = journal.recent(3).await.unwrap();
        assert_eq!(recents.len(), 3);
    }

    #[tokio::test]
    async fn ttl_cleanup_removes_old_entries() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("j.db");
        let journal = Journal::spawn(db_path.clone(), 0, 1).unwrap();

        // Вставляем запись «старше TTL» (2 дня назад) напрямую вторым
        // соединением — writer один, но WAL позволяет параллельное чтение.
        let old_ts = now_unix() - 2 * 24 * 60 * 60;
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO journal_events (ts, level, category, message)
                 VALUES (?1, 'info', 'health', 'old')",
                rusqlite::params![old_ts.to_string()],
            )
            .unwrap();
        }

        // Свежая запись — при ней cleanup удаляет старые (retention=1 день).
        journal
            .append(Level::Info, "health", "fresh")
            .await
            .unwrap();

        let all = journal.recent(100).await.unwrap();
        assert_eq!(all.len(), 1, "запись старше TTL должна быть удалена");
        assert_eq!(all[0].message, "fresh");
    }

    #[tokio::test]
    async fn size_cleanup_removes_oldest_when_over_limit() {
        let dir = tempdir().unwrap();
        // max_size_mb=1 — набиваем крупными сообщениями до превышения.
        let journal = Journal::spawn(dir.path().join("j.db"), 1, 0).unwrap();
        let big = "x".repeat(8 * 1024);
        for i in 0..300 {
            journal
                .append(Level::Info, "health", &format!("{big}{i}"))
                .await
                .unwrap();
        }
        let all = journal.recent(1000).await.unwrap();
        assert!(
            all.len() < 300,
            "самоочистка должна была удалить старейшие, осталось {}",
            all.len()
        );
        assert_eq!(
            all[0].seq,
            all[1].seq + 1,
            "остались самые свежие (по убыванию seq)"
        );
    }
}