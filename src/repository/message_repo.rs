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

//! 消息仓库 - PostgreSQL 实现

use crate::error::DatabaseError;
use crate::model::message::Message;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

/// 消息仓库 trait
pub trait MessageRepository: Send + Sync {
    /// 根据ID查找消息
    async fn find_by_id(&self, message_id: u64) -> Result<Option<Message>, DatabaseError>;

    /// 创建消息
    async fn create(&self, message: &Message) -> Result<Message, DatabaseError>;

    /// 创建消息并带幂等键（RP-12：资金消息卡片注入 exactly-once）。
    /// 返回 `true` = 本次真正插入；`false` = dedup_key 已存在（未插入，调用方应跳过推送）。
    async fn create_with_dedup_key(
        &self,
        message: &Message,
        dedup_key: Option<&str>,
    ) -> Result<bool, DatabaseError>;

    /// 按 dedup_key 查已存在消息的 (message_id, created_at_ms)；无则 None。
    async fn find_message_id_by_dedup_key(
        &self,
        dedup_key: &str,
    ) -> Result<Option<(u64, i64)>, DatabaseError>;

    /// 更新消息
    async fn update(&self, message: &Message) -> Result<Message, DatabaseError>;

    /// 删除消息（软删除）
    async fn delete(&self, message_id: &Uuid) -> Result<(), DatabaseError>;

    /// 获取会话的消息列表（分页）
    async fn list_by_channel(
        &self,
        channel_id: u64,
        limit: i64,
        before_created_at: Option<i64>,
    ) -> Result<Vec<Message>, DatabaseError>;

    /// 根据 pts 获取消息
    async fn find_by_pts(
        &self,
        sender_id: &Uuid,
        pts: i64,
    ) -> Result<Option<Message>, DatabaseError>;

    /// 撤回消息（标记为已撤回，但保留内容）
    async fn revoke_message(
        &self,
        message_id: u64,
        revoker_id: u64,
    ) -> Result<Message, DatabaseError>;

    /// 搜索消息（管理 API）
    async fn search_messages(
        &self,
        keyword: &str,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        message_type: Option<i16>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DatabaseError>;
}

/// 消息仓库 (PostgreSQL 实现)
#[derive(Clone)]
pub struct PgMessageRepository {
    pool: Arc<PgPool>,
}

#[derive(Debug, Clone)]
pub struct AtomicMessageCommitRequest {
    pub message: Message,
    /// None = 客户端未提供幂等键（local_message_id=0），事务内不 claim dedup key。
    pub dedup_key: Option<String>,
    pub attachment_file_ids: Vec<u64>,
    pub channel_type: i16,
    pub commit_content: serde_json::Value,
    pub sender_username: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AtomicMessageCommitResult {
    pub message: Message,
    pub inserted: bool,
}

impl PgMessageRepository {
    /// 创建新的消息仓库
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 获取数据库连接池（用于管理 API）
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn message_from_row(row: MessageRow) -> Message {
        Message::from_db_row(
            row.message_id,
            row.channel_id,
            row.sender_id,
            row.pts,
            row.local_message_id.map(|n| n as u64),
            row.content,
            row.message_type,
            row.metadata
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            row.reply_to_message_id,
            row.created_at,
            row.updated_at,
            row.deleted,
            row.deleted_at,
            row.revoked,
            row.revoked_at,
            row.revoked_by,
        )
    }

    /// 按 dedup_key 查完整消息。
    pub async fn find_message_by_dedup_key(
        &self,
        dedup_key: &str,
    ) -> Result<Option<Message>, DatabaseError> {
        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT
                m.message_id, m.channel_id, m.sender_id, m.pts, m.local_message_id,
                m.content, m.message_type, m.metadata, m.reply_to_message_id,
                m.created_at, m.updated_at, m.deleted, m.deleted_at,
                m.revoked, m.revoked_at, m.revoked_by
            FROM privchat_message_dedup d
            JOIN privchat_messages m ON m.message_id = d.message_id
            WHERE d.dedup_key = $1
            ORDER BY m.created_at DESC
            LIMIT 1
            "#,
        )
        .bind(dedup_key)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query dedup message: {}", e)))?;

        Ok(row.map(Self::message_from_row))
    }

