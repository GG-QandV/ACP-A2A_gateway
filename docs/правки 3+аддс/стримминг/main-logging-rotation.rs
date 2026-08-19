// gatewayd/src/main.rs — задача F (tracing-appender + ротация логов).
// Написано против ТОЧНОГО текущего кода main.rs (уже проверен в этой
// сессии) — расширяет tracing_subscriber_init() и RawConfig по тому же
// паттерну default_*() функций, что уже используется в файле.
// Схема конфига — по streaming-roadmap-checklist.md, раздел 4.3, числа
// оттуда не меняются (согласованы между собой: 100 x 10 = 1000).

use std::path::PathBuf;
use serde::Deserialize;

// =========================================================================
// 1. Cargo.toml — единственная новая зависимость этой задачи.
// =========================================================================
//
// ЗАМЕНИТЬ В gatewayd/Cargo.toml, секция [dependencies]:
//     tracing-appender = "0.2"

// =========================================================================
// 2. RawConfig — новая опциональная секция, по образцу уже существующих
//    default_agent_call_timeout_secs()/default_task_retention_days().
// =========================================================================

/*
ЗАМЕНИТЬ В RawConfig, добавить поле:
    #[serde(default)]
    logging: LoggingConfig,
*/

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// "info" | "debug" | "trace" | "warn" | "error" | "off"
    pub level: String,
    /// "stdout" | "file" | "both"
    pub output: String,
    pub file: LoggingFileConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            output: "stdout".to_string(), // дефолт = текущее поведение, без изменений
            file: LoggingFileConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingFileConfig {
    pub path: PathBuf,
    pub max_file_size_mb: u64,
    pub max_files: usize,
    pub max_total_size_mb: u64,
    pub compress_rotated: bool,
    /// Как часто фоновая задача проверяет суммарный объём каталога
    /// логов и принудительно чистит его при превышении max_total_size_mb
    /// (защита от расхождения между "N файлов" и "N мегабайт" —
    /// см. streaming-roadmap-checklist.md, раздел 4.4).
    pub check_interval_secs: u64,
}

impl Default for LoggingFileConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("/var/log/acp-a2a-gateway/gateway.log"),
            max_file_size_mb: 100,
            max_files: 10,
            max_total_size_mb: 1000, // 100 x 10 — согласовано с max_file_size_mb x max_files
            compress_rotated: true,
            check_interval_secs: 3600, // раз в час, тот же интервал что TASK_SWEEP_INTERVAL
        }
    }
}

// =========================================================================
// 3. Валидация на старте — по конвенции проекта (пустой токен, отсутствующая
//    env-переменная уже фейлят старт таким же образом в build_registry()).
// =========================================================================

/// Вызывается из main() до tracing_subscriber_init() — но ВНИМАНИЕ: до
/// инициализации tracing нет возможности залогировать саму ошибку
/// валидации через tracing::error! — используем anyhow::bail! как и
/// остальные валидации конфига в этом файле (resolve_env_placeholders,
/// build_registry), они тоже падают до подъёма логирования.
fn validate_logging_config(cfg: &LoggingConfig) -> anyhow::Result<()> {
    if !matches!(cfg.level.as_str(), "info" | "debug" | "trace" | "warn" | "error" | "off") {
        anyhow::bail!("logging.level: неизвестное значение {:?} (допустимо: info|debug|trace|warn|error|off)", cfg.level);
    }
    if !matches!(cfg.output.as_str(), "stdout" | "file" | "both") {
        anyhow::bail!("logging.output: неизвестное значение {:?} (допустимо: stdout|file|both)", cfg.output);
    }
    if cfg.output != "stdout" {
        if cfg.file.max_file_size_mb == 0 {
            anyhow::bail!("logging.file.max_file_size_mb не может быть 0 при output={:?}", cfg.output);
        }
        if cfg.file.max_files == 0 {
            anyhow::bail!("logging.file.max_files не может быть 0 при output={:?}", cfg.output);
        }
        if cfg.file.max_total_size_mb < cfg.file.max_file_size_mb {
            // Не строгая ошибка логики (max_total < один файл — абсурдно
            // мало), но именно тот случай, где "тихий дефолт" хуже явного
            // отказа старта — по тому же принципу, что уже применён к
            // пустому токену в build_registry().
            anyhow::bail!(
                "logging.file.max_total_size_mb ({}) меньше max_file_size_mb ({}) — так суммарный лимит никогда не сможет вместить даже один файл",
                cfg.file.max_total_size_mb, cfg.file.max_file_size_mb
            );
        }
    }
    Ok(())
}

