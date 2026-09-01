// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0 (the "License").

//! 数据库迁移执行器。
//!
//! 🔴 **迁移的 SQL 和它的记账必须在同一个事务里。**
//!
//! 分成两条独立语句时存在这样一个窗口：SQL 已经生效，记账还没写。进程在那一刻
//! 被杀（部署超时、OOM、连接断开）就会留下"改了但没记"的数据库，而下一次迁移会
//! 把同一个文件**再跑一遍**。
//!
//! 对幂等的迁移这只是浪费；对不幂等的就是事故。`032` 会 `DROP COLUMN file_hash`，
//! 重跑时它的存量审计语句还在引用那一列——迁移当场失败，而数据库停在一个既不是
//! 旧版也不是新版的状态。
//!
//! 事务化之后没有那个窗口：要么两件事都发生，要么一件都没发生。

use anyhow::{Context, Result};
use sqlx::{Acquire, PgConnection};

/// 编译期扫描 `migrations/` 生成，跳过 `000_` 开头的文件。
///
/// 🔴 **生产和测试必须读同一份**。测试自己扫一遍目录的话，它跑的迁移集合可以和
/// 生产不同（比如把生产刻意跳过的 `000_drop_all_tables.sql` 也跑了），
/// 于是"测过了"证明的是另一件事。
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// 记账表。基线迁移会重建 public schema、把它一起删掉，所以每次都要确保存在。
const ENSURE_LEDGER: &str = "CREATE TABLE IF NOT EXISTS public.privchat_migrations (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)";

/// 已执行的迁移名。
pub async fn applied_migrations(conn: &mut PgConnection) -> Result<Vec<String>> {
    sqlx::query(ENSURE_LEDGER)
        .execute(&mut *conn)
        .await
        .context("创建迁移记录表失败")?;
    sqlx::query_scalar("SELECT name FROM public.privchat_migrations ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .context("查询迁移记录失败")
}

/// 执行尚未执行的迁移，返回这次真正跑了哪些。
///
/// 🔴 **必须传一条独占的连接，不能用连接池。** 基线迁移是 pg_dump 的产物，开头带
/// `set_config('search_path', '', false)`，而那是会话级的。跨连接执行时，后续那些
/// 不带 `public.` 前缀的迁移会随机报 "no schema has been selected"。
pub async fn apply_pending(conn: &mut PgConnection, report: impl Fn(&str)) -> Result<Vec<String>> {
    let applied = applied_migrations(conn).await?;
    let mut executed = Vec::new();

    for (name, sql) in MIGRATIONS {
        if applied.iter().any(|a| a == name) {
            report(&format!("  ⏭ {name} (已执行，跳过)"));
            continue;
        }
        report(&format!("  ▶ 执行 {name}..."));

        // SQL 与记账同一事务：中途崩溃只会整个回滚，不会留下"改了但没记"的库。
        let mut tx = conn.begin().await.context("开启迁移事务失败")?;

        sqlx::raw_sql(sql)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("执行迁移失败: {name}"))?;

        // 基线迁移会把 search_path 清空，后面的记账语句不带前缀会找不到表。
        sqlx::query("SET LOCAL search_path TO public")
            .execute(&mut *tx)
            .await
            .with_context(|| format!("恢复迁移 search_path 失败: {name}"))?;

        // 基线迁移重建 public schema 时会把记账表一起删掉。
        sqlx::query(ENSURE_LEDGER)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("重建迁移记录表失败: {name}"))?;

        sqlx::query("INSERT INTO public.privchat_migrations (name) VALUES ($1)")
            .bind(*name)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("记录迁移状态失败: {name}"))?;

        tx.commit()
            .await
            .with_context(|| format!("提交迁移事务失败: {name}"))?;

        // 事务里的 SET LOCAL 随提交失效，后面的迁移要在会话级恢复。
        sqlx::query("SET search_path TO public")
            .execute(&mut *conn)
            .await
            .with_context(|| format!("恢复会话 search_path 失败: {name}"))?;

        report(&format!("  ✅ {name} 完成"));
        executed.push(name.to_string());
    }
    Ok(executed)
}