    /// 普通客户端消息的权威写入路径：在同一个 DB transaction 内完成
    /// dedup claim、pts 分配、消息落库、附件绑定和 commit log 写入。
    pub async fn create_message_and_commit_atomic(
        &self,
        request: AtomicMessageCommitRequest,
    ) -> Result<AtomicMessageCommitResult, DatabaseError> {
        let mut tx =
            self.pool.begin().await.map_err(|e| {
                DatabaseError::Database(format!("Failed to begin message tx: {}", e))
            })?;

        let created_at = request.message.created_at.timestamp_millis();
        // dedup_key=None（local_message_id=0）时不 claim：无幂等键的发送不做判重。
        if let Some(dedup_key) = request.dedup_key.as_deref() {
            let claim = sqlx::query(
                r#"
                INSERT INTO privchat_message_dedup (dedup_key, message_id, created_at)
                VALUES ($1, $2, $3)
                ON CONFLICT (dedup_key) DO NOTHING
                "#,
            )
            .bind(dedup_key)
            .bind(request.message.message_id as i64)
            .bind(created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to claim dedup_key: {}", e)))?;

            if claim.rows_affected() == 0 {
                let existing = sqlx::query_as::<_, MessageRow>(
                    r#"
                    SELECT
                        m.message_id, m.channel_id, m.sender_id, m.pts, m.local_message_id,
                        m.content, m.message_type, m.metadata, m.reply_to_message_id,
                        m.created_at, m.updated_at, m.deleted, m.deleted_at,
                        m.revoked, m.revoked_at, m.revoked_by
                    FROM privchat_message_dedup d
                    JOIN privchat_messages m ON m.message_id = d.message_id
                    WHERE d.dedup_key = $1
                    ORDER BY m.created_at DESC
                    LIMIT 1
                    "#,
                )
                .bind(dedup_key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| {
                    DatabaseError::Database(format!("Failed to fetch duplicate message: {}", e))
                })?;

                tx.rollback().await.ok();
                let message = existing.ok_or_else(|| {
                    DatabaseError::Database(format!(
                        "dedup_key exists but message is missing: {}",
                        dedup_key
                    ))
                })?;
                return Ok(AtomicMessageCommitResult {
                    message: Self::message_from_row(message),
                    inserted: false,
                });
            }
        }

        let now = chrono::Utc::now().timestamp_millis();
        let pts_row = sqlx::query(
            r#"
            INSERT INTO privchat_channel_pts
            (channel_id, current_pts, created_at, updated_at)
            VALUES ($1, 1, $2, $2)
            ON CONFLICT (channel_id) DO UPDATE
            SET current_pts = privchat_channel_pts.current_pts + 1,
                updated_at = $2
            RETURNING current_pts
            "#,
        )
        .bind(request.message.channel_id as i64)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to allocate pts: {}", e)))?;

        let pts = pts_row.get::<i64, _>("current_pts");
        let mut message = request.message;
        message.pts = Some(pts);

        let (
            message_id,
            channel_id,
            sender_id,
            pts,
            local_message_id,
            content,
            message_type,
            metadata,
            reply_to_message_id,
            created_at,
            updated_at,
            deleted,
            deleted_at,
            revoked,
            revoked_at,
            revoked_by,
        ) = message.to_db_values();

        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(sender_id)
        .bind(pts)
        .bind(local_message_id)
        .bind(message_type)
        .bind(content)
        .bind(&metadata_json)
        .bind(reply_to_message_id)
        .bind(created_at)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create message: {}", e)))?;

        for file_id in request.attachment_file_ids {
            // P1-19 归属守卫：file_id 来自客户端可控的 metadata，必须校验
            // ① 上传者就是发送者（不能引用他人的 file）
            // ② 未绑定到其它业务（不能把已绑定的附件重绑劫持；同消息重绑幂等放行）
            let updated = sqlx::query(
                r#"
                UPDATE privchat_file_uploads
                SET business_type = $1, business_id = $2
                WHERE file_id = $3
                  AND uploader_id = $4
                  AND (
                    business_type IS NULL
                    OR business_type = ''
                    OR (business_type = $1 AND business_id = $2)
                  )
                "#,
            )
            .bind("message")
            .bind(message.message_id.to_string())
            .bind(file_id as i64)
            .bind(message.sender_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                DatabaseError::Database(format!(
                    "Failed to bind attachment file_id={} to message_id={}: {}",
                    file_id, message.message_id, e
                ))
            })?;

            if updated.rows_affected() == 0 {
                return Err(DatabaseError::Database(format!(
                    "attachment file_id={} rejected while binding message_id={}: \
                     not found, not uploaded by sender {}, or already bound to another business",
                    file_id, message.message_id, message.sender_id
                )));
            }
        }

        sqlx::query(
            r#"
            INSERT INTO privchat_commit_log
            (pts, server_msg_id, local_message_id, channel_id, channel_type,
             message_type, content, server_timestamp, sender_id, sender_username, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(message.pts.unwrap_or(0))
        .bind(message.message_id as i64)
        .bind(message.local_message_id.map(|v| v as i64))
        .bind(message.channel_id as i64)
        .bind(request.channel_type)
        .bind(message.message_type.as_str())
        .bind(&request.commit_content)
        .bind(message.created_at.timestamp_millis())
        .bind(message.sender_id as i64)
        .bind(request.sender_username.as_deref())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to save commit: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to commit message tx: {}", e)))?;

        Ok(AtomicMessageCommitResult {
            message,
            inserted: true,
        })
    }

    /// 按 message_id 取 channel_id（附件访问授权用：file→message→channel）。
    /// 返回 None 表示消息不存在（授权方应据此拒绝）。
    pub async fn get_channel_id(&self, message_id: u64) -> Result<Option<u64>, DatabaseError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT channel_id FROM privchat_messages WHERE message_id = $1")
                .bind(message_id as i64)
                .fetch_optional(self.pool())
                .await
                .map_err(|e| DatabaseError::Database(format!("查询消息 channel_id 失败: {}", e)))?;
        Ok(row.map(|r| r.0 as u64))
    }

    // =====================================================
    // 管理 API 方法
    // =====================================================

    /// 获取消息列表（管理 API）
    pub async fn list_messages_admin(
        &self,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DatabaseError> {
        let offset = (page - 1) * page_size;

        let mut sql = String::from(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE 1=1
            "#,
        );

        let mut bind_count = 0;
        if channel_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }

        sql.push_str(" ORDER BY created_at DESC");
        bind_count += 1;
        sql.push_str(&format!(" LIMIT ${}", bind_count));
        bind_count += 1;
        sql.push_str(&format!(" OFFSET ${}", bind_count));

        let mut query_builder = sqlx::query_as::<_, MessageRow>(&sql);

        if let Some(ch_id) = channel_id {
            query_builder = query_builder.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            query_builder = query_builder.bind(uid as i64);
        }
        if let Some(st) = start_time {
            query_builder = query_builder.bind(st);
        }
        if let Some(et) = end_time {
            query_builder = query_builder.bind(et);
        }
        query_builder = query_builder.bind(page_size as i64);
        query_builder = query_builder.bind(offset as i64);

        let rows = query_builder
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("查询消息列表失败: {}", e)))?;

