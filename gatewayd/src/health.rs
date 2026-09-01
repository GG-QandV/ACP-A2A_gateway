//! gatewayd/src/health.rs
//!
//! User-facing health monitoring (Phase 5): a background watcher
//! that periodically checks the sizes of all durable DBs against limits
//! and stream-slot occupancy. Alerts are written to the journal (journal.rs) and to
//! tracing. Runs from main.rs as a background task (like the sweeper).
//!
//! Phase 5 minimum (per the plan): DB sizes + summary of active streams.
//! Full accounting of dropped streams/disconnects without reconnect requires
//! registering breaks in the journal from relay — a separate sub-step later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::HealthConfig;
use crate::registry::Registry;

use crate::journal::{Journal, Level};

/// One target DB for size checking.
#[derive(Debug, Clone)]
pub struct DbTarget {
    pub label: &'static str,
    pub path: PathBuf,
    /// 0 = no limit (not checked).
    pub max_mb: u64,
}

/// DB file size in bytes (file on disk, including the WAL journal of active
/// writes). Fine for a rare periodic check.
pub fn db_size_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Summary for one run — pure function, testable without a task.
pub struct HealthSnapshot {
    pub db_usage: Vec<DbUsage>,
    pub stream_usage: Vec<StreamUsageRow>,
}

pub struct DbUsage {
    pub label: &'static str,
    pub size_bytes: u64,
    /// 0 = no limit.
    pub max_mb: u64,
    /// Limit usage in % (0 when max_mb == 0).
    pub pct: u64,
}

pub struct StreamUsageRow {
    pub agent_id: String,
    pub active: usize,
    pub limit: usize,
}

pub fn collect_snapshot(
    targets: &[DbTarget],
    registry: &Registry,
) -> HealthSnapshot {
    let db_usage = targets
        .iter()
        .map(|t| {
            let size_bytes = db_size_bytes(&t.path);
            let pct = if t.max_mb == 0 {
                0
            } else {
                let max_bytes = t.max_mb * 1024 * 1024;
                (size_bytes as f64 / max_bytes as f64 * 100.0) as u64
            };
            DbUsage {
                label: t.label,
                size_bytes,
                max_mb: t.max_mb,
                pct,
            }
        })
        .collect();

    let stream_usage = registry
        .stream_usage()
        .into_iter()
        .map(|u| StreamUsageRow {
            agent_id: u.agent_id,
            active: u.active,
            limit: u.limit,
        })
        .collect();

    HealthSnapshot {
        db_usage,
        stream_usage,
    }
}

/// Checks the snapshot and returns a list of alerts (level + message).
/// An alert = a warning/error on DB usage. The summary (info) is not included.
pub fn alerts_from_snapshot(snap: &HealthSnapshot, warn_pct: u64) -> Vec<(Level, String)> {
    let mut alerts = Vec::new();
    if warn_pct == 0 {
        return alerts;
    }
    for d in &snap.db_usage {
        if d.max_mb == 0 {
            continue;
        }
        let size_mb = d.size_bytes / (1024 * 1024);
        if d.pct >= 100 {
            alerts.push((
                Level::Error,
                format!(
                    "db {}: размер {} МБ превысил лимит {} МБ — включённая самоочистка может не успевать",
                    d.label, size_mb, d.max_mb
                ),
            ));
        } else if d.pct >= warn_pct {
            alerts.push((
                Level::Warn,
                format!(
                    "db {}: размер {} МБ достиг {}% лимита {} МБ",
                    d.label, size_mb, d.pct, d.max_mb
                ),
            ));
        }
    }
    alerts
}

