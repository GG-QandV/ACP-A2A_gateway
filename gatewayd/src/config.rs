//! gatewayd/src/config.rs
//! Buffering config (Phase 1): `event_log` and `task_store` sections of the local
//! durable storage for streaming. Extracted into lib so that integration tests
//! (gatewayd/tests/) can parse the config without running the binary.

use std::path::PathBuf;

use serde::Deserialize;

/// `event_log:` section — durable buffer of stream events (source of truth
/// for `tasks/resubscribe`, see Phase 2.1 / T4). Absence of the section =
/// disabled (previous behavior: ephemeral channel only).
#[derive(Debug, Clone, Deserialize)]
pub struct EventLogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sqlite")]
    pub storage_backend: String,
    #[serde(default = "default_event_log_path")]
    pub storage_path: PathBuf,
    /// Cap on the DB file size in MB. Once reached — self-cleanup
    /// of the oldest events by seq (Phase 2, writer task). 0 = no limit.
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
}

/// `task_store:` section — durable task storage. Absence of the section =
/// previous file-based storage (core/src/task_store.rs).
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

/// ADDED (Phase 5, journal for the user): `journal:` section — durable
/// event journal (health alerts, stream drops, approvals). Written
/// by the writer task (like event_log), cleaned by retention_days and by
/// max_size_mb. Absence of the section = disabled.
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
    /// How many days to keep journal events. 0 = no TTL (cleaned only
    /// by max_size_mb).
    #[serde(default = "default_journal_retention_days")]
    pub retention_days: u64,
}

/// ADDED (Phase 5, health monitoring): `health:` section — a background
/// watcher: sizes of all DBs against limits, occupied stream slots,
/// periodic summary. Alerts are written to the journal + tracing. Absence
/// of the section = disabled (previous behavior).
#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Period of running the check in seconds. 0 = the check is not started
    /// (no summary is written).
    #[serde(default = "default_health_interval_secs")]
    pub check_interval_secs: u64,
    /// DB limit occupancy threshold (in %), once reached a warning
    /// is written to the journal. 0 = do not warn.
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

    /// Both sections absent = disabled, no panic (backward compat).
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

    /// ADDED (Phase 5): missing journal/health sections = disabled,
    /// defaults filled, backward-compat.
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

    /// ADDED (Phase 5): explicit sections override defaults.
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