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

use crate::error::{Result, ServerError};
use crate::infra::MessageRouter;
use crate::model::channel::{ChannelId, ChannelKind, UserId};
use crate::service::{ChannelService, UnreadCountService};
use privchat_protocol::notification::ChannelReadCursorNotification;
use privchat_protocol::protocol::PushMessageRequest;
use sqlx::{PgPool, Row};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ReadPtsUpdateResult {
    pub channel_id: ChannelId,
    pub last_read_pts: u64,
}

#[derive(Debug, Clone)]
pub struct ChannelReadCursorRow {
    pub channel_id: ChannelId,
    pub channel_type: i32,
    pub reader_id: UserId,
    pub last_read_pts: u64,
    pub version: u64,
}

#[derive(Debug, Clone)]
pub struct ChannelReadMemberRow {
    pub user_id: UserId,
    pub last_read_pts: u64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy)]
struct UpsertCursorResult {
    last_read_pts: u64,
    advanced: bool,
}

async fn upsert_channel_read_cursor_row(
    pool: &PgPool,
    reader_id: UserId,
    channel_id: ChannelId,
    last_read_pts: u64,
    last_read_message_id: Option<u64>,
) -> std::result::Result<UpsertCursorResult, sqlx::Error> {
    let upserted = sqlx::query(
        r#"
        WITH existing AS (
            SELECT last_read_pts
            FROM privchat_channel_read_cursor
            WHERE user_id = $1 AND channel_id = $2
        ),
        upserted AS (
            INSERT INTO privchat_channel_read_cursor (
                user_id, channel_id, last_read_pts, last_read_message_id, updated_at
            )
            VALUES ($1, $2, $3, $4, NOW())
            ON CONFLICT (user_id, channel_id)
            DO UPDATE SET
                last_read_pts = GREATEST(privchat_channel_read_cursor.last_read_pts, EXCLUDED.last_read_pts),
                last_read_message_id = CASE
                    WHEN EXCLUDED.last_read_pts >= privchat_channel_read_cursor.last_read_pts
                    THEN EXCLUDED.last_read_message_id
                    ELSE privchat_channel_read_cursor.last_read_message_id
                END,
                updated_at = CASE
                    WHEN EXCLUDED.last_read_pts > privchat_channel_read_cursor.last_read_pts
                    THEN NOW()
                    ELSE privchat_channel_read_cursor.updated_at
                END
            RETURNING last_read_pts
        )
        SELECT
            upserted.last_read_pts,
            (
                (SELECT last_read_pts FROM existing LIMIT 1) IS NULL
                OR $3 > (SELECT last_read_pts FROM existing LIMIT 1)
            ) AS advanced
        FROM upserted
        "#,
    )
    .bind(reader_id as i64)
    .bind(channel_id as i64)
    .bind(last_read_pts as i64)
    .bind(last_read_message_id.map(|v| v as i64))
    .fetch_one(pool)
    .await?;

    Ok(UpsertCursorResult {
        last_read_pts: upserted.get::<i64, _>("last_read_pts") as u64,
        advanced: upserted.get::<bool, _>("advanced"),
    })
}

/// 已读状态服务（read_pts 单一路径）
pub struct ReadStateService {
    channel_service: Arc<ChannelService>,
    unread_count_service: Arc<UnreadCountService>,
    message_router: Arc<MessageRouter>,
    pool: Arc<PgPool>,
}

impl ReadStateService {
    pub fn new(
        channel_service: Arc<ChannelService>,
        unread_count_service: Arc<UnreadCountService>,
        message_router: Arc<MessageRouter>,
        pool: Arc<PgPool>,
    ) -> Self {
        Self {
            channel_service,
            unread_count_service,
            message_router,
            pool,
        }
    }