// =========================================================================
// 4. tracing_subscriber_init() — расширение существующей функции.
// =========================================================================

/*
БЫЛО:
    fn tracing_subscriber_init() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    }

СТАНЕТ (принимает LoggingConfig параметром — вызывающий код в main()
передаёт raw_config.logging ПОСЛЕ его парсинга, но ДО остальной
инициализации, чтобы дальнейшие tracing::info!/warn! в build_registry
и остальном коде main() уже шли через новый конфиг):
*/

pub fn tracing_subscriber_init(cfg: &LoggingConfig) -> anyhow::Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    use tracing_subscriber::prelude::*;

    // Аварийный клапан "off" — см. streaming-roadmap-checklist.md, 4.5.
    // Стартовое сообщение печатается НАПРЯМУЮ в stderr, минуя tracing,
    // потому что после этой ветки фильтр будет "off" и обычный
    // tracing::warn! до подписчика не дойдёт.
    if cfg.level == "off" {
        eprintln!("[gatewayd] ВНИМАНИЕ: логирование полностью отключено (logging.level: off) — диагностика по логам будет недоступна");
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("off"))
            .try_init();
        return Ok(None);
    }

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(cfg.level.clone()));

    match cfg.output.as_str() {
        // Дефолт — БЕЗ ИЗМЕНЕНИЙ относительно текущего поведения.
        "stdout" => {
            let _ = tracing_subscriber::fmt().with_env_filter(env_filter).try_init();
            Ok(None)
        }
        "file" | "both" => {
            let rotation = tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("gateway")
                .filename_suffix("log")
                .max_log_files(cfg.file.max_files) // <- механизм затирания старших версий
                .build(cfg.file.path.parent().unwrap_or_else(|| std::path::Path::new(".")))
                .map_err(|e| anyhow::anyhow!("не удалось инициализировать ротацию логов: {e}"))?;

            // non_blocking() возвращает WorkerGuard, который ДОЛЖЕН жить
            // до конца процесса (Drop останавливает фоновый writer) —
            // main() обязан удерживать возвращённый Option в переменной
            // до конца функции main, иначе логи в файл перестанут писаться
            // сразу после возврата из tracing_subscriber_init().
            let (non_blocking, guard) = tracing_appender::non_blocking(rotation);
            let file_layer = tracing_subscriber::fmt::layer().with_writer(non_blocking);

            if cfg.output == "both" {
                let stdout_layer = tracing_subscriber::fmt::layer();
                let _ = tracing_subscriber::registry()
                    .with(env_filter)
                    .with(stdout_layer)
                    .with(file_layer)
                    .try_init();
            } else {
                let _ = tracing_subscriber::registry()
                    .with(env_filter)
                    .with(file_layer)
                    .try_init();
            }
            Ok(Some(guard))
        }
        _ => unreachable!("валидировано в validate_logging_config()"),
    }
}

// =========================================================================
// 5. Правка main() — порядок вызовов и удержание WorkerGuard.
// =========================================================================

