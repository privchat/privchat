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
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::time::Duration;

/// 编译期扫描 `migrations/` 生成，跳过 `000_` 开头的文件。
///
/// 🔴 **生产和测试必须读同一份**。测试自己扫一遍目录的话，它跑的迁移集合可以和
/// 生产不同（比如把生产刻意跳过的 `000_drop_all_tables.sql` 也跑了），
/// 于是"测过了"证明的是另一件事。
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// 迁移的全局互斥锁。
///
/// 🔴 **事务只防崩溃，不防两个进程同时跑。** 滚动部署里两个实例可以在同一瞬间读到
/// 同一份"已执行"清单，然后各自执行同一条迁移——事务会让两次都"成功"，因为它们
/// 各自看到的都是合法状态。对 001 尤其致命：它会重建 public schema。
///
/// session 级 advisory lock 在读账本**之前**取得、持有到整个序列结束：后到的那个
/// 进程会等，等到之后重新读账本，看到的是完整的清单，于是一条都不跑。
const MIGRATION_LOCK_KEY: i64 = 0x7076_6368_6174_0001;

/// 等锁的上限。
///
/// 🔴 无限等是不行的。一个活着但卡死的 migrator（连接没断、锁没还）会让后续每一次
/// 部署都静默挂起——部署系统看到的是"还在跑"，而实际上永远不会结束。有界等待把它
/// 变成一次明确的失败，运维能看到"谁占着锁"再去处理。
const LOCK_WAIT: Duration = Duration::from_secs(60);

/// 记账表。基线迁移会重建 public schema、把它一起删掉，所以每次都要确保存在。
///
/// `content_sha256` 是迁移文件内容的摘要：🔴 只按名字记账的话，一条已执行的迁移
/// 后来被人改了，runner 会照样跳过——两台机器上同名的 `031` 可以是完全不同的 SQL，
/// 而没有任何东西会发现。迁移文件一旦执行过就是不可变的，这一列把它变成可检测的。
const ENSURE_LEDGER: &str = "CREATE TABLE IF NOT EXISTS public.privchat_migrations (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    content_sha256 TEXT
)";

/// 老库的记账表没有这一列。
const ADD_LEDGER_DIGEST: &str =
    "ALTER TABLE public.privchat_migrations ADD COLUMN IF NOT EXISTS content_sha256 TEXT";

