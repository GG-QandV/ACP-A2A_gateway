//! gatewayd/src/config.rs
//! Буферный конфиг (Фаза 1): секции `event_log` и `task_store` локального
//! durable-хранилища стриминга. Вынесены в lib, чтобы интеграционные тесты
//! (gatewayd/tests/) могли парсить конфиг без запуска бинаря.

use std::path::PathBuf;

use serde::Deserialize;

/// Секция `event_log:` — durable-буфер событий стрима (источник истины
/// для `tasks/resubscribe`, см. Фаза 2.1 / T4). Отсутствие секции =
/// выключено (прежнее поведение: только эфемерный канал).
#[derive(Debug, Clone, Deserialize)]
pub struct EventLogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sqlite")]
    pub storage_backend: String,
    #[serde(default = "default_event_log_path")]
    pub storage_path: PathBuf,
    /// Потолок размера файла БД в МБ. По достижении — самоочистка
    /// старейших событий по seq (Фаза 2, writer-задача). 0 = без лимита.
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
}

/// Секция `task_store:` — durable-хранилище задач. Отсутствие секции =
/// прежнее файловое хранилище (core/src/task_store.rs).
#[derive(Debug, Clone, Deserialize)]
pub struct TaskStoreConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sqlite")]
    pub storage_backend: String,
    #[serde(default = "default_task_store_path")]
    pub storage_path: PathBuf,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
}

/// ДОБАВЛЕНО (Фаза 5, журнал для юзера): секция `journal:` — durable
/// журнал событий (health-алерты, обрывы стримов, апрувы). Пишется
/// writer-таском (как event_log), чистится по retention_days и по
/// max_size_mb. Отсутствие секции = выключено.
#[derive(Debug, Clone, Deserialize)]
pub struct JournalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sqlite")]
    pub storage_backend: String,
    #[serde(default = "default_journal_path")]
    pub storage_path: PathBuf,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
    /// Сколько дней хранить события журнала. 0 = без TTL (чистится только
    /// по max_size_mb).
    #[serde(default = "default_journal_retention_days")]
    pub retention_days: u64,
}

/// ДОБАВЛЕНО (Фаза 5, health-мониторинг): секция `health:` — фоновый
/// наблюдатель: размеры всех БД против лимитов, занятые слоты стримов,
/// периодическая сводка. Алерты пишутся в журнал + tracing. Отсутствие
/// секции = выключено (прежнее поведение).
#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Период прогона проверки в секундах. 0 = проверка не запускается
    /// (сводка не пишется).
    #[serde(default = "default_health_interval_secs")]
    pub check_interval_secs: u64,
    /// Порог занятости лимита БД (в %), по достижении которого пишется
    /// предупреждение в журнал. 0 = не предупреждать.
    #[serde(default = "default_health_db_warn_pct")]
    pub db_size_warn_pct: u64,
}

/// ADDED (Phase 7, approvals): `approvals:` section — human control over
/// which agents from config.yaml the gateway actually serves. A new agent
/// enters status pending and is not started until approved via CLI
/// (`gatewayd --approve <name>`). Section absent = disabled (previous
/// behavior: all agents are admitted).
#[derive(Debug, Clone, Deserialize)]
pub struct ApprovalsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_approvals_path")]
    pub storage_path: PathBuf,
}

impl Default for ApprovalsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_path: default_approvals_path(),
        }
    }
}

fn default_sqlite() -> String {
    "sqlite".to_string()
}

fn default_event_log_path() -> PathBuf {
    PathBuf::from("/tmp/gateway/event_log.db")
}

fn default_task_store_path() -> PathBuf {
    PathBuf::from("/tmp/gateway/task_store.db")
}

fn default_journal_path() -> PathBuf {
    PathBuf::from("/tmp/gateway/journal.db")
}

fn default_approvals_path() -> PathBuf {
    PathBuf::from("/tmp/gateway/approvals.db")
}

fn default_journal_retention_days() -> u64 {
    30
}

fn default_health_interval_secs() -> u64 {
    300
}

fn default_health_db_warn_pct() -> u64 {
    80
}

fn default_max_size_mb() -> u64 {
    100
}

impl Default for EventLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_backend: default_sqlite(),
            storage_path: default_event_log_path(),
            max_size_mb: default_max_size_mb(),
        }
    }
}

impl Default for TaskStoreConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_backend: default_sqlite(),
            storage_path: default_task_store_path(),
            max_size_mb: default_max_size_mb(),
        }
    }
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_backend: default_sqlite(),
            storage_path: default_journal_path(),
            max_size_mb: default_max_size_mb(),
            retention_days: default_journal_retention_days(),
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            check_interval_secs: default_health_interval_secs(),
            db_size_warn_pct: default_health_db_warn_pct(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отсутствие обеих секций = выключено, не паникает (backward compat).
    #[test]
    fn absent_sections_are_disabled() {
        let yaml = "event_log: {}\ntask_store: {}\n";
        let raw: serde_yaml::Value = serde_yaml::from_str(yaml).expect("YAML парсится");
        let event_log: EventLogConfig =
            serde_yaml::from_value(raw.get("event_log").cloned().unwrap()).unwrap();
        let task_store: TaskStoreConfig =
            serde_yaml::from_value(raw.get("task_store").cloned().unwrap()).unwrap();
        assert!(!event_log.enabled);
        assert_eq!(event_log.storage_backend, "sqlite");
        assert_eq!(event_log.max_size_mb, 100);
        assert!(!task_store.enabled);
    }

    /// ДОБАВЛЕНО (Фаза 5): отсутствующие journal/health секции = выключено,
    /// дефолты заполнены, backward-compat.
    #[test]
    fn absent_journal_and_health_are_disabled() {
        let journal: JournalConfig = serde_yaml::from_str("").unwrap();
        assert!(!journal.enabled);
        assert_eq!(journal.retention_days, 30);
        assert_eq!(journal.max_size_mb, 100);

        let health: HealthConfig = serde_yaml::from_str("").unwrap();
        assert!(!health.enabled);
        assert_eq!(health.check_interval_secs, 300);
        assert_eq!(health.db_size_warn_pct, 80);
    }

    /// ДОБАВЛЕНО (Фаза 5): явные секции переопределяют дефолты.
    #[test]
    fn explicit_journal_and_health_override_defaults() {
        let journal: JournalConfig =
            serde_yaml::from_str("enabled: true\nstorage_path: /tmp/j.db\nretention_days: 7\n")
                .unwrap();
        assert!(journal.enabled);
        assert_eq!(journal.storage_path, PathBuf::from("/tmp/j.db"));
        assert_eq!(journal.retention_days, 7);

        let health: HealthConfig =
            serde_yaml::from_str("enabled: true\ncheck_interval_secs: 60\ndb_size_warn_pct: 90\n")
                .unwrap();
        assert!(health.enabled);
        assert_eq!(health.check_interval_secs, 60);
        assert_eq!(health.db_size_warn_pct, 90);
    }
}