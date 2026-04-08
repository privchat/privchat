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

use sqlx::PgPool;
/// 好友服务 - 处理好友关系管理
///
/// 提供完整的好友系统功能：
/// - 好友请求发送/接受/拒绝
/// - 好友列表管理
/// - 好友关系状态
/// - entity/sync_entities 业务逻辑（好友分页与 payload 构建）
use std::sync::Arc;

use tracing::{info, warn};

use crate::error::{Result, ServerError};
use crate::infra::CacheManager;
use crate::model::friend::*;
use crate::model::privacy::FriendRequestSource;
use crate::repository::UserRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendAcceptState {
    Pending,
    AlreadyFriends,
    Expired,
}

#[derive(Debug, Clone)]
pub enum AcceptFriendRequestResult {
    Accepted(Option<crate::model::privacy::FriendRequestSource>),
    AlreadyFriends,
}

/// 将 FriendRequestSource 转为 (source_type, source_id) 字符串，用于落库与列表返回
fn source_to_strings(source: &Option<FriendRequestSource>) -> (Option<String>, Option<String>) {
    match source.as_ref() {
        Some(FriendRequestSource::Search { search_session_id }) => (
            Some("search".to_string()),
            Some(search_session_id.to_string()),
        ),
        Some(FriendRequestSource::Group { group_id }) => {
            (Some("group".to_string()), Some(group_id.to_string()))
        }
        Some(FriendRequestSource::CardShare { share_id }) => {
            (Some("card_share".to_string()), Some(share_id.to_string()))
        }
        Some(FriendRequestSource::Qrcode { qrcode }) => {
            (Some("qrcode".to_string()), Some(qrcode.clone()))
        }
        Some(FriendRequestSource::Phone { phone }) => {
            (Some("phone".to_string()), Some(phone.clone()))
        }
        None => (None, None),
    }
}

/// 好友服务（基于内存存储）
pub struct FriendService {
    /// 数据库连接池
    pool: Arc<PgPool>,
}

impl FriendService {
    /// 创建新的好友服务
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 发送好友请求
    pub async fn send_friend_request(
        &self,
        from_user_id: u64,
        to_user_id: u64,
        message: Option<String>,
    ) -> Result<u64> {
        self.send_friend_request_with_source(from_user_id, to_user_id, message, None)
            .await
    }

    /// 发送带来源的好友请求
    pub async fn send_friend_request_with_source(
        &self,
        from_user_id: u64,
        to_user_id: u64,
        message: Option<String>,
        source: Option<crate::model::privacy::FriendRequestSource>,
    ) -> Result<u64> {
        info!(
            "📤 发送好友请求: {} -> {} (source: {:?})",
            from_user_id, to_user_id, source
        );

        if from_user_id == to_user_id {
            return Err(ServerError::Validation(
                "Cannot add yourself as friend".to_string(),
            ));
        }

        if self.is_friend(from_user_id, to_user_id).await {
            return Err(ServerError::Duplicate("Already friends".to_string()));
        }

        let (source_str, source_id_str) = source_to_strings(&source);
        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO privchat_friendships (
                user_id, friend_id, status, source, source_id, request_message, created_at, updated_at
            )
            VALUES ($1, $2, 0, $3, $4, $5, $6, $6)
            ON CONFLICT (user_id, friend_id) DO UPDATE SET
                status = CASE
                    WHEN privchat_friendships.status = 1 THEN 1
                    ELSE 0
                END,
                source = EXCLUDED.source,
                source_id = EXCLUDED.source_id,
                request_message = EXCLUDED.request_message,
                updated_at = EXCLUDED.updated_at
            "#
        )
        .bind(from_user_id as i64)
        .bind(to_user_id as i64)
        .bind(source_str)
        .bind(source_id_str)
        .bind(message)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to save friend request: {}", e)))?;

