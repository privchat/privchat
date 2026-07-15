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
use privchat_protocol::{
    CanonicalTimelineEvent, FlatBufferMessage, CANONICAL_TIMELINE_EVENT_SCHEMA_V1,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
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
    /// sync/submit durable idempotency claim. Mutually exclusive with dedup_key.
    pub client_registry_claim: Option<ClientRegistryClaim>,
    pub attachment_file_ids: Vec<u64>,
    pub channel_type: i16,
    pub commit_content: serde_json::Value,
    pub sender_username: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClientRegistryClaim {
    pub device_id: String,
    pub decision: String,
}

#[derive(Debug, Clone)]
pub struct AtomicMessageCommitResult {
    pub message: Message,
    pub inserted: bool,
    pub event_id: Option<u64>,
    pub event_schema_version: Option<u16>,
    pub canonical_event: Option<Vec<u8>>,
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

    async fn create_dispatch_snapshot_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        event_id: i64,
        channel_id: i64,
        wire_channel_type: i16,
        pts: i64,
        sender_id: i64,
        event_kind: i16,
        now: i64,
    ) -> Result<(), DatabaseError> {
        let channel = sqlx::query(
            r#"
            SELECT channel_type, membership_version
            FROM privchat_channels
            WHERE channel_id = $1
            FOR UPDATE
            "#,
        )
        .bind(channel_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to lock channel: {e}")))?
        .ok_or_else(|| DatabaseError::NotFound(format!("Channel not found: {channel_id}")))?;

        let expected_db_channel_type = match wire_channel_type {
            1 => 0, // Direct
            2 => 1, // Group
            3 => 2, // Room
            value => {
                return Err(DatabaseError::Database(format!(
                    "unsupported wire channel type for {channel_id}: {value}"
                )))
            }
        };
        let channel_type = channel.get::<i16, _>("channel_type");
        if channel_type != expected_db_channel_type {
            return Err(DatabaseError::Database(format!(
                "channel type mismatch for {channel_id}: wire {wire_channel_type} maps to DB {expected_db_channel_type}, actual {channel_type}"
            )));
        }
        let membership_version = channel.get::<i64, _>("membership_version");

        sqlx::query(
            r#"
            INSERT INTO privchat_message_dispatch_outbox
                (event_id, channel_id, channel_type, pts, sender_id, event_kind,
                 membership_version, status, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8)
            "#,
        )
        .bind(event_id)
        .bind(channel_id)
        .bind(channel_type)
        .bind(pts)
        .bind(sender_id)
        .bind(event_kind)
        .bind(membership_version)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create dispatch outbox: {e}")))?;

        // Direct membership is stored on the channel row; group membership is
        // authoritative in group_members. Room-like channels retain the
        // participant-table fallback until their membership model is frozen.
        let inserted = sqlx::query(
            r#"
            INSERT INTO privchat_message_dispatch_recipient
                (event_id, user_id, state, attempts, next_attempt_at)
            SELECT $1, recipients.user_id, 0, 0, $3
            FROM (
                SELECT direct_user1_id AS user_id
                FROM privchat_channels
                WHERE channel_id = $2 AND channel_type = 0 AND direct_user1_id IS NOT NULL
                UNION
                SELECT direct_user2_id AS user_id
                FROM privchat_channels
                WHERE channel_id = $2 AND channel_type = 0 AND direct_user2_id IS NOT NULL
                UNION
                SELECT user_id
                FROM privchat_group_members
                WHERE group_id = $2 AND left_at IS NULL
                  AND EXISTS (
                    SELECT 1 FROM privchat_channels
                    WHERE channel_id = $2 AND channel_type = 1
                  )
                UNION
                SELECT user_id
                FROM privchat_channel_participants
                WHERE channel_id = $2 AND left_at IS NULL
                  AND EXISTS (
                    SELECT 1 FROM privchat_channels
                    WHERE channel_id = $2 AND channel_type NOT IN (0, 1)
                  )
            ) recipients
            ON CONFLICT (event_id, user_id) DO NOTHING
            "#,
        )
        .bind(event_id)
        .bind(channel_id)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            DatabaseError::Database(format!("Failed to snapshot dispatch recipients: {e}"))
        })?;

        if inserted.rows_affected() == 0 {
            return Err(DatabaseError::Database(format!(
                "dispatch recipient snapshot is empty for channel {channel_id}"
            )));
        }
        Ok(())
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

    pub async fn find_message_by_client_registry(
        &self,
        sender_id: u64,
        device_id: &str,
        local_message_id: u64,
    ) -> Result<Option<Message>, DatabaseError> {
        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT
                m.message_id, m.channel_id, m.sender_id, m.pts, m.local_message_id,
                m.content, m.message_type, m.metadata, m.reply_to_message_id,
                m.created_at, m.updated_at, m.deleted, m.deleted_at,
                m.revoked, m.revoked_at, m.revoked_by
            FROM privchat_client_msg_registry r
            JOIN privchat_messages m ON m.message_id = r.server_msg_id
            WHERE r.sender_id = $1 AND r.device_id = $2 AND r.local_message_id = $3
            LIMIT 1
            "#,
        )
        .bind(sender_id as i64)
        .bind(device_id)
        .bind(local_message_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| {
            DatabaseError::Database(format!("Failed to query client registry message: {e}"))
        })?;
        Ok(row.map(Self::message_from_row))
    }

    /// 普通客户端消息的权威写入路径：在同一个 DB transaction 内完成
    /// dedup claim、pts 分配、消息落库、附件绑定和 commit log 写入。
    pub async fn create_message_and_commit_atomic(
        &self,
        request: AtomicMessageCommitRequest,
    ) -> Result<AtomicMessageCommitResult, DatabaseError> {
        if request.dedup_key.is_some() && request.client_registry_claim.is_some() {
            return Err(DatabaseError::Database(
                "dedup_key and client_registry_claim are mutually exclusive".to_string(),
            ));
        }
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
                    event_id: None,
                    event_schema_version: None,
                    canonical_event: None,
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
                    -- 上传 token 会预设 business_type='message'（业务分类），但此时
                    -- business_id 仍为空 —— 文件还没附着到任何具体消息，可以绑定。
                    -- 归属守卫真正要防的是「已指向其它 business_id」的劫持，判据应看
                    -- business_id 是否已落到具体实体，而非 business_type 是否已分类。
                    OR business_id IS NULL
                    OR business_id = ''
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

        let canonical_event = CanonicalTimelineEvent::from_legacy(
            message.message_type.as_str(),
            &request.commit_content,
            message.message_id,
            message.sender_id,
            message.created_at.timestamp_millis(),
        )
        .map_err(|e| DatabaseError::Database(format!("canonical event mapping failed: {e}")))?
        .map(|event| event.encode_fb())
        .transpose()
        .map_err(|e| DatabaseError::Database(format!("canonical event encoding failed: {e}")))?;
        let event_schema_version = canonical_event
            .as_ref()
            .map(|_| CANONICAL_TIMELINE_EVENT_SCHEMA_V1);

        let event_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO privchat_commit_log
            (pts, server_msg_id, local_message_id, channel_id, channel_type,
             message_type, content, server_timestamp, sender_id, sender_username, created_at,
             event_schema_version, canonical_event)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING id
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
        .bind(event_schema_version.map(|v| v as i16))
        .bind(canonical_event.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to save commit: {}", e)))?;

        Self::create_dispatch_snapshot_in_tx(
            &mut tx,
            event_id,
            message.channel_id as i64,
            request.channel_type,
            message.pts.unwrap_or(0),
            message.sender_id as i64,
            1,
            now,
        )
        .await?;

        if let Some(claim) = request.client_registry_claim.as_ref() {
            let local_message_id = message.local_message_id.unwrap_or(0);
            if local_message_id != 0 {
                let registered = sqlx::query(
                    r#"
                    INSERT INTO privchat_client_msg_registry
                        (local_message_id, server_msg_id, pts, channel_id, channel_type,
                         sender_id, device_id, decision, created_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    ON CONFLICT (sender_id, device_id, local_message_id) DO NOTHING
                    "#,
                )
                .bind(local_message_id as i64)
                .bind(message.message_id as i64)
                .bind(message.pts.unwrap_or(0))
                .bind(message.channel_id as i64)
                .bind(request.channel_type)
                .bind(message.sender_id as i64)
                .bind(&claim.device_id)
                .bind(&claim.decision)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    DatabaseError::Database(format!("Failed to claim client registry: {e}"))
                })?;

                if registered.rows_affected() == 0 {
                    tx.rollback().await.ok();
                    let existing = self
                        .find_message_by_client_registry(
                            message.sender_id,
                            &claim.device_id,
                            local_message_id,
                        )
                        .await?
                        .ok_or_else(|| {
                            DatabaseError::Database(
                                "client registry conflict without message projection".to_string(),
                            )
                        })?;
                    return Ok(AtomicMessageCommitResult {
                        message: existing,
                        inserted: false,
                        event_id: None,
                        event_schema_version: None,
                        canonical_event: None,
                    });
                }
            }
        }

        tx.commit()
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to commit message tx: {}", e)))?;

        Ok(AtomicMessageCommitResult {
            message,
            inserted: true,
            event_id: Some(event_id as u64),
            event_schema_version,
            canonical_event,
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

// ============================================================================
// 客户端消息搜索与 around 上下文（MESSAGE_HISTORY_AND_SEARCH spec §4/§5）
//
// 与上面 admin `search_messages`（全库、X-Service-Key、offset 分页）完全独立：
// 这里的一切查询都以"当前用户可见范围"为边界，禁止复用 admin 路径给客户端。
// ============================================================================

/// search 命中投影（spec §4：只回 snippet 所需字段，不是完整消息——
/// 命中结果不允许回填本地 message 表，完整消息走 get/around）。
#[derive(Debug, sqlx::FromRow)]
pub struct MessageSearchHit {
    pub message_id: i64,
    pub channel_id: i64,
    pub sender_id: i64,
    pub message_type: i16,
    pub content: String,
    pub created_at: i64,
}

impl PgMessageRepository {
    /// 把原始 query 交给库内 `privchat_search_tokens`（011）分词并拼成 AND tsquery 串。
    /// 分词逻辑只存在于该 SQL 函数一处（写入索引与查询共用），避免 Rust 侧
    /// 重复实现造成漂移。返回空串 = 分不出 token（调用方直接回空结果）。
    pub async fn search_tokens_tsquery(&self, raw_query: &str) -> Result<String, DatabaseError> {
        let row: (Option<String>,) = sqlx::query_as(
            "SELECT array_to_string(tsvector_to_array(privchat_search_tokens($1)), ' & ')",
        )
        .bind(raw_query)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("tokenize query failed: {}", e)))?;
        Ok(row.0.unwrap_or_default())
    }

    /// 用户可见范围内的消息搜索（spec §4）：
    /// - GLOBAL（scope_channel=None）：成员过滤压在 SQL 内的 EXISTS semi-join
    ///   （privchat_channel_participants + left_at IS NULL），禁止应用层拼 IN 列表；
    /// - CHANNEL：调用方已过 ensure_channel_visible，这里只加 channel_id 等值；
    /// - 可见性：revoked=false AND deleted=false——撤回正文库内并未物理清除，
    ///   不过滤会经 snippet 泄露（spec §0.1-3）；
    /// - keyset：created_at DESC, message_id DESC（cursor 同方向解释）；
    /// - 语义：GIN bigram tsquery 先缩候选集，content ILIKE recheck 保证精确子串；
    /// - 兜底：事务内 SET LOCAL statement_timeout 防慢查询拖死连接。
    pub async fn search_visible(
        &self,
        user_id: u64,
        scope_channel: Option<u64>,
        tsquery: &str,
        ilike_pattern: &str,
        cursor: Option<(i64, i64)>,
        limit: i64,
    ) -> Result<Vec<MessageSearchHit>, DatabaseError> {
        let mut sql = String::from(
            r#"
            SELECT message_id, channel_id, sender_id, message_type, content, created_at
            FROM privchat_messages m
            WHERE privchat_search_tokens(m.content) @@ to_tsquery('simple', $1)
              AND m.content ILIKE $2 ESCAPE '\'
              AND m.revoked = false
              AND m.deleted = false
            "#,
        );
        // $1=tsquery $2=ilike；$3=scope（channel_id 或 user_id）；cursor 追加 $4/$5
        if scope_channel.is_some() {
            sql.push_str(" AND m.channel_id = $3");
        } else {
            // GLOBAL 成员可见严格按 channel_type：Direct 只认 direct_user1/2（权威源，
            // 冗余 participant 不作旁路）；群/房间只认 participants(left_at IS NULL)。
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM privchat_channels c \
                   WHERE c.channel_id = m.channel_id AND ( \
                     (c.channel_type = 0 AND (c.direct_user1_id = $3 OR c.direct_user2_id = $3)) \
                     OR (c.channel_type <> 0 AND EXISTS ( \
                         SELECT 1 FROM privchat_channel_participants p \
                         WHERE p.channel_id = m.channel_id AND p.user_id = $3 AND p.left_at IS NULL)) \
                   ))",
            );
        }
        if cursor.is_some() {
            sql.push_str(" AND (m.created_at < $4 OR (m.created_at = $4 AND m.message_id < $5))");
        }
        sql.push_str(&format!(
            " ORDER BY m.created_at DESC, m.message_id DESC LIMIT {}",
            limit.clamp(1, 50)
        ));

        let mut query = sqlx::query_as::<_, MessageSearchHit>(&sql)
            .bind(tsquery)
            .bind(ilike_pattern);
        query = match scope_channel {
            Some(cid) => query.bind(cid as i64),
            None => query.bind(user_id as i64),
        };
        if let Some((ts, id)) = cursor {
            query = query.bind(ts).bind(id);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DatabaseError::Database(format!("search begin tx failed: {}", e)))?;
        sqlx::query("SET LOCAL statement_timeout = 4000")
            .execute(&mut *tx)
            .await
            .map_err(|e| DatabaseError::Database(format!("search set timeout failed: {}", e)))?;
        let rows = query
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| DatabaseError::Database(format!("search query failed: {}", e)))?;
        tx.commit()
            .await
            .map_err(|e| DatabaseError::Database(format!("search commit failed: {}", e)))?;
        Ok(rows)
    }

    /// around：anchor 之前（更旧）的一页。严格 (created_at, message_id) 元组 keyset，
    /// DESC 取出，调用方反转为 ASC 展示。软删过滤；撤回保留占位（响应层清 content），
    /// 与 message/history/get 的语义一致——上下文里撤回消息必须占位而不是消失。
    pub async fn list_context_before(
        &self,
        channel_id: u64,
        anchor_created_at: i64,
        anchor_message_id: i64,
        limit: i64,
    ) -> Result<Vec<Message>, DatabaseError> {
        self.list_context(
            channel_id,
            anchor_created_at,
            anchor_message_id,
            limit,
            true,
        )
        .await
    }

    /// around：anchor 之后（更新）的一页，ASC 自然序。
    pub async fn list_context_after(
        &self,
        channel_id: u64,
        anchor_created_at: i64,
        anchor_message_id: i64,
        limit: i64,
    ) -> Result<Vec<Message>, DatabaseError> {
        self.list_context(
            channel_id,
            anchor_created_at,
            anchor_message_id,
            limit,
            false,
        )
        .await
    }

    async fn list_context(
        &self,
        channel_id: u64,
        anchor_created_at: i64,
        anchor_message_id: i64,
        limit: i64,
        before: bool,
    ) -> Result<Vec<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct ContextRow {
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

        let sql = if before {
            r#"
            SELECT message_id, channel_id, sender_id, pts, local_message_id, content,
                   message_type, metadata, reply_to_message_id, created_at, updated_at,
                   deleted, deleted_at, revoked, revoked_at, revoked_by
            FROM privchat_messages
            WHERE channel_id = $1 AND deleted = false
              AND (created_at < $2 OR (created_at = $2 AND message_id < $3))
            ORDER BY created_at DESC, message_id DESC
            LIMIT $4
            "#
        } else {
            r#"
            SELECT message_id, channel_id, sender_id, pts, local_message_id, content,
                   message_type, metadata, reply_to_message_id, created_at, updated_at,
                   deleted, deleted_at, revoked, revoked_at, revoked_by
            FROM privchat_messages
            WHERE channel_id = $1 AND deleted = false
              AND (created_at > $2 OR (created_at = $2 AND message_id > $3))
            ORDER BY created_at ASC, message_id ASC
            LIMIT $4
            "#
        };

        let rows = sqlx::query_as::<_, ContextRow>(sql)
            .bind(channel_id as i64)
            .bind(anchor_created_at)
            .bind(anchor_message_id)
            .bind(limit.clamp(1, 50))
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("list context failed: {}", e)))?;

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
}

