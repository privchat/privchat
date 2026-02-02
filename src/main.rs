use privchat_server::{ChatServer, config::ServerConfig, cli::Cli, logging};
use anyhow::{Result, Context};
use std::fs;
use std::process;

#[tokio::main]
async fn main() -> Result<()> {
    // 加载 .env 文件（如果存在）
    let _ = dotenvy::dotenv();
    
    // 解析命令行参数
    let cli = Cli::parse();

    // 处理子命令
    if let Some(command) = &cli.command {
        match command {
            privchat_server::cli::Commands::GenerateConfig { path } => {
                return generate_config(path);
            }
            privchat_server::cli::Commands::ValidateConfig { path } => {
                return validate_config(path);
            }
            privchat_server::cli::Commands::ShowConfig => {
                return show_config(&cli);
            }
        }
    }

    // 初始化日志系统（需要在加载配置之前，但可以使用默认值）
    let log_level = cli.get_log_level().unwrap_or_else(|| "info".to_string());
    let log_format = cli.get_log_format();
    logging::init_logging(
        &log_level,
        log_format.as_deref(),
        cli.log_file.as_deref(),
        cli.quiet,
    )?;

    tracing::info!("🚀 PrivChat Server starting...");

    // 加载配置（按优先级：命令行 > 环境变量 > 配置文件 > 默认值）
    let config = ServerConfig::load(&cli)
        .context("加载配置失败")?;

    // 如果开发模式，应用开发友好设置
    if cli.dev {
        tracing::info!("🔧 开发模式已启用");
    }

    // 显示配置信息
    tracing::info!("📊 Server Configuration:");
    tracing::info!("  - Host: {}", config.host);
    tracing::info!("  - TCP: {}", config.tcp_bind_address);
    tracing::info!("  - WebSocket: {}", config.websocket_bind_address);
    tracing::info!("  - QUIC: {}", config.quic_bind_address);
    tracing::info!("  - HTTP File Server: {}", config.http_file_server_port);
    tracing::info!("  - Max Connections: {}", config.max_connections);
    tracing::info!("  - L1 Cache Memory: {}MB", config.cache.l1_max_memory_mb);
    tracing::info!("  - L1 Cache TTL: {}s", config.cache.l1_ttl_secs);
    tracing::info!("  - Redis L2 Cache: {}", config.cache.has_redis());
    tracing::info!("  - Log Level: {}", config.log_level);
    tracing::info!("  - Log Format: {:?}", log_format);
    tracing::info!("  - Protocols: {:?}", config.enabled_protocols);

    // 创建服务器（如果数据库连接失败，会直接退出）
    let server = match ChatServer::new(config).await {
        Ok(server) => server,
        Err(_) => {
            process::exit(1);
        }
    };

    // 运行服务器
    if let Err(e) = server.run().await {
        tracing::error!("❌ 服务器运行失败: {}", e);
        tracing::error!("💡 服务器将退出");
        process::exit(1);
    }

    Ok(())
}

/// 生成默认配置文件
fn generate_config(path: &str) -> Result<()> {
    let default_config = r#"# PrivChat Server 配置文件
# 此文件由 privchat-server generate-config 生成

[gateway_server]
max_connections = 100000
connection_timeout = 300
heartbeat_interval = 60
use_internal_auth = true

[[gateway_server.listeners]]
protocol = "tcp"
host = "0.0.0.0"
port = 9001

[[gateway_server.listeners]]
protocol = "quic"
host = "0.0.0.0"
port = 9001

[[gateway_server.listeners]]
protocol = "websocket"
host = "0.0.0.0"
port = 9080
path = "/gate"
compression = true

[file_server]
port = 9083
api_base_url = "http://localhost:9083/api/app"

[cache]
l1_max_memory_mb = 256
l1_ttl_secs = 3600

[cache.online_status]
timeout_seconds = 300
cleanup_interval_seconds = 60

[file]
default_storage_source_id = 0

[[file.storage_sources]]
id = 0
storage_type = "local"
storage_root = "./storage/files"
base_url = "http://localhost:9083/files"

[logging]
level = "info"
format = "compact"
"#;

    fs::write(path, default_config)
        .with_context(|| format!("无法写入配置文件: {}", path))?;
    
    println!("✅ 配置文件已生成: {}", path);
    Ok(())
}

/// 验证配置文件
fn validate_config(path: &str) -> Result<()> {
    let config = ServerConfig::from_toml_file(path)
        .with_context(|| format!("配置文件验证失败: {}", path))?;
    
    println!("✅ 配置文件有效: {}", path);
    println!("📊 配置摘要:");
    println!("  - Host: {}", config.host);
    println!("  - Port: {}", config.port);
    println!("  - Max Connections: {}", config.max_connections);
    println!("  - Cache Memory: {}MB", config.cache.l1_max_memory_mb);
    
    Ok(())
}

/// 显示最终配置（合并后的配置）
fn show_config(cli: &Cli) -> Result<()> {
    // 初始化基本日志（用于显示配置）
    logging::init_logging("info", None, None, false)?;
    
    let config = ServerConfig::load(cli)
        .context("加载配置失败")?;
    
    println!("📊 最终配置（合并后的配置）:");
    println!("{}", serde_json::to_string_pretty(&config)?);
    
    Ok(())
}