fn digest_of(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

async fn ensure_ledger(conn: &mut PgConnection) -> Result<()> {
    sqlx::query(ENSURE_LEDGER)
        .execute(&mut *conn)
        .await
        .context("创建迁移记录表失败")?;
    sqlx::query(ADD_LEDGER_DIGEST)
        .execute(&mut *conn)
        .await
        .context("补齐迁移记录表失败")?;
    Ok(())
}

/// 已执行的迁移名。
pub async fn applied_migrations(conn: &mut PgConnection) -> Result<Vec<String>> {
    ensure_ledger(conn).await?;
    sqlx::query_scalar("SELECT name FROM public.privchat_migrations ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .context("查询迁移记录失败")
}

/// 已执行的迁移及其内容摘要（老记录没有摘要时为 `None`）。
pub async fn applied_with_digests(conn: &mut PgConnection) -> Result<Vec<(String, Option<String>)>> {
    ensure_ledger(conn).await?;
    sqlx::query_as("SELECT name, content_sha256 FROM public.privchat_migrations ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .context("查询迁移记录失败")
}

/// 执行尚未执行的迁移，返回这次真正跑了哪些。
///
/// 🔴 **必须传一条独占的连接，不能用连接池。** 基线迁移是 pg_dump 的产物，开头带
/// `set_config('search_path', '', false)`，而那是会话级的。跨连接执行时，后续那些
/// 不带 `public.` 前缀的迁移会随机报 "no schema has been selected"。
///
/// 🔴 **这个 future 被取消时，调用方必须销毁这条连接，不能归还连接池。** 解锁语句
/// 在取消时不会执行，而 session 级 advisory lock 只随连接断开释放——把一条还持着锁的
/// 连接放回池里，锁就一直占着，下一次部署要等到超时才失败。
pub async fn apply_pending(conn: &mut PgConnection, report: impl Fn(&str)) -> Result<Vec<String>> {
    apply_pending_with_hook(conn, report, |_| Ok(())).await
}

/// 与 [`apply_pending`] 相同，但在**每条迁移的 SQL 已执行、记账尚未写入**的位置
/// 调用 `after_sql`。
///
/// 🔴 这个注入点存在是为了让"事务真的会回滚"可被证明。没有它，测试只能在迁移
/// **整体成功之后**去删账本，那模拟的是"外部把账本改坏了"，不是"跑到一半崩了"——
/// 而后者才是事务要解决的问题。生产路径传的是一个永远成功的闭包。
pub async fn apply_pending_with_hook(
    conn: &mut PgConnection,
    report: impl Fn(&str),
    after_sql: impl Fn(&str) -> Result<()>,
) -> Result<Vec<String>> {
    // 🔴 先上锁，再读账本。反过来的话，两个进程可以都读到"还没跑过"再各自去跑。
    //
    // 有界等待：`pg_try_advisory_lock` 轮询到超时。阻塞式的 `pg_advisory_lock` 在
    // 对方卡死时会永远等下去，而部署系统只会看到"还在跑"。
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .context("获取迁移全局锁失败")?;
        if got {
            break;
        }
        if std::time::Instant::now() >= deadline {
            // 报清楚是谁占着，否则运维只知道"拿不到锁"。
            let holder: Option<String> = sqlx::query_scalar(
                "SELECT format('pid=%s application_name=%s state=%s since=%s', \
                        a.pid, a.application_name, a.state, a.backend_start) \
                 FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
                 WHERE l.locktype = 'advisory' AND l.objid = ($1::bigint & 4294967295)::int \
                 LIMIT 1",
            )
            .bind(MIGRATION_LOCK_KEY)
            .fetch_optional(&mut *conn)
            .await
            .ok()
            .flatten();
            anyhow::bail!(
                "等待迁移全局锁超过 {:?}：另一个 migrator 仍持有它{}。\
                 迁移未执行，**不要**带着旧 schema 启动新版本。",
                LOCK_WAIT,
                holder.map(|h| format!("（{h}）")).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let result = apply_pending_locked(conn, report, after_sql).await;

    // 🔴 显式解锁，并且**无论成功失败都要走到这里**。
    //
    // session 级 advisory lock 只在连接断开时自动释放。这条连接通常还会被复用
    // （迁移完继续用、或归还连接池），所以漏还一次就等于把锁一直占着，
    // 而下一次部署会等到超时才失败。
    //
    // 至于 future 被取消：那种情况下这一段也不会执行，所以调用方必须**销毁**
    // 这条连接而不是归还——见函数文档。
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *conn)
        .await;
    result
}

async fn apply_pending_locked(
    conn: &mut PgConnection,
    report: impl Fn(&str),
    after_sql: impl Fn(&str) -> Result<()>,
) -> Result<Vec<String>> {
    let applied = applied_with_digests(conn).await?;
    let mut executed = Vec::new();

    for (name, sql) in MIGRATIONS {
        if let Some((_, recorded)) = applied.iter().find(|(a, _)| a == name) {
            // 🔴 已执行的迁移文件是**不可变**的。改了它，这台机器会跳过、新机器会执行
            // 修改后的版本，于是同名的 `031` 在两台机器上是不同的 schema，而没有任何
            // 东西会发现。摘要对不上就拒绝启动，让人来解释这次改动。
            if let Some(recorded) = recorded {
                let current = digest_of(sql);
                if *recorded != current {
                    anyhow::bail!(
                        "迁移 {name} 的内容与执行时不一致（账本 {recorded}，当前 {current}）。\
                         已执行的迁移文件不可修改：要改行为请新增一条迁移。"
                    );
                }
            }
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

        // 注入点：模拟"SQL 已生效、账还没记"就崩溃。`?` 会带着事务一起回滚。
        after_sql(name)?;

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
        sqlx::query(ADD_LEDGER_DIGEST)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("补齐迁移记录表失败: {name}"))?;

        sqlx::query(
            "INSERT INTO public.privchat_migrations (name, content_sha256) VALUES ($1, $2)",
        )
        .bind(*name)
        .bind(digest_of(sql))
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