    pub async fn mark_read_pts(
        &self,
        reader_id: UserId,
        channel_id: ChannelId,
        read_pts: u64,
    ) -> Result<ReadPtsUpdateResult> {
        let max_pts_row = sqlx::query(
            "SELECT COALESCE(MAX(pts), 0) AS max_pts FROM privchat_messages WHERE channel_id = $1",
        )
        .bind(channel_id as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询频道最大 pts 失败: {}", e)))?;
        let max_pts = max_pts_row.get::<i64, _>("max_pts") as u64;
        if read_pts > max_pts {
            return Err(ServerError::Validation(format!(
                "read_pts 超出频道最大 pts: read_pts={}, max_pts={}",
                read_pts, max_pts
            )));
        }

        let new_last_read_pts = self
            .channel_service
            .mark_read_pts(&reader_id, &channel_id, read_pts)
            .await?
            .ok_or_else(|| ServerError::Validation("用户不在该频道".to_string()))?;
        let upserted = upsert_channel_read_cursor_row(
            self.pool.as_ref(),
            reader_id,
            channel_id,
            new_last_read_pts,
            None,
        )
        .await
        .map_err(|e| {
            ServerError::Database(format!("更新 privchat_channel_read_cursor 失败: {}", e))
        })?;

        self.channel_service
            .clear_user_channel_unread(reader_id, channel_id)
            .await?;
        self.unread_count_service
            .clear_channel(reader_id, channel_id)
            .await
            .map_err(|e| ServerError::Internal(format!("清理 unread_count cache 失败: {}", e)))?;

        let advanced = upserted.advanced;
        if advanced {
            self.broadcast_read_cursor(reader_id, channel_id, upserted.last_read_pts)
                .await?;
        }

        Ok(ReadPtsUpdateResult {
            channel_id,
            last_read_pts: upserted.last_read_pts,
        })
    }

    pub async fn sync_channel_read_cursor_page(
        &self,
        user_id: UserId,
        since_version: u64,
        scope_channel_id: Option<ChannelId>,
        limit: u32,
    ) -> Result<(Vec<ChannelReadCursorRow>, u64, bool)> {
        let limit = limit.clamp(1, 200);
        let db_rows = sqlx::query(
            r#"
            SELECT
                channel_id,
                user_id,
                last_read_pts,
                sync_version
            FROM privchat_channel_read_cursor
            WHERE user_id = $1
              AND sync_version > $2
              AND ($3::BIGINT IS NULL OR channel_id = $3)
            ORDER BY sync_version ASC, channel_id ASC
            LIMIT $4
            "#,
        )
        .bind(user_id as i64)
        .bind(since_version as i64)
        .bind(scope_channel_id.map(|v| v as i64))
        .bind((limit + 1) as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| {
            ServerError::Database(format!("同步 privchat_channel_read_cursor 失败: {}", e))
        })?;

        let channels = self
            .channel_service
            .get_user_channels(user_id)
            .await
            .channels;
        let has_more = db_rows.len() > limit as usize;
        let mut rows: Vec<ChannelReadCursorRow> = Vec::new();
        for row in db_rows.into_iter().take(limit as usize) {
            let channel_id = row.get::<i64, _>("channel_id") as u64;
            let channel_type = channels
                .iter()
                .find(|ch| ch.id == channel_id)
                .map(|ch| match ChannelKind::from(ch.channel_type.clone()) {
                    ChannelKind::PrivateChat => 1,
                    ChannelKind::GroupChat => 2,
                    _ => 1,
                })
                .unwrap_or(1);
            rows.push(ChannelReadCursorRow {
                channel_id,
                channel_type,
                reader_id: row.get::<i64, _>("user_id") as u64,
                last_read_pts: row.get::<i64, _>("last_read_pts") as u64,
                version: row.get::<i64, _>("sync_version") as u64,
            });
        }
        let next_version = rows.last().map(|r| r.version).unwrap_or(since_version);
        Ok((rows, next_version, has_more))
    }

    pub async fn list_read_members_by_message_pts(
        &self,
        channel_id: ChannelId,
        message_pts: u64,
        member_ids: &[UserId],
    ) -> Result<Vec<ChannelReadMemberRow>> {
        let ids: Vec<i64> = member_ids.iter().map(|v| *v as i64).collect();
        let rows = sqlx::query(
            r#"
            SELECT
                user_id,
                last_read_pts,
                updated_at
            FROM privchat_channel_read_cursor
            WHERE channel_id = $1
              AND user_id = ANY($2)
              AND last_read_pts >= $3
            ORDER BY updated_at DESC
            "#,
        )
        .bind(channel_id as i64)
        .bind(&ids)
        .bind(message_pts as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询 read_list 失败: {}", e)))?;
        Ok(rows
            .into_iter()
            .map(|row| ChannelReadMemberRow {
                user_id: row.get::<i64, _>("user_id") as u64,
                last_read_pts: row.get::<i64, _>("last_read_pts") as u64,
                updated_at: row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            })
            .collect())
    }

    pub async fn count_read_members_by_message_pts(
        &self,
        channel_id: ChannelId,
        message_pts: u64,
        member_ids: &[UserId],
    ) -> Result<u32> {
        let ids: Vec<i64> = member_ids.iter().map(|v| *v as i64).collect();
        let row = sqlx::query(
            r#"
            SELECT COUNT(*)::BIGINT AS read_count
            FROM privchat_channel_read_cursor
            WHERE channel_id = $1
              AND user_id = ANY($2)
              AND last_read_pts >= $3
            "#,
        )
        .bind(channel_id as i64)
        .bind(&ids)
        .bind(message_pts as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询 read_stats 失败: {}", e)))?;
        Ok(row.get::<i64, _>("read_count") as u32)
    }

    async fn broadcast_read_cursor(
        &self,
        reader_id: UserId,
        channel_id: ChannelId,
        last_read_pts: u64,
    ) -> Result<()> {
        let channel = self.channel_service.get_channel(&channel_id).await?;
        let channel_kind = ChannelKind::from(channel.channel_type.clone());
        let channel_type = match channel_kind {
            ChannelKind::PrivateChat => 1,
            ChannelKind::GroupChat => 2,
            _ => 1,
        };

        self.send_read_cursor_event(
            reader_id,
            "self_read_pts_updated",
            channel_id,
            channel_type,
            reader_id,
            last_read_pts,
        )
        .await?;

        if matches!(channel_kind, ChannelKind::PrivateChat) {
            if let Some(peer_id) = channel.members.keys().find(|id| **id != reader_id).copied() {
                self.send_read_cursor_event(
                    peer_id,
                    "peer_read_pts_updated",
                    channel_id,
                    channel_type,
                    reader_id,
                    last_read_pts,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn send_read_cursor_event(
        &self,
        target_user_id: UserId,
        notification_type: &str,
        channel_id: ChannelId,
        channel_type: i32,
        reader_id: UserId,
        last_read_pts: u64,
    ) -> Result<()> {
        // 已读游标事件必须带非零序列，避免客户端把该事件当作无效/旧版本丢弃。
        // 这里使用 read_pts 作为单调序列来源（同频道内只增不减）。
        let seq_u32 = u32::try_from(last_read_pts)
            .unwrap_or(u32::MAX)
            .max(1);
        let server_message_id = u64::from(seq_u32);
        let payload = ChannelReadCursorNotification::new(
            channel_id,
            channel_type,
            reader_id,
            last_read_pts,
            notification_type,
            chrono::Utc::now().timestamp_millis(),
        );
        let notification = PushMessageRequest {
            setting: Default::default(),
            msg_key: String::new(),
            server_message_id,
            message_seq: seq_u32,
            local_message_id: 0,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: chrono::Utc::now().timestamp() as u32,
            channel_id,
            channel_type: channel_type as u8,
            message_type: privchat_protocol::ContentMessageType::System.as_u32(),
            expire: 0,
            topic: String::new(),
            from_uid: reader_id,
            payload: serde_json::to_vec(&payload)
                .map_err(|e| ServerError::Serialization(e.to_string()))?,
            deleted: false,
        };
        self.message_router
            .route_message_to_user(&target_user_id, notification)
            .await
            .map_err(|e| ServerError::Network(format!("发送 read cursor 事件失败: {}", e)))?;
        Ok(())
    }
}
