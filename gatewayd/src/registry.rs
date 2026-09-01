//! gatewayd/src/registry.rs
//! Flat agent registry + token check. The token is allow/deny for entry
//! into the gateway, not bound to the agent list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Transport {
    Stdio {
        command: Vec<String>,
        cwd: Option<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        push_token: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub transport: Transport,
    /// Semaphore for concurrent streams per agent (Streaming roadmap Part 2).
    /// Capacity comes from the `streaming.max_concurrent_streams` config,
    /// not hardcoded.
    pub stream_permits: Arc<tokio::sync::Semaphore>,
    /// Configured semaphore capacity (Semaphore does not store it publicly).
    pub stream_limit: usize,
    /// ADDED (Streaming roadmap Part 2): timeout BEFORE the first chunk
    /// of the agent stream — from the `streaming.first_chunk_timeout_secs` config.
    pub first_chunk_timeout: std::time::Duration,
    /// ADDED (Streaming roadmap Part 2): timeout BETWEEN chunks
    /// of the agent stream — from the `streaming.idle_chunk_timeout_secs` config.
    pub idle_chunk_timeout: std::time::Duration,
}

/// ADDED (Phase 5): one summary line of occupied stream slots.
#[derive(Debug, Clone)]
pub struct StreamUsage {
    pub agent_id: String,
    pub active: usize,
    pub limit: usize,
}

impl AgentEntry {
    pub fn new(
        transport: Transport,
        max_concurrent_streams: usize,
        first_chunk_timeout: std::time::Duration,
        idle_chunk_timeout: std::time::Duration,
    ) -> Self {
        Self {
            transport,
            stream_permits: Arc::new(tokio::sync::Semaphore::new(max_concurrent_streams)),
            stream_limit: max_concurrent_streams,
            first_chunk_timeout,
            idle_chunk_timeout,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("agent {agent_id}: stream capacity exhausted (active={active_streams}, limit={limit})")]
pub struct StreamCapacityExhausted {
    pub agent_id: String,
    pub active_streams: usize,
    pub limit: usize,
}

pub struct Registry {
    tokens: HashSet<String>,
    agents: HashMap<String, AgentEntry>,
}

impl Registry {
    pub fn new(tokens: HashSet<String>, agents: HashMap<String, AgentEntry>) -> Self {
        Self { tokens, agents }
    }

    pub fn check_token(&self, token: &str) -> bool {
        self.tokens.contains(token)
    }

    pub fn lookup(&self, agent_id: &str) -> Option<&AgentEntry> {
        self.agents.get(agent_id)
    }

    /// ADDED (Phase 5, health monitoring): summary of occupied stream
    /// slots across all agents — for the periodic health check.
    pub fn stream_usage(&self) -> Vec<StreamUsage> {
        let mut usage: Vec<StreamUsage> = self
            .agents
            .iter()
            .map(|(id, entry)| StreamUsage {
                agent_id: id.clone(),
                active: entry.stream_limit - entry.stream_permits.available_permits(),
                limit: entry.stream_limit,
            })
            .collect();
        usage.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        usage
    }

    /// Tries to acquire a stream slot on an agent (fail-closed). Returns
    /// a permit — hold it until the stream ends to release the slot.
    pub fn try_acquire_stream(
        &self,
        agent_id: &str,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, StreamCapacityExhausted> {
        let entry = self
            .agents
            .get(agent_id)
            .ok_or_else(|| StreamCapacityExhausted {
                agent_id: agent_id.to_string(),
                active_streams: 0,
                limit: 0,
            })?;
        let limit = entry.stream_limit;
        let active = limit - entry.stream_permits.available_permits();
        let permit = entry
            .stream_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                tracing::warn!(
                    agent_id,
                    active_streams = active,
                    limit,
                    "agent stream capacity exhausted — запрос отклонён fail-closed"
                );
                StreamCapacityExhausted {
                    agent_id: agent_id.to_string(),
                    active_streams: active,
                    limit,
                }
            })?;
        Ok(permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_registry() -> Registry {
        let tokens: HashSet<String> = ["t-valid".to_string()].into_iter().collect();
        let mut agents = HashMap::new();
        agents.insert(
            "claurst-main".to_string(),
            AgentEntry::new(
                Transport::Stdio {
                    command: vec!["claurst".into(), "acp".into()],
                    cwd: Some("/srv/workspace".into()),
                    env: HashMap::new(),
                },
                4,
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(120),
            ),
        );
        Registry::new(tokens, agents)
    }

    #[test]
    fn valid_token_passes() {
        assert!(sample_registry().check_token("t-valid"));
    }

    #[test]
    fn invalid_token_denied() {
        assert!(!sample_registry().check_token("t-garbage"));
    }

    #[test]
    fn agent_lookup_by_id() {
        let reg = sample_registry();
        assert!(reg.lookup("claurst-main").is_some());
        assert!(reg.lookup("nonexistent").is_none());
    }

    /// T7: the semaphore rejects requests beyond the limit — explicit error, no hang.
    #[test]
    fn semaphore_rejects_stream_beyond_limit() {
        let tokens: HashSet<String> = ["t-valid".to_string()].into_iter().collect();
        let mut agents = HashMap::new();
        agents.insert(
            "a1".to_string(),
            AgentEntry::new(
                Transport::Stdio {
                    command: vec!["echo".into()],
                    cwd: None,
                    env: HashMap::new(),
                },
                2,
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(120),
            ),
        );
        let reg = Registry::new(tokens, agents);

        let _p1 = reg.try_acquire_stream("a1").expect("first permit");
        let _p2 = reg.try_acquire_stream("a1").expect("second permit");
        let err = reg.try_acquire_stream("a1").unwrap_err();
        assert_eq!(err.active_streams, 2);
        assert_eq!(err.limit, 2);
        assert!(err.to_string().contains("capacity exhausted"));
    }

    #[test]
    fn unknown_agent_stream_acquisition_fails() {
        let reg = sample_registry();
        let err = reg.try_acquire_stream("nonexistent").unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    /// T7: dropping the permit (RAII, like TurnGuard) frees the slot — a new
    /// stream goes through after the previous one closed.
    #[test]
    fn releasing_a_permit_allows_new_stream() {
        let tokens: HashSet<String> = ["t-valid".to_string()].into_iter().collect();
        let mut agents = HashMap::new();
        agents.insert(
            "a1".to_string(),
            AgentEntry::new(
                Transport::Stdio {
                    command: vec!["echo".into()],
                    cwd: None,
                    env: HashMap::new(),
                },
                1,
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(120),
            ),
        );
        let reg = Registry::new(tokens, agents);

        let p1 = reg.try_acquire_stream("a1").expect("первый permit");
        assert!(
            reg.try_acquire_stream("a1").is_err(),
            "лимит 1: второй должен быть отклонён"
        );
        drop(p1);
        assert!(
            reg.try_acquire_stream("a1").is_ok(),
            "после drop permit'а слот свободен"
        );
    }
}
