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

//! MessageService — 通用消息发送服务
//!
//! 封装完整的服务端消息发送链路：
//! 分配 pts → 写入 DB → 记录 sync commit → 写入 user_message_index
//! → 更新未读计数 → 实时推送 / 离线队列 → 更新 last_message
//!
//! 供登录通知、Admin API 发消息、系统公告等所有服务端主动发消息场景复用。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use privchat_protocol::protocol::{MessageSetting, PushMessageRequest};
use privchat_protocol::ContentMessageType;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::error::ServerError;
use crate::infra::ConnectionManager;
use crate::model::channel::MemberRole;
use crate::model::message::Message;
use crate::model::pts::{PtsGenerator, UserMessageIndex};
use crate::repository::{MessageRepository, PgMessageRepository};
use crate::service::message_history_service::MessageHistoryService;
use crate::service::sync::get_global_sync_service;
use crate::service::{ChannelService, OfflineQueueService, UnreadCountService};

/// 撤回时限（秒）——普通用户在此窗口内才能撤回自己的消息；群主/管理员不受限。
pub const DEFAULT_REVOKE_TIME_LIMIT_SECS: i64 = 172800; // 48h

/// 服务端发消息的请求参数
pub struct ServerSendMessageRequest {
    /// 目标频道 ID
    pub channel_id: u64,
    /// 发送者用户 ID（系统消息传 SYSTEM_USER_ID）
    pub sender_id: u64,
    /// 消息内容
    pub content: String,
    /// 消息类型
    pub message_type: ContentMessageType,
    /// 元数据（可选）
    pub metadata: Value,
    /// 频道类型（1=DM, 2=Group）
    pub channel_type: u8,
    /// 需要接收该消息的用户 ID 列表
    pub recipient_user_ids: Vec<u64>,
}

/// 服务端发消息的结果
pub struct ServerSendMessageResult {
    /// 分配的消息 ID
    pub message_id: u64,
    /// 分配的 pts
    pub pts: u64,
    /// 创建时间（毫秒时间戳）
    pub created_at: i64,
}

/// 撤回成功后的摘要——供 HTTP / RPC 入口层装配响应。
pub struct RevokedMessageSummary {
    pub message_id: u64,
    pub channel_id: u64,
    pub channel_type: u8,
    pub revoker_id: u64,
    /// 撤回事件推送时间戳（毫秒）
    pub revoked_at_ms: i64,
}

pub struct MessageService {
    channel_service: Arc<ChannelService>,
    pts_generator: Arc<PtsGenerator>,
    message_repository: Arc<PgMessageRepository>,
    user_message_index: Arc<UserMessageIndex>,
    offline_queue_service: Arc<OfflineQueueService>,
    connection_manager: Arc<ConnectionManager>,
    unread_count_service: Arc<UnreadCountService>,
    message_history_service: Arc<MessageHistoryService>,
}

impl MessageService {
    pub fn new(
        channel_service: Arc<ChannelService>,
        pts_generator: Arc<PtsGenerator>,
        message_repository: Arc<PgMessageRepository>,
        user_message_index: Arc<UserMessageIndex>,
        offline_queue_service: Arc<OfflineQueueService>,
        connection_manager: Arc<ConnectionManager>,
        unread_count_service: Arc<UnreadCountService>,
        message_history_service: Arc<MessageHistoryService>,
    ) -> Self {
        Self {
            channel_service,
            pts_generator,
            message_repository,
            user_message_index,
            offline_queue_service,
            connection_manager,
            unread_count_service,
            message_history_service,
        }
    }