#[cfg(test)]
mod atomic_dispatch_tests {
    use super::*;
    use chrono::Utc;
    use sqlx::postgres::PgPoolOptions;

    const GROUP_ID: i64 = 987_674_001;
    const OWNER_ID: i64 = 987_674_101;
    const MEMBER_ID: i64 = 987_674_102;
    const LATE_MEMBER_ID: i64 = 987_674_103;
    const MESSAGE_ID: u64 = 987_674_201;

    async fn open_repo() -> Option<PgMessageRepository> {
        let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        Some(PgMessageRepository::new(Arc::new(pool)))
    }

    async fn cleanup(repo: &PgMessageRepository) {
        let _ = sqlx::query("DELETE FROM privchat_message_dedup WHERE dedup_key = $1")
            .bind("test:p2:group-snapshot")
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_commit_log WHERE server_msg_id = $1")
            .bind(MESSAGE_ID as i64)
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_messages WHERE message_id = $1")
            .bind(MESSAGE_ID as i64)
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_group_members WHERE group_id = $1")
            .bind(GROUP_ID)
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = $1")
            .bind(GROUP_ID)
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
            .bind(GROUP_ID)
            .execute(repo.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
            .bind(GROUP_ID)
            .execute(repo.pool())
            .await;
    }

    async fn ensure_user(repo: &PgMessageRepository, user_id: i64) {
        sqlx::query(
            r#"
            INSERT INTO privchat_users (user_id, username, display_name, qr_key)
            VALUES ($1, $2, $2, $3)
            ON CONFLICT (user_id) DO NOTHING
            "#,
        )
        .bind(user_id)
        .bind(format!("p2{}", user_id % 1_000_000))
        .bind(format!("qp2{}", user_id % 1_000_000))
        .execute(repo.pool())
        .await
        .expect("ensure test user");
    }

    #[tokio::test]
    async fn atomic_commit_freezes_group_recipients_and_outbox() {
        let Some(repo) = open_repo().await else {
            eprintln!("skip atomic dispatch test: DATABASE_URL not configured");
            return;
        };
        cleanup(&repo).await;
        for user_id in [OWNER_ID, MEMBER_ID, LATE_MEMBER_ID] {
            ensure_user(&repo, user_id).await;
        }
        sqlx::query(
            r#"
            INSERT INTO privchat_groups
                (group_id, name, owner_id, member_count, created_at, updated_at, qr_key)
            VALUES ($1, 'p2-group', $2, 2, $3, $3, $4)
            "#,
        )
        .bind(GROUP_ID)
        .bind(OWNER_ID)
        .bind(Utc::now().timestamp_millis())
        .bind(format!("q{GROUP_ID}"))
        .execute(repo.pool())
        .await
        .expect("insert group");
        sqlx::query(
            "INSERT INTO privchat_channels (channel_id, channel_type, group_id) VALUES ($1, 1, $1)",
        )
        .bind(GROUP_ID)
        .execute(repo.pool())
        .await
        .expect("insert channel");
        for user_id in [OWNER_ID, MEMBER_ID] {
            sqlx::query(
                r#"
                INSERT INTO privchat_group_members
                    (group_id, user_id, role, joined_at, updated_at)
                VALUES ($1, $2, 2, $3, $3)
                "#,
            )
            .bind(GROUP_ID)
            .bind(user_id)
            .bind(Utc::now().timestamp_millis())
            .execute(repo.pool())
            .await
            .expect("insert group member");
        }

        let now = Utc::now();
        let legacy = privchat_protocol::LocalMessagePayloadEnvelope {
            content: "atomic snapshot".to_string(),
            ..Default::default()
        };
        let message = Message {
            message_id: MESSAGE_ID,
            channel_id: GROUP_ID as u64,
            sender_id: OWNER_ID as u64,
            pts: None,
            local_message_id: Some(MESSAGE_ID),
            content: legacy.content.clone(),
            message_type: privchat_protocol::ContentMessageType::Text,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            reply_to_message_id: None,
            created_at: now,
            updated_at: now,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };
        let result = repo
            .create_message_and_commit_atomic(AtomicMessageCommitRequest {
                message,
                dedup_key: Some("test:p2:group-snapshot".to_string()),
                client_registry_claim: None,
                attachment_file_ids: Vec::new(),
                channel_type: 2,
                commit_content: serde_json::to_value(legacy).expect("serialize legacy envelope"),
                sender_username: None,
            })
            .await
            .expect("atomic message commit");
        let event_id = result.event_id.expect("event id");

        let snapshot_version: i64 = sqlx::query_scalar(
            "SELECT membership_version FROM privchat_message_dispatch_outbox WHERE event_id = $1",
        )
        .bind(event_id as i64)
        .fetch_one(repo.pool())
        .await
        .expect("outbox row");
        assert_eq!(snapshot_version, 2);

        sqlx::query(
            r#"
            INSERT INTO privchat_group_members
                (group_id, user_id, role, joined_at, updated_at)
            VALUES ($1, $2, 2, $3, $3)
            "#,
        )
        .bind(GROUP_ID)
        .bind(LATE_MEMBER_ID)
        .bind(Utc::now().timestamp_millis())
        .execute(repo.pool())
        .await
        .expect("insert late member");
        sqlx::query(
            "UPDATE privchat_group_members SET left_at = $3, updated_at = $3 WHERE group_id = $1 AND user_id = $2",
        )
        .bind(GROUP_ID)
        .bind(MEMBER_ID)
        .bind(Utc::now().timestamp_millis())
        .execute(repo.pool())
        .await
        .expect("leave after commit");

        let recipients: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM privchat_message_dispatch_recipient WHERE event_id = $1 ORDER BY user_id",
        )
        .bind(event_id as i64)
        .fetch_all(repo.pool())
        .await
        .expect("recipient snapshot");
        assert_eq!(recipients, vec![OWNER_ID, MEMBER_ID]);

        cleanup(&repo).await;
    }
}

