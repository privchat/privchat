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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::error::Result;
use crate::infra::CacheManager;

/// 黑名单条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlacklistEntry {
    /// 拉黑者 ID
    pub user_id: u64,
    /// 被拉黑用户 ID
    pub blocked_user_id: u64,
    /// 拉黑时间
    pub blocked_at: DateTime<Utc>,
    /// 拉黑原因（可选）
    pub reason: Option<String>,
}

/// 黑名单服务。
///
/// 🔴 **DB 是真源**（`privchat_blacklist`）。这里曾经只有一个进程内 `HashMap`：
/// 表建了但从没被读写，于是拉黑关系在服务重启后立即消失，多实例下各说各话。
/// 拉黑是「对方不能再发消息给我」这类用户明确预期长期生效的设置，
/// 掉一次就是「我明明拉黑了他，他还能发」。
pub struct BlacklistService {
    pool: Arc<sqlx::PgPool>,
    cache_manager: Arc<CacheManager>,
}

impl BlacklistService {
    /// 创建新的黑名单服务
    pub fn new(pool: Arc<sqlx::PgPool>, cache_manager: Arc<CacheManager>) -> Self {
        Self {
            pool,
            cache_manager,
        }
    }

    /// 添加用户到黑名单
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `blocked_user_id` - 被拉黑用户 ID
    /// * `reason` - 拉黑原因（可选）
    ///
    /// # Returns
    /// 黑名单条目
    pub async fn add_to_blacklist(
        &self,
        user_id: u64,
        blocked_user_id: u64,
        reason: Option<String>,
    ) -> Result<BlacklistEntry> {
        // 检查是否是自己
        if user_id == blocked_user_id {
            return Err(crate::error::ServerError::Validation(
                "不能拉黑自己".to_string(),
            ));
        }

        sqlx::query(
            r#"
            INSERT INTO privchat_blacklist (user_id, blocked_user_id, reason, created_at)
            VALUES ($1, $2, $3, now_millis())
            ON CONFLICT (user_id, blocked_user_id) DO UPDATE SET reason = EXCLUDED.reason
            "#,
        )
        .bind(user_id as i64)
        .bind(blocked_user_id as i64)
        .bind(reason.as_deref())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("写入黑名单失败: {e}")))?;

        info!("✅ 用户 {} 已将 {} 加入黑名单", user_id, blocked_user_id);
        Ok(BlacklistEntry {
            user_id,
            blocked_user_id,
            blocked_at: Utc::now(),
            reason,
        })
    }

    /// 从黑名单移除用户
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `blocked_user_id` - 被拉黑用户 ID
    ///
    /// # Returns
    /// 是否成功移除
    pub async fn remove_from_blacklist(&self, user_id: u64, blocked_user_id: u64) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM privchat_blacklist WHERE user_id = $1 AND blocked_user_id = $2",
        )
        .bind(user_id as i64)
        .bind(blocked_user_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("移除黑名单失败: {e}")))?;
        Ok(result.rows_affected() > 0)
    }

    /// 检查用户是否在黑名单中
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `target_user_id` - 要检查的用户 ID
    ///
    /// # Returns
    /// 是否在黑名单中
    pub async fn is_blocked(&self, user_id: u64, target_user_id: u64) -> Result<bool> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM privchat_blacklist \
             WHERE user_id = $1 AND blocked_user_id = $2)",
        )
        .bind(user_id as i64)
        .bind(target_user_id as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("查询黑名单失败: {e}")))?;
        Ok(exists)
    }

    /// 获取用户的黑名单列表
    ///
    /// # Arguments
    /// * `user_id` - 用户 ID
    ///
    /// # Returns
    /// 黑名单条目列表
    pub async fn get_blacklist(&self, user_id: u64) -> Result<Vec<BlacklistEntry>> {
        let rows: Vec<(i64, i64, Option<String>, i64)> = sqlx::query_as(
            "SELECT user_id, blocked_user_id, reason, created_at FROM privchat_blacklist \
             WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("查询黑名单列表失败: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|(user_id, blocked_user_id, reason, created_at)| BlacklistEntry {
                user_id: user_id as u64,
                blocked_user_id: blocked_user_id as u64,
                blocked_at: chrono::DateTime::from_timestamp_millis(created_at)
                    .unwrap_or_else(Utc::now),
                reason,
            })
            .collect())
    }

    pub async fn get_blacklist_entry(
        &self,
        user_id: u64,
        blocked_user_id: u64,
    ) -> Result<Option<BlacklistEntry>> {
        let row: Option<(Option<String>, i64)> = sqlx::query_as(
            "SELECT reason, created_at FROM privchat_blacklist \
             WHERE user_id = $1 AND blocked_user_id = $2",
        )
        .bind(user_id as i64)
        .bind(blocked_user_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("查询黑名单条目失败: {e}")))?;

        Ok(row.map(|(reason, created_at)| BlacklistEntry {
            user_id,
            blocked_user_id,
            blocked_at: chrono::DateTime::from_timestamp_millis(created_at)
                .unwrap_or_else(Utc::now),
            reason,
        }))
    }

    /// 检查两个用户之间是否存在任意方向的拉黑关系
    ///
    /// # Arguments
    /// * `user_a` - 用户A ID
    /// * `user_b` - 用户B ID
    ///
    /// # Returns
    /// (A是否拉黑B, B是否拉黑A)
    pub async fn check_mutual_block(&self, user_a: u64, user_b: u64) -> Result<(bool, bool)> {
        // 🔴 一条 SQL 取两个方向，不做两次 `is_blocked`。
        //
        // 两个理由：这是每条私聊消息都要过的热路径，两次串行往返白白加一倍延迟；
        // 而且两次查询来自**两个快照**——中间有人取消拉黑时，会读出一个
        // 从未真实存在过的组合状态。
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT user_id, blocked_user_id FROM privchat_blacklist \
             WHERE (user_id = $1 AND blocked_user_id = $2) \
                OR (user_id = $2 AND blocked_user_id = $1)",
        )
        .bind(user_a as i64)
        .bind(user_b as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| crate::error::ServerError::Database(format!("查询双向拉黑失败: {e}")))?;

        let a_blocks_b = rows.iter().any(|(u, t)| *u == user_a as i64 && *t == user_b as i64);
        let b_blocks_a = rows.iter().any(|(u, t)| *u == user_b as i64 && *t == user_a as i64);
        Ok((a_blocks_b, b_blocks_a))
    }

    /// 获取黑名单统计信息
    ///
    /// # Arguments
    /// * `user_id` - 用户 ID
    ///
    /// # Returns
    /// 黑名单数量
    pub async fn get_blacklist_count(&self, user_id: u64) -> Result<usize> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM privchat_blacklist WHERE user_id = $1")
                .bind(user_id as i64)
                .fetch_one(self.pool.as_ref())
                .await
                .map_err(|e| {
                    crate::error::ServerError::Database(format!("统计黑名单失败: {e}"))
                })?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;
    use sqlx::postgres::PgPoolOptions;

    /// 这些用例原本跑在进程内 `HashMap` 上，因此「拉黑生效」只证明了当前进程。
    /// 黑名单改成以 DB 为真源之后，它们必须打真库——否则测的还是一个
    /// 重启就消失的东西。
    async fn service() -> Option<BlacklistService> {
        let url = crate::require_test_database_url()?;
        let pool = Arc::new(
            PgPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
        );
        let cache = Arc::new(CacheManager::new(CacheConfig::default()).await.ok()?);
        Some(BlacklistService::new(pool, cache))
    }

    const A: u64 = 9_970_001;
    const B: u64 = 9_970_002;

    /// 用例共享同一对 uid，必须串行——并发时一条的 reset 会把另一条刚写的关系删掉，
    /// 于是「重启后仍然生效」那条会以完全误导的方式失败。
    fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// 落库之后多了一条真实约束：黑名单行有指向用户表的外键。
    /// 内存实现时不存在这个约束——这正是「持久化」与「进程内 map」的区别之一。
    async fn ensure_users(service: &BlacklistService) {
        for uid in [A, B] {
            sqlx::query(
                r#"
                INSERT INTO privchat_users (user_id, username, display_name, qr_key)
                VALUES ($1, $2, $2, $3)
                ON CONFLICT (user_id) DO NOTHING
                "#,
            )
            .bind(uid as i64)
            .bind(format!("bl_{uid}"))
            .bind(crate::rpc::qr::generate_qr_key())
            .execute(service.pool.as_ref())
            .await
            .expect("ensure user");
        }
    }

    async fn reset(service: &BlacklistService) {
        ensure_users(service).await;
        for (u, t) in [(A, B), (B, A)] {
            let _ = service.remove_from_blacklist(u, t).await;
        }
    }

    #[tokio::test]
    async fn blacklist_survives_in_the_database() {
        let _guard = fixture_lock().lock().await;
        let Some(service) = service().await else {
            return;
        };
        reset(&service).await;

        let entry = service
            .add_to_blacklist(A, B, Some("骚扰".to_string()))
            .await
            .expect("add");
        assert_eq!(entry.blocked_user_id, B);
        assert!(service.is_blocked(A, B).await.expect("check"));
        assert!(!service.is_blocked(B, A).await.expect("reverse"));
        assert_eq!(service.get_blacklist(A).await.expect("list").len(), 1);

        // 换一个**全新的 service 实例**（模拟重启/另一个进程）：关系必须还在。
        let fresh = service_from_same_url().await.expect("fresh service");
        assert!(
            fresh.is_blocked(A, B).await.expect("check after restart"),
            "拉黑必须落库；只存内存的话重启后对方又能发消息了",
        );

        assert!(service.remove_from_blacklist(A, B).await.expect("remove"));
        assert!(!service.is_blocked(A, B).await.expect("check removed"));
        reset(&service).await;
    }

    async fn service_from_same_url() -> Option<BlacklistService> {
        service().await
    }

    #[tokio::test]
    async fn a_user_cannot_block_themselves() {
        let _guard = fixture_lock().lock().await;
        let Some(service) = service().await else {
            return;
        };
        assert!(service.add_to_blacklist(A, A, None).await.is_err());
    }

    #[tokio::test]
    async fn mutual_block_reports_both_directions() {
        let _guard = fixture_lock().lock().await;
        let Some(service) = service().await else {
            return;
        };
        reset(&service).await;
        service.add_to_blacklist(A, B, None).await.expect("a blocks b");
        service.add_to_blacklist(B, A, None).await.expect("b blocks a");
        let (a_blocks_b, b_blocks_a) = service.check_mutual_block(A, B).await.expect("mutual");
        assert!(a_blocks_b && b_blocks_a);
        reset(&service).await;
    }
}
