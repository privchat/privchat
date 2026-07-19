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
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::error::{Result, ServerError};
use crate::model::channel::{
    Channel, ChannelListResponse, ChannelMember, ChannelResponse, ChannelStatus, ChannelType,
    CreateChannelRequest, MemberRole,
};
use crate::repository::{ChannelRepository, PgChannelRepository};
use crate::rpc::qr::{generate_qr_key, is_qr_key_unique_violation};

/// 最后消息预览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastMessagePreview {
    /// 消息 ID
    pub message_id: u64,
    /// 发送者 ID
    pub sender_id: u64,
    /// 消息内容预览（截断到 50 字）
    pub content: String,
    /// 消息类型（用于显示占位符如 "[图片]"）
    pub message_type: String,
    /// 发送时间
    pub timestamp: DateTime<Utc>,
}

/// 增强的会话项（包含预览、未读、置顶等信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedChannelItem {
    /// 会话信息
    pub channel: Channel,
    /// 最后消息预览
    pub last_message: Option<LastMessagePreview>,
    /// 未读消息数
    pub unread_count: u64,
    /// 是否置顶
    pub is_pinned: bool,
    /// 是否免打扰
    pub is_muted: bool,
}

/// 增强的会话列表响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedChannelListResponse {
    /// 会话列表
    pub channels: Vec<EnhancedChannelItem>,
    /// 总数
    pub total: usize,
    /// 是否还有更多
    pub has_more: bool,
}

/// 会话服务配置
#[derive(Debug, Clone)]
pub struct ChannelServiceConfig {
    /// 单个用户最大会话数
    pub max_channels_per_user: usize,
    /// 群聊最大成员数
    pub max_group_members: usize,
    /// 是否允许创建公开群聊
    pub allow_public_groups: bool,
    /// 会话ID生成前缀
    pub channel_id_prefix: String,
}

impl Default for ChannelServiceConfig {
    fn default() -> Self {
        Self {
            max_channels_per_user: 1000,
            max_group_members: 500,
            allow_public_groups: true,
            channel_id_prefix: "conv_".to_string(),
        }
    }
}

/// 会话服务统计信息
#[derive(Debug, Clone)]
pub struct ChannelServiceStats {
    /// 总会话数
    pub total_channels: usize,
    /// 活跃会话数
    pub active_channels: usize,
    /// 私聊会话数
    pub direct_channels: usize,
    /// 群聊会话数
    pub group_channels: usize,
    /// 总成员数
    pub total_members: usize,
    /// 平均每个会话的成员数
    pub avg_members_per_channel: f64,
}

/// 频道服务（原会话服务）
pub struct ChannelService {
    /// 配置
    config: ChannelServiceConfig,
    /// 会话仓库（PostgreSQL）
    channel_repository: Arc<PgChannelRepository>,
    /// 会话存储 channel_id -> Channel（内存缓存）
    channels: Arc<RwLock<HashMap<u64, Channel>>>,
    /// 用户会话索引 user_id -> Vec<channel_id>
    user_channels: Arc<RwLock<HashMap<u64, Vec<u64>>>>,
    /// 私聊会话索引 (user1_id, user2_id) -> channel_id
    direct_channel_index: Arc<RwLock<HashMap<(u64, u64), u64>>>,
    /// 置顶会话 (user_id, channel_id) -> is_pinned
    pinned_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 免打扰会话 (user_id, channel_id) -> is_muted
    muted_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 隐藏会话 (user_id, channel_id) -> is_hidden
    hidden_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 最后消息预览缓存 channel_id -> LastMessagePreview
    last_message_cache: Arc<RwLock<HashMap<u64, LastMessagePreview>>>,
}

impl ChannelService {
    /// 创建新的会话服务
    pub fn new(config: ChannelServiceConfig, channel_repository: Arc<PgChannelRepository>) -> Self {
        Self {
            config,
            channel_repository,
            channels: Arc::new(RwLock::new(HashMap::new())),
            user_channels: Arc::new(RwLock::new(HashMap::new())),
            direct_channel_index: Arc::new(RwLock::new(HashMap::new())),
            pinned_channels: Arc::new(RwLock::new(HashSet::new())),
            muted_channels: Arc::new(RwLock::new(HashSet::new())),
            hidden_channels: Arc::new(RwLock::new(HashSet::new())),
            last_message_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 使用默认配置创建会话服务（需要 Repository）
    pub fn new_with_repository(channel_repository: Arc<PgChannelRepository>) -> Self {
        Self::new(ChannelServiceConfig::default(), channel_repository)
    }

    /// 使用默认配置创建会话服务（向后兼容，但会警告）
    #[deprecated(note = "使用 new_with_repository 替代，以支持数据库持久化")]
    pub fn new_default() -> Self {
        // 创建一个临时的 Repository（这不应该在生产环境使用）
        // 注意：这会导致编译错误，因为需要 PgPool
        // 保留此方法仅用于向后兼容，实际应该使用 new_with_repository
        panic!("new_default() 已废弃，请使用 new_with_repository()")
    }

    /// 获取数据库连接池（用于事务操作）
    pub fn pool(&self) -> &sqlx::PgPool {
        self.channel_repository.pool()
    }

    pub(crate) fn channel_repository(&self) -> Arc<PgChannelRepository> {
        self.channel_repository.clone()
    }

    /// admin 场景下确保两个用户之间的私聊频道存在（不存在则新建），返回 `channel_id`。
    ///
    /// 业务封装：admin handler 不再直接持有 `PgChannelRepository`，由 ChannelService
    /// 集中保管 source/source_id 等 admin-context 信息。
    ///
    /// - `sender_id == target_user_id` → 拒绝（拒接系统账号给自身写消息这类场景）
    /// - 频道已存在 → 复用，不重复创建
    /// - 已实现一并触发 `update_user_channel_index` 等 ChannelService 的副作用——
    ///   完全由 service 决定，handler 看不到 repository。
    pub async fn ensure_direct_channel_for_admin(
        &self,
        sender_id: u64,
        target_user_id: u64,
    ) -> Result<u64> {
        if sender_id == target_user_id {
            return Err(ServerError::Validation(
                "不能给自己创建/取得私聊频道".to_string(),
            ));
        }
        let (channel, _created) = self
            .channel_repository
            .create_or_get_direct_channel(sender_id, target_user_id, Some("admin"), None)
            .await
            .map_err(|e| {
                ServerError::Database(format!(
                    "ensure direct channel {} ⇄ {} 失败: {}",
                    sender_id, target_user_id, e
                ))
            })?;
        Ok(channel.id)
    }

    fn sync_channel_type(channel_type: ChannelType) -> i32 {
        match channel_type {
            ChannelType::Direct => 1,
            ChannelType::Group => 2,
            ChannelType::Room => 3,
        }
    }

    async fn persist_user_channel_flags(
        &self,
        user_id: u64,
        channel_id: u64,
        pinned: Option<bool>,
        muted: Option<bool>,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO privchat_user_channels (
                user_id,
                channel_id,
                is_pinned,
                is_muted,
                updated_at
            )
            VALUES (
                $1,
                $2,
                COALESCE($3, false),
                COALESCE($4, false),
                $5
            )
            ON CONFLICT (user_id, channel_id) DO UPDATE SET
                is_pinned = COALESCE($3, privchat_user_channels.is_pinned),
                is_muted = COALESCE($4, privchat_user_channels.is_muted),
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user_id as i64)
        .bind(channel_id as i64)
        .bind(pinned)
        .bind(muted)
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("persist user_channel flags failed: {}", e)))?;

