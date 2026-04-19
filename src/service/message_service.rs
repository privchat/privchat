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

use chrono::Utc;
use privchat_protocol::ContentMessageType;
use serde_json::Value;
use tracing::{info, warn};

use crate::infra::ConnectionManager;
use crate::model::message::Message;
use crate::model::pts::{PtsGenerator, UserMessageIndex};
use crate::repository::{MessageRepository, PgMessageRepository};
use crate::service::sync::get_global_sync_service;
use crate::service::{ChannelService, OfflineQueueService, UnreadCountService};

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

pub struct MessageService {
    channel_service: Arc<ChannelService>,
    pts_generator: Arc<PtsGenerator>,
    message_repository: Arc<PgMessageRepository>,
    user_message_index: Arc<UserMessageIndex>,
    offline_queue_service: Arc<OfflineQueueService>,
    connection_manager: Arc<ConnectionManager>,
    unread_count_service: Arc<UnreadCountService>,
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
    ) -> Self {
        Self {
            channel_service,
            pts_generator,
            message_repository,
            user_message_index,
            offline_queue_service,
            connection_manager,
            unread_count_service,
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
                info!(
                    "📭 MessageService: 用户离线，写入离线队列 user_id={}",
                    uid
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
}
