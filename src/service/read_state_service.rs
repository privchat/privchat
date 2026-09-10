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
    /// 本次推进之前的水位。群聚合通知只处理 `(previous_pts, last_read_pts]` 这一段，
    /// 不重扫整段历史。首次建行时为 0。
    previous_pts: u64,
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
            COALESCE((SELECT last_read_pts FROM existing LIMIT 1), 0) AS previous_pts,
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
        previous_pts: upserted.get::<i64, _>("previous_pts").max(0) as u64,
    })
}

/// 已读状态服务（read_pts 单一路径）
pub struct ReadStateService {
    channel_service: Arc<ChannelService>,
    unread_count_service: Arc<UnreadCountService>,
    message_router: Arc<MessageRouter>,
    pool: Arc<PgPool>,
    delivery_tracker: Option<Arc<crate::service::DeliveryTracker>>,
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
            delivery_tracker: None,
        }
    }

    pub fn set_delivery_tracker(&mut self, tracker: Arc<crate::service::DeliveryTracker>) {
        self.delivery_tracker = Some(tracker);
    }

    /// 获取指定用户在指定频道的连续送达水位
    pub async fn get_server_delivered_pts(&self, user_id: UserId, channel_id: ChannelId) -> u64 {
        match &self.delivery_tracker {
            Some(tracker) => tracker.get_delivered_pts(user_id, channel_id).await,
            None => u64::MAX,
        }
    }

    pub async fn mark_read_pts(
        &self,
        reader_id: UserId,
        channel_id: ChannelId,
        read_pts: u64,
    ) -> Result<ReadPtsUpdateResult> {
        self.mark_read_pts_with_visible(reader_id, channel_id, read_pts, None)
            .await
    }

    /// 双重水位裁剪版 mark_read_pts
    ///
    /// accepted_read_pts = min(requested_read_pts, client_visible_pts, server_delivered_contiguous_pts)
    pub async fn mark_read_pts_with_visible(
        &self,
        reader_id: UserId,
        channel_id: ChannelId,
        read_pts: u64,
        client_visible_pts: Option<u64>,
    ) -> Result<ReadPtsUpdateResult> {
        // 以 privchat_channel_pts.current_pts 为 pts 的唯一权威来源（与 sync/get_channel_pts、
        // sync/submit 的分配器一致），privchat_messages 只是下游投影，允许异步落地。
        // 用投影表校验会出现 current_pts 先行、MAX(pts) 尚未落库的瞬时越界，从而误拒合法的 markRead。
        let channel_pts_row = sqlx::query(
            "SELECT COALESCE(current_pts, 0) AS current_pts FROM privchat_channel_pts WHERE channel_id = $1",
        )
        .bind(channel_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询频道 current_pts 失败: {}", e)))?;
        let current_pts = channel_pts_row
            .map(|row| row.get::<i64, _>("current_pts") as u64)
            .unwrap_or(0);
        if read_pts > current_pts {
            return Err(ServerError::Validation(format!(
                "read_pts 超出频道 current_pts: read_pts={}, current_pts={}",
                read_pts, current_pts
            )));
        }

        // 单一水位裁剪：accepted = min(requested, client_visible)
        //
        // 为何不再用 server_delivered 裁剪：delivery_tracker.try_advance 仅在严格连续
        // (pts == current+1) 时推进，任一条消息因 TCP 路由瞬时失败而漏 ack 都会让水位卡死，
        // 导致后续合法 markRead 被误压低到旧水位，私聊 `peer_read_pts_updated` 广播闸门
        // 因 advanced=false 而静默丢弃，最终发送方看不到「已读」。
        // 客户端调用 markRead(N) 在因果上意味着客户端已经看到 pts<=N（无论走 push 还是
        // sync 路径），应以客户端声明为权威。delivery_tracker 退化为本地 telemetry，
        // 由本函数末尾按 effective_read_pts 反向修复对齐。
        let mut effective_read_pts = read_pts;

        if let Some(visible) = client_visible_pts {
            effective_read_pts = effective_read_pts.min(visible);
        }

        if effective_read_pts < read_pts {
            tracing::info!(
                "📏 mark_read_pts 水位裁剪: user={} channel={} requested={} accepted={} client_visible={:?}",
                reader_id, channel_id, read_pts, effective_read_pts, client_visible_pts
            );
        }

        if effective_read_pts == 0 {
            return Ok(ReadPtsUpdateResult {
                channel_id,
                last_read_pts: 0,
            });
        }

        // 因果对齐：客户端已读 effective_read_pts ⇒ 已收到 effective_read_pts。
        // 若 tracker 因 try_advance gap 落后于真实送达，这里强制对齐。
        if let Some(tracker) = &self.delivery_tracker {
            let current_delivered = tracker.get_delivered_pts(reader_id, channel_id).await;
            if effective_read_pts > current_delivered {
                tracker
                    .force_set(reader_id, channel_id, effective_read_pts)
                    .await;
            }
        }

        let new_last_read_pts = self
            .channel_service
            .mark_read_pts(&reader_id, &channel_id, effective_read_pts)
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
            self.broadcast_read_cursor(
                reader_id,
                channel_id,
                upserted.previous_pts,
                upserted.last_read_pts,
            )
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
        // Returns two row classes in one page:
        //   1. self rows  — caller's own cursor for every channel
        //   2. peer rows  — for direct channels (channel_type=0), the
        //      OTHER party's cursor on the same channel. Lets clients
        //      hydrate `peer_read_pts` baseline at bootstrap so "已读"
        //      survives re-login (otherwise read state only flows via
        //      live `peer_read_pts_updated` push). See client-side
        //      `absorbPeerReadCursor`.
        //
        // Both classes share `sync_version` as the monotonic page
        // cursor — peer rows are real channel_read_cursor rows with
        // their own sync_version, so increasing-since scans work
        // unchanged. Caller distinguishes self/peer via `user_id` in
        // the payload.
        //
        // Group channels (channel_type=1 by channels-table convention)
        // are excluded from the peer branch because group "已读"
        // semantics need per-member cursors and a different surface.
        let db_rows = sqlx::query(
            r#"
            WITH self_rows AS (
                SELECT channel_id, user_id, last_read_pts, sync_version
                FROM privchat_channel_read_cursor
                WHERE user_id = $1
                  AND sync_version > $2
                  AND ($3::BIGINT IS NULL OR channel_id = $3)
            ),
            peer_rows AS (
                SELECT cur.channel_id, cur.user_id, cur.last_read_pts, cur.sync_version
                FROM privchat_channel_read_cursor cur
                JOIN privchat_channels ch ON ch.channel_id = cur.channel_id
                WHERE ch.channel_type = 0
                  AND cur.sync_version > $2
                  AND ($3::BIGINT IS NULL OR cur.channel_id = $3)
                  AND cur.user_id <> $1
                  AND (
                    (ch.direct_user1_id = $1 AND cur.user_id = ch.direct_user2_id)
                    OR
                    (ch.direct_user2_id = $1 AND cur.user_id = ch.direct_user1_id)
                  )
            )
            SELECT channel_id, user_id, last_read_pts, sync_version FROM self_rows
            UNION ALL
            SELECT channel_id, user_id, last_read_pts, sync_version FROM peer_rows
            ORDER BY sync_version ASC, channel_id ASC, user_id ASC
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

    /// 本次推进**新覆盖**的那段消息里，由别人发出的消息作者去重集合。
    ///
    /// 只发给这些人——「你的消息有人读了」，不给全群广播：一次已读上报可能跨很多条
    /// 消息，逐条逐人推会变成推送风暴（§6.5.8）。
    ///
    /// 范围是 `(previous_pts, last_read_pts]` 而不是 `<= last_read_pts`：后者每次推进
    /// 都要重扫整段历史、把早已通知过的作者再通知一遍，聚合状态其实没有变化。
    async fn list_message_authors_in_range(
        &self,
        channel_id: ChannelId,
        previous_pts: u64,
        last_read_pts: u64,
        reader_id: UserId,
    ) -> Result<Vec<UserId>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT sender_id
            FROM privchat_messages
            WHERE channel_id = $1
              AND pts > $2
              AND pts <= $3
              AND sender_id <> $4
              AND revoked = false
            "#,
        )
        .bind(channel_id as i64)
        .bind(previous_pts as i64)
        .bind(last_read_pts as i64)
        .bind(reader_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询消息作者失败: {}", e)))?;
        Ok(rows
            .into_iter()
            .map(|row| row.get::<i64, _>("sender_id") as u64)
            .collect())
    }

    /// 键集分页版：只取 `user_id > after_user_id` 的一页。
    ///
    /// offset 分页在这里是错的——名单会持续增长。第一页拿到 [20,30] 之后有人（uid=10）
    /// 读了，第二页从 offset=2 开始会把 30 再返回一次。按稳定键翻页没有这个问题，
    /// 新读者靠刷新看到，符合 spec §6.5.7 的「查询时刻实时结果」语义。
    pub async fn page_read_members_by_message_pts(
        &self,
        channel_id: ChannelId,
        message_pts: u64,
        member_ids: &[UserId],
        after_user_id: u64,
        limit: u32,
    ) -> Result<Vec<ChannelReadMemberRow>> {
        let ids: Vec<i64> = member_ids.iter().map(|v| *v as i64).collect();
        let rows = sqlx::query(
            r#"
            SELECT user_id, last_read_pts, updated_at
            FROM privchat_channel_read_cursor
            WHERE channel_id = $1
              AND user_id = ANY($2)
              AND last_read_pts >= $3
              AND user_id > $4
            ORDER BY user_id
            LIMIT $5
            "#,
        )
        .bind(channel_id as i64)
        .bind(&ids)
        .bind(message_pts as i64)
        .bind(after_user_id as i64)
        .bind(limit as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询 read_list 分页失败: {}", e)))?;
        Ok(rows
            .into_iter()
            .map(|row| ChannelReadMemberRow {
                user_id: row.get::<i64, _>("user_id") as u64,
                last_read_pts: row.get::<i64, _>("last_read_pts") as u64,
                updated_at: row.get("updated_at"),
            })
            .collect())
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
        previous_pts: u64,
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
            // Direct 对端权威识别（direct_user1/2，不靠 members——脏数据/缺行都对）。
            if let Some(peer_id) = channel.direct_peer(reader_id) {
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
        } else if matches!(channel_kind, ChannelKind::GroupChat) {
            // 群：只推**聚合**变化，不广播全群每个人的阅读轨迹（READ_STATUS_SPEC §6.5.8）。
            //
            // 这里之前只有 Direct 分支，群读完谁都不知道——发送者的气泡永远停在"已发送"。
            //
            // 通知只发给**消息的发送者**们（也就是水位以内、非本人发的消息的作者），
            // 语义是"你的消息有人读了"。收到的一方只需要知道"变成已读了"，
            // 不需要知道是谁——那属于按需查询的明细层。
            // 🔴 数据库错误不吞。原来这里 `unwrap_or_default()`，SQL 写错列名后
            // 直接退化成"没人需要通知"，群通知静默不发，而且不会有任何报错。
            let authors = self
                .list_message_authors_in_range(channel_id, previous_pts, last_read_pts, reader_id)
                .await?;
            for author_id in authors {
                // 🔴 聚合必须是**面向该接收者的权威结果**，不是把某个人的游标改个事件名。
                //
                // 这里下发的是「除你之外，其他人里读得最靠前的位置」——它是全群的 MAX，
                // 不指向任何具体的人。之前直接复用 send_read_cursor_event，payload 和
                // 外层 from_uid 都带 reader_id，等于把逐人阅读轨迹长期推给作者，
                // 客户端可以绕开名单接口和 7 天窗口拿到谁读过。
                let aggregate_pts = self
                    .aggregate_read_pts_excluding(channel_id, author_id)
                    .await?;
                if aggregate_pts == 0 {
                    continue;
                }
                self.send_group_read_aggregate_event(
                    author_id,
                    channel_id,
                    channel_type,
                    aggregate_pts,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// 除 `exclude_user` 之外，本频道其他成员的已读水位最大值。
    ///
    /// 这是群聚合的定义：**任意一人读到这里**，所以是 MAX 而不是某个人的游标。
    /// 它不指向具体的人，因此可以长期下发而不泄露阅读者身份。
    async fn aggregate_read_pts_excluding(
        &self,
        channel_id: ChannelId,
        exclude_user: UserId,
    ) -> Result<u64> {
        let row: Option<(Option<i64>,)> = sqlx::query_as(
            r#"
            SELECT MAX(last_read_pts)
            FROM privchat_channel_read_cursor
            WHERE channel_id = $1 AND user_id <> $2
            "#,
        )
        .bind(channel_id as i64)
        .bind(exclude_user as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询群聚合已读水位失败: {}", e)))?;
        Ok(row.and_then(|r| r.0).unwrap_or(0).max(0) as u64)
    }

    /// 群聚合已读通知：**不带阅读者身份**。
    ///
    /// `reader_id` 与外层 `from_uid` 都置 0，表示"这是聚合，不指向任何人"。
    async fn send_group_read_aggregate_event(
        &self,
        target_user_id: UserId,
        channel_id: ChannelId,
        channel_type: i32,
        aggregate_read_pts: u64,
    ) -> Result<()> {
        let seq_u32 = u32::try_from(aggregate_read_pts).unwrap_or(u32::MAX).max(1);
        let payload = ChannelReadCursorNotification::new(
            channel_id,
            channel_type,
            0, // 匿名：聚合不指向任何具体阅读者
            aggregate_read_pts,
            "group_read_aggregate_updated",
            chrono::Utc::now().timestamp_millis(),
        );
        let notification = PushMessageRequest {
            setting: Default::default(),
            msg_key: String::new(),
            server_message_id: u64::from(seq_u32),
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
            from_uid: 0, // 同上：不暴露阅读者
            payload: serde_json::to_vec(&payload)
                .map_err(|e| ServerError::Serialization(e.to_string()))?,
            deleted: false,
        };
        self.message_router
            .route_message_to_user(&target_user_id, notification)
            .await
            .map_err(|e| ServerError::Network(format!("发送群聚合已读事件失败: {}", e)))?;
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
        let seq_u32 = u32::try_from(last_read_pts).unwrap_or(u32::MAX).max(1);
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
