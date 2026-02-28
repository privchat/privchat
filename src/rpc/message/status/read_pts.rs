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

//! 按 pts 推进已读（正确模型）：mark_read(channel_id, read_pts)
//!
//! 语义：本用户已读该频道内 pts <= read_pts 的所有消息。
//! O(1) 存储、O(1) 广播，天然幂等、单调。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::PushMessageRequest;
use serde_json::{json, Value};
use tracing::warn;

/// 处理 按 pts 推进已读 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("channel_id is required (must be u64)".to_string()))?;

    let read_pts = body
        .get("read_pts")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("read_pts is required (must be u64)".to_string()))?;

    tracing::debug!(
        "🔧 处理 read_pts 请求: user_id={}, channel_id={}, read_pts={}",
        user_id,
        channel_id,
        read_pts
    );

    let new_last_read_pts = match services
        .channel_service
        .mark_read_pts(&user_id, &channel_id, read_pts)
        .await
    {
        Ok(Some(pts)) => pts,
        Ok(None) => {
            warn!("用户 {} 不在频道 {} 或频道不存在", user_id, channel_id);
            return Err(RpcError::validation("用户不在该频道".to_string()));
        }
        Err(e) => {
            return Err(RpcError::internal(format!("更新已读 pts 失败: {}", e)));
        }
    };

    if let Err(e) = broadcast_user_read_pts(&services, user_id, channel_id, new_last_read_pts).await
    {
        warn!("⚠️ 广播 UserReadEvent 失败: {}，但已读 pts 已更新", e);
    }

    Ok(json!({
        "status": "success",
        "message": "已读 pts 已更新",
        "channel_id": channel_id,
        "last_read_pts": new_last_read_pts
    }))
}

/// 广播 UserReadEvent（O(1)）：一条事件，客户端用 msg.pts <= read_pts 标记已读
async fn broadcast_user_read_pts(
    services: &RpcServiceContext,
    reader_id: u64,
    channel_id: u64,
    read_pts: u64,
) -> Result<(), String> {
    let channel = match services.channel_service.get_channel(&channel_id).await {
        Ok(ch) => ch,
        Err(e) => {
            tracing::debug!(
                "频道不存在，跳过 read_pts 广播: channel_id={}, error={}",
                channel_id,
                e
            );
            return Ok(());
        }
    };

    use crate::model::channel::ChannelKind;
    let channel_kind: ChannelKind = channel.channel_type.clone().into();

    match channel_kind {
        ChannelKind::PrivateChat => {
            let other_user_id = channel.members.keys().find(|id| **id != reader_id).copied();
            if let Some(uid) = other_user_id {
                let payload = json!({
                    "message_type": "notification",
                    "content": "已读位置更新",
                    "metadata": {
                        "notification_type": "user_read_pts",
                        "channel_id": channel_id,
                        "reader_id": reader_id.to_string(),
                        "read_pts": read_pts,
                    }
                });
                let notification = PushMessageRequest {
                    setting: Default::default(),
                    msg_key: String::new(),
                    server_message_id: 0,
                    message_seq: 0,
                    local_message_id: 0,
                    stream_no: String::new(),
                    stream_seq: 0,
                    stream_flag: 0,
                    timestamp: chrono::Utc::now().timestamp() as u32,
                    channel_id,
                    channel_type: 1,
                    message_type: privchat_protocol::ContentMessageType::System.as_u32(),
                    expire: 0,
                    topic: String::new(),
                    from_uid: 0,
                    payload: serde_json::to_vec(&payload)
                        .map_err(|e| format!("序列化失败: {}", e))?,
                };
                services
                    .message_router
                    .route_message_to_user(&uid, notification)
                    .await
                    .map_err(|e| format!("发送失败: {}", e))?;
                tracing::debug!(
                    "✅ UserReadEvent(reader={}, channel={}, read_pts={}) 已发给 {}",
                    reader_id,
                    channel_id,
                    read_pts,
                    uid
                );
            }
        }
        ChannelKind::GroupChat => {
            tracing::debug!("群聊 read_pts 不主动广播，避免 O(n) 爆炸；发送者可按需查询");
        }
        _ => {}
    }

    Ok(())
}