        info!("✅ 好友请求已发送: {} -> {}", from_user_id, to_user_id);
        Ok(now as u64)
    }

    /// 接受好友请求
    pub async fn accept_friend_request(&self, user_id: u64, from_user_id: u64) -> Result<()> {
        self.accept_friend_request_with_source(user_id, from_user_id)
            .await
            .map(|_| ())
    }

    /// 检查接受好友申请的状态
    pub async fn check_accept_state(&self, user_id: u64, from_user_id: u64) -> FriendAcceptState {
        if self.is_friend(user_id, from_user_id).await {
            return FriendAcceptState::AlreadyFriends;
        }

        let has_pending = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT 1
            FROM privchat_friendships
            WHERE user_id = $1
              AND friend_id = $2
              AND status = 0
            LIMIT 1
            "#,
        )
        .bind(from_user_id as i64)
        .bind(user_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map(|v| v.is_some())
        .unwrap_or(false);

        if has_pending {
            FriendAcceptState::Pending
        } else {
            FriendAcceptState::Expired
        }
    }

    /// 接受好友请求并返回来源信息
    pub async fn accept_friend_request_with_source(
        &self,
        user_id: u64,
        from_user_id: u64,
    ) -> Result<AcceptFriendRequestResult> {
        info!("✅ 用户 {} 接受来自 {} 的好友请求", user_id, from_user_id);

        if self.is_friend(user_id, from_user_id).await {
            info!("ℹ️ 用户 {} 与 {} 已是好友", user_id, from_user_id);
            return Ok(AcceptFriendRequestResult::AlreadyFriends);
        }

        let mut tx =
            self.pool.begin().await.map_err(|e| {
                ServerError::Database(format!("Failed to begin transaction: {}", e))
            })?;

        let pending_row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            r#"
            SELECT source, source_id
            FROM privchat_friendships
            WHERE user_id = $1
              AND friend_id = $2
              AND status = 0
            FOR UPDATE
            "#,
        )
        .bind(from_user_id as i64)
        .bind(user_id as i64)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to query pending request: {}", e)))?;

        if let Some((source_str, source_id_str)) = pending_row {
            let now = chrono::Utc::now().timestamp_millis();

            sqlx::query(
                r#"
                UPDATE privchat_friendships
                SET status = 1, updated_at = $3
                WHERE user_id = $1
                  AND friend_id = $2
                  AND status = 0
                "#,
            )
            .bind(from_user_id as i64)
            .bind(user_id as i64)
            .bind(now)
            .execute(tx.as_mut())
            .await
            .map_err(|e| {
                ServerError::Database(format!("Failed to update request status: {}", e))
            })?;

            sqlx::query(
                r#"
                INSERT INTO privchat_friendships (
                    user_id, friend_id, status, source, source_id, request_message, created_at, updated_at
                )
                VALUES ($1, $2, 1, $3, $4, NULL, $5, $5)
                ON CONFLICT (user_id, friend_id) DO UPDATE SET
                    status = 1,
                    source = EXCLUDED.source,
                    source_id = EXCLUDED.source_id,
                    updated_at = EXCLUDED.updated_at
                "#
            )
            .bind(user_id as i64)
            .bind(from_user_id as i64)
            .bind(source_str.clone())
            .bind(source_id_str.clone())
            .bind(now)
            .execute(tx.as_mut())
            .await
            .map_err(|e| ServerError::Database(format!("Failed to upsert reciprocal friendship: {}", e)))?;

            tx.commit().await.map_err(|e| {
                ServerError::Database(format!("Failed to commit transaction: {}", e))
            })?;

            let source = source_from_strings(source_str, source_id_str);
            info!("✅ 好友关系已建立: {} <-> {}", user_id, from_user_id);
            Ok(AcceptFriendRequestResult::Accepted(source))
        } else {
            // 兜底二次检查，避免并发下把“已是好友”误判为过期
            if self.is_friend(user_id, from_user_id).await {
                info!(
                    "ℹ️ 用户 {} 与 {} 已是好友（并发兜底）",
                    user_id, from_user_id
                );
                Ok(AcceptFriendRequestResult::AlreadyFriends)
            } else {
                warn!(
                    "⚠️ 未找到待处理的好友请求（已过期或已处理）: {} -> {}",
                    from_user_id, user_id
                );
                Err(ServerError::NotFound("Friend request expired".to_string()))
            }
        }
    }

    /// 获取好友列表
    pub async fn get_friends(&self, user_id: u64) -> Result<Vec<u64>> {
        info!("📋 获取用户 {} 的好友列表", user_id);

        let rows = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT friend_id
            FROM privchat_friendships
            WHERE user_id = $1
              AND status = 1
            ORDER BY updated_at DESC
            "#,
        )
        .bind(user_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to query friends: {}", e)))?;

        Ok(rows.into_iter().map(|(id,)| id as u64).collect())
    }

    /// 删除好友
    pub async fn remove_friend(&self, user_id: u64, friend_id: u64) -> Result<()> {
        info!("🗑️ 用户 {} 删除好友 {}", user_id, friend_id);

        sqlx::query(
            r#"
            UPDATE privchat_friendships
            SET status = 2,
                updated_at = $3
            WHERE (user_id = $1 AND friend_id = $2)
               OR (user_id = $2 AND friend_id = $1)
            "#,
        )
        .bind(user_id as i64)
        .bind(friend_id as i64)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to remove friendship: {}", e)))?;

        info!("✅ 好友关系已删除: {} <-> {}", user_id, friend_id);
        Ok(())
    }

    /// 检查是否是好友
    pub async fn is_friend(&self, user_id: u64, friend_id: u64) -> bool {
        self.try_is_friend(user_id, friend_id)
            .await
            .unwrap_or(false)
    }

    /// 检查是否是好友（保留数据库错误）
    pub async fn try_is_friend(&self, user_id: u64, friend_id: u64) -> Result<bool> {
        sqlx::query_scalar::<_, i32>(
            r#"
            SELECT 1
            FROM privchat_friendships
            WHERE status = 1
              AND (
                  (user_id = $1 AND friend_id = $2)
               OR (user_id = $2 AND friend_id = $1)
              )
            LIMIT 1
            "#,
        )
        .bind(user_id as i64)
        .bind(friend_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map(|v| v.is_some())
        .map_err(|e| ServerError::Database(format!("Failed to check friendship: {}", e)))
    }

    /// 获取与某用户的好友关系（用于列表返回 source_type/source_id）
    pub async fn get_friendship(&self, user_id: u64, friend_id: u64) -> Option<Friendship> {
        let row = sqlx::query_as::<_, (i64, i64, i16, Option<String>, Option<String>, i64, i64)>(
            r#"
            SELECT user_id, friend_id, status, source, source_id, created_at, updated_at
            FROM privchat_friendships
            WHERE user_id = $1
              AND friend_id = $2
            LIMIT 1
            "#,
        )
        .bind(user_id as i64)
        .bind(friend_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .ok()
        .flatten()?;

        Some(Friendship::from_db_row(
            row.0, row.1, row.2, row.3, row.4, row.5, row.6,
        ))
    }

    /// 获取待处理的好友申请列表（接收到的）
    pub async fn get_pending_requests(&self, user_id: u64) -> Result<Vec<FriendRequest>> {
        info!("📋 获取用户 {} 的待处理好友申请列表", user_id);

        let rows = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
                i64,
                i64,
            ),
        >(
            r#"
            SELECT user_id, friend_id, request_message, source, source_id, created_at, updated_at
            FROM privchat_friendships
            WHERE friend_id = $1
              AND status = 0
            ORDER BY updated_at DESC
            "#,
        )
        .bind(user_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to query pending requests: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(
                |(from_uid, to_uid, msg, source, source_id, created_at, updated_at)| {
                    FriendRequest {
                        id: build_request_id(from_uid as u64, to_uid as u64, created_at),
                        from_user_id: from_uid as u64,
                        to_user_id: to_uid as u64,
                        message: msg,
                        status: FriendshipStatus::Pending,
                        source: source_from_strings(source, source_id),
                        created_at: chrono::DateTime::from_timestamp_millis(created_at)
                            .unwrap_or_else(chrono::Utc::now),
                        updated_at: chrono::DateTime::from_timestamp_millis(updated_at)
                            .unwrap_or_else(chrono::Utc::now),
                    }
                },
            )
            .collect())
    }

    /// 获取发送的好友申请列表（已发送但未处理）
    pub async fn get_sent_requests(&self, user_id: u64) -> Result<Vec<FriendRequest>> {
        info!("📋 获取用户 {} 已发送的好友申请列表", user_id);

        let rows = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
                i64,
                i64,
            ),
        >(
            r#"
            SELECT user_id, friend_id, request_message, source, source_id, created_at, updated_at
            FROM privchat_friendships
            WHERE user_id = $1
              AND status = 0
            ORDER BY updated_at DESC
            "#,
        )
        .bind(user_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to query sent requests: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(
                |(from_uid, to_uid, msg, source, source_id, created_at, updated_at)| {
                    FriendRequest {
                        id: build_request_id(from_uid as u64, to_uid as u64, created_at),
                        from_user_id: from_uid as u64,
                        to_user_id: to_uid as u64,
                        message: msg,
                        status: FriendshipStatus::Pending,
                        source: source_from_strings(source, source_id),
                        created_at: chrono::DateTime::from_timestamp_millis(created_at)
                            .unwrap_or_else(chrono::Utc::now),
                        updated_at: chrono::DateTime::from_timestamp_millis(updated_at)
                            .unwrap_or_else(chrono::Utc::now),
                    }
                },
            )
            .collect())
    }

    /// entity/sync_entities 业务逻辑：好友分页与 SyncEntitiesResponse 构建
    pub async fn sync_entities_page(
        &self,
        user_id: u64,
        since_version: Option<u64>,
        _scope: Option<&str>,
        limit: u32,
        user_repository: &Arc<UserRepository>,
        cache_manager: &Arc<CacheManager>,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use crate::rpc::helpers::get_user_profile_with_fallback;
        use privchat_protocol::rpc::sync::{
            FriendSyncFriendPayload, FriendSyncPayload, FriendSyncUserPayload,
            SyncEntitiesResponse, SyncEntityItem,
        };

        let limit = limit.min(200).max(1);
        let since_v = since_version.unwrap_or(0);

        #[derive(sqlx::FromRow)]
        struct FriendSyncRow {
            friend_id: i64,
            status: i16,
            created_at: i64,
            updated_at: i64,
            sync_version: i64,
        }

        let rows = sqlx::query_as::<_, FriendSyncRow>(
            r#"
            SELECT friend_id, status, created_at, updated_at, sync_version
            FROM privchat_friendships
            WHERE user_id = $1
              AND status != 0
              AND sync_version > $2
            ORDER BY sync_version ASC, friend_id ASC
            LIMIT $3
            "#,
        )
        .bind(user_id as i64)
        .bind(since_v as i64)
        .bind(limit as i64 + 1)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("Failed to query friend sync page: {}", e)))?;

        let has_more = rows.len() > limit as usize;
        let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
        let next_version = page
            .last()
            .map(|row| row.sync_version as u64)
            .unwrap_or(since_v);

        let mut items = Vec::with_capacity(page.len());
        for row in &page {
            let friend_id = row.friend_id as u64;
            if row.status != 1 {
                items.push(SyncEntityItem {
                    entity_id: friend_id.to_string(),
                    version: row.sync_version as u64,
                    deleted: true,
                    payload: None,
                });
                continue;
            }
            let profile_opt =
                get_user_profile_with_fallback(friend_id, user_repository, cache_manager)
                    .await
                    .ok()
                    .flatten();
            let profile = match profile_opt {
                Some(p) => p,
                None => continue,
            };

            let payload_typed = FriendSyncPayload {
                user_id: Some(friend_id),
                uid: Some(friend_id),
                tags: None,
                is_pinned: Some(false),
                pinned: Some(false),
                created_at: Some(row.created_at),
                updated_at: Some(row.updated_at),
                version: Some(row.sync_version),
                friend: Some(FriendSyncFriendPayload {
                    created_at: Some(row.created_at),
                    updated_at: Some(row.updated_at),
                    version: Some(row.sync_version),
                }),
                user: Some(FriendSyncUserPayload {
                    username: Some(profile.username.clone()),
                    nickname: Some(profile.nickname.clone()),
                    name: Some(profile.nickname.clone()),
                    alias: None,
                    avatar: Some(profile.avatar_url.as_deref().unwrap_or("").to_string()),
                    user_type: Some(i32::from(profile.user_type)),
                    type_field: Some(i32::from(profile.user_type)),
                    updated_at: None,
                    version: None,
                }),
            };
            let payload = serde_json::to_value(payload_typed).map_err(|e| {
                ServerError::Internal(format!("friend payload serialize failed: {e}"))
            })?;

            items.push(SyncEntityItem {
                entity_id: friend_id.to_string(),
                version: row.sync_version as u64,
                deleted: false,
                payload: Some(payload),
            });
        }

        Ok(SyncEntitiesResponse {
            items,
            next_version,
            has_more,
            min_version: None,
        })
    }
}

