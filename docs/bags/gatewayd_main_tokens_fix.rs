//! gatewayd/src/main.rs — PATCH: {env:...} in tokens: now resolves.
//! The only change — build_registry(), one line.
//!
//! ⚠️ WARNING: this patch version is OLD (written against the old signature
//! of resolve_env_placeholders with unwrap_or_default()). Current main.rs:82
//! already returns anyhow::Result<String> (audit fix P1-10 — a missing
//! variable = configuration error at startup). Applying the patch as is
//! is NOT ALLOWED: .collect() would give HashSet<Result<String>> and fail to compile.
//!
//! The equivalent that was actually applied (in main.rs, line 97):
//!
//!     let tokens: std::collections::HashSet<String> = raw
//!         .tokens
//!         .iter()
//!         .map(|t| resolve_env_placeholders(t))
//!         .collect::<anyhow::Result<_>>()?;
//!
//! Behavior: {env:VAR} in tokens: resolves just like in agents' env:;
//! a missing variable → startup error (not a silently empty token).

fn build_registry(raw: &RawConfig) -> Registry {
    // BEFORE:
    // let tokens: HashSet<String> = raw.tokens.iter().cloned().collect();
    //
    // AFTER: the same resolve_env_placeholders already applied to env:
    // of agents (see RawAgentEntry::Stdio below) — now tokens: also
    // goes through the same function. This was forgotten before: resolve_env_placeholders
    // was written with the agents' env dictionary in mind; the flat
    // tokens: list was never explicitly considered.
    let tokens: HashSet<String> = raw
        .tokens
        .iter()
        .map(|t| resolve_env_placeholders(t))
        .collect::<anyhow::Result<_>>()?;

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