        Ok(())
    }

    async fn ensure_user_channel_rows(&self, channel_id: u64, user_ids: &[u64]) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();

        for user_id in user_ids {
            sqlx::query(
                r#"
                INSERT INTO privchat_user_channels (
                    user_id,
                    channel_id,
                    unread_count,
                    is_pinned,
                    is_muted,
                    updated_at
                )
                VALUES ($1, $2, 0, false, false, $3)
                ON CONFLICT (user_id, channel_id) DO NOTHING
                "#,
            )
            .bind(*user_id as i64)
            .bind(channel_id as i64)
            .bind(now)
            .execute(self.pool())
            .await
            .map_err(|e| {
                ServerError::Database(format!(
                    "ensure privchat_user_channels failed for user={} channel={}: {}",
                    user_id, channel_id, e
                ))
            })?;
        }

        Ok(())
    }

    pub async fn increment_user_channel_unread(
        &self,
        channel_id: u64,
        user_ids: &[u64],
        count: i32,
    ) -> Result<()> {
        if user_ids.is_empty() || count <= 0 {
            return Ok(());
        }

        let now = chrono::Utc::now().timestamp_millis();
        let user_ids: Vec<i64> = user_ids.iter().map(|id| *id as i64).collect();
        sqlx::query(
            r#"
            INSERT INTO privchat_user_channels (
                user_id, channel_id, unread_count, is_pinned, is_muted, updated_at
            )
            SELECT user_id, $2, $3, false, false, $4
            FROM UNNEST($1::BIGINT[]) AS users(user_id)
            ON CONFLICT (user_id, channel_id) DO UPDATE SET
                unread_count = GREATEST(
                    0,
                    privchat_user_channels.unread_count + EXCLUDED.unread_count
                ),
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(&user_ids)
        .bind(channel_id as i64)
        .bind(count)
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(|e| {
            ServerError::Database(format!(
                "batch increment privchat_user_channels unread failed for channel={}: {}",
                channel_id, e
            ))
        })?;

        Ok(())
    }

    pub async fn clear_user_channel_unread(&self, user_id: u64, channel_id: u64) -> Result<()> {
        self.ensure_user_channel_rows(channel_id, &[user_id])
            .await?;

        sqlx::query(
            r#"
            UPDATE privchat_user_channels
            SET unread_count = 0,
                updated_at = $3
            WHERE user_id = $1 AND channel_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(channel_id as i64)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(self.pool())
        .await
        .map_err(|e| {
            ServerError::Database(format!(
                "clear privchat_user_channels unread failed for user={} channel={}: {}",
                user_id, channel_id, e
            ))
        })?;

        Ok(())
    }

    // =====================================================
    // 管理 API 方法
    // =====================================================

    /// 获取群组列表（管理 API）
    pub async fn list_groups_admin(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;

        let groups = sqlx::query!(
            r#"
            SELECT
                c.channel_id,
                c.group_id,
                g.name as group_name,
                g.description,
                g.owner_id as owner_id,
                g.member_count,
                COALESCE(g.created_at, 0) as group_created_at,
                COALESCE(g.updated_at, 0) as group_updated_at,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at as channel_created_at,
                c.updated_at as channel_updated_at
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            WHERE c.channel_type = 1
            ORDER BY c.updated_at DESC
            LIMIT $1 OFFSET $2
            "#,
            page_size as i64,
            offset as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组列表失败: {}", e)))?;

        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 1
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计群组数失败: {}", e)))?;

        let total = total_result.count.unwrap_or(0) as u32;

        let group_list: Vec<serde_json::Value> = groups
            .into_iter()
            .map(|g| {
                let owner_id = Some(g.owner_id as u64);
                serde_json::json!({
                    "group_id": g.group_id.unwrap_or(0) as u64,
                    "channel_id": g.channel_id as u64,
                    "name": g.group_name,
                    "description": g.description,
                    "owner_id": owner_id,
                    "member_count": g.member_count.unwrap_or(0) as u32,
                    "last_message_id": g.last_message_id.map(|id| id as u64),
                    "last_message_at": g.last_message_at.map(|ts| ts),
                    "message_count": g.message_count.unwrap_or(0) as u64,
                    "created_at": g.group_created_at,
                    "updated_at": g.group_updated_at,
                })
            })
            .collect();

        Ok((group_list, total))
    }

    /// 获取群组详情（管理 API）
    pub async fn get_group_admin(&self, group_id: u64) -> Result<serde_json::Value> {
        // JOIN privchat_users 把群主的 username/display_name/avatar 一并带回，admin
        // 不需要额外再调一次 user 详情。
        let group = sqlx::query!(
            r#"
            SELECT
                c.channel_id,
                c.group_id,
                g.name as group_name,
                g.description,
                g.owner_id as owner_id,
                ou.username as owner_username,
                ou.display_name as owner_display_name,
                ou.avatar_url as owner_avatar_url,
                g.member_count,
                COALESCE(g.created_at, 0) as group_created_at,
                COALESCE(g.updated_at, 0) as group_updated_at,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at as channel_created_at,
                c.updated_at as channel_updated_at
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            LEFT JOIN privchat_users ou ON ou.user_id = g.owner_id
            WHERE c.channel_type = 1 AND c.group_id = $1
            LIMIT 1
            "#,
            group_id as i64,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组详情失败: {}", e)))?;

        let group =
            group.ok_or_else(|| ServerError::NotFound(format!("群组 {} 不存在", group_id)))?;

        // 群成员：JOIN users 拿全局 username/display_name/avatar；只返活跃成员
        // （left_at IS NULL）。已离开的不在 admin 列表里露出。
        let members = sqlx::query!(
            r#"
            SELECT
                m.user_id,
                m.role,
                m.joined_at,
                m.nickname,
                u.username,
                u.display_name,
                u.avatar_url
            FROM privchat_group_members m
            LEFT JOIN privchat_users u ON u.user_id = m.user_id
            WHERE m.group_id = $1 AND m.left_at IS NULL
            ORDER BY m.role ASC, m.joined_at ASC
            "#,
            group_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组成员失败: {}", e)))?;

        let member_list: Vec<serde_json::Value> = members
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "user_id": m.user_id as u64,
                    "role": m.role,
                    "joined_at": m.joined_at,
                    "nickname": m.nickname,
                    "username": m.username,
                    "display_name": m.display_name,
                    "avatar_url": m.avatar_url,
                })
            })
            .collect();

        let owner_id = Some(group.owner_id as u64);
        Ok(serde_json::json!({
            "group_id": group_id,
            "channel_id": group.channel_id as u64,
            "name": group.group_name,
            "description": group.description,
            "owner_id": owner_id,
            "owner_username": group.owner_username,
            "owner_display_name": group.owner_display_name,
            "owner_avatar_url": group.owner_avatar_url,
            "member_count": group.member_count.unwrap_or(0) as u32,
            "members": member_list,
            "last_message_id": group.last_message_id.map(|id| id as u64),
            "last_message_at": group.last_message_at.map(|ts| ts),
            "message_count": group.message_count.unwrap_or(0) as u64,
            "created_at": group.group_created_at,
            "updated_at": group.group_updated_at,
        }))
    }

    // =====================================================
    // 群成员事实源一致性 helper
    //
    // 群成员的权威表是 `privchat_group_members`；`privchat_groups.member_count`
    // 仅是它的派生缓存。所有改动都走以下三个 helper（全部接 tx），并在结尾
    // 用 [recompute_group_member_count] 把 cache 刷一遍 —— 用 COUNT(*) 重算
    // 而不是 +/- 1，避免历史上 increment/decrement 导致的脱节。
    //
    // 调用方负责 begin/commit。
    // =====================================================

    /// 把若干用户登记为群成员（owner 用 Owner 角色，其他用 Member）。已存在的活跃成员
    /// 不做改动；曾经离开的复活（left_at = NULL）。`extra_member_ids` 中与 owner 相同
    /// 的或重复出现的会被去重。
    async fn upsert_group_members_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        group_id: u64,
        owner_id: u64,
        extra_member_ids: &[u64],
        now_ms: i64,
    ) -> Result<()> {
        // owner 优先以 Owner 角色入表
        Self::upsert_one_member_in_tx(tx, group_id, owner_id, MemberRole::Owner, now_ms).await?;

        let mut seen: HashSet<u64> = HashSet::new();
        seen.insert(owner_id);
        for uid in extra_member_ids {
            if !seen.insert(*uid) {
                continue;
            }
            Self::upsert_one_member_in_tx(tx, group_id, *uid, MemberRole::Member, now_ms).await?;
        }

        Self::recompute_group_member_count_in_tx(tx, group_id, now_ms).await
    }

    /// 把单个用户加入群（INSERT 或复活）；不重算 member_count。
    /// 想要立刻反映在 `privchat_groups.member_count` 上，调用方需配合 [recompute_group_member_count_in_tx]。
    async fn upsert_one_member_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        group_id: u64,
        user_id: u64,
        role: MemberRole,
        now_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO privchat_group_members (
                group_id, user_id, role, joined_at, nickname, left_at, updated_at
            )
            VALUES ($1, $2, $3, $4, NULL, NULL, $4)
            ON CONFLICT (group_id, user_id) DO UPDATE SET
                role = EXCLUDED.role,
                joined_at = CASE
                    WHEN privchat_group_members.left_at IS NOT NULL THEN EXCLUDED.joined_at
                    ELSE privchat_group_members.joined_at
                END,
                left_at = NULL,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .bind(role.to_i16())
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("插入/复活群成员失败: {}", e)))?;
        Ok(())
    }

    /// 标记一个活跃成员为已离开（left_at = now）。如果该成员已不活跃则什么都不做。
    /// 不重算 member_count，调用方负责。
    async fn mark_one_member_left_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        group_id: u64,
        user_id: u64,
        now_ms: i64,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE privchat_group_members
            SET left_at = $3, updated_at = $3
            WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL
            "#,
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("标记群成员离开失败: {}", e)))?;
        Ok(result.rows_affected() > 0)
    }

    /// 把 `privchat_groups.member_count` 重新刷成 `group_members WHERE left_at IS NULL` 的真实数量。
    async fn recompute_group_member_count_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        group_id: u64,
        now_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE privchat_groups
            SET member_count = (
                SELECT COUNT(*)::int FROM privchat_group_members
                WHERE group_id = $1 AND left_at IS NULL
            ),
            updated_at = $2
            WHERE group_id = $1
            "#,
        )
        .bind(group_id as i64)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("重算群成员数失败: {}", e)))?;
        Ok(())
    }

    /// 解散群组（管理 API）
    pub async fn dissolve_group_admin(&self, group_id: u64) -> Result<()> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;

        // 检查群组是否存在
        let exists: Option<(i32,)> = sqlx::query_as(
            r#"
            SELECT 1
            FROM privchat_channels
            WHERE channel_type = 1 AND group_id = $1
            LIMIT 1
            "#,
        )
        .bind(group_id as i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("检查群组失败: {}", e)))?;

        if exists.is_none() {
            return Err(ServerError::NotFound(format!("群组 {} 不存在", group_id)));
        }

        // 删除群组成员
        sqlx::query!(
            r#"
            DELETE FROM privchat_group_members
            WHERE group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组成员失败: {}", e)))?;

        // 删除群组会话
        sqlx::query!(
            r#"
            DELETE FROM privchat_channels
            WHERE channel_type = 1 AND group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组会话失败: {}", e)))?;

        // 删除群组记录
        sqlx::query!(
            r#"
            DELETE FROM privchat_groups
            WHERE group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组记录失败: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;

        info!("✅ 群组已解散: group_id={}", group_id);
        Ok(())
    }

    /// 获取会话列表（管理 API，包括私聊和群聊）
    pub async fn list_channels_admin(
        &self,
        page: u32,
        page_size: u32,
        channel_type: Option<i16>,
        user_id: Option<u64>,
    ) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;

        let mut where_clauses = vec![];
        if let Some(ct) = channel_type {
            where_clauses.push(format!("c.channel_type = {}", ct));
        }
        if user_id.is_some() {
            // 成员表是 privchat_channel_participants（此前误写成不存在的
            // privchat_channel_members，带 user_id 过滤的 admin 频道列表会直接 SQL 报错）。
            where_clauses.push(format!(
                "c.channel_id IN (SELECT channel_id FROM privchat_channel_participants WHERE user_id = {} AND left_at IS NULL)",
                user_id.unwrap()
            ));
        }

        let where_str = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT
                c.channel_id,
                c.channel_type,
                c.group_id,
                COALESCE(g.name, '') as name,
                g.description,
                g.member_count as group_member_count,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at,
                c.updated_at
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            {}
            ORDER BY c.updated_at DESC
            LIMIT $1 OFFSET $2
            "#,
            where_str
        );

        let channels = sqlx::query(&sql)
            .bind(page_size as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
            .map_err(|e| ServerError::Database(format!("查询会话列表失败: {}", e)))?;

        let channel_list: Vec<serde_json::Value> = channels
            .into_iter()
            .map(|c| {
                let channel_id: i64 = c.get("channel_id");
                let channel_type: i16 = c.get("channel_type");
                let group_id: Option<i64> = c.get("group_id");
                let group_member_count: Option<i32> = c.get("group_member_count");
                let last_message_id: Option<i64> = c.get("last_message_id");
                let last_message_at: Option<i64> = c.get("last_message_at");
                let message_count: Option<i64> = c.get("message_count");
                let created_at: i64 = c.get("created_at");
                let updated_at: Option<i64> = c.get("updated_at");

                serde_json::json!({
                    "channel_id": channel_id,
                    "channel_type": channel_type,
                    "group_id": group_id,
                    "name": c.get::<String, _>("name"),
                    "description": c.get::<Option<String>, _>("description"),
                    "member_count": group_member_count.unwrap_or(0),
                    "last_message": last_message_id.map(|mid| {
                        serde_json::json!({
                            "message_id": mid,
                            "timestamp": last_message_at
                        })
                    }),
                    "message_count": message_count.unwrap_or(0),
                    "created_at": created_at,
                    "updated_at": updated_at,
                })
            })
            .collect();

        // 统计总数
        let count_sql = if where_str.is_empty() {
            "SELECT COUNT(*) as count FROM privchat_channels c".to_string()
        } else {
            format!(
                "SELECT COUNT(*) as count FROM privchat_channels c {}",
                where_str
            )
        };

        let count_row = sqlx::query(&count_sql)
            .fetch_one(self.pool())
            .await
            .map_err(|e| ServerError::Database(format!("统计会话数失败: {}", e)))?;

        let total: i64 = count_row.get("count");

        Ok((channel_list, total as u32))
    }

    /// 获取会话详情（管理 API）
    pub async fn get_channel_admin(&self, channel_id: u64) -> Result<Option<serde_json::Value>> {
        let channel = sqlx::query!(
            r#"
            SELECT
                c.channel_id,
                c.channel_type,
                c.group_id,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at,
                c.updated_at,
                COALESCE(g.name, '') as name,
                g.description,
                g.member_count as group_member_count
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            WHERE c.channel_id = $1
            "#,
            channel_id as i64,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询会话详情失败: {}", e)))?;

        Ok(channel.map(|c| {
            let channel_id: i64 = c.channel_id;
            let channel_type: i16 = c.channel_type;
            let group_id: Option<i64> = c.group_id;
            let last_message_id: Option<i64> = c.last_message_id;
            let last_message_at: Option<i64> = c.last_message_at;
            let message_count: Option<i64> = c.message_count;
            let created_at: i64 = c.created_at;
            let updated_at: i64 = c.updated_at;
            let group_member_count: Option<i32> = c.group_member_count;

            serde_json::json!({
                "channel_id": channel_id,
                "channel_type": channel_type,
                "group_id": group_id,
                "name": c.name,
                "description": c.description,
                "member_count": group_member_count.unwrap_or(0),
                "last_message_id": last_message_id,
                "last_message_at": last_message_at,
                "message_count": message_count.unwrap_or(0),
                "created_at": created_at,
                "updated_at": updated_at,
            })
        }))
    }

    /// 获取会话参与者列表（管理 API）
    pub async fn list_participants_admin(
        &self,
        channel_id: u64,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;

        let participants = sqlx::query!(
            r#"
            SELECT
                user_id,
                role,
                nickname,
                joined_at
            FROM privchat_group_members gm
            WHERE gm.group_id = $1
            ORDER BY joined_at ASC
            LIMIT $2 OFFSET $3
            "#,
            channel_id as i64,
            page_size as i64,
            offset as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询参与者列表失败: {}", e)))?;

        let participant_list: Vec<serde_json::Value> = participants
            .into_iter()
            .map(|p| {
                let user_id: i64 = p.user_id;
                serde_json::json!({
                    "user_id": user_id,
                    "role": p.role,
                    "nickname": p.nickname,
                    "joined_at": p.joined_at,
                })
            })
            .collect();

        // 统计总数
        let count_row =
            sqlx::query("SELECT COUNT(*) as count FROM privchat_group_members WHERE group_id = $1")
                .bind(channel_id as i64)
                .fetch_one(self.pool())
                .await
                .map_err(|e| ServerError::Database(format!("统计参与者数失败: {}", e)))?;

        let total: i64 = count_row.get("count");

        Ok((participant_list, total as u32))
    }

    /// 获取好友关系列表（管理 API，通过私聊会话推断）
    pub async fn list_friendships_admin(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;

        let channels = sqlx::query!(
            r#"
            SELECT
                channel_id,
                direct_user1_id,
                direct_user2_id,
                created_at,
                updated_at,
                last_message_at
            FROM privchat_channels
            WHERE channel_type = 0
            ORDER BY updated_at DESC
            LIMIT $1 OFFSET $2
            "#,
            page_size as i64,
            offset as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询好友关系失败: {}", e)))?;

        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 0
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计好友关系数失败: {}", e)))?;

        let total = total_result.count.unwrap_or(0) as u32;

        let friendship_list: Vec<serde_json::Value> = channels
            .into_iter()
            .map(|c| {
                serde_json::json!({
                    "channel_id": c.channel_id as u64,
                    "user1_id": c.direct_user1_id.unwrap_or(0) as u64,
                    "user2_id": c.direct_user2_id.unwrap_or(0) as u64,
                    "created_at": c.created_at,
                    "updated_at": c.updated_at,
                    "last_message_at": c.last_message_at,
                })
            })
            .collect();

        Ok((friendship_list, total))
    }

    /// 获取用户的好友列表（管理 API，通过私聊会话推断）
    pub async fn get_user_friends_admin(&self, user_id: u64) -> Result<Vec<u64>> {
        let channels = sqlx::query!(
            r#"
            SELECT
                direct_user1_id,
                direct_user2_id
            FROM privchat_channels
            WHERE channel_type = 0
              AND (direct_user1_id = $1 OR direct_user2_id = $1)
            "#,
            user_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询用户好友列表失败: {}", e)))?;

        let mut friend_ids = Vec::new();
        for conv in channels {
            let friend_id = if conv.direct_user1_id.unwrap_or(0) as u64 == user_id {
                conv.direct_user2_id.unwrap_or(0) as u64
            } else {
                conv.direct_user1_id.unwrap_or(0) as u64
            };
            friend_ids.push(friend_id);
        }

        Ok(friend_ids)
    }

    /// 判断两个用户之间是否存在可承载 Presence 可见性的合法直聊会话。
    pub async fn has_presence_context(&self, viewer_user_id: u64, target_user_id: u64) -> bool {
        let result = sqlx::query!(
            r#"
            SELECT channel_id
            FROM privchat_channels
            WHERE channel_type = 0
              AND (
                    (direct_user1_id = $1 AND direct_user2_id = $2)
                 OR (direct_user1_id = $2 AND direct_user2_id = $1)
              )
            LIMIT 1
            "#,
            viewer_user_id as i64,
            target_user_id as i64,
        )
        .fetch_optional(self.pool())
        .await;

        matches!(result, Ok(Some(_)))
    }

    /// 获取某个用户相关的可发布 Presence 的直聊 channelId 列表。
    pub async fn list_presence_channels_for_user(&self, user_id: u64) -> Result<Vec<u64>> {
        let rows = sqlx::query!(
            r#"
            SELECT channel_id
            FROM privchat_channels
            WHERE channel_type = 0
              AND (direct_user1_id = $1 OR direct_user2_id = $1)
            "#,
            user_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询 Presence 关联会话失败: {}", e)))?;

        Ok(rows.into_iter().map(|row| row.channel_id as u64).collect())
    }

    /// 获取用户加入的群组列表（管理 API）
    pub async fn get_user_groups_admin(&self, user_id: u64) -> Result<Vec<serde_json::Value>> {
        let groups = sqlx::query!(
            r#"
            SELECT
                c.channel_id,
                c.group_id,
                g.name as group_name,
                g.description,
                g.owner_id,
                g.member_count,
                gm.role as member_role,
                gm.nickname as member_nickname,
                gm.joined_at
            FROM privchat_channels c
            INNER JOIN privchat_groups g ON c.group_id = g.group_id
            INNER JOIN privchat_group_members gm ON g.group_id = gm.group_id
            WHERE gm.user_id = $1
            ORDER BY gm.joined_at DESC
            "#,
            user_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询用户群组列表失败: {}", e)))?;

        let group_list: Vec<serde_json::Value> = groups
            .into_iter()
            .map(|g| {
                let owner_id: i64 = g.owner_id;
                serde_json::json!({
                    "channel_id": g.channel_id as u64,
                    "group_id": g.group_id.unwrap_or(0) as u64,
                    "name": g.group_name,
                    "description": g.description,
                    "owner_id": owner_id as u64,
                    "member_count": g.member_count.unwrap_or(0) as u32,
                    "role": g.member_role,
                    "nickname": g.member_nickname,
                    "joined_at": g.joined_at,
                })
            })
            .collect();

        Ok(group_list)
    }

    /// 获取群组统计（管理 API）
    pub async fn get_group_stats_admin(&self) -> Result<serde_json::Value> {
        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 1
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计群组数失败: {}", e)))?;

        let total = total_result.count.unwrap_or(0) as u32;

        // TODO: 实现活跃群组和已解散群组的统计
        Ok(serde_json::json!({
            "total": total,
            "active": total,  // TODO: 实现活跃群组统计
            "dissolved": 0,   // TODO: 实现已解散群组统计
        }))
    }

    /// 创建好友关系（管理 API）
    ///
    /// 严格遵守 ADMIN_API_SPEC §1.4「Admin Path Convergence Rule」：
    /// admin 仅负责 admin 特有的数据主表写入（跳过 pending，直接落 status=1），
    /// direct channel 的建立 / 缓存同步 / user_channel 行补齐 / hidden 清理
    /// 全部委托给与 RPC accept 共用的 `get_or_create_direct_channel`，
    /// 禁止在 admin 路径自建平行 side-effect。
    pub async fn create_friendship_admin(&self, user1_id: u64, user2_id: u64) -> Result<u64> {
        if user1_id == user2_id {
            return Err(ServerError::Validation("不能添加自己为好友".to_string()));
        }

        // admin 特有：跳过 pending 流程，直接 upsert 双向 status=1
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO privchat_friendships (
                user_id, friend_id, status, source, source_id, request_message, created_at, updated_at
            )
            VALUES
                ($1, $2, 1, NULL, NULL, NULL, $3, $3),
                ($2, $1, 1, NULL, NULL, NULL, $3, $3)
            ON CONFLICT (user_id, friend_id) DO UPDATE
            SET status = 1,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user1_id as i64)
        .bind(user2_id as i64)
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("写入好友关系失败: {}", e)))?;

        // direct channel 建立 + 完整 side-effect 编排收敛到主路径
        let (channel_id, created) = self
            .get_or_create_direct_channel(user1_id, user2_id, None, None)
            .await?;

        info!(
            "✅ 好友关系建立成功: {} <-> {}, channel_id={}, created={}",
            user1_id, user2_id, channel_id, created
        );
        Ok(channel_id)
    }

    /// 获取群组成员列表（管理 API）
    pub async fn list_members_admin(
        &self,
        group_id: u64,
    ) -> Result<Vec<(u64, Option<i16>, i64, Option<String>)>> {
        let members = sqlx::query!(
            r#"
            SELECT
                user_id,
                role,
                joined_at,
                nickname
            FROM privchat_group_members
            WHERE group_id = $1
            ORDER BY joined_at ASC
            "#,
            group_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组成员失败: {}", e)))?;

        Ok(members
            .into_iter()
            .map(|m| (m.user_id as u64, m.role, m.joined_at, m.nickname))
            .collect())
    }

    /// 移除群组成员（管理 API）
    pub async fn remove_member_admin(&self, group_id: u64, user_id: u64) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;

        let actually_left =
            Self::mark_one_member_left_in_tx(&mut tx, group_id, user_id, now).await?;
        if !actually_left {
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(format!(
                "群组 {} 中不存在活跃成员 {}",
                group_id, user_id
            )));
        }

        Self::recompute_group_member_count_in_tx(&mut tx, group_id, now).await?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;

        info!(
            "✅ 群组成员已移除: group_id={}, user_id={}",
            group_id, user_id
        );

        Ok(())
    }

    /// 添加群成员（管理 API）
    ///
    /// 1. 在事务里 upsert `privchat_group_members` + 重算 `privchat_groups.member_count`
    /// 2. 同步内存 cache（仅当 channel 已 load 进 cache）
    pub async fn add_member_admin(&self, group_id: u64, user_id: u64) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;

        // 已经是活跃成员 → 直接拒。曾经离开 / 不存在 → upsert 进去。
        let was_active: Option<bool> = sqlx::query_scalar(
            r#"
            SELECT (left_at IS NULL) FROM privchat_group_members
            WHERE group_id = $1 AND user_id = $2
            "#,
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("查询群组成员失败: {}", e)))?;

        if was_active == Some(true) {
            tx.rollback().await.ok();
            return Err(ServerError::Validation(format!(
                "用户 {} 已是群组 {} 的成员",
                user_id, group_id
            )));
        }

        Self::upsert_one_member_in_tx(&mut tx, group_id, user_id, MemberRole::Member, now).await?;
        Self::recompute_group_member_count_in_tx(&mut tx, group_id, now).await?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;

        // 同步内存 cache：忽略 join_channel 的 DB 写（upsert 幂等，重写一次代价低且不影响一致性）。
        let _ = self.join_channel(group_id, user_id, None).await;

        info!(
            "✅ 群组成员已添加: group_id={}, user_id={}",
            group_id, user_id
        );

        Ok(())
    }

    /// 设置群成员角色（管理 API）
    ///
    /// 只支持 Admin / Member 互切。Owner 角色禁止通过本接口设置 / 取消 ——
    /// owner 转让走独立流程（v1 不实现）。如果目标用户当前已是 Owner 或目标
    /// 角色是 Owner，直接返 Validation。
    ///
    /// 不存在该成员（或已离开）→ NotFound。
    pub async fn set_member_role_admin(
        &self,
        group_id: u64,
        user_id: u64,
        role: MemberRole,
    ) -> Result<()> {
        if role == MemberRole::Owner {
            return Err(ServerError::Validation(
                "禁止通过此接口将成员设为 Owner（请走转让群主流程）".to_string(),
            ));
        }

        let now = chrono::Utc::now().timestamp_millis();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;

        let current_role: Option<i16> = sqlx::query_scalar(
            r#"
            SELECT role FROM privchat_group_members
            WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL
            "#,
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("查询成员当前角色失败: {}", e)))?;

        let current = match current_role {
            Some(r) => MemberRole::from_i16(r),
            None => {
                tx.rollback().await.ok();
                return Err(ServerError::NotFound(format!(
                    "群组 {} 中不存在活跃成员 {}",
                    group_id, user_id
                )));
            }
        };

        if current == MemberRole::Owner {
            tx.rollback().await.ok();
            return Err(ServerError::Validation(
                "禁止修改群主角色（请走转让群主流程）".to_string(),
            ));
        }

        sqlx::query(
            r#"
            UPDATE privchat_group_members
            SET role = $3, updated_at = $4
            WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL
            "#,
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .bind(role.to_i16())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("更新群成员角色失败: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;

        // 内存缓存（如果 channel 已 load）
        let _ = self.set_member_role(&group_id, &user_id, role).await;

        info!(
            "✅ 群成员角色已更新: group_id={}, user_id={}, role={:?}",
            group_id, user_id, role
        );
        Ok(())
    }

    /// 生成私聊会话的索引键（已排序，确保一致性）
    fn make_direct_key(user1_id: u64, user2_id: u64) -> (u64, u64) {
        if user1_id < user2_id {
            (user1_id, user2_id)
        } else {
            (user2_id, user1_id)
        }
    }

    /// 创建会话
    pub async fn create_channel(
        &self,
        creator_id: u64,
        request: CreateChannelRequest,
    ) -> Result<ChannelResponse> {
        tracing::info!(
            "🔧 create_channel 开始: creator_id={}, type={:?}, member_ids={:?}",
            creator_id,
            request.channel_type,
            request.member_ids
        );

        // 验证请求
        if let Err(error) = self.validate_create_request(&creator_id, &request).await {
            tracing::error!("❌ 验证失败: {}", error);
            return Ok(ChannelResponse {
                channel: Channel::new_direct(0, 0, 0),
                success: false,
                error: Some(error),
            });
        }

        tracing::info!("✅ 验证通过");

        let mut channel = match request.channel_type {
            ChannelType::Direct => {
                // 私聊必须有且仅有一个目标用户
                if request.member_ids.len() != 1 {
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some("Direct channel must have exactly one target user".to_string()),
                    });
                }

                let target_user_id = request.member_ids[0];

                // Direct 频道统一走完整业务入口，避免仓储创建路径漏掉缓存、用户频道行
                // 或隐藏状态恢复等副作用。
                let (channel_id, _) = match self
                    .get_or_create_direct_channel(creator_id, target_user_id, None, None)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        error!("❌ 创建或获取私聊会话失败: {}", e);
                        return Ok(ChannelResponse {
                            channel: Channel::new_direct(0, 0, 0),
                            success: false,
                            error: Some(format!("创建私聊会话失败: {}", e)),
                        });
                    }
                };

                // 从缓存读回带成员的 Channel；若缓存未命中则返回 DB 版本
                let final_channel = self
                    .get_channel_opt(channel_id)
                    .await
                    .unwrap_or_else(|| Channel::new_direct(channel_id, creator_id, target_user_id));

                return Ok(ChannelResponse {
                    channel: final_channel,
                    success: true,
                    error: None,
                });
            }
            ChannelType::Group => {
                // 群聊需要名称
                if request.name.is_none() {
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some("Group channel must have a name".to_string()),
                    });
                }

                let now = chrono::Utc::now().timestamp_millis();
                let group_name = request.name.clone().unwrap();
                let group_description = request.description.clone();
                let initial_members = request.member_ids.clone();

                // 一个事务里完成：INSERT privchat_groups + INSERT owner / 初始成员
                // 到 privchat_group_members + 重算 member_count。任何一步失败都回滚，
                // 保证 groups 行不会出现没成员的孤儿状态。
                let mut tx = match self.pool().begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!("❌ 开启群组创建事务失败: {}", e);
                        return Ok(ChannelResponse {
                            channel: Channel::new_direct(0, 0, 0),
                            success: false,
                            error: Some(format!("创建群组失败: {}", e)),
                        });
                    }
                };

                // QR_CODE_SPEC v1.3：群创建时生成 qr_key opaque token。16-base62 ≈ 95 bits 熵，
                // 碰撞概率 ~ 1/10^16，单次生成即可；若极端碰撞则整个事务失败回滚，
                // 用户重试一次几乎必然成功。retry loop 不放在事务内是为了避免 SAVEPOINT
                // 复杂度——存储侧 UNIQUE 约束已经兜底数据正确性。
                let qr_key = generate_qr_key();
                let group_id = match sqlx::query_as::<_, (i64,)>(
                    r#"
                    INSERT INTO privchat_groups (
                        name, description, owner_id, member_count, created_at, updated_at, qr_key
                    )
                    VALUES ($1, $2, $3, 0, $4, $4, $5)
                    RETURNING group_id
                    "#,
                )
                .bind(&group_name)
                .bind(group_description.as_deref())
                .bind(creator_id as i64)
                .bind(now)
                .bind(&qr_key)
                .fetch_one(&mut *tx)
                .await
                {
                    Ok(row) => row.0 as u64,
                    Err(e) => {
                        let qr_collision = is_qr_key_unique_violation(&e);
                        if qr_collision {
                            error!("❌ 创建群组记录失败（qr_key 碰撞，极罕见）: {}", e);
                        } else {
                            error!("❌ 创建群组记录失败: {}", e);
                        }
                        let _ = tx.rollback().await;
                        return Ok(ChannelResponse {
                            channel: Channel::new_direct(0, 0, 0),
                            success: false,
                            error: Some(format!("创建群组记录失败: {}", e)),
                        });
                    }
                };

                if let Err(e) = Self::upsert_group_members_in_tx(
                    &mut tx,
                    group_id,
                    creator_id,
                    &initial_members,
                    now,
                )
                .await
                {
                    error!("❌ 群成员落库失败: {}", e);
                    let _ = tx.rollback().await;
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("群成员落库失败: {}", e)),
                    });
                }

                if let Err(e) = tx.commit().await {
                    error!("❌ 提交群组创建事务失败: {}", e);
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("提交群组事务失败: {}", e)),
                    });
                }

                info!(
                    "✅ 群组记录已创建: group_id={}, owner={}, extra_members={}",
                    group_id,
                    creator_id,
                    initial_members.len()
                );

                let mut conv = Channel::new_group(group_id, creator_id, request.name.clone());
                conv.metadata.description = request.description;
                conv.metadata.is_public = request.is_public.unwrap_or(false);
                conv.metadata.max_members =
                    request.max_members.or(Some(self.config.max_group_members));

                for member_id in initial_members {
                    if let Err(error) = conv.add_member(member_id, None) {
                        warn!("Failed to add member to group cache: {}", error);
                    }
                }

                conv
            }
            ChannelType::Room => {
                return Ok(ChannelResponse {
                    channel: Channel::new_direct(0, 0, 0),
                    success: false,
                    error: Some("Room channels are created via Admin API".to_string()),
                });
            }
        };

        // ✨ 保存会话到数据库
        // 如果是群聊，使用 create_channel_with_id（因为 group_id 已经创建）
        // 如果是私聊，使用 create（让数据库自动生成 channel_id）
        tracing::info!(
            "🔧 准备保存会话到数据库: type={:?}, id={}",
            channel.channel_type,
            channel.id
        );

        let created_channel = if channel.channel_type == ChannelType::Group {
            // 群聊：使用指定的 channel_id（等于 group_id）
            match self.channel_repository.create(&channel).await {
                Ok(created) => {
                    info!("✅ 群聊会话已保存到数据库: {}", created.id);
                    created
                }
                Err(e) => {
                    error!("❌ 保存群聊会话到数据库失败: {}", e);
                    // 如果创建失败，删除已创建的群组记录
                    if let Some(group_id) = channel.group_id {
                        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
                            .bind(group_id as i64)
                            .execute(self.channel_repository.pool())
                            .await;
                    }
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("创建会话失败: {}", e)),
                    });
                }
            }
        } else {
            // 私聊：让数据库自动生成 channel_id
            tracing::info!("🔧 调用 repository.create() 创建私聊会话");
            match self.channel_repository.create(&channel).await {
                Ok(created) => {
                    info!("✅ 私聊会话已保存到数据库: id={}", created.id);
                    created
                }
                Err(e) => {
                    error!("❌ 保存私聊会话到数据库失败: {}", e);
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("创建会话失败: {}", e)),
                    });
                }
            }
        };

        let actual_conv_id = created_channel.id;

        // ✨ 保存成员信息到数据库（因为 channel_repository.create 返回的 channel 没有成员信息）
        // 使用原始的 channel 对象中的成员信息
        for (member_id, member) in &channel.members {
            if let Err(e) = self
                .channel_repository
                .add_participant(actual_conv_id, *member_id, member.role)
                .await
            {
                warn!(
                    "⚠️ 保存成员到数据库失败: channel_id={}, user_id={}, error={}",
                    actual_conv_id, member_id, e
                );
            }
        }

        // ✨ 更新 channel 的 ID（如果数据库自动生成了新的 ID）
        channel.id = actual_conv_id;

        // 存储会话到内存缓存（使用包含成员的原始 channel 对象）
        let mut channels = self.channels.write().await;
        channels.insert(actual_conv_id, channel.clone());
        drop(channels);

        let member_ids = channel.get_member_ids();
        self.ensure_user_channel_rows(actual_conv_id, &member_ids)
            .await?;

        // 更新用户会话索引（使用包含成员的原始 channel 对象）
        self.update_user_channel_index(&channel).await;

        // 如果是私聊，更新私聊索引（使用包含成员的原始 channel 对象）
        if channel.channel_type == ChannelType::Direct {
            if member_ids.len() == 2 {
                let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                let mut direct_index = self.direct_channel_index.write().await;
                direct_index.insert(key, actual_conv_id);
            }
        }

        info!(
            "Created channel: {} (type: {:?}, creator: {}, members: {})",
            actual_conv_id,
            channel.channel_type,
            creator_id,
            channel.members.len()
        );

        Ok(ChannelResponse {
            channel,
            success: true,
            error: None,
        })
    }

    /// 使用指定ID创建会话（用于系统内部创建会话）
    pub async fn create_channel_with_id(
        &self,
        channel_id: u64,
        creator_id: u64,
        request: CreateChannelRequest,
    ) -> Result<ChannelResponse> {
        // ✨ 修复：检查会话是否已存在（内存和数据库）
        {
            let channels = self.channels.read().await;
            if let Some(existing_conv) = channels.get(&channel_id) {
                // 会话在内存中存在，但还需要确保数据库中也有记录
                // 尝试从数据库查询，如果不存在则创建
                if self
                    .channel_repository
                    .find_by_id(channel_id)
                    .await
                    .ok()
                    .flatten()
                    .is_none()
                {
                    warn!(
                        "⚠️ 会话 {} 在内存中存在但数据库中不存在，尝试补充创建",
                        channel_id
                    );
                    // 保存到数据库
                    if let Err(e) = self.channel_repository.create(&existing_conv).await {
                        warn!("⚠️ 补充创建会话到数据库失败: {}", e);
                    } else {
                        info!("✅ 已补充创建会话到数据库: {}", channel_id);
                        // 同时保存参与者
                        for (member_id, member) in &existing_conv.members {
                            let _ = self
                                .channel_repository
                                .add_participant(channel_id, *member_id, member.role)
                                .await;
                        }
                    }
                }
                return Ok(ChannelResponse {
                    channel: existing_conv.clone(),
                    success: true,
                    error: None,
                });
            }
        }

        // 验证请求
        if let Err(error) = self.validate_create_request(&creator_id, &request).await {
            return Ok(ChannelResponse {
                channel: Channel::new_direct(0, 0, 0),
                success: false,
                error: Some(error),
            });
        }

        let channel = match request.channel_type {
            ChannelType::Direct => {
                // Direct 频道必须走 create_or_get_direct_channel
                // （channel_id 由数据库 BIGSERIAL 分配，方向规范化 + 唯一约束）。
                // 任何走到这里的调用都是编程错误，历史上这条路径会产生
                // 反向 / 重复的 privchat_channels 行。
                warn!(
                    "❌ create_channel_with_id 不支持 Direct 频道，请改用 get_or_create_direct_channel：creator={}, channel_id={}",
                    creator_id, channel_id
                );
                return Ok(ChannelResponse {
                    channel: Channel::new_direct(0, 0, 0),
                    success: false,
                    error: Some(
                        "Direct channels must be created via get_or_create_direct_channel"
                            .to_string(),
                    ),
                });
            }
            ChannelType::Group => {
                let mut conv = Channel::new_group(channel_id, creator_id, request.name.clone());
                let now = chrono::Utc::now().timestamp_millis();
                let initial_members = request.member_ids.clone();

                // 同 create_channel：用事务串起 INSERT privchat_groups +
                // upsert privchat_group_members + 重算 member_count。
                let mut tx = match self.pool().begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        warn!("⚠️ 开启群组创建事务失败: {}", e);
                        return Ok(ChannelResponse {
                            channel: conv,
                            success: false,
                            error: Some(format!("创建群组失败: {}", e)),
                        });
                    }
                };

                // QR_CODE_SPEC v1.3：同上，群创建时生成 qr_key。`ON CONFLICT (group_id) DO NOTHING`
                // 保证幂等：如果该 group_id 行已存在，本次 INSERT 跳过（不会因 qr_key 与已有行
                // 冲突而失败，因为 ON CONFLICT 优先匹配 group_id PK）。
                let qr_key = generate_qr_key();
                let group_created = match sqlx::query(
                    r#"
                    INSERT INTO privchat_groups (
                        group_id, name, description, owner_id, member_count, created_at, updated_at, qr_key
                    )
                    VALUES ($1, $2, $3, $4, 0, $5, $5, $6)
                    ON CONFLICT (group_id) DO NOTHING
                    "#,
                )
                .bind(channel_id as i64)
                .bind(request.name.as_ref().unwrap_or(&"未命名群组".to_string()))
                .bind(request.description.as_ref())
                .bind(creator_id as i64)
                .bind(now)
                .bind(&qr_key)
                .execute(&mut *tx)
                .await
                {
                    Ok(res) => res.rows_affected() > 0,
                    Err(e) => {
                        warn!("⚠️ 创建群组记录失败: {}，可能是用户不存在于数据库中", e);
                        let _ = tx.rollback().await;
                        return Ok(ChannelResponse {
                            channel: conv,
                            success: false,
                            error: Some(format!(
                                "无法创建群组记录：用户 {} 可能不存在于数据库中",
                                creator_id
                            )),
                        });
                    }
                };

                if !group_created {
                    let _ = tx.rollback().await;
                    return Ok(ChannelResponse {
                        channel: conv,
                        success: false,
                        error: Some(format!("群组 {} 已存在", channel_id)),
                    });
                }

                if let Err(e) = Self::upsert_group_members_in_tx(
                    &mut tx,
                    channel_id,
                    creator_id,
                    &initial_members,
                    now,
                )
                .await
                {
                    error!("❌ 群成员落库失败: {}", e);
                    let _ = tx.rollback().await;
                    return Ok(ChannelResponse {
                        channel: conv,
                        success: false,
                        error: Some(format!("群成员落库失败: {}", e)),
                    });
                }

                if let Err(e) = tx.commit().await {
                    error!("❌ 提交群组创建事务失败: {}", e);
                    return Ok(ChannelResponse {
                        channel: conv,
                        success: false,
                        error: Some(format!("提交群组事务失败: {}", e)),
                    });
                }

                info!(
                    "✅ 群组记录已创建: group_id={}, owner={}, extra_members={}",
                    channel_id,
                    creator_id,
                    initial_members.len()
                );

                conv.group_id = Some(channel_id);
                conv.metadata.description = request.description;
                conv.metadata.is_public = request.is_public.unwrap_or(false);
                conv.metadata.max_members =
                    request.max_members.or(Some(self.config.max_group_members));

                for member_id in &initial_members {
                    let _ = conv.add_member(*member_id, None);
                }

                conv
            }
            ChannelType::Room => {
                return Ok(ChannelResponse {
                    channel: Channel::new_direct(0, 0, 0),
                    success: false,
                    error: Some("Room channels are created via Admin API".to_string()),
                });
            }
        };

        // ✨ 保存会话到数据库
        if let Err(e) = self.channel_repository.create(&channel).await {
            error!("❌ 保存会话到数据库失败: {}", e);
            // 继续执行，但记录错误（降级到内存存储）
        } else {
            info!("✅ 会话已保存到数据库: {}", channel_id);

            // ✨ 保存所有成员到数据库的参与者表
            for (member_id, member) in &channel.members {
                if let Err(e) = self
                    .channel_repository
                    .add_participant(channel_id, *member_id, member.role)
                    .await
                {
                    warn!("⚠️ 添加参与者 {} 到数据库失败: {}", member_id, e);
                }
            }
        }

        // 保存会话到内存缓存
        let mut channels = self.channels.write().await;
        channels.insert(channel_id, channel.clone());
        drop(channels);

        let member_ids: Vec<u64> = channel.members.keys().cloned().collect();
        self.ensure_user_channel_rows(channel_id, &member_ids)
            .await?;

        // 更新用户会话索引
        let mut user_channels = self.user_channels.write().await;
        for member_id in &member_ids {
            user_channels
                .entry(*member_id)
                .or_insert_with(Vec::new)
                .push(channel_id);
        }
        drop(user_channels);

        // 如果是私聊，更新私聊索引
        if channel.channel_type == ChannelType::Direct {
            if member_ids.len() == 2 {
                let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                let mut index = self.direct_channel_index.write().await;
                index.insert(key, channel_id);
            }
        }

        info!(
            "Created channel with ID: {}, type: {:?}, creator: {}, members: {}",
            channel_id,
            channel.channel_type,
            creator_id,
            channel.members.len()
        );

        Ok(ChannelResponse {
            channel,
            success: true,
            error: None,
        })
    }

    /// 获取会话（返回 Option，用于可选场景）
    pub async fn get_channel_opt(&self, channel_id: u64) -> Option<Channel> {
        // 先从内存缓存获取
        {
            let channels = self.channels.read().await;
            if let Some(conv) = channels.get(&channel_id) {
                return Some(conv.clone());
            }
        }

        // 如果内存缓存中没有，从数据库查询
        match self.channel_repository.find_by_id(channel_id).await {
            Ok(Some(conv)) => {
                // 加载参与者信息
                let participants = self
                    .get_channel_participants(channel_id)
                    .await
                    .unwrap_or_default();

                // 将参与者添加到会话成员中
                let mut conv_with_members = conv.clone();
                for participant in participants {
                    // 直接使用 participant 的 user_id (u64) 创建 ChannelMember
                    let member = ChannelMember::new(participant.user_id, participant.role);
                    conv_with_members
                        .members
                        .insert(participant.user_id, member);
                }

                // P1-16 hydration：恢复只带基础行 + 成员 role 时，群名/群策略/成员禁言
                // 全部丢失——重启后禁言直接失效（安全语义破坏）。从各自 DB 真源补齐。
                if conv_with_members.channel_type == ChannelType::Group {
                    let group_id = conv_with_members.group_id.unwrap_or(channel_id);
                    match sqlx::query_as::<_, (Option<String>, bool, i16, bool, bool, bool)>(
                        "SELECT name, allow_search, join_policy, allow_member_invite, \
                                allow_member_add_friend, all_muted \
                         FROM privchat_groups WHERE group_id = $1",
                    )
                    .bind(group_id as i64)
                    .fetch_optional(self.pool())
                    .await
                    {
                        Ok(Some((
                            name,
                            allow_search,
                            join_policy,
                            allow_member_invite,
                            allow_member_add_friend,
                            all_muted,
                        ))) => {
                            if conv_with_members.metadata.name.is_none() {
                                conv_with_members.metadata.name = name;
                            }
                            let settings = conv_with_members.settings.get_or_insert_with(
                                crate::model::channel::ChannelSettings::default,
                            );
                            settings.is_muted = all_muted;
                            settings.allow_search = allow_search;
                            settings.join_policy = join_policy.clamp(0, 2) as u8;
                            settings.require_approval = join_policy == 1;
                            settings.allow_member_invite = allow_member_invite;
                            settings.allow_member_add_friend = allow_member_add_friend;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!(
                                "hydrate group policy failed: group_id={} err={}",
                                group_id, e
                            )
                        }
                    }
                    // 成员禁言（privchat_group_members.mute_until，set_member_muted 的落库真源）。
                    // 语义：NULL = 未禁言；「永久禁言」由 RPC 层编码为 now+100 年时间戳
                    // （见 group/member/mute.rs 与 mute_reject_message 的 PERMANENT_THRESHOLD），
                    // 故 mute_until 非 NULL 即禁言，无 NULL 二义。
                    match sqlx::query_as::<_, (i64, Option<i64>)>(
                        "SELECT user_id, mute_until FROM privchat_group_members \
                         WHERE group_id = $1 AND left_at IS NULL AND mute_until IS NOT NULL",
                    )
                    .bind(group_id as i64)
                    .fetch_all(self.pool())
                    .await
                    {
                        Ok(rows) => {
                            let now = chrono::Utc::now();
                            for (uid, mute_until_ms) in rows {
                                if let Some(member) =
                                    conv_with_members.members.get_mut(&(uid as u64))
                                {
                                    let until = mute_until_ms
                                        .and_then(chrono::DateTime::from_timestamp_millis);
                                    if crate::model::channel::mute_is_active(true, until, now) {
                                        member.is_muted = true;
                                        member.mute_until = until;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "hydrate member mutes failed: group_id={} err={}",
                                group_id, e
                            )
                        }
                    }
                }

                // 缓存到内存
                let mut channels = self.channels.write().await;
                channels.insert(channel_id, conv_with_members.clone());

                Some(conv_with_members)
            }
            Ok(None) => None,
            Err(e) => {
                warn!("查询会话失败: {}", e);
                None
            }
        }
    }

    /// 获取会话（返回 Result，用于需要错误处理的场景）
    pub async fn get_channel(&self, channel_id: &u64) -> std::result::Result<Channel, ServerError> {
        self.get_channel_opt(*channel_id)
            .await
            .ok_or_else(|| ServerError::NotFound(format!("Channel not found: {}", channel_id)))
    }

    /// 获取会话参与者（从数据库查询）
    pub async fn get_channel_participants(
        &self,
        channel_id: u64,
    ) -> std::result::Result<Vec<crate::model::ChannelParticipant>, crate::error::DatabaseError>
    {
        self.channel_repository.get_participants(channel_id).await
    }

    /// 添加参与者到数据库
    pub async fn add_participant(
        &self,
        channel_id: u64,
        user_id: u64,
        role: crate::model::channel::MemberRole,
    ) -> std::result::Result<(), crate::error::DatabaseError> {
        self.channel_repository
            .add_participant(channel_id, user_id, role)
            .await
    }

    /// 获取用户的所有会话
    pub async fn get_user_channels(&self, user_id: u64) -> ChannelListResponse {
        self.ensure_user_channels_loaded(user_id).await;

        let user_channels = self.user_channels.read().await;
        let channel_ids = user_channels.get(&user_id).cloned().unwrap_or_default();
        drop(user_channels);

        let (mut channels, missing) = {
            let channels_lock = self.channels.read().await;
            let mut have = Vec::new();
            let mut miss = Vec::new();
            for conv_id in channel_ids {
                match channels_lock.get(&conv_id) {
                    Some(conv) if conv.is_active() => have.push(conv.clone()),
                    Some(_) => {}
                    // P1-15：被逐出的条目按需回源（访问驱动 rehydrate），
                    // 用户列表不因逐出缺条目。锁外处理避免与 get_channel_opt 死锁。
                    None => miss.push(conv_id),
                }
            }
            (have, miss)
        };
        for conv_id in missing {
            if let Some(conv) = self.get_channel_opt(conv_id).await {
                if conv.is_active() {
                    channels.push(conv);
                }
            }
        }

        // 按最后消息时间排序
        channels.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));

        ChannelListResponse {
            total: channels.len(),
            has_more: false,
            channels,
        }
    }

    /// 获取用户与系统用户的私聊会话 ID（用于发送欢迎等系统消息）。
    /// 若用户尚未有系统会话则返回 None（由调用方决定是否创建）。
    pub async fn get_system_channel_id_for_user(&self, user_id: u64) -> Option<u64> {
        use crate::config::SYSTEM_USER_ID;
        let response = self.get_user_channels(user_id).await;
        for ch in response.channels {
            if ch.channel_type == ChannelType::Direct
                && ch.get_member_ids().contains(&SYSTEM_USER_ID)
            {
                return Some(ch.id);
            }
        }
        None
    }

    /// entity/sync_entities 业务逻辑：群组分页与 SyncEntitiesResponse 构建
    pub async fn sync_entities_page_for_groups(
        &self,
        user_id: u64,
        since_version: Option<u64>,
        _scope: Option<&str>,
        limit: u32,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{
            GroupSyncPayload, SyncEntitiesResponse, SyncEntityItem,
        };
        let since_v = since_version.unwrap_or(0);

        #[derive(sqlx::FromRow)]
        struct GroupSyncRow {
            group_id: i64,
            name: String,
            description: Option<String>,
            avatar_url: Option<String>,
            owner_id: i64,
            member_count: i32,
            created_at: i64,
            updated_at: i64,
            sync_version: i64,
        }

        let limit = limit.min(200).max(1) as i64;
        let rows = sqlx::query_as::<_, GroupSyncRow>(
            r#"
            SELECT
                g.group_id,
                g.name,
                g.description,
                g.avatar_url,
                g.owner_id,
                g.member_count,
                g.created_at,
                g.updated_at,
                g.sync_version
            FROM privchat_groups g
            INNER JOIN privchat_group_members gm
                ON gm.group_id = g.group_id
            WHERE gm.user_id = $1
              AND gm.left_at IS NULL
              AND g.status = 0
              AND g.sync_version > $2
            ORDER BY g.sync_version ASC, g.group_id ASC
            LIMIT $3
            "#,
        )
        .bind(user_id as i64)
        .bind(since_v as i64)
        .bind(limit + 1)
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组同步分页失败: {}", e)))?;

        let (page, has_more) = take_sync_page(rows, limit as usize);

        let items: Vec<SyncEntityItem> = page
            .iter()
            .map(|row| {
                let payload = serde_json::to_value(GroupSyncPayload {
                    group_id: Some(row.group_id as u64),
                    name: Some(row.name.clone()),
                    description: row.description.clone(),
                    avatar: row.avatar_url.clone(),
                    avatar_url: row.avatar_url.clone(),
                    owner_id: Some(row.owner_id as u64),
                    member_count: Some(row.member_count.max(0) as u32),
                    created_at: Some(row.created_at),
                    updated_at: Some(row.updated_at),
                })
                .unwrap_or_else(|_| serde_json::json!({}));
                SyncEntityItem {
                    entity_id: row.group_id.to_string(),
                    version: row.sync_version as u64,
                    deleted: false,
                    payload: Some(payload),
                }
            })
            .collect();

        Ok(SyncEntitiesResponse {
            items,
            next_version: next_version_from_page(&page, since_v, |row| row.sync_version as u64),
            has_more,
            min_version: None,
        })
    }

    /// entity/sync_entities 业务逻辑：会话列表分页与 SyncEntitiesResponse 构建（私聊+群聊+系统）
    /// P1-00/P1-15：进程内 channel 相关 cache 的条目数快照（水位观测）。
    /// 返回 (channels, user_channels, direct_channel_index, last_message_cache)。
    pub async fn memory_cache_entries(&self) -> (usize, usize, usize, usize) {
        let channels = self.channels.read().await.len();
        let user_channels = self.user_channels.read().await.len();
        let direct_index = self.direct_channel_index.read().await.len();
        let last_message = self.last_message_cache.read().await.len();
        (channels, user_channels, direct_index, last_message)
    }

    /// P1-15：channels / last_message_cache 有界逐出（60s 统计循环驱动）。
    /// hydration 已验证完整（P1-16：群名/settings/成员禁言/direct 都能回源重建），
    /// 逐出后读路径 get_channel_opt / get_user_channels 按需 rehydrate。
    /// 按最旧 updated_at / preview.timestamp 逐出超额部分。user_channels 与
    /// direct_channel_index 是小条目索引，不逐出（水位 warning 盯守）。
    pub async fn evict_stale_memory(
        &self,
        max_channels: usize,
        max_previews: usize,
    ) -> (usize, usize) {
        let mut evicted_channels = 0usize;
        {
            let mut channels = self.channels.write().await;
            if channels.len() > max_channels {
                let mut by_age: Vec<(u64, chrono::DateTime<chrono::Utc>)> =
                    channels.iter().map(|(id, c)| (*id, c.updated_at)).collect();
                by_age.sort_by_key(|(_, t)| *t);
                let overflow = channels.len() - max_channels;
                for (id, _) in by_age.into_iter().take(overflow) {
                    channels.remove(&id);
                    evicted_channels += 1;
                }
            }
        }
        let mut evicted_previews = 0usize;
        {
            let mut cache = self.last_message_cache.write().await;
            if cache.len() > max_previews {
                let mut by_age: Vec<(u64, chrono::DateTime<chrono::Utc>)> =
                    cache.iter().map(|(id, p)| (*id, p.timestamp)).collect();
                by_age.sort_by_key(|(_, t)| *t);
                let overflow = cache.len() - max_previews;
                for (id, _) in by_age.into_iter().take(overflow) {
                    cache.remove(&id);
                    evicted_previews += 1;
                }
            }
        }
        (evicted_channels, evicted_previews)
    }

    pub async fn sync_entities_page_for_channels(
        &self,
        user_id: u64,
        since_version: Option<u64>,
        scope: Option<&str>,
        limit: u32,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{
            ChannelSyncPayload, SyncEntitiesResponse, SyncEntityItem,
        };

        #[derive(sqlx::FromRow)]
        struct ChannelSyncRow {
            channel_id: i64,
            channel_type: i16,
            direct_user1_id: Option<i64>,
            direct_user2_id: Option<i64>,
            group_id: Option<i64>,
            last_message_id: Option<i64>,
            last_message_at: Option<i64>,
            message_count: i64,
            created_at: i64,
            updated_at: i64,
            channel_sync_version: i64,
            group_name: Option<String>,
            group_avatar_url: Option<String>,
            peer_display_name: Option<String>,
            peer_username: Option<String>,
            unread_count: i32,
            is_pinned: bool,
            is_muted: bool,
            sync_version: i64,
        }

        // P0-12：since_version / cursor / hidden / LIMIT 全部下推到 SQL。
        // 旧实现按 user_id 全量 fetch_all 再内存过滤分页——重连风暴下每一页都是
        // O(用户全部 channel) 的查询、JOIN 与传输放大。增量 sync（since 命中 0~少数行）
        // 现在只扫描并返回变化行。
        //
        // keyset 语义：ORDER BY (effective_version, channel_id)，游标沿用 wire 契约
        // `scope="cursor:{last_channel_id}"`——先用子查询取游标行当前的 effective_version，
        // 再做 row-value 比较。游标行已不存在/不可见时 COALESCE(-1) 退化为整个 since
        // 窗口重扫（客户端 upsert 幂等，安全）。游标行版本在翻页间被并发抬升时的漂移
        // 语义与旧内存分页一致（按当前排序重新定位），不劣化。
        let limit = limit.min(200).max(1);
        let since: Option<i64> = since_version.map(|v| v as i64);
        let after_id: Option<i64> = scope
            .and_then(|s| s.strip_prefix("cursor:"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|v| v as i64);
        // hidden 是进程内状态，下推为排除数组；若 LIMIT 后再内存过滤会产生短页/空页。
        let hidden_ids: Vec<i64> = {
            let hidden_lock = self.hidden_channels.read().await;
            hidden_lock
                .iter()
                .filter(|(uid, _)| *uid == user_id)
                .map(|(_, cid)| *cid as i64)
                .collect()
        };

        let rows = sqlx::query_as::<_, ChannelSyncRow>(
            r#"
            SELECT
                c.channel_id,
                c.channel_type,
                c.direct_user1_id,
                c.direct_user2_id,
                c.group_id,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at,
                c.updated_at,
                c.sync_version AS channel_sync_version,
                g.name AS group_name,
                g.avatar_url AS group_avatar_url,
                pu.display_name AS peer_display_name,
                pu.username AS peer_username,
                uc.unread_count,
                uc.is_pinned,
                uc.is_muted,
                GREATEST(c.sync_version, uc.sync_version) AS sync_version
            FROM privchat_user_channels uc
            INNER JOIN privchat_channels c
                ON c.channel_id = uc.channel_id
            LEFT JOIN privchat_groups g
                ON g.group_id = c.group_id
            LEFT JOIN privchat_users pu
                ON c.channel_type = 0
               AND pu.user_id = CASE
                    WHEN c.direct_user1_id = $1 THEN c.direct_user2_id
                    ELSE c.direct_user1_id
                END
            WHERE uc.user_id = $1
              AND ($2::BIGINT IS NULL OR c.sync_version > $2 OR uc.sync_version > $2)
              AND (
                $3::BIGINT IS NULL
                OR (GREATEST(c.sync_version, uc.sync_version), c.channel_id) > (
                    COALESCE(
                        (
                            SELECT GREATEST(c2.sync_version, uc2.sync_version)
                            FROM privchat_user_channels uc2
                            INNER JOIN privchat_channels c2
                                ON c2.channel_id = uc2.channel_id
                            WHERE uc2.user_id = $1 AND uc2.channel_id = $3
                        ),
                        -1
                    ),
                    $3
                )
              )
              AND NOT (c.channel_id = ANY($4::BIGINT[]))
            ORDER BY GREATEST(c.sync_version, uc.sync_version) ASC, c.channel_id ASC
            LIMIT $5
            "#,
        )
        .bind(user_id as i64)
        .bind(since)
        .bind(after_id)
        .bind(&hidden_ids)
        .bind(limit as i64 + 1)
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询频道同步分页失败: {}", e)))?;

        let (page, has_more) = take_sync_page(rows, limit as usize);
        let items: Vec<SyncEntityItem> = page
            .iter()
            .map(|row| {
                let channel_id = row.channel_id as u64;
                let channel_type_enum = ChannelType::from_i16(row.channel_type);
                // DM 对端 user_id：仅私聊设置，群聊为空。直接下发给客户端持久化，
                // 客户端不再靠 channel_member JOIN / message.from_uid 推断。
                let peer_user_id = if channel_type_enum == ChannelType::Direct {
                    match (
                        row.direct_user1_id.map(|v| v as u64),
                        row.direct_user2_id.map(|v| v as u64),
                    ) {
                        (Some(left), Some(right)) if left == user_id => Some(right),
                        (Some(left), Some(right)) if right == user_id => Some(left),
                        _ => None,
                    }
                } else {
                    None
                };
                let channel_name = if channel_type_enum == ChannelType::Direct {
                    // DM 名下发对端显示名(display_name > username);两者皆空才回退
                    // uid 字符串——否则新登录设备在 user 资料同步到达前会一直显示裸 id。
                    row.peer_display_name
                        .clone()
                        .filter(|v| !v.is_empty())
                        .or_else(|| row.peer_username.clone().filter(|v| !v.is_empty()))
                        .or_else(|| peer_user_id.map(|uid| uid.to_string()))
                        .unwrap_or_default()
                } else {
                    row.group_name.clone().unwrap_or_default()
                };

                // Conversation preview content is a client-side projection of
                // locally loaded messages. Keep this legacy wire field empty;
                // the server only supplies the authoritative ordering time.
                let last_msg_timestamp = row.last_message_at.unwrap_or(0);

                let channel_type = match channel_type_enum {
                    ChannelType::Direct => 1i64,
                    ChannelType::Group => 2i64,
                    ChannelType::Room => 2i64,
                };
                let payload = serde_json::to_value(ChannelSyncPayload {
                    channel_id: Some(channel_id),
                    channel_type: Some(channel_type),
                    type_field: Some(channel_type),
                    channel_name: Some(channel_name.clone()),
                    name: Some(channel_name),
                    avatar: Some(row.group_avatar_url.clone().unwrap_or_default()),
                    unread_count: Some(row.unread_count),
                    last_msg_content: Some(String::new()),
                    last_msg_timestamp: Some(last_msg_timestamp),
                    top: Some(if row.is_pinned { 1 } else { 0 }),
                    mute: Some(if row.is_muted { 1 } else { 0 }),
                    peer_user_id,
                })
                .unwrap_or_else(|_| serde_json::json!({}));
                SyncEntityItem {
                    entity_id: channel_id.to_string(),
                    version: row.sync_version as u64,
                    deleted: false,
                    payload: Some(payload),
                }
            })
            .collect();

        Ok(SyncEntitiesResponse {
            items,
            next_version: next_version_from_page(&page, since_version.unwrap_or(0), |row| {
                row.sync_version as u64
            }),
            has_more,
            min_version: None,
        })
    }

    /// entity/sync_entities 业务逻辑：群成员分页与 SyncEntitiesResponse 构建
    pub async fn sync_entities_page_for_group_members(
        &self,
        user_id: u64,
        since_version: Option<u64>,
        scope: Option<&str>,
        limit: u32,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{
            GroupMemberSyncPayload, SyncEntitiesResponse, SyncEntityItem,
        };

        let group_id = scope
            .and_then(|s| {
                if let Ok(v) = s.parse::<u64>() {
                    return Some(v);
                }
                s.split([':', '/', '|', ','])
                    .filter_map(|t| t.trim().parse::<u64>().ok())
                    .next_back()
            })
            .ok_or_else(|| {
                ServerError::Validation("group_member sync requires scope=group_id".to_string())
            })?;

        let Some(channel) = self.get_channel_opt(group_id).await else {
            return Ok(SyncEntitiesResponse {
                items: vec![],
                next_version: since_version.unwrap_or(0),
                has_more: false,
                min_version: None,
            });
        };

        if channel.channel_type != ChannelType::Group {
            return Ok(SyncEntitiesResponse {
                items: vec![],
                next_version: since_version.unwrap_or(0),
                has_more: false,
                min_version: None,
            });
        }

        if !channel.members.contains_key(&user_id) {
            return Ok(SyncEntitiesResponse {
                items: vec![],
                next_version: since_version.unwrap_or(0),
                has_more: false,
                min_version: None,
            });
        }

        #[derive(sqlx::FromRow)]
        struct GroupMemberSyncRow {
            group_id: i64,
            user_id: i64,
            role: i16,
            nickname: Option<String>,
            mute_until: Option<i64>,
            joined_at: i64,
            left_at: Option<i64>,
            updated_at: i64,
            sync_version: i64,
        }

        let since_v = since_version.unwrap_or(0);
        let page_limit = limit.min(200).max(1) as i64;
        let rows = sqlx::query_as::<_, GroupMemberSyncRow>(
            r#"
            SELECT
                group_id,
                user_id,
                role,
                nickname,
                mute_until,
                joined_at,
                left_at,
                updated_at,
                sync_version
            FROM privchat_group_members
            WHERE group_id = $1
              AND sync_version > $2
            ORDER BY sync_version ASC, user_id ASC
            LIMIT $3
            "#,
        )
        .bind(group_id as i64)
        .bind(since_v as i64)
        .bind(page_limit + 1)
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群成员同步分页失败: {}", e)))?;

        let (page_rows, has_more) = take_sync_page(rows, page_limit as usize);
        let next_version =
            next_version_from_page(&page_rows, since_v, |row| row.sync_version as u64);

        let items: Vec<SyncEntityItem> = page_rows
            .into_iter()
            .map(|row| {
                let deleted = row.left_at.is_some();
                let payload = if deleted {
                    None
                } else {
                    Some(
                        serde_json::to_value(GroupMemberSyncPayload {
                            group_id: Some(row.group_id as u64),
                            user_id: Some(row.user_id as u64),
                            uid: Some(row.user_id as u64),
                            role: Some(i32::from(row.role)),
                            status: Some(0),
                            alias: row.nickname,
                            is_muted: Some(
                                row.mute_until.unwrap_or(0) > chrono::Utc::now().timestamp_millis(),
                            ),
                            joined_at: Some(row.joined_at),
                            updated_at: Some(row.updated_at),
                            version: Some(row.sync_version),
                        })
                        .unwrap_or_else(|_| serde_json::json!({})),
                    )
                };
                SyncEntityItem {
                    entity_id: format!("{}:{}", row.group_id, row.user_id),
                    version: row.sync_version as u64,
                    deleted,
                    payload,
                }
            })
            .collect();

        Ok(SyncEntitiesResponse {
            items,
            next_version,
            has_more,
            min_version: None,
        })
    }

    /// 加入会话
    pub async fn join_channel(
        &self,
        channel_id: u64,
        user_id: u64,
        role: Option<MemberRole>,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;

        let channel = match channels.get_mut(&channel_id) {
            Some(c) => c,
            None => return Ok(false),
        };

        if !channel.is_active() {
            return Ok(false);
        }
        if !channel.metadata.allow_invite && channel.channel_type == ChannelType::Group {
            return Ok(false);
        }

        let channel_type = channel.channel_type;
        if let Err(error) = channel.add_member(user_id, role) {
            warn!(
                "Failed to add user {} to channel {}: {}",
                user_id, channel_id, error
            );
            return Ok(false);
        }
        drop(channels);

        // 群聊：把成员真实落到 privchat_group_members + 刷 member_count，事务保证一致。
        if channel_type == ChannelType::Group {
            let now = chrono::Utc::now().timestamp_millis();
            let mut tx = self
                .pool()
                .begin()
                .await
                .map_err(|e| ServerError::Database(format!("开启加群事务失败: {}", e)))?;
            Self::upsert_one_member_in_tx(
                &mut tx,
                channel_id,
                user_id,
                role.unwrap_or(MemberRole::Member),
                now,
            )
            .await?;
            Self::recompute_group_member_count_in_tx(&mut tx, channel_id, now).await?;
            tx.commit()
                .await
                .map_err(|e| ServerError::Database(format!("提交加群事务失败: {}", e)))?;
        }

        self.ensure_user_channel_rows(channel_id, &[user_id])
            .await?;

        let mut user_channels = self.user_channels.write().await;
        user_channels
            .entry(user_id)
            .or_insert_with(Vec::new)
            .push(channel_id);

        info!("User {} joined channel {}", user_id, channel_id);
        Ok(true)
    }

    /// #72A.1 群审批原子提交：CAS 把 pending 申请置 approved + **同一 DB 事务内**加群成员 + 重算 member_count。
    ///
    /// 并发/多实例安全（不依赖内存锁）：
    ///   - CAS `WHERE status = 0` 让并发/重复审批中至多一个成功（DB 裁决）；
    ///   - approve 与加成员同事务：加成员失败整体回滚，绝不产生「approved 但非群成员」孤儿；
    ///   - upsert_one_member_in_tx 幂等（ON CONFLICT），重复不产生重复成员。
    ///
    /// 返回 Ok(true)=本次批准成功并已入群；Ok(false)=申请已被处理（并发/重复 loser），未改任何状态。
    pub async fn approve_join_request_in_tx(
        &self,
        request_id: &str,
        group_id: u64,
        user_id: u64,
        handler_id: u64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let now_ms = now.timestamp_millis();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启审批事务失败: {}", e)))?;

        // CAS：仅当申请仍是 pending(0) 才置 approved(1)；命中 0 行 = 已被并发/重复处理。
        let updated = sqlx::query(
            r#"
            UPDATE privchat_group_join_requests
            SET status = 1, handler_id = $2, updated_at = $3
            WHERE request_id = $1 AND status = 0
            "#,
        )
        .bind(request_id)
        .bind(handler_id as i64)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("审批 CAS 失败: {}", e)))?;

        if updated.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Ok(false);
        }

        // 同事务加成员（幂等）+ 重算成员数。任一失败 → 整体回滚，status 保持 pending。
        Self::upsert_one_member_in_tx(&mut tx, group_id, user_id, MemberRole::Member, now_ms)
            .await?;
        Self::recompute_group_member_count_in_tx(&mut tx, group_id, now_ms).await?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交审批事务失败: {}", e)))?;

        // 提交后同步内存缓存（DB 已是真相；缓存失败不破坏一致性，reconnect/sync 会重建）。
        {
            let mut channels = self.channels.write().await;
            if let Some(channel) = channels.get_mut(&group_id) {
                let _ = channel.add_member(user_id, Some(MemberRole::Member));
            }
        }
        self.ensure_user_channel_rows(group_id, &[user_id]).await?;
        {
            let mut user_channels = self.user_channels.write().await;
            let list = user_channels.entry(user_id).or_insert_with(Vec::new);
            if !list.contains(&group_id) {
                list.push(group_id);
            }
        }

        info!(
            "✅ 审批入群(原子): request_id={}, user_id={}, group_id={}",
            request_id, user_id, group_id
        );
        Ok(true)
    }

    /// 离开会话
    pub async fn leave_channel(&self, channel_id: u64, user_id: u64) -> Result<bool> {
        let mut channels = self.channels.write().await;

        let channel = match channels.get_mut(&channel_id) {
            Some(c) => c,
            None => return Ok(false),
        };

        let channel_type = channel.channel_type;
        if let Err(error) = channel.remove_member(&user_id) {
            warn!(
                "Failed to remove user {} from channel {}: {}",
                user_id, channel_id, error
            );
            return Ok(false);
        }
        drop(channels);

        if channel_type == ChannelType::Group {
            let now = chrono::Utc::now().timestamp_millis();
            let mut tx = self
                .pool()
                .begin()
                .await
                .map_err(|e| ServerError::Database(format!("开启退群事务失败: {}", e)))?;
            let actually_left =
                Self::mark_one_member_left_in_tx(&mut tx, channel_id, user_id, now).await?;
            if actually_left {
                Self::recompute_group_member_count_in_tx(&mut tx, channel_id, now).await?;
            }
            tx.commit()
                .await
                .map_err(|e| ServerError::Database(format!("提交退群事务失败: {}", e)))?;
        }

        let mut user_channels = self.user_channels.write().await;
        if let Some(conv_list) = user_channels.get_mut(&user_id) {
            conv_list.retain(|id| *id != channel_id);
        }

        info!("User {} left channel {}", user_id, channel_id);
        Ok(true)
    }

    /// 更新会话信息
    pub async fn update_channel_info(
        &self,
        channel_id: u64,
        user_id: u64,
        name: Option<String>,
        description: Option<String>,
        avatar_url: Option<String>,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;

        if let Some(channel) = channels.get_mut(&channel_id) {
            // 检查权限
            if !channel.check_permission(&user_id, |perms| perms.can_edit_info) {
                return Ok(false);
            }

            // 更新信息
            if let Some(name) = name {
                channel.metadata.name = Some(name);
            }
            if let Some(description) = description {
                channel.metadata.description = Some(description);
            }
            if let Some(avatar_url) = avatar_url {
                channel.metadata.avatar_url = Some(avatar_url);
            }

            channel.updated_at = chrono::Utc::now();

            info!("Updated channel {} info by user {}", channel_id, user_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 更新成员角色
    pub async fn update_member_role(
        &self,
        channel_id: u64,
        operator_id: u64,
        target_user_id: u64,
        new_role: MemberRole,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;

        if let Some(channel) = channels.get_mut(&channel_id) {
            // 检查操作者权限
            if !channel.check_permission(&operator_id, |perms| perms.can_manage_permissions) {
                return Ok(false);
            }

            match channel.update_member_role(&target_user_id, new_role) {
                Ok(_) => {
                    info!(
                        "Updated member {} role in channel {} by {}",
                        target_user_id, channel_id, operator_id
                    );
                    Ok(true)
                }
                Err(error) => {
                    warn!("Failed to update member role: {}", error);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// 标记消息已读（单条，兼容旧 RPC）
    pub async fn mark_message_read(
        &self,
        channel_id: u64,
        user_id: u64,
        message_id: u64,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;

        if let Some(channel) = channels.get_mut(&channel_id) {
            match channel.mark_member_read(&user_id, message_id) {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    /// 按 pts 推进已读（正确模型，O(1)）：last_read_pts = max(last_read_pts, read_pts)
    /// 返回更新后的 last_read_pts（若用户不在频道返回 None）
    pub async fn mark_read_pts(
        &self,
        user_id: &u64,
        channel_id: &u64,
        read_pts: u64,
    ) -> Result<Option<u64>> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(member) = channel.members.get_mut(user_id) {
                member.mark_read_pts(read_pts);
                return Ok(Some(member.last_read_pts));
            }
        }
        Ok(None)
    }

    /// 更新最后消息
    pub async fn update_last_message(&self, channel_id: u64, message_id: u64) -> Result<bool> {
        let mut updated_channel = {
            let mut channels = self.channels.write().await;

            if let Some(channel) = channels.get_mut(&channel_id) {
                channel.update_last_message(message_id);
                Some(channel.clone())
            } else {
                None
            }
        };

        if updated_channel.is_none() {
            // cache miss：走 get_channel_opt 加载（会一并读取 participants 填充 members），
            // 避免把 members 为空的 Channel 塞进缓存，导致后续 get_member_ids() 返回空
            // （曾导致 admin 发消息 recipients=0）。
            if let Some(mut channel) = self.get_channel_opt(channel_id).await {
                channel.update_last_message(message_id);
                updated_channel = Some(channel.clone());

                let mut channels = self.channels.write().await;
                channels.insert(channel_id, channel);
            }
        }

        if let Some(channel) = updated_channel {
            self.channel_repository
                .update(&channel)
                .await
                .map_err(|e| ServerError::Database(format!("更新会话最后消息失败: {}", e)))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 查找私聊会话
    pub async fn find_direct_channel(&self, user1_id: u64, user2_id: u64) -> Option<u64> {
        let key = Self::make_direct_key(user1_id, user2_id);
        let direct_index = self.direct_channel_index.read().await;
        direct_index.get(&key).copied()
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> ChannelServiceStats {
        let channels = self.channels.read().await;

        let total_channels = channels.len();
        let mut active_channels = 0;
        let mut direct_channels = 0;
        let mut group_channels = 0;
        let mut total_members = 0;

        for conv in channels.values() {
            if conv.is_active() {
                active_channels += 1;
            }

            match conv.channel_type {
                ChannelType::Direct => direct_channels += 1,
                ChannelType::Group => group_channels += 1,
                ChannelType::Room => {}
            }

            total_members += conv.members.len();
        }

        let avg_members_per_channel = if total_channels > 0 {
            total_members as f64 / total_channels as f64
        } else {
            0.0
        };

        ChannelServiceStats {
            total_channels,
            active_channels,
            direct_channels,
            group_channels,
            total_members,
            avg_members_per_channel,
        }
    }

    /// 验证创建请求
    async fn validate_create_request(
        &self,
        creator_id: &u64,
        request: &CreateChannelRequest,
    ) -> std::result::Result<(), String> {
        // 检查用户会话数量限制
        let user_channels = self.user_channels.read().await;
        if let Some(conv_list) = user_channels.get(creator_id) {
            if conv_list.len() >= self.config.max_channels_per_user {
                return Err("Maximum channels per user limit reached".to_string());
            }
        }

        // 检查群聊设置
        if request.channel_type == ChannelType::Group {
            if request.name.is_none() {
                return Err("Group channel must have a name".to_string());
            }

            if !self.config.allow_public_groups && request.is_public.unwrap_or(false) {
                return Err("Public groups are not allowed".to_string());
            }

            if let Some(max_members) = request.max_members {
                if max_members > self.config.max_group_members {
                    return Err("Maximum group members limit exceeded".to_string());
                }
            }
        }

        Ok(())
    }

    /// 更新用户会话索引
    async fn update_user_channel_index(&self, channel: &Channel) {
        let mut user_channels = self.user_channels.write().await;

        for user_id in channel.get_member_ids() {
            let entry = user_channels.entry(user_id).or_insert_with(Vec::new);
            if !entry.contains(&channel.id) {
                entry.push(channel.id);
            }
        }
    }

    /// 确保某个用户的会话索引已从 DB 回填到内存（服务重启后恢复列表）
    async fn ensure_user_channels_loaded(&self, user_id: u64) {
        let has_in_memory = {
            let user_channels = self.user_channels.read().await;
            user_channels
                .get(&user_id)
                .is_some_and(|ids| !ids.is_empty())
        };
        if has_in_memory {
            return;
        }

        let db_ids = match self
            .channel_repository
            .list_channel_ids_by_user(user_id)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                warn!(
                    "list_channel_ids_by_user failed for user {}: {}",
                    user_id, e
                );
                return;
            }
        };

        for cid in db_ids {
            if let Some(channel) = self.get_channel_opt(cid).await {
                self.update_user_channel_index(&channel).await;
                if channel.channel_type == ChannelType::Direct {
                    let member_ids = channel.get_member_ids();
                    if member_ids.len() == 2 {
                        let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                        let mut index = self.direct_channel_index.write().await;
                        index.insert(key, cid);
                    }
                }
            }
        }

        // 从 privchat_user_channels 加载隐藏/置顶/静音状态到内存缓存
        #[derive(sqlx::FromRow)]
        struct UserChannelFlagRow {
            channel_id: i64,
            is_hidden: bool,
            is_pinned: bool,
            is_muted: bool,
        }
        if let Ok(rows) = sqlx::query_as::<_, UserChannelFlagRow>(
            "SELECT channel_id, is_hidden, is_pinned, is_muted FROM privchat_user_channels WHERE user_id = $1",
        )
        .bind(user_id as i64)
        .fetch_all(self.channel_repository.pool())
        .await
        {
            let mut hidden_channels = self.hidden_channels.write().await;
            let mut pinned_channels = self.pinned_channels.write().await;
            let mut muted_channels = self.muted_channels.write().await;
            for row in rows {
                let cid = row.channel_id as u64;
                if row.is_hidden {
                    hidden_channels.insert((user_id, cid));
                }
                if row.is_pinned {
                    pinned_channels.insert((user_id, cid));
                }
                if row.is_muted {
                    muted_channels.insert((user_id, cid));
                }
            }
        }
    }

    /// 清理无效会话
    pub async fn cleanup_invalid_channels(&self) -> usize {
        let mut channels = self.channels.write().await;
        let mut user_channels = self.user_channels.write().await;
        let mut direct_index = self.direct_channel_index.write().await;

        let mut removed_count = 0;
        let mut to_remove = Vec::new();

        // 找出需要删除的会话
        for (conv_id, conv) in channels.iter() {
            if conv.status == ChannelStatus::Deleted || conv.members.is_empty() {
                to_remove.push(conv_id.clone());
            }
        }

        // 删除会话
        for conv_id in to_remove {
            if let Some(conv) = channels.remove(&conv_id) {
                removed_count += 1;

                // 从用户索引中删除
                for user_id in conv.get_member_ids() {
                    if let Some(conv_list) = user_channels.get_mut(&user_id) {
                        conv_list.retain(|id| id != &conv_id);
                    }
                }

                // 从私聊索引中删除
                if conv.channel_type == ChannelType::Direct {
                    let member_ids = conv.get_member_ids();
                    if member_ids.len() == 2 {
                        let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                        direct_index.remove(&key);
                    }
                }
            }
        }

        if removed_count > 0 {
            info!("Cleaned up {} invalid channels", removed_count);
        }

        removed_count
    }

    /// 置顶会话
    pub async fn pin_channel(&self, user_id: u64, channel_id: u64, pinned: bool) -> Result<bool> {
        let key = (user_id, channel_id);
        let mut pinned_channels = self.pinned_channels.write().await;

        if pinned {
            pinned_channels.insert(key);
            info!("User {} pinned channel {}", user_id, channel_id);
        } else {
            pinned_channels.remove(&key);
            info!("User {} unpinned channel {}", user_id, channel_id);
        }

        self.persist_user_channel_flags(user_id, channel_id, Some(pinned), None)
            .await?;

        Ok(true)
    }

    /// 检查会话是否置顶
    pub async fn is_channel_pinned(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let pinned_channels = self.pinned_channels.read().await;
        pinned_channels.contains(&key)
    }

    /// 设置会话免打扰
    pub async fn mute_channel(&self, user_id: u64, channel_id: u64, muted: bool) -> Result<()> {
        let key = (user_id, channel_id);
        let mut muted_channels = self.muted_channels.write().await;

        if muted {
            muted_channels.insert(key);
            info!("User {} muted channel {}", user_id, channel_id);
        } else {
            muted_channels.remove(&key);
            info!("User {} unmuted channel {}", user_id, channel_id);
        }

        self.persist_user_channel_flags(user_id, channel_id, None, Some(muted))
            .await?;

        Ok(())
    }

    /// 检查会话是否免打扰
    pub async fn is_channel_muted(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let muted_channels = self.muted_channels.read().await;
        muted_channels.contains(&key)
    }

    /// 隐藏频道（不显示在会话列表中）
    pub async fn hide_channel(&self, user_id: u64, channel_id: u64, hidden: bool) -> Result<()> {
        let key = (user_id, channel_id);

        // 持久化到数据库
        sqlx::query(
            r#"
            INSERT INTO privchat_user_channels (user_id, channel_id, is_hidden, updated_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (user_id, channel_id) DO UPDATE SET
                is_hidden = EXCLUDED.is_hidden,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(user_id as i64)
        .bind(channel_id as i64)
        .bind(hidden)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(self.channel_repository.pool())
        .await
        .map_err(|e| ServerError::Database(format!("hide_channel DB update failed: {}", e)))?;

        // 同步更新内存缓存
        let mut hidden_channels = self.hidden_channels.write().await;
        if hidden {
            hidden_channels.insert(key);
            info!("User {} hidden channel {} (persisted)", user_id, channel_id);
        } else {
            hidden_channels.remove(&key);
            info!(
                "User {} unhidden channel {} (persisted)",
                user_id, channel_id
            );
        }

        Ok(())
    }

    /// 检查频道是否隐藏
    pub async fn is_channel_hidden(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let hidden_channels = self.hidden_channels.read().await;
        hidden_channels.contains(&key)
    }

    /// 更新最后消息预览缓存
    pub async fn update_last_message_preview(&self, channel_id: u64, preview: LastMessagePreview) {
        let mut cache = self.last_message_cache.write().await;
        cache.insert(channel_id, preview);
    }

    /// 获取最后消息预览
    pub async fn get_last_message_preview(&self, channel_id: u64) -> Option<LastMessagePreview> {
        let cache = self.last_message_cache.read().await;
        cache.get(&channel_id).cloned()
    }

    /// 获取增强的用户会话列表（包含预览、未读、置顶等信息）
    pub async fn get_user_channels_enhanced(&self, user_id: u64) -> EnhancedChannelListResponse {
        self.ensure_user_channels_loaded(user_id).await;
        let channel_ids = {
            let user_channels = self.user_channels.read().await;
            user_channels.get(&user_id).cloned().unwrap_or_default()
        };

        let channels_lock = self.channels.read().await;
        let pinned_lock = self.pinned_channels.read().await;
        let muted_lock = self.muted_channels.read().await;
        let hidden_lock = self.hidden_channels.read().await;
        let cache_lock = self.last_message_cache.read().await;

        let mut items = Vec::new();

        for conv_id in channel_ids {
            // 过滤隐藏的频道
            if hidden_lock.contains(&(user_id, conv_id)) {
                continue;
            }

            if let Some(conv) = channels_lock.get(&conv_id) {
                if !conv.is_active() {
                    continue;
                }

                // 检查置顶状态
                let is_pinned = pinned_lock.contains(&(user_id, conv_id));

                // 检查免打扰状态
                let is_muted = muted_lock.contains(&(user_id, conv_id));

                // 获取最后消息预览
                let last_message = cache_lock.get(&conv_id).cloned();

                // 计算未读消息数（简化版：使用 last_message_id 和 user_read_position）
                let unread_count = if let Some(member) = conv.members.get(&user_id) {
                    // TODO: 实现基于消息序号的未读数计算
                    if conv.last_message_id.is_some() && member.last_read_message_id.is_none() {
                        1 // 简化处理：如果有新消息且未读，返回 1
                    } else {
                        0
                    }
                } else {
                    0
                };

                items.push(EnhancedChannelItem {
                    channel: conv.clone(),
                    last_message,
                    unread_count,
                    is_pinned,
                    is_muted,
                });
            }
        }

        drop(channels_lock);
        drop(pinned_lock);
        drop(muted_lock);
        drop(cache_lock);

        // 排序：置顶的在前，然后按最后消息时间排序
        items.sort_by(|a, b| {
            match (a.is_pinned, b.is_pinned) {
                (true, false) => std::cmp::Ordering::Less, // a 置顶，b 不置顶，a 在前
                (false, true) => std::cmp::Ordering::Greater, // b 置顶，a 不置顶，b 在前
                _ => {
                    // 都置顶或都不置顶，按最后消息时间排序
                    b.channel.last_message_at.cmp(&a.channel.last_message_at)
                }
            }
        });

        EnhancedChannelListResponse {
            total: items.len(),
            has_more: false,
            channels: items,
        }
    }

    // ============================================================================
    // 兼容方法：为了兼容原 channel_service 的 API
    // ============================================================================

    /// 获取或创建私聊会话（RPC channel/direct/get_or_create 使用）
    /// 返回 (channel_id, 是否本次新创建)。source/source_id 与添加好友规范一致。
    pub async fn get_or_create_direct_channel(
        &self,
        user_id: u64,
        target_user_id: u64,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<(u64, bool)> {
        let (channel, created) = self
            .channel_repository
            .create_or_get_direct_channel(user_id, target_user_id, source, source_id)
            .await
            .map_err(|e| ServerError::Database(format!("get_or_create_direct_channel: {}", e)))?;
        let channel_id = channel.id;
        if created {
            self.ensure_user_channel_rows(channel_id, &[user_id, target_user_id])
                .await?;
            if let Err(e) = self
                .create_private_chat_with_id(user_id, target_user_id, channel_id)
                .await
            {
                tracing::warn!(
                    "⚠️ 同步私聊频道到缓存失败: {}，channel_id={}",
                    e,
                    channel_id
                );
            }
        } else if self.get_channel_opt(channel_id).await.is_none() {
            self.ensure_user_channel_rows(channel_id, &[user_id, target_user_id])
                .await?;
            // 已存在但缓存没有，从 DB 加载到缓存
            if let Err(e) = self
                .create_private_chat_with_id(user_id, target_user_id, channel_id)
                .await
            {
                tracing::warn!(
                    "⚠️ 加载已存在私聊到缓存失败: {}，channel_id={}",
                    e,
                    channel_id
                );
            }
        } else {
            self.ensure_user_channel_rows(channel_id, &[user_id, target_user_id])
                .await?;
        }

        // DB hydration and already-cached paths must converge on the same direct lookup index.
        self.direct_channel_index
            .write()
            .await
            .insert(Self::make_direct_key(user_id, target_user_id), channel_id);

        // 如果该频道之前被用户隐藏过，自动取消隐藏
        if self.is_channel_hidden(user_id, channel_id).await {
            if let Err(e) = self.hide_channel(user_id, channel_id, false).await {
                tracing::warn!(
                    "⚠️ 自动取消隐藏频道失败: user={} channel={} error={}",
                    user_id,
                    channel_id,
                    e
                );
            }
        }

        Ok((channel_id, created))
    }

    /// 创建私聊频道（使用指定 ID）
    pub async fn create_private_chat_with_id(
        &self,
        user1: u64,
        user2: u64,
        channel_id: u64,
    ) -> Result<()> {
        let direct_key = Self::make_direct_key(user1, user2);

        // 检查会话是否已存在
        if self.get_channel_opt(channel_id).await.is_some() {
            self.direct_channel_index
                .write()
                .await
                .insert(direct_key, channel_id);
            return Ok(()); // 已存在，直接返回成功
        }

        // 创建私聊会话
        let channel = Channel::new_direct(channel_id, user1, user2);

        // 保存到内存缓存
        {
            let mut channels = self.channels.write().await;
            channels.insert(channel_id, channel.clone());
        }

        // 更新用户会话索引
        self.update_user_channel_index(&channel).await;
        self.direct_channel_index
            .write()
            .await
            .insert(direct_key, channel_id);

        Ok(())
    }

    /// 创建群聊频道（使用指定 ID）
    pub async fn create_group_chat_with_id(
        &self,
        owner_id: u64,
        name: String,
        channel_id: u64,
    ) -> Result<()> {
        // 检查会话是否已存在
        if self.get_channel_opt(channel_id).await.is_some() {
            return Ok(()); // 已存在，直接返回成功
        }

        // 创建群聊会话
        let channel = Channel::new_group(channel_id, owner_id, Some(name));

        // 保存到内存缓存
        {
            let mut channels = self.channels.write().await;
            channels.insert(channel_id, channel.clone());
        }

        // 更新用户会话索引
        self.update_user_channel_index(&channel).await;

        Ok(())
    }

    /// 获取频道成员列表
    /// 判断 user 是否是 channel 成员（附件访问授权用，ATTACHMENT_ENCRYPTION_SPEC §授权）。
    /// 1.0 复用 get_channel_members 遍历；TODO: 改 SQL `EXISTS` 查询避免拉全量。
    pub async fn is_channel_member(&self, channel_id: u64, user_id: u64) -> Result<bool> {
        let members = self.get_channel_members(&channel_id).await?;
        Ok(members.iter().any(|m| m.user_id == user_id))
    }

    pub async fn get_channel_members(
        &self,
        channel_id: &u64,
    ) -> Result<Vec<crate::model::ChannelMember>> {
        if let Some(channel) = self.get_channel_opt(*channel_id).await {
            Ok(channel.members.values().cloned().collect())
        } else {
            // 如果内存中没有，从数据库查询参与者
            let participants = self
                .get_channel_participants(*channel_id)
                .await
                .map_err(|e| ServerError::Database(format!("查询参与者失败: {}", e)))?;
            Ok(participants
                .into_iter()
                .map(|p| p.to_channel_member())
                .collect())
        }
    }

    /// 添加成员到群组
    pub async fn add_member_to_group(&self, group_id: u64, user_id: u64) -> Result<()> {
        self.join_channel(group_id, user_id, None)
            .await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("添加成员失败: {}", e)))
    }

    /// 设置成员角色
    pub async fn set_member_role(
        &self,
        channel_id: &u64,
        user_id: &u64,
        role: crate::model::channel::MemberRole,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            channel
                .update_member_role(user_id, role)
                .map_err(|e| ServerError::Validation(e))?;
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置成员禁言状态
    pub async fn set_member_muted(
        &self,
        channel_id: &u64,
        user_id: &u64,
        muted: bool,
        mute_until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        // 存储约定：mute_until NULL = 未禁言；「永久」必须编码为远期时间戳
        // （RPC 层用 now+100 年）。muted=true + None 会被存成 NULL、跨重启即失效——
        // 拦下未来新调用方误用。
        if muted && mute_until.is_none() {
            warn!(
                "set_member_muted(muted=true, mute_until=None) 会退化为未禁言，\
                 永久禁言请传远期时间戳: channel={} user={}",
                channel_id, user_id
            );
        }
        let mute_until_ms = if muted {
            mute_until.map(|ts| ts.timestamp_millis())
        } else {
            None
        };
        sqlx::query(
            r#"
            UPDATE privchat_group_members
            SET mute_until = $3,
                updated_at = $4
            WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL
            "#,
        )
        .bind(*channel_id as i64)
        .bind(*user_id as i64)
        .bind(mute_until_ms)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("更新群成员禁言状态失败: {}", e)))?;

        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(member) = channel.members.get_mut(user_id) {
                member.is_muted = muted;
                member.mute_until = if muted { mute_until } else { None };
                Ok(())
            } else {
                Err(ServerError::NotFound(format!(
                    "Member not found in channel: {}",
                    channel_id
                )))
            }
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 离开频道（兼容方法，接受引用参数）
    pub async fn leave_channel_ref(&self, channel_id: &u64, user_id: &u64) -> Result<()> {
        self.leave_channel(*channel_id, *user_id)
            .await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("离开频道失败: {}", e)))
    }

    /// 加入频道（兼容方法，接受引用参数）
    pub async fn join_channel_ref(
        &self,
        channel_id: &u64,
        user_id: u64,
        role: crate::model::channel::MemberRole,
    ) -> Result<()> {
        self.join_channel(*channel_id, user_id, Some(role))
            .await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("加入频道失败: {}", e)))
    }

    /// 获取用户未读计数
    pub async fn get_user_unread_count(&self, user_id: &u64) -> Result<u32> {
        let user_channels = self.user_channels.read().await;
        let channel_ids = user_channels.get(user_id).cloned().unwrap_or_default();
        drop(user_channels);

        let channels = self.channels.read().await;
        let mut total_unread = 0u32;

        for channel_id in channel_ids {
            if let Some(channel) = channels.get(&channel_id) {
                if let Some(member) = channel.members.get(user_id) {
                    // TODO: 实现基于消息序号的未读数计算
                    if channel.last_message_id.is_some() && member.last_read_message_id.is_none() {
                        total_unread += 1;
                    }
                }
            }
        }

        Ok(total_unread)
    }

    /// 更新频道元数据
    pub async fn update_channel_metadata(
        &self,
        channel_id: &u64,
        name: Option<String>,
        description: Option<String>,
        avatar_url: Option<String>,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(name) = name {
                channel.metadata.name = Some(name);
            }
            if let Some(description) = description {
                channel.metadata.description = Some(description);
            }
            if let Some(avatar_url) = avatar_url {
                channel.metadata.avatar_url = Some(avatar_url);
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置频道公告
    pub async fn set_channel_announcement(
        &self,
        channel_id: &u64,
        announcement: Option<String>,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            channel.metadata.announcement = announcement;
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置频道全员禁言
    pub async fn set_channel_all_muted(&self, channel_id: &u64, muted: bool) -> Result<()> {
        // 存在性校验（内存 miss 时顺带触发 hydration）
        if self.get_channel_opt(*channel_id).await.is_none() {
            return Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )));
        }
        // P1-16：先落库（真源）。send 校验与重启恢复都读 privchat_groups.all_muted，
        // 此前只写内存导致 mute-all 跨重启失效。channel_id == group_id（005 id 统一）。
        self.update_group_policy(*channel_id, None, None, None, None, Some(muted))
            .await?;
        // 再刷内存缓存
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.is_muted = muted;
            }
            channel.updated_at = chrono::Utc::now();
        }
        Ok(())
    }

    /// 设置频道加群审批
    pub async fn set_channel_join_approval(
        &self,
        channel_id: &u64,
        require_approval: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.require_approval = require_approval;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置频道成员邀请权限
    pub async fn set_channel_member_invite(
        &self,
        channel_id: &u64,
        allow_invite: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.allow_member_invite = allow_invite;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置"群成员之间是否允许私自加好友"（群业务策略，P0-5）。
    ///
    /// 注意：与现有 set_channel_* 群设置一致，权威值维护在内存频道缓存的 ChannelSettings 上。
    /// 持久化到 privchat_groups.allow_member_add_friend 列为后续硬化项（需 DB 在环）。
    pub async fn set_channel_allow_member_add_friend(
        &self,
        channel_id: &u64,
        allow: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.allow_member_add_friend = allow;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置"群是否允许被搜索发现"（群业务策略，P0-4）。
    pub async fn set_channel_allow_search(&self, channel_id: &u64, allow: bool) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.allow_search = allow;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 设置"群加入策略"（群业务策略，P0-4）：0=不允许 1=需审核 2=直接加入。
    pub async fn set_channel_join_policy(&self, channel_id: &u64, policy: u8) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.join_policy = policy;
                // 与既有 require_approval 语义保持一致：policy==1 视为需审核。
                settings.require_approval = policy == 1;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 从 DB(`privchat_groups`) 读取群业务策略（source-of-truth）。
    ///
    /// 用运行时 `sqlx::query_as`，不依赖编译期 DB。群不存在返回 `Ok(None)`。
    pub async fn get_group_policy(
        &self,
        group_id: u64,
    ) -> Result<Option<crate::model::channel::GroupPolicy>> {
        let row = sqlx::query_as::<_, (bool, i16, bool, bool, bool)>(
            "SELECT allow_search, join_policy, allow_member_invite, allow_member_add_friend, all_muted \
             FROM privchat_groups WHERE group_id = $1",
        )
        .bind(group_id as i64)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群设置失败: {}", e)))?;

        Ok(row.map(
            |(
                allow_search,
                join_policy,
                allow_member_invite,
                allow_member_add_friend,
                all_muted,
            )| {
                crate::model::channel::GroupPolicy {
                    allow_search,
                    join_policy,
                    allow_member_invite,
                    allow_member_add_friend,
                    all_muted,
                }
            },
        ))
    }

    /// 将群业务策略落库到 `privchat_groups`（DB 为真源）。
    ///
    /// 各字段 `None` 表示不修改（`COALESCE` 保留原值）。用运行时 `sqlx::query`，不依赖编译期 DB。
    pub async fn update_group_policy(
        &self,
        group_id: u64,
        allow_search: Option<bool>,
        join_policy: Option<i16>,
        allow_member_invite: Option<bool>,
        allow_member_add_friend: Option<bool>,
        all_muted: Option<bool>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE privchat_groups SET \
             allow_search = COALESCE($2, allow_search), \
             join_policy = COALESCE($3, join_policy), \
             allow_member_invite = COALESCE($4, allow_member_invite), \
             allow_member_add_friend = COALESCE($5, allow_member_add_friend), \
             all_muted = COALESCE($6, all_muted), \
             updated_at = $7 \
             WHERE group_id = $1",
        )
        .bind(group_id as i64)
        .bind(allow_search)
        .bind(join_policy)
        .bind(allow_member_invite)
        .bind(allow_member_add_friend)
        .bind(all_muted)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("更新群设置失败: {}", e)))?;
        Ok(())
    }

    /// 设置频道最大成员数
    pub async fn set_channel_max_members(&self, channel_id: &u64, max_members: u32) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.max_members = Some(max_members);
            }
            channel.metadata.max_members = Some(max_members as usize);
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!(
                "Channel not found: {}",
                channel_id
            )))
        }
    }

    /// 标记消息已读（兼容方法）
    pub async fn mark_as_read(
        &self,
        user_id: &u64,
        channel_id: &u64,
        message_id: u64,
    ) -> Result<()> {
        self.mark_message_read(*channel_id, *user_id, message_id)
            .await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("标记已读失败: {}", e)))
    }

    /// 获取用户频道视图
    pub async fn get_user_channel(
        &self,
        user_id: &u64,
        channel_id: &u64,
    ) -> Result<crate::model::channel::UserChannelView> {
        // 先获取频道
        let channel = self.get_channel(channel_id).await?;

        // 检查用户是否在频道中
        if !channel.members.contains_key(user_id) {
            return Err(ServerError::NotFound(format!(
                "User {} not in channel {}",
                user_id, channel_id
            )));
        }

        // 获取或创建 UserChannelView
        // 这里简化实现，实际应该从数据库查询 user_channel 表
        let member = channel.members.get(user_id).unwrap();
        Ok(crate::model::channel::UserChannelView {
            user_id: *user_id,
            channel_id: *channel_id,
            last_read_message_id: member.last_read_message_id,
            is_muted: member.is_muted,
            is_pinned: false, // TODO: 从数据库查询
            unread_count: 0,  // TODO: 计算未读数
            remark: None,
            custom_title: None,
            last_viewed_at: member.last_active_at,
            created_at: channel.created_at,
            updated_at: channel.updated_at,
            is_hidden: false,
            sort_weight: 0,
        })
    }
}