fn build_request_id(from_user_id: u64, to_user_id: u64, created_at: i64) -> u64 {
    let ts = created_at.max(0) as u64;
    (from_user_id.wrapping_shl(32)) ^ to_user_id ^ ts
}

fn source_from_strings(
    source: Option<String>,
    source_id: Option<String>,
) -> Option<crate::model::privacy::FriendRequestSource> {
    match (source.as_deref(), source_id) {
        (Some("search"), Some(id)) => id.parse::<u64>().ok().map(|search_session_id| {
            crate::model::privacy::FriendRequestSource::Search { search_session_id }
        }),
        (Some("group"), Some(id)) => id
            .parse::<u64>()
            .ok()
            .map(|group_id| crate::model::privacy::FriendRequestSource::Group { group_id }),
        (Some("card_share"), Some(id)) => id
            .parse::<u64>()
            .ok()
            .map(|share_id| crate::model::privacy::FriendRequestSource::CardShare { share_id }),
        (Some("qrcode"), Some(qrcode)) => {
            Some(crate::model::privacy::FriendRequestSource::Qrcode { qrcode })
        }
        (Some("phone"), Some(phone)) => {
            Some(crate::model::privacy::FriendRequestSource::Phone { phone })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;
    use crate::infra::CacheManager;
    use crate::repository::UserRepository;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    struct FriendServiceTestContext {
        service: FriendService,
        pool: Arc<PgPool>,
        user_repository: Arc<UserRepository>,
        cache_manager: Arc<CacheManager>,
    }

    async fn open_test_context() -> Option<FriendServiceTestContext> {
        let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()?;
        let pool = Arc::new(
            PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .ok()?,
        );
        let cache_manager = Arc::new(CacheManager::new(CacheConfig::default()).await.ok()?);
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let service = FriendService::new(pool.clone());
        Some(FriendServiceTestContext {
            service,
            pool,
            user_repository,
            cache_manager,
        })
    }

    async fn ensure_user(pool: &PgPool, user_id: u64, username: &str) {
        sqlx::query(
            r#"
            INSERT INTO privchat_users (user_id, username, display_name)
            VALUES ($1, $2, $2)
            ON CONFLICT (user_id) DO UPDATE
            SET username = EXCLUDED.username,
                display_name = EXCLUDED.display_name
            "#,
        )
        .bind(user_id as i64)
        .bind(username)
        .execute(pool)
        .await
        .expect("ensure user");
    }

    async fn cleanup_friendship(pool: &PgPool, user_a: u64, user_b: u64) {
        let _ = sqlx::query(
            r#"
            DELETE FROM privchat_friendships
            WHERE (user_id = $1 AND friend_id = $2)
               OR (user_id = $2 AND friend_id = $1)
            "#,
        )
        .bind(user_a as i64)
        .bind(user_b as i64)
        .execute(pool)
        .await;
    }

    async fn cleanup_user(pool: &PgPool, user_id: u64) {
        let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
            .bind(user_id as i64)
            .execute(pool)
            .await;
    }

    async fn ensure_friendship(pool: &PgPool, user_a: u64, user_b: u64, now_ms: i64) {
        sqlx::query(
            r#"
            INSERT INTO privchat_friendships (
                user_id, friend_id, status, created_at, updated_at
            )
            VALUES
                ($1, $2, 1, $3, $3),
                ($2, $1, 1, $3, $3)
            ON CONFLICT (user_id, friend_id) DO UPDATE
            SET status = EXCLUDED.status,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user_a as i64)
        .bind(user_b as i64)
        .bind(now_ms)
        .execute(pool)
        .await
        .expect("ensure friendship");
    }

    #[tokio::test]
    async fn remove_friend_emits_deleted_tombstones_in_friend_sync() {
        let Some(ctx) = open_test_context().await else {
            eprintln!("skip remove_friend_emits_deleted_tombstones_in_friend_sync: DATABASE_URL not configured");
            return;
        };

        let alice_id = 9_810_001_u64;
        let bob_id = 9_810_002_u64;
        let now_ms = chrono::Utc::now().timestamp_millis();

        cleanup_friendship(&ctx.pool, alice_id, bob_id).await;
        cleanup_user(&ctx.pool, alice_id).await;
        cleanup_user(&ctx.pool, bob_id).await;

        ensure_user(&ctx.pool, alice_id, "friend_tombstone_alice").await;
        ensure_user(&ctx.pool, bob_id, "friend_tombstone_bob").await;
        ensure_friendship(&ctx.pool, alice_id, bob_id, now_ms).await;

        let initial = ctx
            .service
            .sync_entities_page(
                alice_id,
                None,
                None,
                50,
                &ctx.user_repository,
                &ctx.cache_manager,
            )
            .await
            .expect("initial friend sync");
        let initial_item = initial
            .items
            .iter()
            .find(|item| item.entity_id == bob_id.to_string())
            .expect("initial friend sync item");
        assert!(
            !initial_item.deleted,
            "initial friend sync should expose active friend entry"
        );
        let since_version = initial.next_version;

        ctx.service
            .remove_friend(alice_id, bob_id)
            .await
            .expect("remove friend");

        let alice_delta = ctx
            .service
            .sync_entities_page(
                alice_id,
                Some(since_version),
                None,
                50,
                &ctx.user_repository,
                &ctx.cache_manager,
            )
            .await
            .expect("alice friend tombstone sync");
        let alice_tombstone = alice_delta
            .items
            .iter()
            .find(|item| item.entity_id == bob_id.to_string())
            .expect("alice tombstone item");
        assert!(alice_tombstone.deleted);
        assert!(
            alice_tombstone.version > since_version,
            "friend tombstone should advance sync_version"
        );
        assert!(alice_tombstone.payload.is_none());

        let bob_delta = ctx
            .service
            .sync_entities_page(
                bob_id,
                None,
                None,
                50,
                &ctx.user_repository,
                &ctx.cache_manager,
            )
            .await
            .expect("bob friend tombstone sync");
        let bob_tombstone = bob_delta
            .items
            .iter()
            .find(|item| item.entity_id == alice_id.to_string())
            .expect("bob tombstone item");
        assert!(bob_tombstone.deleted);
        assert!(bob_tombstone.payload.is_none());

        cleanup_friendship(&ctx.pool, alice_id, bob_id).await;
        cleanup_user(&ctx.pool, alice_id).await;
        cleanup_user(&ctx.pool, bob_id).await;
    }
}
