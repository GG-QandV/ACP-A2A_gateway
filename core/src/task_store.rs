//! core/src/task_store.rs
//!
//! Файловое хранилище задач: закрывает get_task в AcpAsA2a (см. §5.3 ТЗ).
//! Один JSON-файл на задачу, atomic write через tmp+rename, переживает
//! рестарт процесса (но не обязательно reboot машины, если base_dir = /tmp).

use std::path::PathBuf;

use protocol::a2a::{Task, TaskId};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::owner::Owner;

/// ИСПРАВЛЕНО (аудит P1-2): раньше на диск ложился голый Task, и понять,
/// чья это задача, было невозможно — tasks/get отдавал любую задачу
/// любому предъявителю валидного токена. Теперь задача обёрнута
/// конвертом с владельцем.
///
/// Владелец в конверте, а не в Task.metadata, намеренно: metadata видна
/// клиенту в ответе, а внутренняя атрибуция туда не относится.
#[derive(Debug, Serialize, Deserialize)]
struct StoredTask {
    #[serde(default)]
    owner: Option<Owner>,
    task: Task,
}

/// Прочитанная задача вместе с атрибуцией. `owner == None` означает
/// запись, сделанную до введения конвертов (старый формат файла).
#[derive(Debug)]
pub struct OwnedTask {
    pub task: Task,
    pub owner: Option<Owner>,
}

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

    pub async fn save(&self, task: &Task, owner: Owner) -> anyhow::Result<()> {
        fs::create_dir_all(&self.base_dir).await?;

        let final_path = self.path_for(&task.id);
        let tmp_path = final_path.with_extension("json.tmp");

        let stored = StoredTask { owner: Some(owner), task: task.clone() };
        let json = serde_json::to_vec_pretty(&stored)?;
        fs::write(&tmp_path, &json).await?;

        // Atomic rename: читатель никогда не увидит недописанный файл.
        fs::rename(&tmp_path, &final_path).await?;
        Ok(())
    }

    pub async fn load(&self, id: &TaskId) -> anyhow::Result<Task> {
        Ok(self.load_owned(id).await?.task)
    }

    /// Задача вместе с владельцем. Файлы старого формата (голый Task,
    /// без конверта) читаются как `owner: None` — иначе обновление
    /// шлюза обесценило бы уже накопленное хранилище.
    pub async fn load_owned(&self, id: &TaskId) -> anyhow::Result<OwnedTask> {
        let path = self.path_for(id);
        let bytes = fs::read(&path).await.map_err(|e| {
            anyhow::anyhow!("task {id:?} не найдена в хранилище ({path:?}): {e}")
        })?;

        match serde_json::from_slice::<StoredTask>(&bytes) {
            Ok(stored) => Ok(OwnedTask { task: stored.task, owner: stored.owner }),
            Err(envelope_err) => {
                // Старый формат: файл содержит Task напрямую.
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

    /// Файлы, записанные до введения конвертов, должны читаться —
    /// иначе обновление шлюза обесценивает накопленное хранилище.
    #[tokio::test]
    async fn legacy_bare_task_file_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::new(dir.path());

        // Пишем в старом формате: голый Task, без конверта.
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