    /// 服务端发送消息的完整链路
    ///
    /// 完整流程：
    /// 1. 分配 pts
    /// 2. 写入 DB
    /// 3. 记录 sync commit
    /// 4. 写入 user_message_index
    /// 5. 更新未读计数
    /// 6. 构建 PushMessageRequest
    /// 7. 实时推送 + 离线队列
    /// 8. 更新 last_message
    pub async fn send_message(
        &self,
        req: ServerSendMessageRequest,
    ) -> anyhow::Result<ServerSendMessageResult> {
        let now = Utc::now();
        let message_id = crate::infra::next_message_id();

        // 1. 分配 pts
        let pts = if let Some(sync_service) = get_global_sync_service() {
            match sync_service.allocate_next_pts(req.channel_id).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "⚠️ MessageService: 分配同步 pts 失败，回退内存计数器: channel_id={}, error={}",
                        req.channel_id, e
                    );
                    self.pts_generator.next_pts(req.channel_id).await
                }
            }
        } else {
            self.pts_generator.next_pts(req.channel_id).await
        };

        // 2. 写入 DB
        let msg = Message {
            message_id,
            channel_id: req.channel_id,
            sender_id: req.sender_id,
            pts: Some(pts as i64),
            local_message_id: Some(message_id),
            content: req.content.clone(),
            message_type: req.message_type,
            metadata: req.metadata.clone(),
            reply_to_message_id: None,
            created_at: now,
            updated_at: now,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };
        self.message_repository
            .create(&msg)
            .await
            .map_err(|e| anyhow::anyhow!("写入消息失败: {}", e))?;

        // 3. 记录 sync commit
        if let Some(sync_service) = get_global_sync_service() {
            let commit = privchat_protocol::rpc::sync::ServerCommit {
                pts,
                server_msg_id: message_id,
                local_message_id: Some(message_id),
                channel_id: req.channel_id,
                channel_type: req.channel_type,
                message_type: req.message_type.as_str().to_string(),
                content: serde_json::json!({ "text": req.content }),
                server_timestamp: now.timestamp_millis(),
                sender_id: req.sender_id,
                sender_info: None,
            };
            if let Err(e) = sync_service.record_existing_commit(&commit).await {
                warn!(
                    "⚠️ MessageService: 记录 sync commit 失败: channel_id={}, error={}",
                    req.channel_id, e
                );
            }
        }

        // 4. 写入 user_message_index + 5. 更新未读计数
        let receivers: Vec<u64> = req
            .recipient_user_ids
            .iter()
            .filter(|uid| **uid != req.sender_id)
            .copied()
            .collect();

        for &uid in &receivers {
            self.user_message_index
                .add_message(uid, pts, message_id)
                .await;
        }

        if !receivers.is_empty() {
            for &uid in &receivers {
                if let Err(e) = self.unread_count_service.increment(uid, req.channel_id, 1).await {
                    warn!(
                        "⚠️ MessageService: 更新未读计数失败: user_id={}, channel_id={}, error={}",
                        uid, req.channel_id, e
                    );
                }
            }
            if let Err(e) = self
                .channel_service
                .increment_user_channel_unread(req.channel_id, &receivers, 1)
                .await
            {
                warn!(
                    "⚠️ MessageService: 更新 user_channels 未读失败: channel_id={}, error={}",
                    req.channel_id, e
                );
            }
        }

        // 6. 构建 PushMessageRequest
        let payload = serde_json::to_vec(&serde_json::json!({ "content": req.content }))
            .map_err(|e| anyhow::anyhow!("encode payload failed: {}", e))?;

        let push_msg = privchat_protocol::protocol::PushMessageRequest {
            setting: privchat_protocol::protocol::MessageSetting::default(),
            msg_key: format!("msg_{}", message_id),
            server_message_id: message_id,
            message_seq: u32::try_from(pts).unwrap_or(u32::MAX),
            local_message_id: message_id,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: now.timestamp().max(0) as u32,
            channel_id: req.channel_id,
            channel_type: req.channel_type,
            message_type: req.message_type.as_u32(),
            expire: 0,
            topic: String::new(),
            from_uid: req.sender_id,
            payload,
            deleted: false,
        };

        // 7. 推送给接收者（跳过 sender，sender 通过 sync pull 同步到自己其他设备）
        //    实时推送成功则跳过离线队列；否则写入离线队列等上线拉取
        for &uid in &req.recipient_user_ids {
            if uid == req.sender_id {
                continue;
            }
            let online_sessions = match self
                .connection_manager
                .send_push_to_user(uid, &push_msg)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "⚠️ MessageService: 实时推送失败: user_id={}, error={:?}",
                        uid, e
                    );
                    0
                }
            };

            if online_sessions > 0 {
                info!(
                    "📡 MessageService: 实时推送成功 user_id={}, sessions={}",
                    uid, online_sessions
                );
            } else {
                crate::infra::metrics::increment_offline_enqueue(1);
                tracing::info!(
                    target: "delivery.offline_enqueue",
                    user_id = uid,
                    server_message_id = push_msg.server_message_id,
                    source = "MessageService",
                    "zero-success delivery queued to offline"
                );
                if let Err(e) = self.offline_queue_service.add(uid, &push_msg).await {
                    warn!(
                        "⚠️ MessageService: 离线队列写入失败: user_id={}, error={:?}",
                        uid, e
                    );
                }
            }
        }

        // 8. 更新 last_message
        let _ = self
            .channel_service
            .update_last_message(req.channel_id, message_id)
            .await;

        info!(
            "✅ MessageService: 消息发送完成 channel_id={}, message_id={}, pts={}, recipients={}",
            req.channel_id,
            message_id,
            pts,
            req.recipient_user_ids.len()
        );

        Ok(ServerSendMessageResult {
            message_id,
            pts,
            created_at: now.timestamp_millis(),
        })
    }

    /// RPC 入口——带权限与时限校验的撤回（`rpc/message/revoke.rs` 使用）。
    ///
    /// - 发送者或群主/管理员可撤回
    /// - 普通发送者受 [`DEFAULT_REVOKE_TIME_LIMIT_SECS`] 限制
    /// - 权限通过后进入 [`revoke_message_core`] 执行完整副作用编排
    pub async fn revoke_message_with_auth(
        &self,
        message_id: u64,
        channel_id: u64,
        user_id: u64,
    ) -> std::result::Result<RevokedMessageSummary, ServerError> {
        let message = self
            .message_repository
            .find_by_id(message_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询消息失败: {}", e)))?
            .ok_or_else(|| ServerError::NotFound("消息不存在".to_string()))?;

        if message.revoked {
            return Err(ServerError::Validation("消息已被撤回".to_string()));
        }

        let is_sender = message.sender_id == user_id;
        let is_admin = self
            .channel_service
            .get_channel(&channel_id)
            .await
            .ok()
            .and_then(|ch| ch.members.get(&user_id).map(|m| m.role))
            .map(|role| matches!(role, MemberRole::Owner | MemberRole::Admin))
            .unwrap_or(false);

        if !is_sender && !is_admin {
            return Err(ServerError::Forbidden(
                "只有发送者或群管理员可以撤回消息".to_string(),
            ));
        }

        if is_sender && !is_admin {
            let now_ts = Utc::now().timestamp();
            let msg_ts = message.created_at.timestamp();
            if now_ts - msg_ts > DEFAULT_REVOKE_TIME_LIMIT_SECS {
                return Err(ServerError::Validation(format!(
                    "消息发送已超过 {} 小时，无法撤回",
                    DEFAULT_REVOKE_TIME_LIMIT_SECS / 3600
                )));
            }
        }

        self.revoke_message_core(message_id, message.channel_id, user_id)
            .await
    }

    /// Admin 入口——跳过发送者/时限校验，仅做"消息存在且未撤回"前置。
    ///
    /// `admin_revoker_id` 必须是真实管理员上下文的用户 ID，禁止传 0。系统级默认
    /// 使用 [`crate::config::SYSTEM_USER_ID`]。
    pub async fn revoke_message_admin(
        &self,
        message_id: u64,
        admin_revoker_id: u64,
    ) -> std::result::Result<RevokedMessageSummary, ServerError> {
        debug_assert!(
            admin_revoker_id != 0,
            "revoke_message_admin 禁止使用 0 作为 revoker_id，请传入真实管理员或 SYSTEM_USER_ID"
        );

        let message = self
            .message_repository
            .find_by_id(message_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询消息失败: {}", e)))?
            .ok_or_else(|| ServerError::NotFound(format!("消息 {} 不存在", message_id)))?;

        if message.revoked {
            return Err(ServerError::Validation("消息已被撤回".to_string()));
        }

        self.revoke_message_core(message_id, message.channel_id, admin_revoker_id)
            .await
    }

    /// 撤回的完整副作用编排——**唯一**写点，供 `_with_auth` / `_admin` 共享。
    ///
    /// 步骤：
    /// 1. DB 标记撤回
    /// 2. 发 `DomainEvent::MessageRevoked` 到 event_bus
    /// 3. 更新 `message_history_service` 内存缓存
    /// 4. 写 sync `message.revoke` commit（PTS）
    /// 5. 推送撤回事件给所有参与者
    /// 6. 从所有参与者的离线队列中删除该消息
    async fn revoke_message_core(
        &self,
        message_id: u64,
        channel_id: u64,
        revoker_id: u64,
    ) -> std::result::Result<RevokedMessageSummary, ServerError> {
        // 1. DB 标记撤回
        let revoked_msg = self
            .message_repository
            .revoke_message(message_id, revoker_id)
            .await
            .map_err(|e| match e {
                crate::error::DatabaseError::NotFound(msg) => ServerError::NotFound(msg),
                crate::error::DatabaseError::Validation(msg) => ServerError::Validation(msg),
                _ => ServerError::Database(format!("撤回消息失败: {}", e)),
            })?;

        tracing::debug!(
            "✅ 消息已在数据库中标记为撤回: message_id={}, revoked_by={}",
            message_id,
            revoker_id
        );

        // 2. 发布 MessageRevoked 事件
        if let Some(event_bus) = crate::handler::send_message_handler::get_global_event_bus() {
            let event = crate::domain::events::DomainEvent::MessageRevoked {
                message_id,
                conversation_id: channel_id,
                revoker_id,
                timestamp: Utc::now().timestamp(),
            };
            if let Err(e) = event_bus.publish(event) {
                warn!("⚠️ 发布 MessageRevoked 事件失败: {}", e);
            }
        }

        // 3. 更新内存缓存
        if let Err(e) = self
            .message_history_service
            .revoke_message(&message_id, &revoker_id)
            .await
        {
            match e {
                ServerError::NotFound(_) | ServerError::MessageNotFound(_) => {
                    tracing::debug!(
                        "ℹ️ 内存缓存未命中，跳过撤回状态同步: channel_id={}, message_id={}",
                        channel_id,
                        message_id
                    );
                }
                other => warn!("⚠️ 更新内存缓存失败: {}", other),
            }
        }

        // 解析 channel_type
        let channel_type = self
            .channel_service
            .get_channel(&channel_id)
            .await
            .ok()
            .map(|ch| ch.channel_type.to_i16() as u8)
            .unwrap_or(1);
        let channel_type = if channel_type == 0 { 1 } else { channel_type };

        // 4. 写 sync commit
        let revoke_ts_ms = Utc::now().timestamp_millis();
        let revoke_payload = json!({
            "message_id": message_id,
            "channel_id": channel_id,
            "channel_type": channel_type,
            "revoke": true,
            "revoked_by": revoker_id,
            "revoked_at": revoke_ts_ms,
        });
        if let Some(sync_service) = get_global_sync_service() {
            if let Err(e) = sync_service
                .append_server_event_commit(
                    channel_id,
                    channel_type,
                    "message.revoke",
                    revoke_payload,
                    revoker_id,
                )
                .await
            {
                warn!("⚠️ 写入 revoke pts commit 失败: {}", e);
            }
        } else {
            warn!("⚠️ SyncService 未初始化，跳过 revoke pts commit");
        }

        // 5. 推送撤回事件给所有参与者
        if let Err(e) = self
            .distribute_revoke_event(channel_id, channel_type, message_id, revoker_id)
            .await
        {
            warn!("⚠️ 推送撤回事件失败: {}，但消息已撤回", e);
        }

        // 6. 清理所有参与者的离线队列
        match self.channel_service.get_channel_participants(channel_id).await {
            Ok(participants) => {
                for p in participants {
                    if let Err(e) = self
                        .offline_queue_service
                        .remove_message_by_id(p.user_id, message_id)
                        .await
                    {
                        warn!(
                            "⚠️ 从用户 {} 的离线队列中删除消息 {} 失败: {}",
                            p.user_id, message_id, e
                        );
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ 获取会话参与者失败，离线队列清理跳过: {}", e);
            }
        }

        tracing::debug!("✅ 消息撤回完成: message_id={}", message_id);

        let revoked_at_ms = revoked_msg
            .revoked_at
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(revoke_ts_ms);

        Ok(RevokedMessageSummary {
            message_id,
            channel_id,
            channel_type,
            revoker_id,
            revoked_at_ms,
        })
    }

    /// 推送撤回事件给会话内所有参与者——使用与主发消息相同的 `ConnectionManager`
    /// 入口（`CONNECTION_LIFECYCLE_SPEC §8.8`），不走离线队列（撤回事件只通知在线端，
    /// 离线用户通过离线队列清理保证不会再收到原消息）。
    async fn distribute_revoke_event(
        &self,
        channel_id: u64,
        channel_type: u8,
        message_id: u64,
        revoked_by: u64,
    ) -> std::result::Result<(), ServerError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let event_payload = json!({
            "revoked_by": revoked_by,
            "revoked_at": now_ms,
        });

        let revoke_event = PushMessageRequest {
            setting: MessageSetting {
                need_receipt: false,
                signal: 0,
            },
            msg_key: format!("revoke_{}", message_id),
            server_message_id: message_id,
            message_seq: 0,
            local_message_id: 0,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: (now_ms / 1000) as u32,
            channel_id,
            channel_type,
            message_type: ContentMessageType::System.as_u32(),
            expire: 0,
            topic: "message.revoke".to_string(),
            from_uid: revoked_by,
            payload: event_payload.to_string().into_bytes(),
            deleted: true,
        };

        let participants = self
            .channel_service
            .get_channel_participants(channel_id)
            .await
            .map_err(|e| ServerError::Database(format!("获取会话参与者失败: {}", e)))?;

        info!(
            "📣 开始分发撤回事件: channel_id={}, channel_type={}, message_id={}, revoked_by={}, participants={}",
            channel_id,
            channel_type,
            message_id,
            revoked_by,
            participants.len()
        );

        for participant in participants {
            match self
                .connection_manager
                .send_push_to_user(participant.user_id, &revoke_event)
                .await
            {
                Ok(success_count) => {
                    info!(
                        "📤 撤回事件推送: target_user={}, message_id={}, success_sessions={}",
                        participant.user_id, message_id, success_count
                    );
                }
                Err(e) => {
                    warn!(
                        "⚠️ 推送撤回事件给用户 {} 失败: {:?}",
                        participant.user_id, e
                    );
                }
            }
        }

        Ok(())
    }

    // ============================================================
    // admin 只读聚合（ADMIN_PATH_CONVERGENCE_AUDIT §3）
    //
    // admin handler 不得直连 PgMessageRepository —— 下列方法是纯 pass-through + 错误映射,
    // 若未来要加缓存 / 审计 / 限流 hook,都收敛在这里。
    // ============================================================

    pub async fn admin_list(
        &self,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<Value>, u32), ServerError> {
        self.message_repository
            .list_messages_admin(channel_id, user_id, start_time, end_time, page, page_size)
            .await
            .map_err(|e| ServerError::Database(format!("查询消息列表失败: {}", e)))
    }

    pub async fn admin_get(&self, message_id: u64) -> Result<Option<Value>, ServerError> {
        self.message_repository
            .get_message_admin(message_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询消息详情失败: {}", e)))
    }

    pub async fn admin_search(
        &self,
        keyword: &str,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        message_type: Option<i16>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<Value>, u32), ServerError> {
        MessageRepository::search_messages(
            self.message_repository.as_ref(),
            keyword,
            channel_id,
            user_id,
            message_type,
            start_time,
            end_time,
            page,
            page_size,
        )
        .await
        .map_err(|e| ServerError::Database(format!("搜索消息失败: {}", e)))
    }

    pub async fn admin_stats(&self) -> Result<Value, ServerError> {
        self.message_repository
            .get_message_stats_admin()
            .await
            .map_err(|e| ServerError::Database(format!("获取消息统计失败: {}", e)))
    }
}
