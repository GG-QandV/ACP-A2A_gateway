//! gatewayd/src/approvals.rs
//!
//! Approval store for agents discovered in config.yaml. The gateway refuses
//! to serve an agent until it has been explicitly approved via the CLI
//! (`gatewayd --approve <name>`). This is the human gate for new agents.
//!
//! A fingerprint (transport + command/url + cwd) is stored per agent; if the
//! config changes for an already-approved agent, it drops back to `pending`
//! and must be approved again — you approve the exact thing you run.
//!
//! Storage: one SQLite table, same style as `journal.rs` — a separate
//! connection per operation so the CLI can work while the gateway is running.

use std::path::{Path, PathBuf};

use rusqlite::params;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Approved,
    Rejected,
    Pending,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Approved => "approved",
            Status::Rejected => "rejected",
            Status::Pending => "pending",
        }
    }

    fn from_str(s: &str) -> Option<Status> {
        match s {
            "approved" => Some(Status::Approved),
            "rejected" => Some(Status::Rejected),
            "pending" => Some(Status::Pending),
            _ => None,
        }
    }
}

/// One approval row for the CLI listing.
#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub agent_id: String,
    pub fingerprint: String,
    pub status: Status,
    pub decided_at: Option<String>,
    pub last_seen_at: String,
}

pub struct ApprovalStore {
    path: PathBuf,
}

impl ApprovalStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create approvals dir: {}", parent.display()))?;
            }
        }
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("failed to open approvals db: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS approvals (
                 agent_id     TEXT PRIMARY KEY,
                 fingerprint  TEXT NOT NULL,
                 status       TEXT NOT NULL CHECK (status IN ('approved','rejected','pending')),
                 decided_at   TEXT,
                 last_seen_at TEXT NOT NULL
             );",
        )
        .with_context(|| format!("failed to init approvals db: {}", path.display()))?;
        drop(conn);
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn conn(&self) -> anyhow::Result<rusqlite::Connection> {
        Ok(rusqlite::Connection::open(&self.path)?)
    }

    /// Registers the agent on startup. First sight → `pending`. An approved
    /// agent whose fingerprint changed drops back to `pending` (re-approval).
    /// Returns the effective status.
    pub fn register(&self, agent_id: &str, fingerprint: &str) -> anyhow::Result<Status> {
        let now = now_unix_secs();
        let conn = self.conn()?;
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT fingerprint, status FROM approvals WHERE agent_id = ?1",
                params![agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO approvals (agent_id, fingerprint, status, last_seen_at)
                     VALUES (?1, ?2, 'pending', ?3)",
                    params![agent_id, fingerprint, now],
                )?;
                Ok(Status::Pending)
            }
            Some((stored_fp, stored_status)) => {
                if stored_fp != fingerprint {
                    conn.execute(
                        "UPDATE approvals
                         SET fingerprint = ?2, status = 'pending', decided_at = NULL, last_seen_at = ?3
                         WHERE agent_id = ?1",
                        params![agent_id, fingerprint, now],
                    )?;
                    return Ok(Status::Pending);
                }
                conn.execute(
                    "UPDATE approvals SET last_seen_at = ?2 WHERE agent_id = ?1",
                    params![agent_id, now],
                )?;
                Ok(Status::from_str(&stored_status).unwrap_or(Status::Pending))
            }
        }
    }

    /// CLI: approve/reject an agent. Upserts so a brand-new name is accepted.
    pub fn set(&self, agent_id: &str, status: Status) -> anyhow::Result<()> {
        let now = now_unix_secs();
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE approvals
             SET status = ?2, decided_at = ?3, last_seen_at = ?3
             WHERE agent_id = ?1",
            params![agent_id, status.as_str(), now],
        )?;
        if changed == 0 {
            conn.execute(
                "INSERT INTO approvals (agent_id, fingerprint, status, decided_at, last_seen_at)
                 VALUES (?1, '', ?2, ?3, ?3)",
                params![agent_id, status.as_str(), now],
            )?;
        }
        Ok(())
    }

    pub fn list(&self) -> anyhow::Result<Vec<ApprovalRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT agent_id, fingerprint, status, decided_at, last_seen_at
             FROM approvals ORDER BY agent_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ApprovalRecord {
                agent_id: row.get(0)?,
                fingerprint: row.get(1)?,
                status: Status::from_str(&row.get::<_, String>(2)?).unwrap_or(Status::Pending),
                decided_at: row.get(3)?,
                last_seen_at: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn now_unix_secs() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    )
}

use anyhow::Context;
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, ApprovalStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.db");
        let store = ApprovalStore::open(&path).unwrap();
        (dir, store)
    }

    #[test]
    fn new_agent_starts_pending() {
        let (_d, store) = temp_store();
        let st = store.register("claurst-main", "stdio:claurst acp").unwrap();
        assert_eq!(st, Status::Pending);
    }

    #[test]
    fn approve_then_register_returns_approved() {
        let (_d, store) = temp_store();
        store.register("a1", "fp1").unwrap();
        store.set("a1", Status::Approved).unwrap();
        assert_eq!(store.register("a1", "fp1").unwrap(), Status::Approved);
    }

    #[test]
    fn fingerprint_change_drops_back_to_pending() {
        let (_d, store) = temp_store();
        store.register("a1", "stdio:claurst acp").unwrap();
        store.set("a1", Status::Approved).unwrap();
        assert_eq!(
            store.register("a1", "stdio:claurst agent").unwrap(),
            Status::Pending,
            "config change requires re-approval"
        );
    }

    #[test]
    fn reject_keeps_agent_out() {
        let (_d, store) = temp_store();
        store.register("a1", "fp").unwrap();
        store.set("a1", Status::Rejected).unwrap();
        assert_eq!(store.register("a1", "fp").unwrap(), Status::Rejected);
    }

    #[test]
    fn list_returns_sorted_records() {
        let (_d, store) = temp_store();
        store.register("b-agent", "fp").unwrap();
        store.register("a-agent", "fp").unwrap();
        store.set("a-agent", Status::Approved).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].agent_id, "a-agent");
        assert_eq!(list[0].status, Status::Approved);
        assert!(list[0].decided_at.is_some());
        assert_eq!(list[1].agent_id, "b-agent");
    }

    #[test]
    fn set_on_unknown_agent_upserts() {
        let (_d, store) = temp_store();
        store.set("ghost", Status::Approved).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_id, "ghost");
        assert_eq!(list[0].status, Status::Approved);
    }
}