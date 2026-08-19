//! gatewayd/src/main.rs
//! Читает config.yaml, строит Registry, поднимает TCP и HTTP параллельно.
//! Направления 1 и 3 (ACP-клиент как входящая сторона) — TCP.
//! Направления 2 и 4 (A2A-клиент как входящая сторона) — HTTP.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use gatewayd::approvals::Status;
use gatewayd::config::{ApprovalsConfig, EventLogConfig, HealthConfig, JournalConfig, TaskStoreConfig};
use gatewayd::health::DbTarget;
use gatewayd::journal::Journal;
use gatewayd::registry::{AgentEntry, Registry, Transport};
use gatewayd::{transport_a2a_passthrough, transport_http, transport_tcp};
use serde::Deserialize;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

mod cli;
mod setup;

#[derive(Debug, Deserialize)]
struct RawConfig {
    listen: String,
    #[serde(default = "default_http_listen")]
    http_listen: String,
    tokens: Vec<String>,
    agents: HashMap<String, RawAgentEntry>,
    task_store_dir: PathBuf,
    turn_lease_timeout_secs: u64,
    /// ДОБАВЛЕНО (аудит P2-11): таймаут одного JSON-RPC вызова к
    /// stdio-агенту. Был захардкожен как 60s в core/src/stdio_agent.rs.
    #[serde(default = "default_agent_call_timeout_secs")]
    agent_call_timeout_secs: u64,
    /// ДОБАВЛЕНО (аудит P2-12): внешний адрес шлюза. Уходит в
    /// AgentCard.url, который раньше был пустым — карточка невалидна по
    /// A2A-спеке, а agent.json это первое, что читает внешний клиент.
    #[serde(default = "default_public_url")]
    public_url: String,
    /// ДОБАВЛЕНО: сколько дней хранить завершённые задачи. Раньше
    /// TaskStore::delete не вызывался ниоткуда, и файлы копились
    /// бесконечно.
    #[serde(default = "default_task_retention_days")]
    task_retention_days: u64,
    /// ДОБАВЛЕНО (Часть 4 роадмапа стриминга): конфигурация логирования
    /// и ротации. Отсутствие секции = дефолт "stdout" (прежнее поведение).
    #[serde(default)]
    logging: LoggingConfig,
    /// ДОБАВЛЕНО (Фаза 1 буферного конфига): durable-буфер событий стрима
    /// (источник истины для tasks/resubscribe, T4). Отсутствие секции =
    /// выключено (прежнее поведение).
    #[serde(default)]
    event_log: EventLogConfig,
    /// ДОБАВЛЕНО (Фаза 1 буферного конфига): durable-хранилище задач.
    /// Отсутствие секции = прежнее файловое хранилище.
    #[serde(default)]
    task_store: TaskStoreConfig,
    /// ДОБАВЛЕНО (Фаза 5): durable-журнал событий для пользователя
    /// (health-алерты, обрывы стримов, апрувы). Отсутствие секции = выключено.
    #[serde(default)]
    journal: JournalConfig,
    /// ДОБАВЛЕНО (Фаза 5): health-мониторинг — периодическая проверка
    /// размеров БД и занятости стримов. Отсутствие секции = выключено.
    #[serde(default)]
    health: HealthConfig,
    /// ДОБАВЛЕНО (Фаза 7, approvals): человеческий апрув агентов через CLI.
    /// Отсутствие секции = выключено (все агенты допускаются).
    #[serde(default)]
    approvals: ApprovalsConfig,
}

