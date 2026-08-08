//! gatewayd/src/main.rs — читает config.yaml, строит Registry, поднимает
//! TCP (направления 1 и 3) и HTTP (направления 2 и 4) параллельно.

mod registry;
mod transport_a2a_passthrough;
mod transport_http;
mod transport_tcp;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use registry::{AgentEntry, Registry, Transport};

#[derive(Debug, Deserialize)]
struct RawConfig {
    listen: String,
    #[serde(default = "default_http_listen")]
    http_listen: String,
    tokens: Vec<String>,
    agents: HashMap<String, RawAgentEntry>,
    task_store_dir: PathBuf,
    turn_lease_timeout_secs: u64,
}

fn default_http_listen() -> String {
    "0.0.0.0:8348".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
enum RawAgentEntry {
    Stdio {
        command: Vec<String>,
        cwd: Option<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        push_token: Option<String>,
    },
}

fn resolve_env_placeholders(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
        std::env::var(var_name).unwrap_or_default()
    } else {
        value.to_string()
    }
}

fn build_registry(raw: &RawConfig) -> Registry {
    let tokens: std::collections::HashSet<String> = raw.tokens.iter().cloned().collect();

    let agents: HashMap<String, AgentEntry> = raw
        .agents
        .iter()
        .map(|(id, entry)| {
            let transport = match entry {
                RawAgentEntry::Stdio { command, cwd, env } => Transport::Stdio {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    env: env.iter().map(|(k, v)| (k.clone(), resolve_env_placeholders(v))).collect(),
                },
                RawAgentEntry::Http { url, push_token } => Transport::Http {
                    url: url.clone(),
                    push_token: push_token.as_deref().map(resolve_env_placeholders),
                },
            };
            (id.clone(), AgentEntry { transport })
        })
        .collect();

    Registry::new(tokens, agents)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber_init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.yaml".to_string());
    let raw_yaml = std::fs::read_to_string(&config_path)
        .with_context(|| format!("не удалось прочитать конфиг: {config_path}"))?;
    let raw_config: RawConfig = serde_yaml::from_str(&raw_yaml)
        .with_context(|| format!("не удалось распарсить конфиг: {config_path}"))?;

    let tcp_listen = raw_config.listen.clone();
    let http_listen = raw_config.http_listen.clone();
    let task_store_dir = raw_config.task_store_dir.clone();
    let lease_timeout = std::time::Duration::from_secs(raw_config.turn_lease_timeout_secs);

    let registry = std::sync::Arc::new(build_registry(&raw_config));

    tracing::info!(%tcp_listen, %http_listen, "starting acp-a2a gateway (dual transport, 3 directions)");

    let tcp_registry = registry.clone();
    let tcp_task_store_dir = task_store_dir.clone();
    let tcp_server = tokio::spawn(async move {
        transport_tcp::serve(&tcp_listen, tcp_registry, tcp_task_store_dir, lease_timeout).await
    });

    let http_registry = registry.clone();
    let http_server = tokio::spawn(async move {
        let direction_4 = transport_http::router(http_registry.clone(), task_store_dir, lease_timeout);
        let direction_2 = transport_a2a_passthrough::router(http_registry);
        let app = direction_4.merge(direction_2);

        let listener = tokio::net::TcpListener::bind(&http_listen).await?;
        axum::serve(listener, app).await.map_err(anyhow::Error::from)
    });

    tokio::select! {
        res = tcp_server => res??,
        res = http_server => res??,
    }

    Ok(())
}

fn tracing_subscriber_init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}