/*
ЗАМЕНИТЬ В main():

БЫЛО:
    #[tokio::main]
    async fn main() -> anyhow::Result<()> {
        tracing_subscriber_init();

        let config_path = ...;
        let raw_yaml = ...;
        let raw_config: RawConfig = serde_yaml::from_str(&raw_yaml)...;
        ...

СТАНЕТ (конфиг парсится РАНЬШЕ инициализации логирования — иначе
logging.level/output неизвестны до чтения самого конфига; это меняет
порядок относительно текущего кода, где tracing инициализируется первой
строкой main — принято сознательно, riск минимален: до подъёма
логирования maximum что может произойти — ошибка чтения/парсинга
конфига, которая и сейчас выводится через anyhow::Context в exit code,
не через tracing):

    #[tokio::main]
    async fn main() -> anyhow::Result<()> {
        let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.yaml".to_string());
        let raw_yaml = std::fs::read_to_string(&config_path)
            .with_context(|| format!("не удалось прочитать конфиг: {config_path}"))?;
        let raw_config: RawConfig = serde_yaml::from_str(&raw_yaml)
            .with_context(|| format!("не удалось распарсить конфиг: {config_path}"))?;

        validate_logging_config(&raw_config.logging)?;
        let _log_guard = tracing_subscriber_init(&raw_config.logging)?;
        // ^ ОБЯЗАТЕЛЬНО удерживать в переменной _log_guard до конца main() —
        // если файловое логирование включено (output: file|both) и guard
        // будет отброшен раньше, non-blocking writer остановится и
        // последующие tracing::info!/warn! в этой же функции (например,
        // "starting acp-a2a gateway...") не попадут в файл.

        let tcp_listen = raw_config.listen.clone();
        ... остальной код без изменений ...
*/

// =========================================================================
// 6. Фоновая задача контроля суммарного объёма каталога логов —
//    по образцу уже существующего sweeper для TaskStore в этом же файле.
// =========================================================================

/// Считает суммарный размер каталога логов и, если превышен
/// max_total_size_mb, удаляет старейшие ротированные файлы (по mtime,
/// тот же принцип, что уже применён к TaskStore::sweep_expired —
/// см. docs/decisions.md Р-13: "Уборка задач по mtime файла, а не по
/// времени внутри задачи").
///
/// ВАЖНО: это НЕ замена tracing_appender::max_log_files() — это защита
/// от расхождения между "N файлов" и "N мегабайт", если один файл
/// внезапно вырос сильнее ожидаемого (например, кто-то временно включил
/// TRACE и не рассчитал объём) ДО того, как естественная ротация по
/// количеству файлов успеет сработать.
async fn check_log_directory_size(dir: &PathBuf, max_total_bytes: u64) -> anyhow::Result<()> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut files = Vec::new();
    let mut total: u64 = 0;
    while let Some(entry) = entries.next_entry().await? {
        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                total += meta.len();
                files.push((entry.path(), mtime, meta.len()));
            }
        }
    }

    let limit_pct = (total as f64 / max_total_bytes as f64) * 100.0;
    if limit_pct >= 80.0 && limit_pct < 100.0 {
        // ЛОГ-ЛОВУШКА (WARN, по умолчанию включена) — из streaming-roadmap-checklist.md, 4.4.
        tracing::warn!(
            current_size_mb = total / (1024 * 1024),
            limit_mb = max_total_bytes / (1024 * 1024),
            "лог-каталог приближается к max_total_size_mb (>80%) — рассмотрите понижение уровня логирования или увеличение лимита"
        );
    }

    if total > max_total_bytes {
        tracing::error!(
            current_size_mb = total / (1024 * 1024),
            limit_mb = max_total_bytes / (1024 * 1024),
            "лог-каталог превысил max_total_size_mb — принудительное удаление старейших файлов"
        );
        files.sort_by_key(|(_, mtime, _)| *mtime); // старейшие первыми
        for (path, _, size) in files {
            if total <= max_total_bytes {
                break;
            }
            if tokio::fs::remove_file(&path).await.is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }

    Ok(())
}

/*
ДОБАВИТЬ В main(), рядом с уже существующим `sweeper` (тот же паттерн
tokio::spawn + loop + interval), ТОЛЬКО если logging.output != "stdout":

    let log_size_checker = if raw_config.logging.output != "stdout" {
        let log_dir = raw_config.logging.file.path.parent()
            .unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let max_bytes = raw_config.logging.file.max_total_size_mb * 1024 * 1024;
        let interval = std::time::Duration::from_secs(raw_config.logging.file.check_interval_secs);
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if let Err(e) = check_log_directory_size(&log_dir, max_bytes).await {
                    tracing::warn!(error = %e, "проверка объёма лог-каталога не удалась");
                }
            }
        }))
    } else {
        None
    };

    // В tokio::select! добавляется опциональная ветка — если None,
    // просто не участвует в select (через futures::future::pending()
    // как заглушку, либо через отдельный match перед select!, чтобы не
    // усложнять существующий блок tokio::select! излишней generic-логикой).
*/