/// Spawns the background health-monitor task. `journal = None` — alerts go
/// only to tracing (journal disabled in config). `check_interval_secs == 0`
/// or `!enabled` — does not start (returns None).
pub fn spawn(
    registry: Arc<Registry>,
    journal: Option<Arc<Journal>>,
    cfg: &HealthConfig,
    targets: Vec<DbTarget>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !cfg.enabled || cfg.check_interval_secs == 0 {
        return None;
    }
    let interval = std::time::Duration::from_secs(cfg.check_interval_secs);
    let warn_pct = cfg.db_size_warn_pct;
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let snap = collect_snapshot(&targets, &registry);
            let alerts = alerts_from_snapshot(&snap, warn_pct);

            // Alerts: to the journal (if present) + tracing.
            for (level, msg) in &alerts {
                if let Some(j) = &journal {
                    let _ = j.append(*level, "health", msg).await;
                }
                match level {
                    Level::Error => tracing::error!(message = %msg, "health: БД переполнена"),
                    Level::Warn => tracing::warn!(message = %msg, "health: БД приближается к лимиту"),
                    Level::Info => tracing::info!(message = %msg, "health"),
                }
            }

            // Summary: always to tracing, to the journal — as a single info record.
            let dbs = snap
                .db_usage
                .iter()
                .map(|d| {
                    format!(
                        "{}={}MB/{}MB({}%)",
                        d.label,
                        d.size_bytes / (1024 * 1024),
                        d.max_mb,
                        d.pct
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let streams = snap
                .stream_usage
                .iter()
                .map(|s| format!("{}={}/{}", s.agent_id, s.active, s.limit))
                .collect::<Vec<_>>()
                .join(", ");
            let summary = format!("health summary | dbs [{}] | streams [{}]", dbs, streams);
            tracing::info!(summary);
            if let Some(j) = &journal {
                let _ = j.append(Level::Info, "health", &summary).await;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AgentEntry, Transport};
    use std::collections::{HashMap, HashSet};

    fn test_registry() -> Registry {
        let mut agents = HashMap::new();
        agents.insert(
            "claurst-main".to_string(),
            AgentEntry::new(
                Transport::Stdio {
                    command: vec!["claurst".into()],
                    cwd: None,
                    env: HashMap::new(),
                },
                2,
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(120),
            ),
        );
        Registry::new(HashSet::from(["t-1".to_string()]), agents)
    }

    #[test]
    fn snapshot_collects_db_and_stream_usage() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ev.db");
        std::fs::write(&db_path, b"12345").unwrap();
        let targets = vec![DbTarget {
            label: "event_log",
            path: db_path.clone(),
            max_mb: 100,
        }];
        let registry = test_registry();
        let snap = collect_snapshot(&targets, &registry);
        assert_eq!(snap.db_usage.len(), 1);
        assert_eq!(snap.db_usage[0].label, "event_log");
        assert_eq!(snap.stream_usage.len(), 1);
        assert_eq!(snap.stream_usage[0].active, 0);
        assert_eq!(snap.stream_usage[0].limit, 2);
    }

    #[test]
    fn alerts_fire_at_warn_and_error_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        // 0 bytes at max_mb=1 -> pct=0, no alerts.
        let empty = DbTarget {
            label: "event_log",
            path: dir.path().join("empty.db"),
            max_mb: 1,
        };
        let snap = collect_snapshot(&[empty], &test_registry());
        assert!(alerts_from_snapshot(&snap, 80).is_empty());

        // ~1 MB file at max_mb=1 -> pct >= 100 -> error.
        let full = dir.path().join("full.db");
        std::fs::write(&full, vec![0u8; 1024 * 1024]).unwrap();
        let snap = collect_snapshot(
            &[DbTarget {
                label: "event_log",
                path: full,
                max_mb: 1,
            }],
            &test_registry(),
        );
        let alerts = alerts_from_snapshot(&snap, 80);
        assert!(
            alerts.iter().any(|(l, _)| *l == Level::Error),
            "при 100% занятости должен быть error-алерт"
        );

        // ~0.9 MB file -> pct ~90 -> warn (not error).
        let near = dir.path().join("near.db");
        std::fs::write(&near, vec![0u8; 900 * 1024]).unwrap();
        let snap = collect_snapshot(
            &[DbTarget {
                label: "task_store",
                path: near,
                max_mb: 1,
            }],
            &test_registry(),
        );
        let alerts = alerts_from_snapshot(&snap, 80);
        assert!(
            alerts.iter().any(|(l, _)| *l == Level::Warn),
            "при ~90% занятости должен быть warn-алерт"
        );
        assert!(
            !alerts.iter().any(|(l, _)| *l == Level::Error),
            "при ~90% занятости error-алерта быть не должно"
        );
    }
}