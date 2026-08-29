// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{Context, Result};
use privchat::{
    cli::Cli,
    config::{self, ServerConfig},
    logging, ChatServer,
};
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
            privchat::cli::Commands::Migrate => {
                return run_migrate(&cli).await;
            }
            privchat::cli::Commands::GenerateConfig { path } => {
                return generate_config(path);
            }
            privchat::cli::Commands::ValidateConfig { path } => {
                return validate_config(path);
            }
            privchat::cli::Commands::ShowConfig => {
                return show_config(&cli);
            }
            privchat::cli::Commands::BackfillPrivacySettings { input, dry_run } => {
                return run_backfill_privacy_settings(&cli, input, *dry_run).await;
            }
            privchat::cli::Commands::BackfillMediaRefs {
                batch_size,
                since,
                verify_only,
            } => {
                return run_backfill_media_refs(&cli, *batch_size, *since, *verify_only).await;
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

    let log_retention_days = early_log
        .retention_days
        .unwrap_or(logging::DEFAULT_LOG_RETENTION_DAYS);

    logging::init_logging(
        &log_level,
        log_format.as_deref(),
        log_file,
        cli.quiet,
        log_retention_days,
    )?;

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
    tracing::info!("  - Account Mode: {:?}", config.account.mode);

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

/// 生成默认配置文件及其 TLS 证书（实现在 lib 侧，便于单测真实产物）。
fn generate_config(path: &str) -> Result<()> {
    config::generate_config_with_tls(path)
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
    // Run the complete migration sequence on one connection. The baseline SQL
    // intentionally changes session search_path while restoring a pg_dump, so
    // hopping across pooled connections makes later migrations nondeterministic.
    let mut connection = pool.acquire().await.context("获取迁移连接失败")?;

    // 创建迁移记录表（如果不存在）
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS public.privchat_migrations (
            id SERIAL PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(&mut *connection)
    .await
    .context("创建迁移记录表失败")?;

    // 查询已执行的迁移
    let applied: Vec<String> =
        sqlx::query_scalar("SELECT name FROM public.privchat_migrations ORDER BY id")
            .fetch_all(&mut *connection)
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
            .execute(&mut *connection)
            .await
            .with_context(|| format!("执行迁移失败: {}", name))?;

        sqlx::query("SET search_path TO public")
            .execute(&mut *connection)
            .await
            .with_context(|| format!("恢复迁移 search_path 失败: {}", name))?;

        // The baseline schema migration intentionally rebuilds the public
        // schema, including dropping this bookkeeping table. Recreate it before
        // recording the baseline so a fresh database can continue with 002+.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS public.privchat_migrations (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )",
        )
        .execute(&mut *connection)
        .await
        .with_context(|| format!("重建迁移记录表失败: {}", name))?;

        // 记录迁移
        sqlx::query("INSERT INTO public.privchat_migrations (name) VALUES ($1)")
            .bind(*name)
            .execute(&mut *connection)
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

    drop(connection);
    pool.close().await;
    Ok(())
}

/// 显示最终配置（合并后的配置）
fn show_config(cli: &Cli) -> Result<()> {
    // 初始化基本日志（用于显示配置）
    logging::init_logging("info", None, None, false, logging::DEFAULT_LOG_RETENTION_DAYS)?;

    let config = ServerConfig::load(cli).context("加载配置失败")?;

    println!("📊 最终配置（合并后的配置）:");
    println!("{}", serde_json::to_string_pretty(&config)?);

    Ok(())
}

/// 回填消息 → 文件引用表，并做零缺口校验。
///
/// 校验结果**决定 `get_url` 能不能切到引用表**：缺口不为零就切，等于让那部分
/// 消息的附件在切换那一刻起下不动。所以这里把缺口数打出来，而不是只报成功。
async fn run_backfill_media_refs(
    cli: &Cli,
    batch_size: i64,
    since: i64,
    verify_only: bool,
) -> Result<()> {
    let _ = dotenvy::dotenv();
    let database_url = cli
        .database_url
        .clone()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("需要 DATABASE_URL，请在 .env 或环境变量中配置")?;

    println!("🔌 连接数据库...");
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .context("数据库连接失败，请检查 DATABASE_URL")?;

    if !verify_only {
        println!("▶ 回填引用表（batch={batch_size}, since={since}）...");
        let report = privchat::service::media_ref_backfill::backfill_from(&pool, batch_size, since)
            .await
            .context("回填失败")?;
        println!("  扫描消息        {}", report.scanned);
        println!("  含引用消息      {}", report.messages_with_refs);
        println!("  新写入引用行    {}", report.refs_inserted);
        println!(
            "  需人工复核      {}（metadata 解不出 {} / 无引用 {} / 缺主体文件 {}）",
            report.audited(),
            report.audit_undecodable,
            report.audit_no_refs,
            report.audit_missing_original
        );
    }

    println!("▶ 零缺口校验...");
    let report = privchat::service::media_ref_backfill::verify_no_gaps(&pool, batch_size)
        .await
        .context("校验失败")?;
    let missing = &report.mismatches;
    // 🔴 审计条数也是判据：解析不出引用的媒体消息「期望集合」本来就空，
    // 表里也空，集合比对当然一致——那条消息的附件却永远下不动。
    // 只报数字不影响退出码，等于把问题埋进成功日志。
    if report.audited > 0 {
        println!(
            "⚠️ 需人工复核 {} 条（metadata 解不出 {} / 无引用 {} / 缺主体文件 {}）",
            report.audited,
            report.audit_undecodable,
            report.audit_no_refs,
            report.audit_missing_original
        );
    }
    if report.is_clean() {
        println!("✅ 引用集合逐条一致，且没有解析审计问题。");
    } else if missing.is_empty() {
        anyhow::bail!(
            "引用集合一致，但有 {} 条消息的媒体解析不出引用；\
             这些消息的附件在切换后会下不动。要么修数据，要么提供一份\
             经过审批的隔离清单，再谈切换 get_url 授权来源",
            report.audited
        );
    } else {
        // 只报「差了几条」会漏掉「数量对但内容错位」，而 ON CONFLICT DO NOTHING
        // 恰恰会把写错的那条原样留着。所以这里逐条打出 missing / unexpected。
        println!("❌ 引用不一致 {} 条，前 20 条：", missing.len());
        for mismatch in missing.iter().take(20) {
            println!(
                "   message_id={} missing={:?} unexpected={:?}",
                mismatch.message_id, mismatch.missing, mismatch.unexpected
            );
        }
        anyhow::bail!("引用表与 metadata 不一致，不得切换 get_url 授权来源");
    }
    Ok(())
}

/// 把只存在于 Redis 里的隐私设置回填进数据库（上线 DB 真源前的必做步骤）。
async fn run_backfill_privacy_settings(cli: &Cli, input: &str, dry_run: bool) -> Result<()> {
    use std::io::{BufRead, BufReader};

    let _ = dotenvy::dotenv();
    let database_url = cli
        .database_url
        .clone()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("需要 DATABASE_URL")?;
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .context("数据库连接失败")?;

    let file = std::fs::File::open(input).with_context(|| format!("打不开 {input}"))?;
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.context("读行失败")?;
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line).context("导出行不是 JSON")?;
        let user_id = row
            .get("user_id")
            .and_then(|v| v.as_u64())
            .context("缺 user_id")?;
        let settings = row.get("settings").cloned().context("缺 settings")?;
        entries.push((user_id, settings));
    }

    let report =
        privchat::service::privacy_backfill::backfill_from_entries(&pool, entries, dry_run)
            .await
            .context("回填失败")?;
    println!("  扫描        {}", report.scanned);
    println!("  写入        {}{}", report.written, if dry_run { "（dry-run，未写）" } else { "" });
    println!("  未变更      {}（DB 已是目标值，未写、未 bump sync_version）", report.unchanged);
    println!("  解析不出    {}", report.undecodable);
    println!("  用户不存在  {}", report.user_missing);
    if report.undecodable > 0 {
        anyhow::bail!(
            "有 {} 条 Redis 里的隐私设置解析不出来；这些用户的设置会回落默认（允许陌生人消息），\
             必须人工确认后再上线",
            report.undecodable
        );
    }
    Ok(())
}
