//! gatewayd/tests/buffer_config.rs
//!
//! Buffer config Phase 1: the `event_log` and `task_store` sections in
//! config.loc.buffer.yaml parse into gatewayd::config types and reflect the
//! intent (sqlite, enabled, paths/limits from the file). The test reads the
//! real repo file — a regression guard against "the buffer is declared in the
//! config but the parser does not see it".
//!
//! Schema/tables are filled in by Phase 2 (EventLog); only the
//! configuration layer is checked here.

use gatewayd::config::{EventLogConfig, TaskStoreConfig};

fn repo_config_yaml() -> String {
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = here
        .parent()
        .expect("gatewayd/ находится в корне репо")
        .join("config.loc.buffer.yaml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("не удалось прочитать {}: {e}", path.display()))
}

#[test]
fn event_log_section_parses_from_buffer_config() {
    let yaml = repo_config_yaml();
    let value: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("YAML парсится");
    let event_log: EventLogConfig = serde_yaml::from_value(
        value
            .get("event_log")
            .cloned()
            .expect("секция event_log есть"),
    )
    .expect("EventLogConfig парсится");
    assert!(event_log.enabled, "event_log должен быть включён");
    assert_eq!(event_log.storage_backend, "sqlite");
    assert_eq!(
        event_log.storage_path,
        std::path::PathBuf::from("/tmp/gateway/event_log.db")
    );
    assert_eq!(event_log.max_size_mb, 100);
}

#[test]
fn task_store_section_parses_from_buffer_config() {
    let yaml = repo_config_yaml();
    let value: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("YAML парсится");
    let task_store: TaskStoreConfig = serde_yaml::from_value(
        value
            .get("task_store")
            .cloned()
            .expect("секция task_store есть"),
    )
    .expect("TaskStoreConfig парсится");
    assert!(task_store.enabled, "task_store должен быть включён");
    assert_eq!(task_store.storage_backend, "sqlite");
    assert_eq!(
        task_store.storage_path,
        std::path::PathBuf::from("/tmp/gateway/task_store.db")
    );
    assert_eq!(task_store.max_size_mb, 500);
}

/// Regression: a config without buffer sections (like config.example.yaml)
/// must not break parsing — the sections are optional.
#[test]
fn config_without_buffer_sections_parses() {
    let value: serde_yaml::Value = serde_yaml::from_str(
        r#"
event_log: {}
task_store: {}
"#,
    )
    .expect("YAML парсится");
    let event_log: EventLogConfig =
        serde_yaml::from_value(value.get("event_log").cloned().unwrap()).unwrap();
    let task_store: TaskStoreConfig =
        serde_yaml::from_value(value.get("task_store").cloned().unwrap()).unwrap();
    assert!(!event_log.enabled);
    assert!(!task_store.enabled);
}
