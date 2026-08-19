//! gatewayd/tests/buffer_config.rs
//!
//! Фаза 1 буферного конфига: секции `event_log` и `task_store` в
//! config.loc.buffer.yaml парсятся в типы gatewayd::config и отражают
//! намерение (sqlite, включены, пути/лимиты из файла). Тест ходит по
//! реальному файлу репо — это регрессия на «буфер объявлен в конфиге,
//! но парсер его не видит».
//!
//! Схему/таблицы наполняет Фаза 2 (EventLog); здесь проверяется только
//! конфигурационный слой.

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
    let event_log: EventLogConfig =
        serde_yaml::from_value(value.get("event_log").cloned().expect("секция event_log есть"))
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
    let task_store: TaskStoreConfig =
        serde_yaml::from_value(value.get("task_store").cloned().expect("секция task_store есть"))
            .expect("TaskStoreConfig парсится");
    assert!(task_store.enabled, "task_store должен быть включён");
    assert_eq!(task_store.storage_backend, "sqlite");
    assert_eq!(
        task_store.storage_path,
        std::path::PathBuf::from("/tmp/gateway/task_store.db")
    );
    assert_eq!(task_store.max_size_mb, 500);
}

/// Регрессия: конфиг без буферных секций (как config.example.yaml) не
/// должен ломать парсинг — секции опциональны.
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