/// Часть 4 роадмапа стриминга: логирование. `level: "off"` полностью
/// отключает фильтр (аварийный клапан) — стартовое сообщение при этом
/// печатается в stderr напрямую, до отключения.
#[derive(Debug, Deserialize)]
struct LoggingConfig {
    #[serde(default = "default_log_level")]
    level: String,
    #[serde(default = "default_log_output")]
    output: String,
    /// ДОБАВЛЕНО (Часть 4.6): время жизни временно расширенного уровня.
    /// POST /debug/level c level: debug|trace держит его максимум
    /// столько минут, затем автоматический откат к "info". 0 = без отката.
    #[serde(default = "default_debug_ttl_minutes")]
    debug_ttl_minutes: u64,
    #[serde(default)]
    file: LogFileConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            output: default_log_output(),
            debug_ttl_minutes: default_debug_ttl_minutes(),
            file: LogFileConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LogFileConfig {
    #[serde(default = "default_log_path")]
    path: String,
    #[serde(default = "default_max_file_size_mb")]
    max_file_size_mb: u64,
    #[serde(default = "default_max_files")]
    max_files: usize,
    #[serde(default = "default_max_total_size_mb")]
    max_total_size_mb: u64,
    /// ДОБАВЛЕНО (Часть 4.4): сжимать ротированные файлы gzip (файл ->
    /// файл.gz), когда монитор чистки срабатывает из-за max_total_size_mb.
    #[serde(default = "default_compress_rotated")]
    compress_rotated: bool,
}

impl Default for LogFileConfig {
    fn default() -> Self {
        Self {
            path: default_log_path(),
            max_file_size_mb: default_max_file_size_mb(),
            max_files: default_max_files(),
            max_total_size_mb: default_max_total_size_mb(),
            compress_rotated: default_compress_rotated(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_output() -> String {
    "stdout".to_string()
}

fn default_debug_ttl_minutes() -> u64 {
    60
}

fn default_compress_rotated() -> bool {
    true
}

fn default_log_path() -> String {
    "/var/log/acp-a2a-gateway/gateway.log".to_string()
}

fn default_max_file_size_mb() -> u64 {
    100
}

fn default_max_files() -> usize {
    10
}

fn default_max_total_size_mb() -> u64 {
    1000
}

fn default_agent_call_timeout_secs() -> u64 {
    120
}

fn default_public_url() -> String {
    "http://localhost:8348".to_string()
}

fn default_task_retention_days() -> u64 {
    7
}

/// Как часто прогонять уборку. Час — компромисс: диск не ждёт сутки,
/// но и обход каталога не становится фоновой нагрузкой.
const TASK_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

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
        #[serde(default)]
        streaming: StreamingConfig,
    },
    Http {
        url: String,
        push_token: Option<String>,
        #[serde(default)]
        streaming: StreamingConfig,
    },
}

/// ДОБАВЛЕНО (Часть 2 роадмапа стриминга): секция `streaming:` на агента.
/// Дефолты — безопасный минимум (1 стрим на stdio-агента, таймауты по
/// умолчанию), чтобы конфиг без секции не паниковал и не менял поведение.
#[derive(Debug, Deserialize)]
struct StreamingConfig {
    #[serde(default = "default_max_concurrent_streams")]
    max_concurrent_streams: usize,
    #[serde(default = "default_first_chunk_timeout_secs")]
    first_chunk_timeout_secs: u64,
    #[serde(default = "default_idle_chunk_timeout_secs")]
    idle_chunk_timeout_secs: u64,
}

impl Default for StreamingConfig {
    /// Пустая секция / отсутствующая — безопасный минимум. Важно: per-field
    /// `#[serde(default = "fn")]` при `#[serde(default)]` на поле НЕ
    /// применяется (serde зовёт `StreamingConfig::default()`), поэтому
    /// Default реализован вручную с теми же значениями.
    fn default() -> Self {
        Self {
            max_concurrent_streams: default_max_concurrent_streams(),
            first_chunk_timeout_secs: default_first_chunk_timeout_secs(),
            idle_chunk_timeout_secs: default_idle_chunk_timeout_secs(),
        }
    }
}

fn default_max_concurrent_streams() -> usize {
    1
}

fn default_first_chunk_timeout_secs() -> u64 {
    15
}

fn default_idle_chunk_timeout_secs() -> u64 {
    120
}

/// ИСПРАВЛЕНО (аудит P1-10): было unwrap_or_default() — отсутствующая
/// переменная молча становилась пустым ключом/токеном, и шлюз стартовал
/// с нерабочей авторизацией. Теперь это ошибка конфигурации на старте.
fn resolve_env_placeholders(value: &str) -> anyhow::Result<String> {
    match value
        .strip_prefix("{env:")
        .and_then(|s| s.strip_suffix('}'))
    {
        Some(var_name) => std::env::var(var_name).with_context(|| {
            format!("переменная окружения {var_name} не задана (конфиг: {value})")
        }),
        None => Ok(value.to_string()),
    }
}

fn build_registry(
    raw: &RawConfig,
    allowed: &std::collections::HashSet<String>,
) -> anyhow::Result<(Registry, Vec<String>)> {
    // ДОБАВЛЕНО (аудит P1-10): пустой токен в списке = открытый вход для
    // клиента, приславшего "". Ловим на старте, а не в проде.
    if raw.tokens.is_empty() || raw.tokens.iter().any(|t| t.trim().is_empty()) {
        anyhow::bail!("config.tokens: список пуст или содержит пустой токен");
    }
    let tokens: std::collections::HashSet<String> = raw
        .tokens
        .iter()
        .map(|t| resolve_env_placeholders(t))
        .collect::<anyhow::Result<_>>()?;

    let mut agents: HashMap<String, AgentEntry> = HashMap::new();
    let mut excluded: Vec<String> = Vec::new();
    for (id, entry) in &raw.agents {
        if !allowed.contains(id) {
            excluded.push(id.clone());
            continue;
        }
        let streaming = match entry {
            RawAgentEntry::Stdio { streaming, .. } | RawAgentEntry::Http { streaming, .. } => {
                streaming
            }
        };
        if streaming.max_concurrent_streams == 0 {
            anyhow::bail!(
                "agent {id}: streaming.max_concurrent_streams не может быть 0 — используйте отдельный флаг disable_streaming: true, если стрим не нужен"
            );
        }

        let transport = match entry {
            RawAgentEntry::Stdio {
                command, cwd, env, ..
            } => {
                if command.is_empty() {
                    anyhow::bail!("agent {id}: command пустой");
                }
                let mut resolved = HashMap::new();
                for (k, v) in env {
                    resolved.insert(k.clone(), resolve_env_placeholders(v)?);
                }
                Transport::Stdio {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    env: resolved,
                }
            }
            RawAgentEntry::Http {
                url, push_token, ..
            } => Transport::Http {
                url: url.clone(),
                push_token: match push_token {
                    Some(t) => Some(resolve_env_placeholders(t)?),
                    None => None,
                },
            },
        };
        agents.insert(
            id.clone(),
            AgentEntry::new(
                transport,
                streaming.max_concurrent_streams,
                std::time::Duration::from_secs(streaming.first_chunk_timeout_secs),
                std::time::Duration::from_secs(streaming.idle_chunk_timeout_secs),
            ),
        );
    }

    Ok((Registry::new(tokens, agents), excluded))
}

/// Agent fingerprint for approvals: what exactly is approved. Changes when
/// transport/command/cwd/url changes — then re-approval is required.
fn agent_fingerprint(entry: &RawAgentEntry) -> String {
    match entry {
        RawAgentEntry::Stdio { command, cwd, .. } => format!(
            "stdio:{}:{}",
            command.join(" "),
            cwd.as_deref().unwrap_or("")
        ),
        RawAgentEntry::Http { url, .. } => format!("http:{url}"),
    }
}

/// Обходит подкаталоги хранилища (по одному на agent_id) и убирает
/// просроченные задачи в каждом.
async fn sweep_all_agents(base_dir: &PathBuf, ttl: std::time::Duration) -> anyhow::Result<usize> {
    let mut dir = match tokio::fs::read_dir(base_dir).await {
        Ok(dir) => dir,
        // До первой задачи каталога нет — это не ошибка.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };

    let mut removed = 0usize;
    while let Some(entry) = dir.next_entry().await? {
        if !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let store = gateway_core::TaskStore::new(entry.path());
        removed += store.sweep_expired(ttl).await?;
    }
    Ok(removed)
}

/// ДОБАВЛЕНО (Фаза 1 буферного конфига): инициализация durable-БД
/// (event_log, task_store) на старте. Создаёт каталог и файл sqlite;
/// пустую схему наполняет Фаза 2 (EventLog). Отключённая секция — no-op.
/// Ошибка здесь фатальна: включённый в конфиге буфер, который не смог
/// подняться, не должен тихо молчать.
fn init_buffer_dbs(event_log: &EventLogConfig, task_store: &TaskStoreConfig) -> anyhow::Result<()> {
    if let Some(cfg) = event_log.enabled.then_some(event_log) {
        init_sqlite_db(&cfg.storage_path, "event_log")?;
    }
    if let Some(cfg) = task_store.enabled.then_some(task_store) {
        init_sqlite_db(&cfg.storage_path, "task_store")?;
    }
    Ok(())
}

fn init_sqlite_db(path: &std::path::Path, label: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать каталог для {label}: {}", parent.display()))?;
        }
    }
    let conn = rusqlite::Connection::open(path)
        .with_context(|| format!("не удалось открыть sqlite для {label}: {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )
    .with_context(|| format!("не удалось настроить sqlite для {label}: {}", path.display()))?;
    drop(conn);
    tracing::info!(label, path = %path.display(), "sqlite БД инициализирована");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args
        .next()
        .unwrap_or_else(|| "config.yaml".to_string());

    // Фаза 6: интерактивный мастер настройки для пользователя.
    // `gatewayd --setup [file.yaml]` генерирует весь конфиг с дефолтами;
    // дев правит YAML напрямую.
    if config_path == "--setup" {
        return setup::run(args.next());
    }

    // Phase 7: CLI journal viewer. `gatewayd --journal [--db PATH] ...`.
    // Works even while the gateway is stopped (read-only DB open).
    if config_path == "--journal" {
        return cli::run_journal(&args.collect::<Vec<_>>());
    }

    // Phase 7: agent approvals via CLI. `gatewayd --approvals [--db PATH]`,
    // `gatewayd --approve <name> [--db PATH]`, `--reject <name> ...`.
    if matches!(config_path.as_str(), "--approvals" | "--approve" | "--reject") {
        return cli::run_approvals(&config_path, &args.collect::<Vec<_>>());
    }

    let raw_yaml = std::fs::read_to_string(&config_path)
        .with_context(|| format!("не удалось прочитать конфиг: {config_path}"))?;
    let raw_config: RawConfig = serde_yaml::from_str(&raw_yaml)
        .with_context(|| format!("не удалось распарсить конфиг: {config_path}"))?;

    let reload_handle = tracing_subscriber_init(&raw_config.logging);

    // ДОБАВЛЕНО (Фаза 1 буферного конфига): поднять durable-БД до запуска
    // транспортов — стримы, стартовавшие сразу, должны иметь куда писать.
    init_buffer_dbs(&raw_config.event_log, &raw_config.task_store)?;

    let tcp_listen = raw_config.listen.clone();
    let http_listen = raw_config.http_listen.clone();
    let task_store_dir = raw_config.task_store_dir.clone();
    let lease_timeout = std::time::Duration::from_secs(raw_config.turn_lease_timeout_secs);
    let call_timeout = std::time::Duration::from_secs(raw_config.agent_call_timeout_secs);
    let public_url = raw_config.public_url.clone();
    let task_ttl = std::time::Duration::from_secs(raw_config.task_retention_days * 24 * 60 * 60);
    let sweep_dir = task_store_dir.clone();

// Phase 7 (approvals): when enabled, an agent enters the Registry only
    // after being approved via CLI. Not-approved (pending/rejected) agents are
    // excluded — the gateway does not serve them.
    let (registry, excluded_agents) = if raw_config.approvals.enabled {
        let store = gatewayd::approvals::ApprovalStore::open(&raw_config.approvals.storage_path)?;
        let mut allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending: Vec<String> = Vec::new();
        for (id, entry) in &raw_config.agents {
            let fp = agent_fingerprint(entry);
            match store.register(id, &fp)? {
                Status::Approved => {
                    allowed.insert(id.clone());
                }
                Status::Pending => pending.push(id.clone()),
                Status::Rejected => {}
            }
        }
        for id in &pending {
            tracing::warn!(agent_id = %id, "agent awaits approval via CLI: gatewayd --approve {id}");
        }
        build_registry(&raw_config, &allowed)?
    } else {
        let all: std::collections::HashSet<String> = raw_config.agents.keys().cloned().collect();
        build_registry(&raw_config, &all)?
    };
    let registry = std::sync::Arc::new(registry);
    tracing::info!(%tcp_listen, %http_listen, "starting acp-a2a gateway (dual transport, 3 directions)");

    let tcp_registry = registry.clone();
    let tcp_task_store_dir = task_store_dir.clone();
    let tcp_server = tokio::spawn(async move {
        transport_tcp::serve(&tcp_listen, tcp_registry, tcp_task_store_dir, lease_timeout).await
    });

    let http_registry = registry.clone();
    // ДОБАВЛЕНО (Фаза 2/3): поднять writer-таск event_log, если секция
    // включена в конфиге. Arc уходит в router -> HttpState -> stream_to_sse
    // и dispatch_a2a_method (tasks/resubscribe, tasks/get-last-seq).
    let event_log = match raw_config.event_log.enabled {
        true => Some(gatewayd::event_log::EventLog::spawn(
            raw_config.event_log.storage_path.clone(),
            raw_config.event_log.max_size_mb,
        )?),
        false => None,
    };

    // ДОБАВЛЕНО (Фаза 5): durable-журнал событий для пользователя.
    // Arc уходит в health-монитор (алерты) и (позже) в relay/CLI.
    let journal = match raw_config.journal.enabled {
        true => Some(Journal::spawn(
            raw_config.journal.storage_path.clone(),
            raw_config.journal.max_size_mb,
            raw_config.journal.retention_days,
        )?),
        false => None,
    };

    // Phase 7 (approvals): not-approved agents are recorded in the journal.
    for id in &excluded_agents {
        if let Some(j) = &journal {
            j.append(
                gatewayd::journal::Level::Warn,
                "approval",
                &format!("agent {id} is not approved and will not be served (run `gatewayd --approve {id}`)"),
            )
            .await?;
        }
    }

    // ДОБАВЛЕНО (Фаза 5): health-мониторинг. Цели проверки — только
    // включённые durable-БД (event_log, task_store, journal).
    let mut health_targets = Vec::new();
    if raw_config.event_log.enabled {
        health_targets.push(DbTarget {
            label: "event_log",
            path: raw_config.event_log.storage_path.clone(),
            max_mb: raw_config.event_log.max_size_mb,
        });
    }
    if raw_config.task_store.enabled {
        health_targets.push(DbTarget {
            label: "task_store",
            path: raw_config.task_store.storage_path.clone(),
            max_mb: raw_config.task_store.max_size_mb,
        });
    }
    if raw_config.journal.enabled {
        health_targets.push(DbTarget {
            label: "journal",
            path: raw_config.journal.storage_path.clone(),
            max_mb: raw_config.journal.max_size_mb,
        });
    }
    let health_monitor =
        gatewayd::health::spawn(registry.clone(), journal.clone(), &raw_config.health, health_targets);
    // Выключенный монитор = вечная заглушка, чтобы select не выходил
    // преждевременно (тот же приём, что у log_monitor выше).
    let health_task = match health_monitor {
        Some(h) => h,
        None => tokio::spawn(async { std::future::pending::<()>().await }),
    };

    let http_server = tokio::spawn(async move {
        let direction_4 = transport_http::router(
            http_registry.clone(),
            task_store_dir,
            lease_timeout,
            call_timeout,
            public_url,
            event_log,
        );
        let direction_2 = transport_a2a_passthrough::router(http_registry);
        let mut app = direction_4.merge(direction_2);
        // Часть 4.6: смена уровня логирования «на лету» через /debug/level.
        // Роутер отдельный (не в transport_http::router — сигнатура последнего
        // зафиксирована интеграционными тестами), мержим в общий app.
        if let Some(handle) = reload_handle {
            let debug_tokens: std::collections::HashSet<String> = raw_config
                .tokens
                .iter()
                .map(|t| resolve_env_placeholders(t))
                .collect::<anyhow::Result<_>>()
                .expect("токены уже валидированы в build_registry");
            app = app.merge(debug_router(
                handle,
                debug_tokens,
                raw_config.logging.debug_ttl_minutes,
            ));
        }

        let listener = tokio::net::TcpListener::bind(&http_listen).await?;
        axum::serve(listener, app)
            .await
            .map_err(anyhow::Error::from)
    });

    // Фоновая уборка задач. Ходит по каталогам агентов на диске, а не
    // по живым адаптерам: задачи остановленного агента тоже надо
    // убирать, а его адаптера в памяти уже нет.
    let sweeper = tokio::spawn(async move {
        loop {
            tokio::time::sleep(TASK_SWEEP_INTERVAL).await;
            match sweep_all_agents(&sweep_dir, task_ttl).await {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "фоновая уборка задач"),
                Err(e) => tracing::warn!(error = %e, "уборка задач не удалась"),
            }
        }
    });

    // ДОБАВЛЕНО (Часть 4.4 роадмапа стриминга): монитор размера
    // лог-каталога — защита от расхождения между max_files (N файлов)
    // и max_total_size_mb (N мегабайт) при резком скачке размера файла.
    // Ходит раз в час, как sweeper.
    let log_monitor = if raw_config.logging.output == "file" || raw_config.logging.output == "both"
    {
        let log_dir = raw_config
            .logging
            .file
            .path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| "/var/log".to_string());
        let limit_mb = raw_config.logging.file.max_total_size_mb;
        let max_file_size_mb = raw_config.logging.file.max_file_size_mb;
        let compress_rotated = raw_config.logging.file.compress_rotated;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TASK_SWEEP_INTERVAL).await;
                let current_mb = dir_size_mb(&log_dir).await;
                let largest_file_mb = largest_file_size_mb(&log_dir).await;
                // Отдельный порог: один файл перерос max_file_size_mb —
                // ротация по числу файлов могла не сработать на размер.
                if max_file_size_mb > 0 && largest_file_mb > max_file_size_mb {
                    tracing::warn!(
                        current_size_mb = current_mb,
                        max_file_size_mb,
                        largest_file_mb,
                        "один файл лога перерос max_file_size_mb — ротация может не успевать"
                    );
                }
                let pct = if limit_mb == 0 {
                    0
                } else {
                    (current_mb as f64 / limit_mb as f64 * 100.0) as u64
                };
                if pct >= 100 {
                    tracing::error!(
                        current_size_mb = current_mb,
                        limit_mb,
                        "лог-каталог превысил max_total_size_mb — принудительное удаление старейших файлов"
                    );
                    // Часть 4.4: не только предупредить, а реально урезать.
                    // gzip-сжатие ротированных (если включено), затем удаление
                    // старейших, пока суммарный размер не вернётся к лимиту.
                    let removed = prune_log_dir(&log_dir, limit_mb, compress_rotated).await;
                    tracing::warn!(removed, "лог-каталог урезан до max_total_size_mb");
                } else if pct >= 80 {
                    tracing::warn!(
                        current_size_mb = current_mb,
                        limit_mb,
                        "лог-каталог приближается к max_total_size_mb (>80%) — рассмотрите понижение уровня логирования или увеличение лимита"
                    );
                }
            }
        })
    } else {
        tokio::spawn(async { std::future::pending::<()>().await })
    };

    tokio::select! {
        res = tcp_server => res??,
        res = http_server => res??,
        res = sweeper => res?,
        res = log_monitor => res?,
        res = health_task => res?,
    }

    Ok(())
}