        // 统计总数
        let mut count_sql = String::from("SELECT COUNT(*) FROM privchat_messages WHERE 1=1");
        bind_count = 0;
        if channel_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }

        let mut count_query = sqlx::query_as::<_, (i64,)>(&count_sql);
        if let Some(ch_id) = channel_id {
            count_query = count_query.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            count_query = count_query.bind(uid as i64);
        }
        if let Some(st) = start_time {
            count_query = count_query.bind(st);
        }
        if let Some(et) = end_time {
            count_query = count_query.bind(et);
        }

        let total_result: (i64,) = count_query
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("统计消息数失败: {}", e)))?;

        let total = total_result.0 as u32;

        let message_list: Vec<serde_json::Value> = rows.into_iter().map(|r| {
            serde_json::json!({
                "message_id": r.message_id as u64,
                "channel_id": r.channel_id as u64,
                "sender_id": r.sender_id as u64,
                "pts": r.pts,
                "local_message_id": r.local_message_id.map(|n| n as u64),
                "content": r.content,
                "message_type": privchat_protocol::ContentMessageType::from_u32(r.message_type as u32).map(|t| t.as_str()).unwrap_or("text"),
                "metadata": r.metadata,
                "reply_to_message_id": r.reply_to_message_id.map(|id| id as u64),
                "created_at": r.created_at,
                "updated_at": r.updated_at,
                "deleted": r.deleted,
                "deleted_at": r.deleted_at,
                "revoked": r.revoked,
                "revoked_at": r.revoked_at,
                "revoked_by": r.revoked_by.map(|id| id as u64),
            })
        }).collect();

        Ok((message_list, total))
    }

    /// 获取消息详情（管理 API）
    pub async fn get_message_admin(
        &self,
        message_id: u64,
    ) -> Result<Option<serde_json::Value>, DatabaseError> {
        let message = self.find_by_id(message_id).await?;

        Ok(message.map(|m| {
            serde_json::json!({
                "message_id": m.message_id,
                "channel_id": m.channel_id,
                "sender_id": m.sender_id,
                "pts": m.pts,
                "local_message_id": m.local_message_id,
                "content": m.content,
                "message_type": m.message_type.as_u32(),
                "metadata": m.metadata,
                "reply_to_message_id": m.reply_to_message_id,
                "created_at": m.created_at.timestamp_millis(),
                "updated_at": m.updated_at.timestamp_millis(),
                "deleted": m.deleted,
                "deleted_at": m.deleted_at.map(|dt| dt.timestamp_millis()),
                "revoked": m.revoked,
                "revoked_at": m.revoked_at.map(|dt| dt.timestamp_millis()),
                "revoked_by": m.revoked_by,
            })
        }))
    }

    /// 获取消息统计（管理 API）
    pub async fn get_message_stats_admin(&self) -> Result<serde_json::Value, DatabaseError> {
        let now = chrono::Utc::now();
        let today_start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
        let today_start_ts: i64 =
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(today_start, chrono::Utc)
                .timestamp_millis();

        let week_start = today_start - chrono::Duration::days(7);
        let week_start_ts: i64 =
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(week_start, chrono::Utc)
                .timestamp_millis();

        let month_start = today_start - chrono::Duration::days(30);
        let month_start_ts: i64 =
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(month_start, chrono::Utc)
                .timestamp_millis();

        let total_result = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM privchat_messages")
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("统计总消息数失败: {}", e)))?;

        let today_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1",
        )
        .bind(today_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计今日消息数失败: {}", e)))?;

        let week_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1",
        )
        .bind(week_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计本周消息数失败: {}", e)))?;

        let month_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1",
        )
        .bind(month_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计本月消息数失败: {}", e)))?;

        Ok(serde_json::json!({
            "total": total_result.0 as u64,
            "today": today_result.0 as u64,
            "this_week": week_result.0 as u64,
            "this_month": month_result.0 as u64,
        }))
    }

    /// 发送系统消息（管理 API）
    ///
    /// 以 SYSTEM_USER_ID 为发送者，向指定频道写入一条消息。
    /// 返回 (message_id, created_at_millis)。
    pub async fn send_system_message_admin(
        &self,
        channel_id: u64,
        content: &str,
        message_type: i16,
        metadata: &serde_json::Value,
    ) -> std::result::Result<(u64, i64), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();
        let message_id = crate::infra::snowflake::next_message_id();

        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, 0, NULL, $4, $5, $6, NULL, $7, $7, false, NULL, false, NULL, NULL)
            "#,
        )
        .bind(message_id as i64)
        .bind(channel_id as i64)
        .bind(crate::config::SYSTEM_USER_ID as i64)
        .bind(message_type)
        .bind(content)
        .bind(metadata)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("发送系统消息失败: {}", e)))?;

        Ok((message_id, now))
    }

    /// 管理端发送消息（可指定发送者）
    ///
    /// 与 send_system_message_admin 类似，但允许指定任意 sender_id。
    /// 返回 (message_id, created_at_millis)。
    pub async fn send_message_admin(
        &self,
        channel_id: u64,
        sender_id: u64,
        content: &str,
        message_type: i16,
        metadata: &serde_json::Value,
    ) -> std::result::Result<(u64, i64), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();
        let message_id = crate::infra::snowflake::next_message_id();

        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, 0, NULL, $4, $5, $6, NULL, $7, $7, false, NULL, false, NULL, NULL)
            "#,
        )
        .bind(message_id as i64)
        .bind(channel_id as i64)
        .bind(sender_id as i64)
        .bind(message_type)
        .bind(content)
        .bind(metadata)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("发送消息失败: {}", e)))?;

        Ok((message_id, now))
    }
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    message_id: i64,
    channel_id: i64,
    sender_id: i64,
    pts: i64,
    local_message_id: Option<i64>,
    content: String,
    message_type: i16,
    metadata: Option<serde_json::Value>,
    reply_to_message_id: Option<i64>,
    created_at: i64,
    updated_at: i64,
    deleted: bool,
    deleted_at: Option<i64>,
    revoked: bool,
    revoked_at: Option<i64>,
    revoked_by: Option<i64>,
}

