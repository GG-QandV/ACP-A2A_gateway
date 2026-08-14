//! gatewayd/src/main.rs — ПАТЧ: {env:...} в tokens: теперь резолвится.
//! Единственное изменение — build_registry(), одна строка.
//!
//! ⚠️ ВНИМАНИЕ: эта версия патча СТАРАЯ (писалась под старую сигнатуру
//! resolve_env_placeholders с unwrap_or_default()). Текущий код main.rs:82
//! уже возвращает anyhow::Result<String> (фикс аудита P1-10 — отсутствующая
//! переменная = ошибка конфигурации на старте). Применять патч как есть
//! НЕЛЬЗЯ: .collect() даст HashSet<Result<String>> и не скомпилируется.
//!
//! Реально применённый эквивалент (в main.rs, строка 97):
//!
//!     let tokens: std::collections::HashSet<String> = raw
//!         .tokens
//!         .iter()
//!         .map(|t| resolve_env_placeholders(t))
//!         .collect::<anyhow::Result<_>>()?;
//!
//! Поведение: {env:VAR} в tokens: резолвится как и в env: агентов;
//! недостающая переменная → ошибка старта (не тихий пустой токен).

fn build_registry(raw: &RawConfig) -> Registry {
    // БЫЛО:
    // let tokens: HashSet<String> = raw.tokens.iter().cloned().collect();
    //
    // СТАЛО: тот же resolve_env_placeholders, что уже применяется к env:
    // агентов (см. RawAgentEntry::Stdio ниже) — теперь и tokens: проходит
    // через ту же функцию. Раньше это было забыто: resolve_env_placeholders
    // писался с прицелом на env-словарь агентов, про плоский список
    // tokens: никто явно не подумал.
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
