//! gatewayd/src/registry.rs

use std::collections::{HashMap, HashSet};

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
}

/// Токен — allow/deny на вход в gateway, не привязан к конкретным агентам.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_registry() -> Registry {
        let tokens: HashSet<String> = ["t-valid".to_string()].into_iter().collect();
        let mut agents = HashMap::new();
        agents.insert(
            "claurst-main".to_string(),
            AgentEntry {
                transport: Transport::Stdio {
                    command: vec!["claurst".into(), "acp".into()],
                    cwd: Some("/srv/workspace".into()),
                    env: HashMap::new(),
                },
            },
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
}
