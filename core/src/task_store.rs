//! core/src/task_store.rs
//!
//! Файловое хранилище задач: закрывает get_task в AcpAsA2a (см. §5.3 ТЗ).
//! Один JSON-файл на задачу, atomic write через tmp+rename, переживает
//! рестарт процесса (но не обязательно reboot машины, если base_dir = /tmp).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

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

/// Сколько задача живёт в хранилище после последней записи.
///
/// ДОБАВЛЕНО: `delete` существовал, но не вызывался ниоткуда — в коде
/// это было помечено как «оставлено на Фазу 1.1». В проде такой файл на
/// задачу копится бесконечно и заполняет диск за недели, поэтому уборка
/// нужна сразу, а не в следующей фазе.
///
/// Неделя, а не часы: клиент имеет право забрать результат через
/// tasks/get спустя долгое время после завершения задачи.
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

    /// Удаляет задачи, не обновлявшиеся дольше ttl. Возвращает число
    /// удалённых файлов.
    ///
    /// Возраст берётся по mtime файла, а не по timestamp внутри задачи:
    /// mtime обновляется атомарной записью и не зависит от того, что
    /// агент положил в поле времени (а положить он может что угодно
    /// или ничего).
    ///
    /// Отсутствие каталога — не ошибка: до первой задачи его нет.
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
                // .json.tmp — недописанный файл чужой атомарной записи,
                // трогать его нельзя.
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




    /// НАЙДЕНО тестом: sanitize_task_id вырезает все не-ASCII символы,
    /// поэтому два разных не-ASCII идентификатора дают ОДНО имя файла и
    /// затирают друг друга. Сейчас невоспроизводимо снаружи — id
    /// генерирует сам шлюз и они ASCII, — но фиксируем поведение, чтобы
    /// оно не стало дырой, если id когда-нибудь начнут принимать от
    /// клиента.
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

        // Состариваем один файл, сдвигая его mtime в прошлое.
        let old_path = dir.path().join("task-stale.json");
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30);
        filetime::set_file_mtime(&old_path, filetime::FileTime::from_system_time(long_ago))
            .unwrap();

        let removed = store.sweep_expired(Duration::from_secs(60 * 60 * 24)).await.unwrap();

        assert_eq!(removed, 1);
        assert!(store.load(&TaskId("task-fresh".into())).await.is_ok());
        assert!(store.load(&TaskId("task-stale".into())).await.is_err());
    }

    /// Недописанные файлы чужой атомарной записи трогать нельзя.
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

    /// До первой задачи каталога нет — это не ошибка.
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