#[cfg(test)]
mod client_search_tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    // 测试专用 ID 段（与 rpc guard 测试的 987_65x 区分开）
    const CH_MINE: u64 = 987_660_001; // user A 是成员
    const CH_OTHER: u64 = 987_660_002; // user A 非成员
    const CH_AROUND: u64 = 987_660_011; // around 测试独立频道（并行防撞）
    const USER_A: u64 = 987_661_001;
    const USER_B: u64 = 987_661_002;
    const USER_C: u64 = 987_661_003;
    const USER_D: u64 = 987_661_011;
    const USER_E: u64 = 987_661_012;
    const MSG_BASE: i64 = 987_662_000;
    const KW: &str = "biggram红包雨测试";

    async fn open_repo() -> Option<PgMessageRepository> {
        let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        Some(PgMessageRepository::new(Arc::new(pool)))
    }

    async fn cleanup(repo: &PgMessageRepository) {
        for ch in [CH_MINE, CH_OTHER] {
            let _ = sqlx::query("DELETE FROM privchat_messages WHERE channel_id = $1")
                .bind(ch as i64)
                .execute(repo.pool.as_ref())
                .await;
            let _ = sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = $1")
                .bind(ch as i64)
                .execute(repo.pool.as_ref())
                .await;
            let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
                .bind(ch as i64)
                .execute(repo.pool.as_ref())
                .await;
        }
    }

    async fn ensure_user(repo: &PgMessageRepository, user_id: u64) {
        // privchat_users.qr_key NOT NULL 且 username varchar(16)
        let _ = sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key)
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(user_id as i64)
        .bind(format!("st{}", user_id % 1_000_000))
        .bind(format!("qs{}", user_id % 1_000_000))
        .execute(repo.pool.as_ref())
        .await
        .expect("ensure user");
    }

    /// 建 direct 频道 + 双方 participants（满足 FK；user A 只进 CH_MINE）
    async fn ensure_channel(repo: &PgMessageRepository, channel_id: u64, u1: u64, u2: u64) {
        ensure_user(repo, u1).await;
        ensure_user(repo, u2).await;
        sqlx::query(
            "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id)
             VALUES ($1, 0, $2, $3) ON CONFLICT (channel_id) DO NOTHING",
        )
        .bind(channel_id as i64)
        .bind(u1 as i64)
        .bind(u2 as i64)
        .execute(repo.pool.as_ref())
        .await
        .expect("insert channel");
        for uid in [u1, u2] {
            sqlx::query(
                "INSERT INTO privchat_channel_participants (channel_id, user_id, role)
                 VALUES ($1, $2, 2)
                 ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
            )
            .bind(channel_id as i64)
            .bind(uid as i64)
            .execute(repo.pool.as_ref())
            .await
            .expect("insert participant");
        }
    }

    /// P1 Direct 权威源修复回归：Direct 会话只有 channels 行（direct_user1/2），
    /// participants 缺行（历史脏数据）——GLOBAL 搜索仍必须靠 direct_user1/2 认可命中，
    /// 否则该 Direct 会话的消息对本人不可见（误拒）。
    #[tokio::test]
    async fn search_visible_direct_without_participants() {
        let Some(repo) = open_repo().await else {
            eprintln!(
                "skip search_visible_direct_without_participants: DATABASE_URL not configured"
            );
            return;
        };
        const CH_DIRECT: u64 = 987_660_051;
        const DU1: u64 = 987_661_021;
        const DU2: u64 = 987_661_022;
        const DOUT: u64 = 987_661_023;
        clean_channel(&repo, CH_DIRECT).await;
        ensure_user(&repo, DU1).await;
        ensure_user(&repo, DU2).await;
        ensure_user(&repo, DOUT).await;
        // 只建 channels（direct_user1=A, direct_user2=B），故意不写 participants
        sqlx::query(
            "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id)              VALUES ($1, 0, $2, $3)")
            .bind(CH_DIRECT as i64).bind(DU1 as i64).bind(DU2 as i64)
            .execute(repo.pool.as_ref()).await.expect("insert channel");
        let kw = "directauthfix测试标记";
        insert_msg(
            &repo,
            MSG_BASE + 51,
            CH_DIRECT,
            &format!("你好 {} 收到", kw),
            1_780_200_000_000,
            false,
        )
        .await;

        let tq = repo.search_tokens_tsquery(kw).await.expect("tokenize");
        let hits = repo
            .search_visible(DU1, None, &tq, &format!("%{}%", kw), None, 10)
            .await
            .expect("global search");
        assert_eq!(
            hits.len(),
            1,
            "direct_user1 must see the message despite missing participants"
        );
        assert_eq!(hits[0].channel_id, CH_DIRECT as i64);

        // 非成员不可见
        let outsider = repo
            .search_visible(DOUT, None, &tq, &format!("%{}%", kw), None, 10)
            .await
            .expect("outsider search");
        assert!(
            outsider.iter().all(|h| h.channel_id != CH_DIRECT as i64),
            "non-participant of the direct channel must not see it"
        );

        // 严格模式：给该 Direct 会话插一个脏 participant(非 direct_user1/2)，它不该获得
        // 可见性(participants 对 Direct 不作旁路)。
        const DSTRAY: u64 = 987_661_024;
        ensure_user(&repo, DSTRAY).await;
        sqlx::query("INSERT INTO privchat_channel_participants (channel_id, user_id, role) VALUES ($1, $2, 2)")
            .bind(CH_DIRECT as i64).bind(DSTRAY as i64)
            .execute(repo.pool.as_ref()).await.expect("insert stray participant");
        let stray = repo
            .search_visible(DSTRAY, None, &tq, &format!("%{}%", kw), None, 10)
            .await
            .expect("stray search");
        assert!(
            stray.iter().all(|h| h.channel_id != CH_DIRECT as i64),
            "strict: a stray participant of a direct channel must NOT see it"
        );

        clean_channel(&repo, CH_DIRECT).await;
    }

    async fn clean_channel(repo: &PgMessageRepository, ch: u64) {
        for tbl in [
            "privchat_messages",
            "privchat_channel_participants",
            "privchat_channels",
        ] {
            let _ = sqlx::query(&format!("DELETE FROM {} WHERE channel_id = $1", tbl))
                .bind(ch as i64)
                .execute(repo.pool.as_ref())
                .await;
        }
    }

    async fn insert_msg(
        repo: &PgMessageRepository,
        message_id: i64,
        channel_id: u64,
        content: &str,
        created_at: i64,
        revoked: bool,
    ) {
        sqlx::query(
            r#"
            INSERT INTO privchat_messages
                (message_id, channel_id, sender_id, pts, message_type, content, created_at, revoked)
            VALUES ($1, $2, $3, 1, 0, $4, $5, $6)
            "#,
        )
        .bind(message_id)
        .bind(channel_id as i64)
        .bind(USER_B as i64)
        .bind(content)
        .bind(created_at)
        .bind(revoked)
        .execute(repo.pool.as_ref())
        .await
        .expect("insert message");
    }

    async fn setup(repo: &PgMessageRepository) -> (String, String) {
        cleanup(repo).await;
        // CH_MINE = A↔B（A 是成员）；CH_OTHER = B↔C（A 非成员）
        ensure_channel(repo, CH_MINE, USER_A, USER_B).await;
        ensure_channel(repo, CH_OTHER, USER_B, USER_C).await;

        let t0 = 1_780_000_000_000_i64; // 固定基准时间（default 分区）
        insert_msg(
            repo,
            MSG_BASE + 1,
            CH_MINE,
            &format!("早一条 {} A", KW),
            t0 + 1000,
            false,
        )
        .await;
        insert_msg(
            repo,
            MSG_BASE + 2,
            CH_MINE,
            &format!("晚一条 {} B", KW),
            t0 + 2000,
            false,
        )
        .await;
        insert_msg(
            repo,
            MSG_BASE + 3,
            CH_MINE,
            &format!("已撤回 {} C", KW),
            t0 + 3000,
            true,
        )
        .await;
        insert_msg(
            repo,
            MSG_BASE + 4,
            CH_OTHER,
            &format!("别人频道 {} D", KW),
            t0 + 4000,
            false,
        )
        .await;

        let tq = repo.search_tokens_tsquery(KW).await.expect("tokenize");
        assert!(
            !tq.is_empty(),
            "tokenizer must produce tokens (migration 011 applied?)"
        );
        (tq, format!("%{}%", KW))
    }

    /// spec §4：GLOBAL 只见成员频道；撤回/他频道不可见；keyset 稳定翻页。
    #[tokio::test]
    async fn search_visible_scopes_and_paginates() {
        let Some(repo) = open_repo().await else {
            eprintln!("skip search_visible_scopes_and_paginates: DATABASE_URL not configured");
            return;
        };
        let (tq, pattern) = setup(&repo).await;

        // GLOBAL：只命中 CH_MINE 的两条未撤回（新→旧）
        let hits = repo
            .search_visible(USER_A, None, &tq, &pattern, None, 10)
            .await
            .expect("global search");
        assert_eq!(
            hits.len(),
            2,
            "revoked + non-member channel must be invisible"
        );
        assert_eq!(hits[0].message_id, MSG_BASE + 2, "DESC: newest first");
        assert_eq!(hits[1].message_id, MSG_BASE + 1);
        assert!(hits.iter().all(|h| h.channel_id == CH_MINE as i64));

        // keyset：limit=1 翻两页无重复无丢失
        let page1 = repo
            .search_visible(USER_A, None, &tq, &pattern, None, 1)
            .await
            .expect("page1");
        assert_eq!(page1.len(), 1);
        let cursor = (page1[0].created_at, page1[0].message_id);
        let page2 = repo
            .search_visible(USER_A, None, &tq, &pattern, Some(cursor), 1)
            .await
            .expect("page2");
        assert_eq!(page2.len(), 1);
        assert_ne!(page1[0].message_id, page2[0].message_id);

        // CHANNEL scope：等值过滤
        let scoped = repo
            .search_visible(USER_A, Some(CH_MINE), &tq, &pattern, None, 10)
            .await
            .expect("channel search");
        assert_eq!(scoped.len(), 2);

        // 退群成员不可见（群语义：left_at 使 EXISTS participants 不认可）。
        // 注意：必须用群会话验证——Direct 会话权威是 direct_user1/2，left_at 对其无效，
        // 那正是 search_visible_direct_without_participants / is_member_direct_* 覆盖的语义。
        const CH_GROUP: u64 = 987_660_061;
        const GID: u64 = 987_663_001;
        clean_channel(&repo, CH_GROUP).await;
        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
            .bind(GID as i64)
            .execute(repo.pool.as_ref())
            .await;
        sqlx::query("INSERT INTO privchat_groups (group_id, name, owner_id, qr_key) VALUES ($1, 'g', $2, $3)")
            .bind(GID as i64).bind(USER_B as i64).bind(format!("qg{}", GID % 1_000_000))
            .execute(repo.pool.as_ref()).await.expect("insert group");
        sqlx::query(
            "INSERT INTO privchat_channels (channel_id, channel_type, group_id) VALUES ($1, 1, $2)",
        )
        .bind(CH_GROUP as i64)
        .bind(GID as i64)
        .execute(repo.pool.as_ref())
        .await
        .expect("insert group channel");
        sqlx::query("INSERT INTO privchat_channel_participants (channel_id, user_id, role) VALUES ($1, $2, 2)")
            .bind(CH_GROUP as i64).bind(USER_A as i64)
            .execute(repo.pool.as_ref()).await.expect("insert group participant");
        // 群退群不可见只在 GLOBAL(EXISTS participants left_at) 生效——CHANNEL scope 不做
        // 成员过滤(信任调用方已过 ensure_channel_visible)。用独立 marker 避免撞 CH_MINE 的 KW。
        let gkw = "groupleavemark测试标记";
        insert_msg(
            &repo,
            MSG_BASE + 61,
            CH_GROUP,
            &format!("群里 {} B", gkw),
            1_780_300_000_000,
            false,
        )
        .await;
        let gtq = repo
            .search_tokens_tsquery(gkw)
            .await
            .expect("tokenize group kw");
        let gpat = format!("%{}%", gkw);

        let in_group = repo
            .search_visible(USER_A, None, &gtq, &gpat, None, 10)
            .await
            .expect("group search");
        assert_eq!(
            in_group.len(),
            1,
            "active group member must see the message"
        );
        assert_eq!(in_group[0].channel_id, CH_GROUP as i64);
        // 退群(left_at=1) → GLOBAL 不可见(群认 participants，无 direct 兜底)
        sqlx::query("UPDATE privchat_channel_participants SET left_at = 1 WHERE channel_id = $1 AND user_id = $2")
            .bind(CH_GROUP as i64).bind(USER_A as i64)
            .execute(repo.pool.as_ref()).await.expect("mark left");
        let after_leave = repo
            .search_visible(USER_A, None, &gtq, &gpat, None, 10)
            .await
            .expect("search after leave");
        assert!(
            after_leave.is_empty(),
            "left group member must lose search visibility"
        );

        clean_channel(&repo, CH_GROUP).await;
        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
            .bind(GID as i64)
            .execute(repo.pool.as_ref())
            .await;
        cleanup(&repo).await;
    }

    async fn cleanup_around(repo: &PgMessageRepository) {
        let _ = sqlx::query("DELETE FROM privchat_messages WHERE channel_id = $1")
            .bind(CH_AROUND as i64)
            .execute(repo.pool.as_ref())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = $1")
            .bind(CH_AROUND as i64)
            .execute(repo.pool.as_ref())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
            .bind(CH_AROUND as i64)
            .execute(repo.pool.as_ref())
            .await;
    }

    /// spec §5：around 两侧元组 keyset；软删过滤由 SQL 保证（撤回占位在响应层）。
    #[tokio::test]
    async fn around_context_windows() {
        let Some(repo) = open_repo().await else {
            eprintln!("skip around_context_windows: DATABASE_URL not configured");
            return;
        };
        cleanup_around(&repo).await;
        ensure_channel(&repo, CH_AROUND, USER_D, USER_E).await;
        let t0 = 1_780_100_000_000_i64;
        // 五条按时间排开 + 一条与 anchor 同毫秒但 id 更大（验证 tie-break）
        for (i, dt) in [(1, 1000), (2, 2000), (3, 3000), (4, 4000), (5, 5000)] {
            insert_msg(
                &repo,
                MSG_BASE + 10 + i,
                CH_AROUND,
                &format!("ctx-{}", i),
                t0 + dt,
                false,
            )
            .await;
        }
        insert_msg(&repo, MSG_BASE + 19, CH_AROUND, "ctx-tie", t0 + 3000, false).await;

        let anchor_id = MSG_BASE + 13; // ctx-3
        let before = repo
            .list_context_before(CH_AROUND, t0 + 3000, anchor_id, 10)
            .await
            .expect("before");
        // DESC：ctx-2, ctx-1（同毫秒 id 更大的 ctx-tie 不属于 before）
        assert_eq!(
            before
                .iter()
                .map(|m| m.message_id as i64)
                .collect::<Vec<_>>(),
            vec![MSG_BASE + 12, MSG_BASE + 11]
        );

        let after = repo
            .list_context_after(CH_AROUND, t0 + 3000, anchor_id, 10)
            .await
            .expect("after");
        // ASC：同毫秒但 id 更大的 ctx-tie 先于 ctx-4/ctx-5
        assert_eq!(
            after
                .iter()
                .map(|m| m.message_id as i64)
                .collect::<Vec<_>>(),
            vec![MSG_BASE + 19, MSG_BASE + 14, MSG_BASE + 15]
        );

        cleanup_around(&repo).await;
    }
}