/// Суммарный размер каталога (в МБ) — для монитора лог-ротации.
async fn dir_size_mb(dir: &str) -> u64 {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut total_bytes = 0u64;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() {
                total_bytes += meta.len();
            }
        }
    }
    total_bytes / (1024 * 1024)
}

/// Размер самого крупного файла в каталоге (в МБ) — для монитора
/// max_file_size_mb.
async fn largest_file_size_mb(dir: &str) -> u64 {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut largest = 0u64;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() && meta.len() > largest {
                largest = meta.len();
            }
        }
    }
    largest / (1024 * 1024)
}

/// Список файлов каталога с mtime и размером — для чистки лог-каталога.
async fn collect_log_files(
    dir: &str,
) -> Vec<(std::path::PathBuf, std::time::SystemTime, u64)> {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() {
                if let Ok(mtime) = meta.modified() {
                    files.push((entry.path(), mtime, meta.len()));
                }
            }
        }
    }
    files
}

/// gzip-сжатие файла на месте: файл -> файл.gz, оригинал удаляется.
async fn gzip_file(path: &std::path::Path) -> bool {
    use std::io::Write;

    let Ok(data) = tokio::fs::read(path).await else {
        return false;
    };
    let gz_path = path.with_extension("log.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    if encoder.write_all(&data).is_err() {
        return false;
    }
    let Ok(gz) = encoder.finish() else {
        return false;
    };
    if tokio::fs::write(&gz_path, gz).await.is_err() {
        return false;
    }
    tokio::fs::remove_file(path).await.is_ok()
}