fn take_sync_page<T>(rows: Vec<T>, limit: usize) -> (Vec<T>, bool) {
    let has_more = rows.len() > limit;
    let page = rows.into_iter().take(limit).collect();
    (page, has_more)
}

fn next_version_from_page<T, F>(page: &[T], since_version: u64, version_of: F) -> u64
where
    F: Fn(&T) -> u64,
{
    page.last().map(version_of).unwrap_or(since_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::channel::ChannelType;
    use crate::repository::PgChannelRepository;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Row;
    use std::sync::Arc;

    #[test]
    fn take_sync_page_reports_has_more_only_when_over_limit() {
        let (page, has_more) = take_sync_page(vec![1, 2, 3], 2);
        assert_eq!(page, vec![1, 2]);
        assert!(has_more);

        let (page, has_more) = take_sync_page(vec![1, 2], 2);
        assert_eq!(page, vec![1, 2]);
        assert!(!has_more);
    }

    #[test]
    fn next_version_from_page_uses_last_row_or_since_version() {
        let page = vec![3_u64, 7_u64, 9_u64];
        assert_eq!(next_version_from_page(&page, 2, |v| *v), 9);
        assert_eq!(next_version_from_page::<u64, _>(&[], 12, |v| *v), 12);
    }

    /// P1-15：channels 内存 cache 超 cap 时按最旧 updated_at 逐出；未超限 no-op。
    /// 纯内存行为，直接向私有 map 注入条目（同模块可见）。
    #[tokio::test]
    async fn evict_stale_memory_drops_oldest_channels() {
        // lazy pool 不建立连接；本测试只碰内存 map。
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("lazy pool");
        let service =
            ChannelService::new_with_repository(Arc::new(PgChannelRepository::new(Arc::new(pool))));
        {
            let mut channels = service.channels.write().await;
            for (i, id) in [901u64, 902, 903].iter().enumerate() {
                let mut c = crate::model::channel::Channel::new_direct(*id, 1, 2);
                c.updated_at = chrono::Utc::now() - chrono::Duration::minutes(10 - i as i64);
                channels.insert(*id, c);
            }
        }
        // cap=3 → no-op
        assert_eq!(service.evict_stale_memory(3, 10).await, (0, 0));
        // cap=1 → 逐出最旧的 901、902
        let (ev_ch, _) = service.evict_stale_memory(1, 10).await;
        assert_eq!(ev_ch, 2);
        let channels = service.channels.read().await;
        assert!(channels.get(&901).is_none());
        assert!(channels.get(&902).is_none());
        assert!(channels.get(&903).is_some());
    }

    async fn open_test_service() -> Option<ChannelService> {
        let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .ok()?;
        let repo = Arc::new(PgChannelRepository::new(Arc::new(pool)));
        Some(ChannelService::new_with_repository(repo))
    }

    async fn cleanup_channel(service: &ChannelService, channel_id: u64) {
        let _ = sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = $1")
            .bind(channel_id as i64)
            .execute(service.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
            .bind(channel_id as i64)
            .execute(service.pool())
            .await;
    }

    async fn ensure_user(service: &ChannelService, user_id: u64, username: &str) {
        // QR_CODE_SPEC v1.3：privchat_users.qr_key NOT NULL，测试 helper 必须填值。
        let qr_key = generate_qr_key();
        let _ = sqlx::query(
            r#"
            INSERT INTO privchat_users (user_id, username, display_name, qr_key)
            VALUES ($1, $2, $2, $3)
            ON CONFLICT (user_id) DO UPDATE
            SET username = EXCLUDED.username,
                display_name = EXCLUDED.display_name
            "#,
        )
        .bind(user_id as i64)
        .bind(username)
        .bind(&qr_key)
        .execute(service.pool())
        .await
        .expect("ensure user");
    }

    async fn ensure_group(service: &ChannelService, group_id: u64, owner_id: u64, name: &str) {
        // QR_CODE_SPEC v1.3：测试 helper 也必须填 qr_key（NOT NULL 约束）。
        // 单一 generate_qr_key()，碰撞重试由整个测试运行外部包覆即可。
        let qr_key = generate_qr_key();
        let _ = sqlx::query(
            r#"
            INSERT INTO privchat_groups (group_id, owner_id, name, description, qr_key)
            VALUES ($1, $2, $3, 'channel-service-test', $4)
            ON CONFLICT (group_id) DO UPDATE
            SET owner_id = EXCLUDED.owner_id,
                name = EXCLUDED.name
            "#,
        )
        .bind(group_id as i64)
        .bind(owner_id as i64)
        .bind(name)
        .bind(&qr_key)
        .execute(service.pool())
        .await
        .expect("ensure group");
    }

    async fn cleanup_user(service: &ChannelService, user_id: u64) {
        let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
            .bind(user_id as i64)
            .execute(service.pool())
            .await;
    }

    async fn cleanup_group(service: &ChannelService, group_id: u64) {
        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
            .bind(group_id as i64)
            .execute(service.pool())
            .await;
    }

    #[tokio::test]
    async fn test_create_direct_channel() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip test_create_direct_channel: DATABASE_URL not configured");
            return;
        };
        let creator_id = 910001_u64;
        let peer_id = 910002_u64;
        ensure_user(&service, creator_id, "channel_test_direct_creator").await;
        ensure_user(&service, peer_id, "channel_test_direct_peer").await;

        let request = CreateChannelRequest {
            channel_type: ChannelType::Direct,
            name: None,
            description: None,
            member_ids: vec![peer_id],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel(creator_id, request).await.unwrap();
        assert!(response.success, "{:?}", response.error);
        assert_eq!(response.channel.channel_type, ChannelType::Direct);
        assert_eq!(response.channel.members.len(), 2);
        cleanup_channel(&service, response.channel.id).await;
        cleanup_user(&service, peer_id).await;
        cleanup_user(&service, creator_id).await;
    }

    #[tokio::test]
    async fn test_create_group_channel() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip test_create_group_channel: DATABASE_URL not configured");
            return;
        };
        let creator_id = 920001_u64;
        let initial_members = [920002_u64, 920003_u64];
        ensure_user(&service, creator_id, "channel_test_group_creator").await;
        for (index, member_id) in initial_members.iter().enumerate() {
            ensure_user(
                &service,
                *member_id,
                &format!("channel_test_group_member_{index}"),
            )
            .await;
        }

        let request = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("Test Group".to_string()),
            description: Some("A test group".to_string()),
            member_ids: initial_members.to_vec(),
            is_public: Some(false),
            max_members: Some(10),
        };

        let response = service.create_channel(creator_id, request).await.unwrap();
        assert!(response.success, "{:?}", response.error);
        assert_eq!(response.channel.channel_type, ChannelType::Group);
        assert_eq!(response.channel.members.len(), 3); // creator + 2 members
        cleanup_channel(&service, response.channel.id).await;
        cleanup_group(&service, response.channel.id).await;
        for member_id in initial_members {
            cleanup_user(&service, member_id).await;
        }
        cleanup_user(&service, creator_id).await;
    }

    #[tokio::test]
    async fn test_find_direct_channel() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip test_find_direct_channel: DATABASE_URL not configured");
            return;
        };
        let creator_id = 930001_u64;
        let peer_id = 930002_u64;
        ensure_user(&service, creator_id, "channel_test_find_creator").await;
        ensure_user(&service, peer_id, "channel_test_find_peer").await;

        let request = CreateChannelRequest {
            channel_type: ChannelType::Direct,
            name: None,
            description: None,
            member_ids: vec![peer_id],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel(creator_id, request).await.unwrap();
        assert!(response.success, "{:?}", response.error);

        let conv_id = response.channel.id.clone();
        let found_id = service.find_direct_channel(creator_id, peer_id).await;
        assert_eq!(found_id, Some(conv_id.clone()));

        let found_id2 = service.find_direct_channel(peer_id, creator_id).await;
        assert_eq!(found_id2, Some(conv_id));
        cleanup_channel(&service, response.channel.id).await;
        cleanup_user(&service, peer_id).await;
        cleanup_user(&service, creator_id).await;
    }

    #[tokio::test]
    async fn test_join_and_leave_channel() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip test_join_and_leave_channel: DATABASE_URL not configured");
            return;
        };
        let creator_id = 940001_u64;
        let joiner_id = 940002_u64;
        ensure_user(&service, creator_id, "channel_test_join_creator").await;
        ensure_user(&service, joiner_id, "channel_test_join_member").await;

        let request = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("Test Group".to_string()),
            description: None,
            member_ids: vec![],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel(creator_id, request).await.unwrap();
        let conv_id = response.channel.id;

        // 加入会话
        let joined = service
            .join_channel(conv_id, joiner_id, None)
            .await
            .unwrap();
        assert!(joined);

        // 检查成员
        let conv = service.get_channel(&conv_id).await.unwrap();
        assert_eq!(conv.members.len(), 2);

        // 离开会话
        let left = service.leave_channel(conv_id, joiner_id).await.unwrap();
        assert!(left);

        // 检查成员
        let conv = service.get_channel(&conv_id).await.unwrap();
        assert_eq!(conv.members.len(), 1);
        cleanup_channel(&service, conv_id).await;
        cleanup_group(&service, conv_id).await;
        cleanup_user(&service, joiner_id).await;
        cleanup_user(&service, creator_id).await;
    }

    #[tokio::test]
    async fn channel_sync_reads_persisted_unread_count_after_increment_and_clear() {
        let Some(service) = open_test_service().await else {
            eprintln!(
                "skip channel_sync_reads_persisted_unread_count_after_increment_and_clear: DATABASE_URL not configured"
            );
            return;
        };
        let owner_id = 9_170_001_u64;
        // 005 迁移统一 channel/group id 空间后，群会话必须 channel_id == group_id。
        let group_id = 9_170_101_u64;
        let channel_id = group_id;

        cleanup_channel(&service, channel_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner_id).await;

        ensure_user(&service, owner_id, "channel_sync_unread_owner").await;
        ensure_group(&service, group_id, owner_id, "channel-sync-unread-group").await;
        // DB 编码 channel_type：0=Direct、1=Group（wire 编码才是 1/2），
        // 且 privchat_channels_check 要求群会话 group_id 非空、direct_user 为空。
        sqlx::query(
            r#"
            INSERT INTO privchat_channels (channel_id, channel_type, group_id)
            VALUES ($1, 1, $2)
            ON CONFLICT (channel_id) DO UPDATE
            SET group_id = EXCLUDED.group_id
            "#,
        )
        .bind(channel_id as i64)
        .bind(group_id as i64)
        .execute(service.pool())
        .await
        .expect("ensure channel");
        service
            .ensure_user_channel_rows(channel_id, &[owner_id])
            .await
            .expect("ensure user channel row");

        let before = sqlx::query(
            "SELECT unread_count, sync_version FROM privchat_user_channels WHERE user_id = $1 AND channel_id = $2",
        )
        .bind(owner_id as i64)
        .bind(channel_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("fetch before row");
        assert_eq!(before.get::<i32, _>("unread_count"), 0);
        let before_sync_version = before.get::<i64, _>("sync_version");

        service
            .increment_user_channel_unread(channel_id, &[owner_id], 1)
            .await
            .expect("increment unread");

        let response = service
            .sync_entities_page_for_channels(owner_id, None, None, 20)
            .await
            .expect("sync entities after increment");
        let item = response
            .items
            .iter()
            .find(|item| item.entity_id == channel_id.to_string())
            .expect("channel sync item after increment");
        let unread_after_increment = item
            .payload
            .as_ref()
            .and_then(|payload| payload.get("unread_count"))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        assert_eq!(unread_after_increment, 1);

        service
            .clear_user_channel_unread(owner_id, channel_id)
            .await
            .expect("clear unread");

        let after = sqlx::query(
            "SELECT unread_count, sync_version FROM privchat_user_channels WHERE user_id = $1 AND channel_id = $2",
        )
        .bind(owner_id as i64)
        .bind(channel_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("fetch after row");
        assert_eq!(after.get::<i32, _>("unread_count"), 0);
        assert!(
            after.get::<i64, _>("sync_version") > before_sync_version,
            "user_channel sync_version should advance after unread persistence changes"
        );

        let response = service
            .sync_entities_page_for_channels(owner_id, None, None, 20)
            .await
            .expect("sync entities after clear");
        let item = response
            .items
            .iter()
            .find(|item| item.entity_id == channel_id.to_string())
            .expect("channel sync item after clear");
        let unread_after_clear = item
            .payload
            .as_ref()
            .and_then(|payload| payload.get("unread_count"))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        assert_eq!(unread_after_clear, 0);

        // P0-12 since_version 下推：since = 变更前版本 → 命中该 channel；
        // since = 当前最新版本 → 增量为空（同一查询在 DB 层过滤，不再全量拉取）。
        let latest_version = sqlx::query(
            "SELECT GREATEST(uc.sync_version, c.sync_version) AS v
             FROM privchat_user_channels uc
             JOIN privchat_channels c ON c.channel_id = uc.channel_id
             WHERE uc.user_id = $1 AND uc.channel_id = $2",
        )
        .bind(owner_id as i64)
        .bind(channel_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("fetch latest effective version")
        .get::<i64, _>("v");

        let incremental = service
            .sync_entities_page_for_channels(owner_id, Some(before_sync_version as u64), None, 20)
            .await
            .expect("incremental sync since before_version");
        assert!(
            incremental
                .items
                .iter()
                .any(|item| item.entity_id == channel_id.to_string()),
            "changed channel must appear when since_version predates the change"
        );

        let drained = service
            .sync_entities_page_for_channels(owner_id, Some(latest_version as u64), None, 20)
            .await
            .expect("incremental sync at latest version");
        assert!(
            !drained
                .items
                .iter()
                .any(|item| item.entity_id == channel_id.to_string()),
            "channel must not reappear when since_version is already at its latest version"
        );

        cleanup_channel(&service, channel_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner_id).await;
    }

    // =====================================================
    // 群成员事实源一致性测试（migration 016 + service 层修复）
    // =====================================================

    /// `privchat_groups.member_count` 必须等于 `group_members WHERE left_at IS NULL` 的真实数量。
    async fn assert_group_count_matches_members(service: &ChannelService, group_id: u64) {
        let cached: (i32,) = sqlx::query_as(
            "SELECT COALESCE(member_count, 0) FROM privchat_groups WHERE group_id = $1",
        )
        .bind(group_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("fetch group");
        let actual: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM privchat_group_members WHERE group_id = $1 AND left_at IS NULL",
        )
        .bind(group_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("count members");
        assert_eq!(
            cached.0 as i64, actual.0,
            "groups.member_count={} 与 active group_members={} 脱节",
            cached.0, actual.0
        );
    }

    async fn count_active_members(service: &ChannelService, group_id: u64) -> i64 {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM privchat_group_members WHERE group_id = $1 AND left_at IS NULL",
        )
        .bind(group_id as i64)
        .fetch_one(service.pool())
        .await
        .expect("count members");
        row.0
    }

    async fn fetch_member_role(
        service: &ChannelService,
        group_id: u64,
        user_id: u64,
    ) -> Option<i16> {
        sqlx::query_scalar(
            "SELECT role FROM privchat_group_members WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL",
        )
        .bind(group_id as i64)
        .bind(user_id as i64)
        .fetch_optional(service.pool())
        .await
        .expect("fetch role")
    }

    #[tokio::test]
    async fn create_group_inserts_owner_member() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip create_group_inserts_owner_member: DATABASE_URL not set");
            return;
        };
        let owner = 950001_u64;
        ensure_user(&service, owner, "g_consist_owner").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-1".into()),
            description: None,
            member_ids: vec![],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        assert!(resp.success, "create_channel failed: {:?}", resp.error);
        let group_id = resp.channel.id;

        assert_eq!(count_active_members(&service, group_id).await, 1);
        assert_eq!(fetch_member_role(&service, group_id, owner).await, Some(0));
        assert_group_count_matches_members(&service, group_id).await;

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
    }

    #[tokio::test]
    async fn create_group_member_count_matches_initial_member_ids() {
        let Some(service) = open_test_service().await else {
            eprintln!(
                "skip create_group_member_count_matches_initial_member_ids: DATABASE_URL not set"
            );
            return;
        };
        let owner = 950101_u64;
        let m1 = 950102_u64;
        let m2 = 950103_u64;
        ensure_user(&service, owner, "g_consist_o2").await;
        ensure_user(&service, m1, "g_consist_m1").await;
        ensure_user(&service, m2, "g_consist_m2").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-2".into()),
            description: None,
            member_ids: vec![m1, m2],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        assert!(resp.success);
        let group_id = resp.channel.id;

        assert_eq!(count_active_members(&service, group_id).await, 3);
        assert_eq!(fetch_member_role(&service, group_id, owner).await, Some(0));
        assert_eq!(fetch_member_role(&service, group_id, m1).await, Some(2));
        assert_eq!(fetch_member_role(&service, group_id, m2).await, Some(2));
        assert_group_count_matches_members(&service, group_id).await;

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
        cleanup_user(&service, m1).await;
        cleanup_user(&service, m2).await;
    }

    #[tokio::test]
    async fn join_channel_persists_member_to_db() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip join_channel_persists_member_to_db: DATABASE_URL not set");
            return;
        };
        let owner = 950201_u64;
        let joiner = 950202_u64;
        ensure_user(&service, owner, "g_consist_o3").await;
        ensure_user(&service, joiner, "g_consist_j1").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-3".into()),
            description: None,
            member_ids: vec![],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        let group_id = resp.channel.id;

        assert!(service.join_channel(group_id, joiner, None).await.unwrap());
        assert_eq!(count_active_members(&service, group_id).await, 2);
        assert_eq!(fetch_member_role(&service, group_id, joiner).await, Some(2));
        assert_group_count_matches_members(&service, group_id).await;

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
        cleanup_user(&service, joiner).await;
    }

    #[tokio::test]
    async fn leave_channel_marks_left_and_decrements_count() {
        let Some(service) = open_test_service().await else {
            eprintln!("skip leave_channel_marks_left_and_decrements_count: DATABASE_URL not set");
            return;
        };
        let owner = 950301_u64;
        let joiner = 950302_u64;
        ensure_user(&service, owner, "g_consist_o4").await;
        ensure_user(&service, joiner, "g_consist_j2").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-4".into()),
            description: None,
            member_ids: vec![joiner],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        let group_id = resp.channel.id;

        assert!(service.leave_channel(group_id, joiner).await.unwrap());
        assert_eq!(count_active_members(&service, group_id).await, 1);
        assert_group_count_matches_members(&service, group_id).await;

        let left_at: Option<i64> = sqlx::query_scalar(
            "SELECT left_at FROM privchat_group_members WHERE group_id = $1 AND user_id = $2",
        )
        .bind(group_id as i64)
        .bind(joiner as i64)
        .fetch_one(service.pool())
        .await
        .expect("fetch left_at");
        assert!(left_at.is_some(), "leaver 应该被标记为 left_at IS NOT NULL");

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
        cleanup_user(&service, joiner).await;
    }

    #[tokio::test]
    async fn admin_add_existing_active_member_does_not_double_count() {
        let Some(service) = open_test_service().await else {
            eprintln!(
                "skip admin_add_existing_active_member_does_not_double_count: DATABASE_URL not set"
            );
            return;
        };
        let owner = 950401_u64;
        ensure_user(&service, owner, "g_consist_o5").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-5".into()),
            description: None,
            member_ids: vec![],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        let group_id = resp.channel.id;

        assert_eq!(count_active_members(&service, group_id).await, 1);

        // owner 已是活跃成员，再 add 应当报 Validation 而非默默 +1
        let err = service.add_member_admin(group_id, owner).await.unwrap_err();
        assert!(
            matches!(err, ServerError::Validation(_)),
            "expected Validation, got {:?}",
            err
        );

        assert_eq!(count_active_members(&service, group_id).await, 1);
        assert_group_count_matches_members(&service, group_id).await;

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
    }

    #[tokio::test]
    async fn admin_add_revives_left_member_without_double_count() {
        let Some(service) = open_test_service().await else {
            eprintln!(
                "skip admin_add_revives_left_member_without_double_count: DATABASE_URL not set"
            );
            return;
        };
        let owner = 950501_u64;
        let other = 950502_u64;
        ensure_user(&service, owner, "g_consist_o6").await;
        ensure_user(&service, other, "g_consist_other").await;

        let req = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("consistency-6".into()),
            description: None,
            member_ids: vec![other],
            is_public: None,
            max_members: None,
        };
        let resp = service.create_channel(owner, req).await.unwrap();
        let group_id = resp.channel.id;
        assert_eq!(count_active_members(&service, group_id).await, 2);

        // 退群
        service.remove_member_admin(group_id, other).await.unwrap();
        assert_eq!(count_active_members(&service, group_id).await, 1);
        assert_group_count_matches_members(&service, group_id).await;

        // 复活：admin add 应当把 left_at 置 NULL，count 回到 2，行数仍为 2（一行复用，一行原 owner）
        service.add_member_admin(group_id, other).await.unwrap();
        assert_eq!(count_active_members(&service, group_id).await, 2);
        assert_group_count_matches_members(&service, group_id).await;

        let total_rows: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM privchat_group_members WHERE group_id = $1")
                .bind(group_id as i64)
                .fetch_one(service.pool())
                .await
                .expect("count rows");
        assert_eq!(total_rows.0, 2, "复活应该复用旧行而非插入第二条");

        cleanup_channel(&service, group_id).await;
        cleanup_group(&service, group_id).await;
        cleanup_user(&service, owner).await;
        cleanup_user(&service, other).await;
    }
}
