use anyhow::{Context, Result};
use privchat_server::{
    cli::Cli,
    config::{self, ServerConfig},
    logging, ChatServer,
};
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
            privchat_server::cli::Commands::Migrate => {
                return run_migrate(&cli).await;
            }
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

    // 快速读取 config.toml 的 [logging] 段（不加载完整配置）
    let early_log = config::load_early_logging_config(cli.config_file.as_deref());

    // 合并日志配置（优先级：CLI > config.toml > 默认值）
    let log_level = cli
        .get_log_level()
        .or(early_log.level)
        .unwrap_or_else(|| "info".to_string());
    let log_format = cli.get_log_format().or(early_log.format);
    let log_file = cli.log_file.as_deref().or(early_log.file.as_deref());

    logging::init_logging(&log_level, log_format.as_deref(), log_file, cli.quiet)?;

    tracing::info!("🚀 PrivChat Server starting...");

    // 加载配置（按优先级：命令行 > 环境变量 > 配置文件 > 默认值）
    let config = ServerConfig::load(&cli).context("加载配置失败")?;

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
    tracing::info!(
        "  - Log Format: {:?}",
        log_format.as_deref().unwrap_or("compact")
    );
    if let Some(f) = log_file {
        tracing::info!("  - Log File: {}", f);
    }
    tracing::info!("  - Protocols: {:?}", config.enabled_protocols);

    // 创建服务器（如果数据库连接或目录创建等失败，会打印错误并退出）
    let server = match ChatServer::new(config).await {
        Ok(server) => server,
        Err(e) => {
            tracing::error!("❌ 服务器初始化失败: {}", e);
            tracing::error!("💡 请检查配置、数据库连接及文件存储目录等后重试");
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

[gateway]
max_connections = 100000
connection_timeout = 300
heartbeat_interval = 60
use_internal_auth = true

[[gateway.listeners]]
protocol = "tcp"
host = "0.0.0.0"
port = 9001

[[gateway.listeners]]
protocol = "quic"
host = "0.0.0.0"
port = 9001

[[gateway.listeners]]
protocol = "websocket"
host = "0.0.0.0"
port = 9080
path = "/gate"
compression = true

[cache]
l1_max_memory_mb = 256
l1_ttl_secs = 3600

[cache.online_status]
timeout_seconds = 300
cleanup_interval_seconds = 60

[file]
default_storage_source_id = 0
server_port = 9083
server_api_base_url = "http://localhost:9083/api/app"

[[file.storage_sources]]
id = 0
storage_type = "local"
storage_root = "./storage/files"
base_url = "http://localhost:9083/files"

[logging]
level = "info"
format = "compact"
# file = "./logs/server.log"
"#;

    fs::write(path, default_config).with_context(|| format!("无法写入配置文件: {}", path))?;

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

// 编译时自动扫描 migrations/ 目录，按文件名排序嵌入（跳过 000_ 开头的文件）
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// 执行数据库迁移
async fn run_migrate(cli: &Cli) -> Result<()> {
    let _ = dotenvy::dotenv();

    // 获取 DATABASE_URL（从 CLI > 环境变量 > 配置文件）
    let database_url = cli
        .database_url
        .clone()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("需要 DATABASE_URL，请在 .env 或环境变量中配置")?;

    println!("🔌 连接数据库...");
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .context("数据库连接失败，请检查 DATABASE_URL")?;

    // 创建迁移记录表（如果不存在）
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS privchat_migrations (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(&pool)
    .await
    .context("创建迁移记录表失败")?;

    // 查询已执行的迁移
    let applied: Vec<String> =
        sqlx::query_scalar("SELECT name FROM privchat_migrations ORDER BY id")
            .fetch_all(&pool)
            .await
            .context("查询迁移记录失败")?;

    let mut count = 0;
    for (name, sql) in MIGRATIONS {
        if applied.contains(&name.to_string()) {
            println!("  ⏭ {} (已执行，跳过)", name);
            continue;
        }

        println!("  ▶ 执行 {}...", name);
        sqlx::raw_sql(sql)
            .execute(&pool)
            .await
            .with_context(|| format!("执行迁移失败: {}", name))?;

        // 记录迁移
        sqlx::query("INSERT INTO privchat_migrations (name) VALUES ($1)")
            .bind(*name)
            .execute(&pool)
            .await
            .with_context(|| format!("记录迁移状态失败: {}", name))?;

        println!("  ✅ {} 完成", name);
        count += 1;
    }

    if count == 0 {
        println!("✅ 数据库已是最新，无需迁移");
    } else {
        println!("✅ 成功执行 {} 个迁移", count);
    }

    pool.close().await;
    Ok(())
}

/// 显示最终配置（合并后的配置）
fn show_config(cli: &Cli) -> Result<()> {
    // 初始化基本日志（用于显示配置）
    logging::init_logging("info", None, None, false)?;

    let config = ServerConfig::load(cli).context("加载配置失败")?;

    println!("📊 最终配置（合并后的配置）:");
    println!("{}", serde_json::to_string_pretty(&config)?);

    Ok(())
}
