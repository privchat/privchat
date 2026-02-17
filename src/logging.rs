use anyhow::Result;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 初始化日志系统
///
/// - `log_file` 为 None 时只输出到 stdout
/// - `log_file` 指定路径时，同时输出到 stdout 和文件（按天轮转）
///   例如 `--log-file /data/logs/privchat/server.log`
///   会在 `/data/logs/privchat/` 目录下生成 `server.log.2026-02-18` 等文件
pub fn init_logging(
    log_level: &str,
    log_format: Option<&str>,
    log_file: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let level = if quiet { "error" } else { log_level };
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    if let Some(path) = log_file {
        let path = std::path::Path::new(path);
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let filename = path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("server.log"))
            .to_string_lossy();

        std::fs::create_dir_all(dir)?;

        let file_appender = tracing_appender::rolling::daily(dir, filename.as_ref());
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        // 将 guard 泄漏以保持 writer 存活到进程结束
        std::mem::forget(_guard);

        let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);

        let stdout_layer = fmt::layer().compact();

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    } else {
        match log_format {
            Some("json") => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().json())
                    .init();
            }
            Some("pretty") | Some("dev") => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().pretty())
                    .init();
            }
            _ => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().compact())
                    .init();
            }
        }
    }

    Ok(())
}
