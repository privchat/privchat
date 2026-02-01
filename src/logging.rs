use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use anyhow::Result;

/// 初始化日志系统
pub fn init_logging(
    log_level: &str,
    log_format: Option<&str>,
    _log_file: Option<&str>,  // TODO: 实现文件输出功能
    quiet: bool,
) -> Result<()> {
    // 如果静默模式，只输出错误
    let level = if quiet {
        "error"
    } else {
        log_level
    };

    // 解析日志级别
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    // 根据格式选择不同的输出方式
    match log_format {
        Some("json") => {
            // JSON 格式（适合生产环境）
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().json())
                .init();
        }
        Some("pretty") | Some("dev") => {
            // Pretty 格式（适合开发环境）
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().pretty())
                .init();
        }
        _ => {
            // Compact 格式（默认）
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().compact())
                .init();
        }
    }

    Ok(())
}