impl MessageRepository for PgMessageRepository {
    /// 根据ID查找消息
    async fn find_by_id(&self, message_id: u64) -> Result<Option<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }

        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE message_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(message_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query message: {}", e)))?;

        match row {
            Some(r) => Ok(Some(Message::from_db_row(
                r.message_id,
                r.channel_id,
                r.sender_id,
                r.pts,
                r.local_message_id.map(|n| n as u64),
                r.content,
                r.message_type,
                r.metadata
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.reply_to_message_id,
                r.created_at,
                r.updated_at,
                r.deleted,
                r.deleted_at,
                r.revoked,
                r.revoked_at,
                r.revoked_by,
            ))),
            None => Ok(None),
        }
    }

    /// 创建消息
    async fn create(&self, message: &Message) -> Result<Message, DatabaseError> {
        let (
            message_id,
            channel_id,
            sender_id,
            pts,
            local_message_id,
            content,
            message_type,
            metadata,
            reply_to_message_id,
            created_at,
            updated_at,
            deleted,
            deleted_at,
            revoked,
            revoked_at,
            revoked_by,
        ) = message.to_db_values();

        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(sender_id)
        .bind(pts)
        .bind(local_message_id)
        .bind(message_type)
        .bind(content)
        .bind(&metadata_json)
        .bind(reply_to_message_id)
        .bind(created_at)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create message: {}", e)))?;

        Ok(message.clone())
    }

    async fn create_with_dedup_key(
        &self,
        message: &Message,
        dedup_key: Option<&str>,
    ) -> Result<bool, DatabaseError> {
        // 无 dedup_key（普通消息路径）→ 与 create 等价，永远算「已插入」。
        let dk = match dedup_key {
            None => {
                self.create(message).await?;
                return Ok(true);
            }
            Some(k) => k,
        };

        let (
            message_id,
            channel_id,
            sender_id,
            pts,
            local_message_id,
            content,
            message_type,
            metadata,
            reply_to_message_id,
            created_at,
            updated_at,
            deleted,
            deleted_at,
            revoked,
            revoked_at,
            revoked_by,
        ) = message.to_db_values();

        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        // privchat_messages 按 created_at RANGE 分区，无法在 dedup_key 上建全局唯一索引，
        // 故用独立非分区表 privchat_message_dedup 承载幂等键：同事务先 claim dedup_key，
        // ON CONFLICT DO NOTHING → rows_affected==0 表示已被抢占（重复注入），跳过消息插入 +
        // 让调用方跳过推送；抢占成功才写消息，二者原子（消息失败则 dedup 键回滚，可重试）。
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to begin tx: {}", e)))?;

        let claim = sqlx::query(
            r#"
            INSERT INTO privchat_message_dedup (dedup_key, message_id, created_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (dedup_key) DO NOTHING
            "#,
        )
        .bind(dk)
        .bind(message_id)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to claim dedup_key: {}", e)))?;

        if claim.rows_affected() == 0 {
            // 已存在同 dedup_key → 不重复插入消息，调用方据此跳过推送。
            tx.rollback().await.ok();
            return Ok(false);
        }

        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(sender_id)
        .bind(pts)
        .bind(local_message_id)
        .bind(message_type)
        .bind(content)
        .bind(&metadata_json)
        .bind(reply_to_message_id)
        .bind(created_at)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create message: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to commit tx: {}", e)))?;

        Ok(true)
    }

    async fn find_message_id_by_dedup_key(
        &self,
        dedup_key: &str,
    ) -> Result<Option<(u64, i64)>, DatabaseError> {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT message_id, created_at FROM privchat_message_dedup WHERE dedup_key = $1 LIMIT 1",
        )
        .bind(dedup_key)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query dedup_key: {}", e)))?;

        Ok(row.map(|(id, created)| (id as u64, created)))
    }

    /// 更新消息
    async fn update(&self, message: &Message) -> Result<Message, DatabaseError> {
        let (
            message_id,
            _channel_id,
            _sender_id,
            _pts,
            _local_message_id,
            content,
            _message_type,
            metadata,
            _reply_to_message_id,
            _created_at,
            updated_at,
            deleted,
            deleted_at,
            revoked,
            revoked_at,
            revoked_by,
        ) = message.to_db_values();

        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        // 注意：由于消息表是分区表，需要包含 created_at 在 WHERE 条件中
        // 但更新时我们只更新非分区键字段
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET 
                content = $2,
                metadata = $3,
                updated_at = $4,
                deleted = $5,
                deleted_at = $6,
                revoked = $7,
                revoked_at = $8,
                revoked_by = $9
            WHERE message_id = $1
            "#,
        )
        .bind(message_id)
        .bind(content)
        .bind(&metadata_json)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to update message: {}", e)))?;

        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Message not found".to_string()));
        }

        Ok(message.clone())
    }

    /// 删除消息（软删除）
    async fn delete(&self, message_id: &Uuid) -> Result<(), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();

        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET deleted = true, deleted_at = $2, updated_at = $2
            WHERE message_id = $1
            "#,
        )
        .bind(message_id)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to delete message: {}", e)))?;

        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Message not found".to_string()));
        }

        Ok(())
    }

    /// 获取会话的消息列表（分页）
    async fn list_by_channel(
        &self,
        channel_id: u64,
        limit: i64,
        before_created_at: Option<i64>,
    ) -> Result<Vec<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }

        let query = if let Some(before_ts) = before_created_at {
            sqlx::query_as::<_, MessageRow>(
                r#"
                SELECT 
                    message_id,
                    channel_id,
                    sender_id,
                    pts,
                    local_message_id,
                    content,
                    message_type,
                    metadata,
                    reply_to_message_id,
                    created_at,
                    updated_at,
                    deleted,
                    deleted_at,
                    revoked,
                    revoked_at,
                    revoked_by
                FROM privchat_messages
                WHERE channel_id = $1 AND created_at < $2 AND deleted = false
                ORDER BY created_at DESC
                LIMIT $3
                "#,
            )
            .bind(channel_id as i64)
            .bind(before_ts)
            .bind(limit)
        } else {
            sqlx::query_as::<_, MessageRow>(
                r#"
                SELECT 
                    message_id,
                    channel_id,
                    sender_id,
                    pts,
                    local_message_id,
                    content,
                    message_type,
                    metadata,
                    reply_to_message_id,
                    created_at,
                    updated_at,
                    deleted,
                    deleted_at,
                    revoked,
                    revoked_at,
                    revoked_by
                FROM privchat_messages
                WHERE channel_id = $1 AND deleted = false
                ORDER BY created_at DESC
                LIMIT $2
                "#,
            )
            .bind(channel_id as i64)
            .bind(limit)
        };

        let rows = query
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to list messages: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                Message::from_db_row(
                    r.message_id,
                    r.channel_id,
                    r.sender_id,
                    r.pts,
                    r.local_message_id.map(|n| n as u64),
                    r.content,
                    r.message_type,
                    r.metadata
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.reply_to_message_id,
                    r.created_at,
                    r.updated_at,
                    r.deleted,
                    r.deleted_at,
                    r.revoked,
                    r.revoked_at,
                    r.revoked_by,
                )
            })
            .collect())
    }

    /// 根据 pts 获取消息
    async fn find_by_pts(
        &self,
        sender_id: &Uuid,
        pts: i64,
    ) -> Result<Option<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }

        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE sender_id = $1 AND pts = $2
            LIMIT 1
            "#,
        )
        .bind(sender_id)
        .bind(pts)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query message by pts: {}", e)))?;

        match row {
            Some(r) => Ok(Some(Message::from_db_row(
                r.message_id,
                r.channel_id,
                r.sender_id,
                r.pts,
                r.local_message_id.map(|n| n as u64),
                r.content,
                r.message_type,
                r.metadata
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.reply_to_message_id,
                r.created_at,
                r.updated_at,
                r.deleted,
                r.deleted_at,
                r.revoked,
                r.revoked_at,
                r.revoked_by,
            ))),
            None => Ok(None),
        }
    }

    /// 撤回消息（标记为已撤回，但保留内容）
    async fn revoke_message(
        &self,
        message_id: u64,
        revoker_id: u64,
    ) -> Result<Message, DatabaseError> {
        let revoked_at = chrono::Utc::now().timestamp_millis();

        // 先查询消息是否存在
        let message = self
            .find_by_id(message_id)
            .await?
            .ok_or_else(|| DatabaseError::NotFound(format!("Message {} not found", message_id)))?;

        // 检查是否已经撤回
        if message.revoked {
            return Err(DatabaseError::Validation(
                "Message already revoked".to_string(),
            ));
        }

        // 更新消息状态（只更新 revoked 相关字段，保留 content）
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET revoked = true,
                revoked_at = $1,
                revoked_by = $2,
                updated_at = $1
            WHERE message_id = $3
            "#,
        )
        .bind(revoked_at)
        .bind(revoker_id as i64)
        .bind(message_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to revoke message: {}", e)))?;

        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound(format!(
                "Message {} not found",
                message_id
            )));
        }

        // 返回更新后的消息
        let mut revoked_message = message;
        revoked_message.revoked = true;
        revoked_message.revoked_at =
            Some(chrono::DateTime::from_timestamp_millis(revoked_at).unwrap());
        revoked_message.revoked_by = Some(revoker_id);
        revoked_message.updated_at = chrono::DateTime::from_timestamp_millis(revoked_at).unwrap();

        Ok(revoked_message)
    }

    async fn search_messages(
        &self,
        keyword: &str,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        message_type: Option<i16>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DatabaseError> {
        let offset = (page - 1) * page_size;

        let mut sql = String::from(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                content,
                message_type,
                created_at
            FROM privchat_messages
            WHERE 1=1
            AND content ILIKE $1
            "#,
        );

        let mut bind_count = 1;
        let keyword_pattern = format!("%{}%", keyword);

        if channel_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if message_type.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND message_type = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }

        sql.push_str(" ORDER BY created_at DESC");
        bind_count += 1;
        sql.push_str(&format!(" LIMIT ${}", bind_count));
        bind_count += 1;
        sql.push_str(&format!(" OFFSET ${}", bind_count));

        let mut query_builder = sqlx::query(&sql).bind(&keyword_pattern);

        if let Some(ch_id) = channel_id {
            query_builder = query_builder.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            query_builder = query_builder.bind(uid as i64);
        }
        if let Some(mt) = message_type {
            query_builder = query_builder.bind(mt as i32);
        }
        if let Some(st) = start_time {
            query_builder = query_builder.bind(st);
        }
        if let Some(et) = end_time {
            query_builder = query_builder.bind(et);
        }
        query_builder = query_builder.bind(page_size as i64);
        query_builder = query_builder.bind(offset as i64);

        let rows = query_builder
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("搜索消息失败: {}", e)))?;

        let messages: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let message_id: i64 = row.get("message_id");
                let channel_id: i64 = row.get("channel_id");
                let sender_id: i64 = row.get("sender_id");
                let content: String = row.get("content");
                let message_type: i32 = row.get("message_type");
                let created_at: i64 = row.get("created_at");

                serde_json::json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "sender_id": sender_id,
                    "content": content,
                    "message_type": message_type,
                    "created_at": created_at,
                })
            })
            .collect();

        // 统计总数
        let mut count_sql =
            String::from("SELECT COUNT(*) FROM privchat_messages WHERE 1=1 AND content ILIKE $1");
        bind_count = 1;

        if channel_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if message_type.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND message_type = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }

        let mut count_query = sqlx::query(&count_sql).bind(&keyword_pattern);

        if let Some(ch_id) = channel_id {
            count_query = count_query.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            count_query = count_query.bind(uid as i64);
        }
        if let Some(mt) = message_type {
            count_query = count_query.bind(mt as i32);
        }
        if let Some(st) = start_time {
            count_query = count_query.bind(st);
        }
        if let Some(et) = end_time {
            count_query = count_query.bind(et);
        }

        let count_row = count_query
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("统计搜索结果失败: {}", e)))?;

        let total: i64 = count_row.get("count");

        Ok((messages, total as u32))
    }
}