/// Часть 4.4: реальная чистка лог-каталога при превышении
/// max_total_size_mb. Сначала gzip-сжимает ротированные (не активные)
/// файлы, затем удаляет старейшие, пока суммарный размер не опустится
/// ниже лимита. Активный (самый свежий) файл не трогается. Возвращает
/// число обработанных файлов.
async fn prune_log_dir(dir: &str, limit_mb: u64, compress: bool) -> usize {
    if limit_mb == 0 {
        return 0;
    }
    let limit_bytes = limit_mb * 1024 * 1024;
    let mut handled = 0usize;

    let mut files = collect_log_files(dir).await;
    if files.is_empty() {
        return 0;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime);
    let active = files.last().map(|(path, _, _)| path.clone());

    // 1) gzip-компрессия старых ротированных файлов.
    if compress {
        for (path, _, _) in &files {
            if Some(path) == active.as_ref() {
                continue;
            }
            if path.extension().map(|e| e.to_str()) == Some(Some("gz")) {
                continue;
            }
            if gzip_file(path).await {
                handled += 1;
            }
        }
    }

    // 2) Удаление старейших, пока суммарный размер выше лимита.
    loop {
        let files = collect_log_files(dir).await;
        if files.len() <= 1 {
            break;
        }
        let total: u64 = files.iter().map(|(_, _, size)| *size).sum();
        if total <= limit_bytes {
            break;
        }
        let mut sorted = files.clone();
        sorted.sort_by_key(|(_, mtime, _)| *mtime);
        let oldest = sorted.first().cloned();
        match oldest {
            Some((path, _, _)) if Some(&path) != active.as_ref() => {
                if tokio::fs::remove_file(&path).await.is_ok() {
                    handled += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    handled
}

/// Часть 4.6: reload-фильтр. Возвращает handle для смены уровня «на
/// лету» через POST /debug/level; None — когда level: "off" (клапан).
fn tracing_subscriber_init(
    logging: &LoggingConfig,
) -> Option<reload::Handle<EnvFilter, tracing_subscriber::Registry>> {
    // Аварийный клапан (Часть 4.5): level: "off" полностью отключает
    // фильтр. Стартовое сообщение печатается в stderr ДО отключения —
    // иначе оператор не отличит "не пишет логи по конфигу" от "не
    // запустился".
    if logging.level == "off" {
        eprintln!(
            "[gatewayd] ВНИМАНИЕ: логирование полностью отключено (logging.level: off) — диагностика по логам будет недоступна"
        );
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("off"))
            .try_init();
        return None;
    }

    // Единая цепочка registry() + reload-слой, чтобы фильтр был меняемым.
    // Обёртка reload::Layer не пишет сама — рядом кладём fmt-слой(и) под
    // output: stdout|file|both, как было. Ветки раздельные, потому что
    // каждый .with() меняет тип Layered — переприсваивание не скомпилилось бы.
    let (filter, handle) = reload::Layer::new(EnvFilter::new(&logging.level));
    let output_stdout = logging.output == "stdout" || logging.output == "both";
    let output_file = logging.output == "file" || logging.output == "both";

    let base = tracing_subscriber::registry().with(filter);
    if output_stdout && output_file {
        let file_appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("gateway")
            .max_log_files(logging.file.max_files)
            .build(&logging.file.path)
            .expect("file appender builds");
        let _ = base
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
            .with(tracing_subscriber::fmt::layer().with_writer(file_appender))
            .try_init();
    } else if output_file {
        let file_appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("gateway")
            .max_log_files(logging.file.max_files)
            .build(&logging.file.path)
            .expect("file appender builds");
        let _ = base
            .with(tracing_subscriber::fmt::layer().with_writer(file_appender))
            .try_init();
    } else {
        let _ = base
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
            .try_init();
    }
    Some(handle)
}

/// Состояние debug-эндпоинта смены уровня логирования (Часть 4.6).
#[derive(Clone)]
struct DebugLevelState {
    handle: reload::Handle<EnvFilter, tracing_subscriber::Registry>,
    tokens: std::sync::Arc<std::collections::HashSet<String>>,
    ttl_minutes: u64,
    current: std::sync::Arc<tokio::sync::RwLock<String>>,
}

/// Часть 4.6: эндпоинт /debug/level — смена уровня логирования «на лету».
///   GET  /debug/level            -> текущий уровень
///   POST /debug/level            -> body {"level":"debug"} + Bearer-токен
/// Уровни debug|trace включают автокат к "info" через debug_ttl_minutes.
fn debug_router(
    handle: reload::Handle<EnvFilter, tracing_subscriber::Registry>,
    tokens: std::collections::HashSet<String>,
    ttl_minutes: u64,
) -> axum::Router {
    use axum::routing::get;

    let state = DebugLevelState {
        handle,
        tokens: std::sync::Arc::new(tokens),
        ttl_minutes,
        current: std::sync::Arc::new(tokio::sync::RwLock::new("info".to_string())),
    };
    axum::Router::new()
        .route("/debug/level", get(get_debug_level).post(set_debug_level))
        .with_state(state)
}

async fn get_debug_level(
    axum::extract::State(state): axum::extract::State<DebugLevelState>,
) -> String {
    state.current.read().await.clone()
}

async fn set_debug_level(
    axum::extract::State(state): axum::extract::State<DebugLevelState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !state.tokens.contains(token) {
        return (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response();
    }

    let level: String = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("level")
                .and_then(|l| l.as_str())
                .map(|s| s.to_lowercase())
        })
        .unwrap_or_default();
    const VALID_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];
    if !VALID_LEVELS.contains(&level.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid level: {level} (expected one of {VALID_LEVELS:?})"),
        )
            .into_response();
    }

    let previous = state.current.read().await.clone();
    if state
        .handle
        .modify(|f| *f = EnvFilter::new(&level))
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "reload failed").into_response();
    }
    *state.current.write().await = level.clone();

    if level == "debug" || level == "trace" {
        tracing::warn!(
            new_level = %level,
            ttl_minutes = state.ttl_minutes,
            "логирование временно расширено — автоматический откат к info через debug_ttl_minutes"
        );
        if state.ttl_minutes > 0 {
            let handle = state.handle.clone();
            let current = state.current.clone();
            let ttl_minutes = state.ttl_minutes;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(ttl_minutes * 60)).await;
                if handle.modify(|f| *f = EnvFilter::new("info")).is_ok() {
                    *current.write().await = "info".to_string();
                }
                tracing::warn!("debug_ttl_minutes истёк — уровень логирования возвращён к info");
            });
        }
    } else {
        tracing::warn!(new_level = %level, previous = %previous, "уровень логирования изменён через /debug/level");
    }

    (
        StatusCode::OK,
        serde_json::json!({ "level": level, "previous": previous }).to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod config_tests {
    use super::*;

    /// T8: конфиг без секции streaming: использует дефолты
    /// (max_concurrent_streams=1, first=15, idle=120), не паникует.
    #[test]
    fn agent_without_streaming_section_gets_defaults() {
        let yaml = r#"
listen: "0.0.0.0:8347"
tokens: ["t-1"]
agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
task_store_dir: "/tmp/x"
turn_lease_timeout_secs: 30
"#;
        let raw: RawConfig = serde_yaml::from_str(yaml).expect("YAML парсится");
        let entry = raw.agents.get("claurst-main").expect("агент есть");
        let streaming = match entry {
            RawAgentEntry::Stdio { streaming, .. } => streaming,
            RawAgentEntry::Http { streaming, .. } => streaming,
        };
        assert_eq!(streaming.max_concurrent_streams, 1);
        assert_eq!(streaming.first_chunk_timeout_secs, 15);
        assert_eq!(streaming.idle_chunk_timeout_secs, 120);
    }

    /// T8: max_concurrent_streams == 0 -> ошибка старта (fail-closed),
    /// по конвенции проекта (пустой токен уже так падает).
    #[test]
    fn max_concurrent_streams_zero_fails_startup() {
        let yaml = r#"
listen: "0.0.0.0:8347"
tokens: ["t-1"]
agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    streaming: { max_concurrent_streams: 0 }
task_store_dir: "/tmp/x"
turn_lease_timeout_secs: 30
"#;
        let raw: RawConfig = serde_yaml::from_str(yaml).expect("YAML парсится");
        let all: std::collections::HashSet<String> = raw.agents.keys().cloned().collect();
        let err = match build_registry(&raw, &all) {
            Ok(_) => panic!("build_registry должен отклонить max_concurrent_streams=0"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("streaming.max_concurrent_streams не может быть 0"),
            "должна быть явная ошибка валидации, got: {err}"
        );
    }

    /// T8: явная секция streaming: переопределяет дефолты.
    #[test]
    fn agent_with_explicit_streaming_section_overrides_defaults() {
        let yaml = r#"
listen: "0.0.0.0:8347"
tokens: ["t-1"]
agents:
  claurst-main:
    transport: stdio
    command: ["claurst", "acp"]
    streaming:
      max_concurrent_streams: 4
      first_chunk_timeout_secs: 5
      idle_chunk_timeout_secs: 60
task_store_dir: "/tmp/x"
turn_lease_timeout_secs: 30
"#;
        let raw: RawConfig = serde_yaml::from_str(yaml).expect("YAML парсится");
        let entry = raw.agents.get("claurst-main").expect("агент есть");
        let streaming = match entry {
            RawAgentEntry::Stdio { streaming, .. } => streaming,
            RawAgentEntry::Http { streaming, .. } => streaming,
        };
        assert_eq!(streaming.max_concurrent_streams, 4);
        assert_eq!(streaming.first_chunk_timeout_secs, 5);
        assert_eq!(streaming.idle_chunk_timeout_secs, 60);
    }
}
