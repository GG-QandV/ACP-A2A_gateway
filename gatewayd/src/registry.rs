//! gatewayd/src/registry.rs
//! Плоский реестр агентов + токен-проверка. Токен — allow/deny на вход
//! в гейтвей, не привязан к списку агентов.

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
    /// Семафор параллельных стримов на агента (Часть 2 роадмапа стриминга).
    /// Ёмкость задаётся из конфига `streaming.max_concurrent_streams`,
    /// не хардкодится.
    pub stream_permits: Arc<tokio::sync::Semaphore>,
    /// Заданная ёмкость семафора (Semaphore не хранит её публично).
    pub stream_limit: usize,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут ДО первого чанка
    /// стрима агента — из конфига `streaming.first_chunk_timeout_secs`.
    pub first_chunk_timeout: std::time::Duration,
    /// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): таймаут МЕЖДУ чанками
    /// стрима агента — из конфига `streaming.idle_chunk_timeout_secs`.
    pub idle_chunk_timeout: std::time::Duration,
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

    /// Пытается занять слот стрима на агента (fail-closed). Возвращает
    /// permit — держать до конца стрима, чтобы освободить слот.
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

    /// T7: семафор отклоняет запрос сверх лимита — явная ошибка, не зависание.
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
}
