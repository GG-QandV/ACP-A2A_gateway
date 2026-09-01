//! core/src/task_store.rs
//!
//! File-backed task store: backs get_task in AcpAsA2a (see §5.3 of the spec).
//! One JSON file per task, atomic write via tmp+rename, survives
//! a process restart (but not necessarily a machine reboot if base_dir = /tmp).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use protocol::a2a::{Task, TaskId};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::owner::Owner;

/// FIXED (audit P1-2): previously a bare Task was written to disk, and telling
/// whose task it was was impossible — tasks/get served any task
/// to anyone presenting a valid token. Now the task is wrapped
/// in an envelope carrying the owner.
///
/// The owner is in the envelope, not in Task.metadata, on purpose: metadata is visible
/// to the client in the response, and internal attribution does not belong there.
#[derive(Debug, Serialize, Deserialize)]
struct StoredTask {
    #[serde(default)]
    owner: Option<Owner>,
    task: Task,
}

/// A loaded task together with its attribution. `owner == None` means
/// a record written before envelopes were introduced (old file format).
#[derive(Debug)]
pub struct OwnedTask {
    pub task: Task,
    pub owner: Option<Owner>,
}

pub struct TaskStore {
    base_dir: PathBuf,
}

/// How long a task lives in the task store after its last write.
///
/// ADDED: `delete` existed but was never called from anywhere — the code
/// marked that as "left for Phase 1.1". In production such a file per
/// task accumulates forever and fills the disk within weeks, so cleanup
/// is needed right away, not in the next phase.
///
/// A week, not hours: the client is entitled to fetch the result via
/// tasks/get long after the task has completed.
pub const DEFAULT_TASK_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl TaskStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into() }
    }

    fn path_for(&self, id: &TaskId) -> PathBuf {
        let safe = sanitize_task_id(&id.0);
        self.base_dir.join(format!("{safe}.json"))
    }

    pub async fn save(&self, task: &Task, owner: Owner) -> anyhow::Result<()> {
        fs::create_dir_all(&self.base_dir).await?;

        let final_path = self.path_for(&task.id);
        let tmp_path = final_path.with_extension("json.tmp");

        let stored = StoredTask { owner: Some(owner), task: task.clone() };
        let json = serde_json::to_vec_pretty(&stored)?;
        fs::write(&tmp_path, &json).await?;

        // Atomic rename: a reader will never see a partially written file.
        fs::rename(&tmp_path, &final_path).await?;
        Ok(())
    }

    pub async fn load(&self, id: &TaskId) -> anyhow::Result<Task> {
        Ok(self.load_owned(id).await?.task)
    }

    /// The task together with its owner. Old-format files (bare Task,
    /// without an envelope) are read as `owner: None` — otherwise a gateway
    /// update would invalidate the already accumulated task store.
    pub async fn load_owned(&self, id: &TaskId) -> anyhow::Result<OwnedTask> {
        let path = self.path_for(id);
        let bytes = fs::read(&path).await.map_err(|e| {
            anyhow::anyhow!("task {id:?} не найдена в хранилище ({path:?}): {e}")
        })?;

        match serde_json::from_slice::<StoredTask>(&bytes) {
            Ok(stored) => Ok(OwnedTask { task: stored.task, owner: stored.owner }),
            Err(envelope_err) => {
                // Old format: the file contains a Task directly.
                let task: Task = serde_json::from_slice(&bytes).map_err(|legacy_err| {
                    anyhow::anyhow!(
                        "task {id:?}: не разобрать ни как конверт ({envelope_err}), \
                         ни как задачу старого формата ({legacy_err})"
                    )
                })?;
                Ok(OwnedTask { task, owner: None })
            }
        }
    }

    pub async fn delete(&self, id: &TaskId) -> anyhow::Result<()> {
        let path = self.path_for(id);
        fs::remove_file(&path).await.ok();
        Ok(())
    }

    /// Deletes tasks not updated for longer than ttl. Returns the number
    /// of deleted files.
    ///
    /// Age is taken from the file mtime, not from the timestamp inside the task:
    /// mtime is refreshed by the atomic write and does not depend on what
    /// the agent put into the time field (and it may put anything
    /// or nothing at all).
    ///
    /// A missing directory is not an error: it does not exist before the first task.
    pub async fn sweep_expired(&self, ttl: Duration) -> anyhow::Result<usize> {
        let mut dir = match fs::read_dir(&self.base_dir).await {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };

        let now = SystemTime::now();
        let mut removed = 0usize;

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                // .json.tmp — a half-written file from someone else's atomic write;
                // it must not be touched.
                continue;
            }

            let Ok(meta) = entry.metadata().await else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let Ok(age) = now.duration_since(modified) else { continue };

            if age > ttl && fs::remove_file(&path).await.is_ok() {
                removed += 1;
            }
        }

        if removed > 0 {
            tracing::info!(removed, dir = ?self.base_dir, "убраны просроченные задачи");
        }
        Ok(removed)
    }
}

