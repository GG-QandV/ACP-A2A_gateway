//! core/src/task_store.rs
//!
//! Файловое хранилище задач: один JSON-файл на задачу, atomic write через
//! tmp+rename, переживает рестарт процесса.

use std::path::PathBuf;

use protocol::a2a::{Task, TaskId};
use tokio::fs;

pub struct TaskStore {
    base_dir: PathBuf,
}

impl TaskStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into() }
    }

    fn path_for(&self, id: &TaskId) -> PathBuf {
        let safe = sanitize_task_id(&id.0);
        self.base_dir.join(format!("{safe}.json"))
    }

    pub async fn save(&self, task: &Task) -> anyhow::Result<()> {
        fs::create_dir_all(&self.base_dir).await?;

        let final_path = self.path_for(&task.id);
        let tmp_path = final_path.with_extension("json.tmp");

        let json = serde_json::to_vec_pretty(task)?;
        fs::write(&tmp_path, &json).await?;

        fs::rename(&tmp_path, &final_path).await?;
        Ok(())
    }

    pub async fn load(&self, id: &TaskId) -> anyhow::Result<Task> {
        let path = self.path_for(id);
        let bytes = fs::read(&path).await.map_err(|e| {
            anyhow::anyhow!("task {id:?} не найдена в хранилище ({path:?}): {e}")
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn delete(&self, id: &TaskId) -> anyhow::Result<()> {
        let path = self.path_for(id);
        fs::remove_file(&path).await.ok();
        Ok(())
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

        store.save(&task).await.unwrap();
        let loaded = store.load(&task.id).await.unwrap();

        assert_eq!(loaded.id.0, "task-123");
        assert_eq!(loaded.status.state, TaskState::Completed);
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