fn sanitize_task_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(128)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::a2a::{ContextId, TaskState, TaskStatus};

    fn sample_task(id: &str) -> Task {
        Task {
            id: TaskId(id.into()),
            context_id: ContextId("ctx-1".into()),
            status: TaskStatus { state: TaskState::Completed, message: None, timestamp: None },
            history: None,
            artifacts: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        let task = sample_task("task-123");

        store.save(&task, Owner::from_token("t-1")).await.unwrap();
        let loaded = store.load(&task.id).await.unwrap();

        assert_eq!(loaded.id.0, "task-123");
        assert_eq!(loaded.status.state, TaskState::Completed);
    }




    /// FOUND by test: sanitize_task_id strips all non-ASCII characters,
    /// so two different non-ASCII identifiers yield ONE file name and
    /// overwrite each other. Currently not reproducible from the outside — ids
    /// are generated by the gateway itself and they are ASCII — but we pin the
    /// behavior so it does not become a hole if ids ever start being accepted
    /// from the client.
    #[test]
    fn non_ascii_ids_collapse_to_same_name() {
        assert_eq!(sanitize_task_id("задача"), sanitize_task_id("другая"));
        assert!(sanitize_task_id("задача").is_empty());
    }

    #[tokio::test]
    async fn sweep_removes_only_expired_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        let owner = Owner::from_token("t-1");

        store.save(&sample_task("task-fresh"), owner).await.unwrap();
        store.save(&sample_task("task-stale"), owner).await.unwrap();

        // Age one file by shifting its mtime into the past.
        let old_path = dir.path().join("task-stale.json");
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30);
        filetime::set_file_mtime(&old_path, filetime::FileTime::from_system_time(long_ago))
            .unwrap();

        let removed = store.sweep_expired(Duration::from_secs(60 * 60 * 24)).await.unwrap();

        assert_eq!(removed, 1);
        assert!(store.load(&TaskId("task-fresh".into())).await.is_ok());
        assert!(store.load(&TaskId("task-stale".into())).await.is_err());
    }

    /// Half-written files from someone else's atomic write must not be touched.
    #[tokio::test]
    async fn sweep_ignores_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());

        let tmp = dir.path().join("half-written.json.tmp");
        tokio::fs::write(&tmp, b"{}").await.unwrap();
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30);
        filetime::set_file_mtime(&tmp, filetime::FileTime::from_system_time(long_ago)).unwrap();

        let removed = store.sweep_expired(Duration::from_secs(1)).await.unwrap();

        assert_eq!(removed, 0);
        assert!(tmp.exists(), ".json.tmp не должен убираться");
    }

    /// Before the first task the directory does not exist — that is not an error.
    #[tokio::test]
    async fn sweep_on_missing_dir_is_not_an_error() {
        let store = TaskStore::new("/tmp/точно-нет-такого-каталога-gateway");
        assert_eq!(store.sweep_expired(Duration::from_secs(1)).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn owner_is_persisted_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        let owner = Owner::from_token("t-alice");

        store.save(&sample_task("task-owned"), owner).await.unwrap();
        let loaded = store.load_owned(&TaskId("task-owned".into())).await.unwrap();

        assert_eq!(loaded.owner, Some(owner));
        assert_eq!(loaded.task.id.0, "task-owned");
    }

    /// Files written before envelopes were introduced must stay readable —
    /// otherwise a gateway update invalidates the accumulated task store.
    #[tokio::test]
    async fn legacy_bare_task_file_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());

        // Write in the old format: bare Task, without an envelope.
        let task = sample_task("task-legacy");
        let path = dir.path().join("task-legacy.json");
        tokio::fs::write(&path, serde_json::to_vec_pretty(&task).unwrap()).await.unwrap();

        let loaded = store.load_owned(&TaskId("task-legacy".into())).await.unwrap();
        assert_eq!(loaded.task.id.0, "task-legacy");
        assert!(loaded.owner.is_none(), "у старой записи владелец неизвестен");
    }

    #[tokio::test]
    async fn corrupted_file_errors_with_both_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        tokio::fs::write(dir.path().join("broken.json"), b"{not json").await.unwrap();

        let err = store.load_owned(&TaskId("broken".into())).await.unwrap_err().to_string();
        assert!(err.contains("конверт"));
    }

    #[tokio::test]
    async fn load_missing_task_errors_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        let result = store.load(&TaskId("nonexistent".into())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn path_traversal_id_is_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());
        let malicious = TaskId("../../etc/passwd".into());
        let path = store.path_for(&malicious);
        assert!(path.starts_with(dir.path()));
    }
}
